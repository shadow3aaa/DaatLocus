use super::{ApiError, ApiResult, ServiceState, access_actor, bounded, manage, owner, redact};
use crate::actors::{ApprovalDecision, ApprovalMode};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::Value;

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/approvals", get(global_list))
        .route(
            "/v1/approvals/{approval}",
            get(global_detail).post(global_resolve),
        )
        .route("/v1/actors/{actor}/approvals", get(list))
        .route(
            "/v1/actors/{actor}/approvals/{approval}",
            get(detail).post(resolve),
        )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    before: Option<i64>,
    limit: Option<usize>,
    #[serde(default)]
    awaiting_owner: bool,
}

async fn list(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    if query.before.is_some_and(|before| before < 0) {
        return Err(ApiError::invalid());
    }
    let limit = bounded(query.limit, 100)?;
    if !query.awaiting_owner
        && let Some(value) = super::remote::core_read(
            &state,
            &headers,
            &actor,
            super::ConversationRead::Approvals {
                before: query.before,
                limit: limit.min(32) as u8,
            },
        )
        .await?
    {
        return Ok(Json(value));
    }
    let page = if query.awaiting_owner {
        state
            .system
            .store()
            .pending_approvals(Some(&actor), query.before, limit)
    } else {
        state
            .system
            .store()
            .approvals(Some(&actor), query.before, limit)
    }
    .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(page).map_err(|_| ApiError::internal())?;
    if query.awaiting_owner && actor == crate::stage::shared::ACTOR_ID {
        aggregate_pending(&state, &mut value, Some(&actor), query.before, limit).await?;
    }
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}

async fn global_list(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    if query.before.is_some_and(|before| before < 0) {
        return Err(ApiError::invalid());
    }
    let cursor = state
        .system
        .store()
        .event_cursor()
        .map_err(ApiError::from_report)?;
    let limit = bounded(query.limit, 100)?;
    let page = if query.awaiting_owner {
        state
            .system
            .store()
            .pending_approvals(None, query.before, limit)
    } else {
        state.system.store().approvals(None, query.before, limit)
    }
    .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(page).map_err(|_| ApiError::internal())?;
    value["event_cursor"] = Value::from(cursor);
    if query.awaiting_owner {
        aggregate_pending(&state, &mut value, None, query.before, limit).await?;
    }
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn global_detail(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    if let Some((stage, id)) = split_device_id(&id) {
        return Ok(Json(device_approval(&state, stage, id, None).await?));
    }
    let approval = state
        .system
        .store()
        .approval(&id)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(approval).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn detail(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    if !id.contains('~')
        && let Some(value) = super::remote::core_read(
            &state,
            &headers,
            &actor,
            super::ConversationRead::Approval { id: id.clone() },
        )
        .await?
    {
        return Ok(Json(value));
    }
    if let Some((stage, id)) = split_device_id(&id) {
        owner(&state, &headers).await?;
        let value = device_approval(&state, stage, id, None).await?;
        if value["actor_id"] != actor && value["actor_id"] != format!("{stage}~{actor}") {
            return Err(ApiError::not_found());
        }
        return Ok(Json(value));
    }
    let approval = state
        .system
        .store()
        .approval(&id)
        .map_err(ApiError::from_report)?;
    if approval.actor_id != actor {
        return Err(ApiError::not_found());
    }
    let mut value = serde_json::to_value(approval).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Approve,
    Deny,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {
    decision: Decision,
}

async fn resolve(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, id)): Path<(String, String)>,
    Json(request): Json<ResolveRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    access_actor(&state, &headers, &actor, false).await?;
    if actor == crate::stage::shared::ACTOR_ID
        && !id.contains('~')
        && let Some(source) = super::remote::core_source(&state)
    {
        return super::remote::core_approval_resolution(
            state,
            headers,
            source,
            id,
            match request.decision {
                Decision::Approve => "approve",
                Decision::Deny => "deny",
            },
        )
        .await;
    }
    if let Some((stage, raw)) = split_device_id(&id) {
        let value = device_approval(&state, stage, raw, None).await?;
        if value["actor_id"] != actor && value["actor_id"] != format!("{stage}~{actor}") {
            return Err(ApiError::not_found());
        }
        return resolve_one(&state, id, request).await;
    }
    let approval = state
        .system
        .store()
        .approval(&id)
        .map_err(ApiError::from_report)?;
    if approval.actor_id != actor {
        return Err(ApiError::not_found());
    }
    resolve_one(&state, id, request).await
}

async fn global_resolve(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ResolveRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    resolve_one(&state, id, request).await
}

async fn resolve_one(
    state: &ServiceState,
    id: String,
    request: ResolveRequest,
) -> ApiResult<Json<Value>> {
    if let Some((stage, raw)) = split_device_id(&id) {
        let decision = match request.decision {
            Decision::Approve => ApprovalDecision::Approved,
            Decision::Deny => ApprovalDecision::Denied,
        };
        return Ok(Json(
            device_approval(state, stage, raw, Some(decision)).await?,
        ));
    }
    let approval = state
        .system
        .store()
        .approval(&id)
        .map_err(ApiError::from_report)?;
    if approval.mode != ApprovalMode::Ask {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: "This action does not await an owner decision",
        });
    }
    if approval.decision != ApprovalDecision::Pending {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: "Approval is already resolved",
        });
    }
    let (decision, reason) = match request.decision {
        Decision::Approve => (ApprovalDecision::Approved, "Approved by owner"),
        Decision::Deny => (ApprovalDecision::Denied, "Denied by owner"),
    };
    let approval = state
        .system
        .store()
        .resolve_approval(&id, decision, reason)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(approval).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
