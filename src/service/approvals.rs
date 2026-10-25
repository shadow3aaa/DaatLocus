//! Read-only sandbox application history. Owner decisions travel through Core.
use super::{ApiError, ApiResult, ServiceState, access_actor, bounded, owner, redact};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::Value;

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/approvals", get(global_list))
        .route("/v1/approvals/{approval}", get(global_detail))
        .route("/v1/actors/{actor}/approvals", get(list))
        .route("/v1/actors/{actor}/approvals/{approval}", get(detail))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    before: Option<i64>,
    limit: Option<usize>,
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
    if let Some(value) = super::remote::core_read(
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
    let page = state
        .system
        .store()
        .approvals(Some(&actor), query.before, limit)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(page).map_err(|_| ApiError::internal())?;
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
    let page = state
        .system
        .store()
        .approvals(None, query.before, bounded(query.limit, 100)?)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(page).map_err(|_| ApiError::internal())?;
    value["event_cursor"] = Value::from(cursor);
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
    let record = state
        .system
        .store()
        .approval(&id)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(record).map_err(|_| ApiError::internal())?;
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
    let mut value = if let Some((stage, id)) = split_device_id(&id) {
        owner(&state, &headers).await?;
        let prefix = format!("{stage}~");
        let origin = actor.strip_prefix(&prefix).unwrap_or(&actor);
        device_approval(&state, stage, id, Some(origin)).await?
    } else {
        let record = state
            .system
            .store()
            .approval(&id)
            .map_err(ApiError::from_report)?;
        if record
            .source_actor(state.system.store())
            .map_err(ApiError::from_report)?
            != actor
        {
            return Err(ApiError::not_found());
        }
        serde_json::to_value(record).map_err(|_| ApiError::internal())?
    };
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
    source_actor: Option<&str>,
) -> ApiResult<Value> {
    let handle = super::stages::runtime(state)?
        .handle(stage)
        .map_err(super::stages::failure)?;
    let crate::stage::protocol::Body::Approval { record } = handle
        .rpc(crate::stage::protocol::Body::ApprovalQuery {
            id: id.into(),
            source_actor: source_actor.map(str::to_owned),
        })
        .await
        .map_err(super::stages::failure)?
    else {
        return Err(ApiError::internal());
    };
    let mut value = serde_json::to_value(&record).map_err(|_| ApiError::internal())?;
    value["id"] = format!("{stage}~{}", record.id).into();
    redact(&mut value, &state.system.config());
    Ok(value)
}