fn split_device_id(id: &str) -> Option<(crate::stage::StageId, &str)> {
    let (stage, id) = id.split_once('~')?;
    Some((
        crate::stage::StageId(uuid::Uuid::parse_str(stage).ok()?),
        id,
    ))
}
async fn device_approval(
    state: &ServiceState,
    stage: crate::stage::StageId,
    id: &str,
    decision: Option<ApprovalDecision>,
) -> ApiResult<Value> {
    let runtime = super::stages::runtime(state)?;
    let handle = runtime.handle(stage).map_err(super::stages::failure)?;
    let command = if let Some(decision) = decision {
        crate::stage::protocol::Body::ApprovalResolve {
            id: id.into(),
            decision,
        }
    } else {
        crate::stage::protocol::Body::ApprovalQuery { id: id.into() }
    };
    let crate::stage::protocol::Body::Approval { record } =
        handle.rpc(command).await.map_err(super::stages::failure)?
    else {
        return Err(ApiError::internal());
    };
    let mut value = serde_json::to_value(&record).map_err(|_| ApiError::internal())?;
    value["id"] = format!("{stage}~{}", record.id).into();
    value["actor_id"] = format!("{stage}~{}", record.actor_id).into();
    super::redact(&mut value, &state.system.config());
    Ok(value)
}
async fn aggregate_pending(
    state: &ServiceState,
    value: &mut Value,
    actor: Option<&str>,
    before: Option<i64>,
    limit: usize,
) -> ApiResult<()> {
    use crate::stage::{StageId, approval_index, protocol::Body};
    use futures_util::StreamExt;
    if let Some(runtime) = &state.stage
        && runtime.host.account_id().is_some()
    {
        let peers =
            crate::stage::ledger::StageLedger::attach(state.system.store(), runtime.host.local)
                .peers(chrono::Utc::now().timestamp_millis())
                .map_err(super::stages::failure)?;
        let reads=futures_util::stream::iter(peers).map(|peer|async move {
            let stage=peer.device.stage_id;let handle=runtime.handle(stage).ok()?;
            let before=state.system.store().stage_transaction(|tx|{tx.execute_batch("CREATE TABLE IF NOT EXISTS account_approval_sync(stage TEXT PRIMARY KEY,cursor INTEGER);")?;
                Ok(tx.query_row("SELECT cursor FROM account_approval_sync WHERE stage=?1",[stage.to_string()],|r|r.get::<_,Option<i64>>(0)).ok().flatten())}).ok()?;
            let Body::Approvals{records,next}=handle.rpc_with_timeout(Body::ApprovalsQuery{before,limit:32},std::time::Duration::from_secs(2)).await.ok()? else{return None;};
            for record in records{approval_index::record(state.system.store(),stage,&record.id,&record.actor_id,record.mode==ApprovalMode::Ask && record.decision==ApprovalDecision::Pending).ok()?;}
            state.system.store().stage_transaction(|tx|{tx.execute("INSERT INTO account_approval_sync VALUES(?1,?2) ON CONFLICT(stage) DO UPDATE SET cursor=excluded.cursor",rusqlite::params![stage.to_string(),next])?;Ok(())}).ok()?;Some(())
        }).buffer_unordered(4);
        reads.collect::<Vec<_>>().await;
    }
    let (refs, next) = approval_index::page(state.system.store(), actor, before, limit)
        .map_err(super::stages::failure)?;
    let items = futures_util::stream::iter(refs)
        .map(|(seq, source, id)| async move {
            let mut record = if source.is_empty() {
                serde_json::to_value(state.system.store().approval(&id).ok()?).ok()?
            } else {
                device_approval(
                    state,
                    StageId(uuid::Uuid::parse_str(&source).ok()?),
                    &id,
                    None,
                )
                .await
                .ok()?
            };
            if record["mode"] != "ask" || record["decision"] != "pending" {
                return None;
            }
            record["seq"] = seq.into();
            if let Some(actor) = actor {
                record["actor_id"] = actor.into();
            }
            Some(record)
        })
        .buffered(4)
        .filter_map(|item| async move { item })
        .collect::<Vec<_>>()
        .await;
    value["items"] = items.into();
    value["next_cursor"] = next.into();
    Ok(())
}
