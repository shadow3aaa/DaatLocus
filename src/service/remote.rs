//! Owner-authorized views of hosted Actors. Source hosts enforce every read.
use super::{ApiError, ApiResult, ServiceState, manage, owner, stages};
use crate::stage::{
    ActorAddress, StageId,
    observation::Query as Read,
    peer::PeerHandle,
    protocol::{Body, SubscriptionLevel},
    sync::Replica,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::get,
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::VecDeque, convert::Infallible, time::Duration};
pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route(
            "/v1/stages/{stage}/actors/{actor}/messages/{message}/cancel",
            axum::routing::post(cancel),
        )
        .route("/v1/stages/{stage}/actors/{actor}", get(actor).patch(patch))
        .route("/v1/stages/{stage}/actors/{actor}/messages", get(messages))
        .route(
            "/v1/stages/{stage}/actors/{actor}/messages/search",
            get(search),
        )
        .route(
            "/v1/stages/{stage}/actors/{actor}/messages/{message}",
            get(message),
        )
        .route(
            "/v1/stages/{stage}/actors/{actor}/messages/{message}/results",
            get(results),
        )
        .route(
            "/v1/stages/{stage}/actors/{actor}/messages/{message}/activity",
            get(activity),
        )
        .route(
            "/v1/stages/{stage}/actors/{actor}/results/{result}",
            get(result),
        )
        .route("/v1/stages/{stage}/actors/{actor}/queue", get(queue))
        .route(
            "/v1/stages/{stage}/actors/{actor}/approvals",
            get(approvals),
        )
        .route(
            "/v1/stages/{stage}/actors/{actor}/approvals/{approval}",
            get(approval),
        )
        .route(
            "/v1/stages/{stage}/actors/{actor}/resources/{resource}",
            get(resource),
        )
        .route(
            "/v1/stages/{stage}/actors/{actor}/event-stream",
            get(events),
        )
}
async fn administer(
    state: ServiceState,
    headers: HeaderMap,
    target: ActorAddress,
    action: crate::stage::protocol::ActorAdminAction,
) -> ApiResult<Value> {
    manage(&state, &headers).await?;
    let handle = authorize(&state, &headers, &target).await?;
    let Body::ActorAdminResult { actor, content } = handle
        .rpc(Body::ActorAdmin {
            actor: target.clone(),
            action,
        })
        .await
        .map_err(stages::failure)?
    else {
        return Err(ApiError::invalid());
    };
    if actor != target {
        return Err(ApiError::forbidden());
    }
    let Some(content) = content else {
        return Ok(Value::Null);
    };
    let bytes = handle
        .content(target.clone(), content)
        .await
        .map_err(stages::failure)?;
    let mut value = serde_json::from_slice(&bytes).map_err(|_| ApiError::internal())?;
    qualify(&mut value, &target);
    Ok(value)
}
async fn patch(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((stage, id)): Path<(StageId, String)>,
    Json(value): Json<Value>,
) -> ApiResult<Json<Value>> {
    if !value.is_object()
        || value
            .as_object()
            .is_some_and(|o| o.is_empty() || o.keys().any(|key| key != "model"))
    {
        return Err(ApiError::invalid());
    }
    let model = if let Some(value) = value.get("model") {
        Some(
            serde_json::from_value::<Option<String>>(value.clone())
                .map_err(|_| ApiError::invalid())?,
        )
    } else {
        None
    };
    Ok(Json(
        administer(
            state,
            headers,
            address(stage, id)?,
            crate::stage::protocol::ActorAdminAction::Preferences {
                model_present: model.is_some(),
                model: model.flatten(),
            },
        )
        .await?,
    ))
}
fn address(stage: StageId, actor: String) -> ApiResult<ActorAddress> {
    let value = ActorAddress {
        stage_id: stage,
        actor_id: actor,
    };
    value.validate().map_err(stages::failure)?;
    Ok(value)
}
async fn control(
    state: &ServiceState,
    headers: &HeaderMap,
    target: ActorAddress,
    action: crate::stage::protocol::ConversationAction,
) -> ApiResult<Value> {
    manage(state, headers).await?;
    let handle = authorize(state, headers, &target).await?;
    let Body::ConversationResult {
        actor,
        record,
        count,
    } = handle
        .rpc(Body::ConversationControl {
            actor: target.clone(),
            action,
        })
        .await
        .map_err(stages::failure)?
    else {
        return Err(ApiError::internal());
    };
    if actor != target {
        return Err(ApiError::forbidden());
    }
    let mut value = if let Some(record) = record {
        serde_json::to_value(record).map_err(|_| ApiError::internal())?
    } else {
        serde_json::json!({"cancelled_count":count})
    };
    qualify(&mut value, &target);
    Ok(value)
}
async fn cancel(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, actor, message)): Path<(StageId, String, String)>,
) -> ApiResult<axum::http::StatusCode> {
    control(
        &s,
        &h,
        address(stage, actor)?,
        crate::stage::protocol::ConversationAction::Cancel { message },
    )
    .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
pub(super) fn qualify(value: &mut Value, target: &ActorAddress) {
    qualify_as(
        value,
        target,
        &format!("{}~{}", target.stage_id, target.actor_id),
    );
}
pub(super) fn qualify_core(value: &mut Value, target: &ActorAddress) {
    qualify_as(value, target, crate::stage::shared::ACTOR_ID);
}
fn qualify_as(value: &mut Value, target: &ActorAddress, key: &str) {
    fn walk(value: &mut Value, target: &ActorAddress, key: &str) {
        match value {
            Value::Array(items) => {
                for item in items {
                    walk(item, target, key)
                }
            }
            Value::Object(object) => {
                if object
                    .get("actor_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| {
                        id == target.actor_id
                            || id == format!("{}:{}", target.stage_id, target.actor_id)
                            || id == format!("{}~{}", target.stage_id, target.actor_id)
                    })
                {
                    object.insert("actor_id".into(), key.into());
                }
                if object.get("id").and_then(Value::as_str).is_some_and(|id| {
                    id == target.actor_id
                        || id == format!("{}~{}", target.stage_id, target.actor_id)
                }) && (object.contains_key("remote") || object.contains_key("config"))
                {
                    object.insert("id".into(), key.into());
                }
                if let Some(Value::String(uri)) = object.get_mut("uri")
                    && uri.starts_with("/v1/actors/")
                {
                    *uri = uri.replacen(
                        "/v1/actors/",
                        &format!("/v1/stages/{}/actors/", target.stage_id),
                        1,
                    );
                }
                if let Some(Value::String(url)) = object.get_mut("conversation_url")
                    && key == crate::stage::shared::ACTOR_ID
                    && *url == target.conversation_url()
                {
                    *url = "/actors/core".into();
                }
                if let Some(Value::String(url)) = object.get_mut("conversation_url")
                    && url.starts_with("/actors/")
                    && !(key == crate::stage::shared::ACTOR_ID && url == "/actors/core")
                {
                    *url = url.replacen(
                        "/actors/",
                        &format!("/stages/{}/actors/", target.stage_id),
                        1,
                    );
                }
                let has_domain_content =
                    object
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| {
                            name.ends_with("artifacts__present")
                                || name.ends_with("actors__create")
                                || name.ends_with("actors__message")
                        });
                if has_domain_content
                    && let Some(Value::Object(payload)) = object.get_mut("payload")
                {
                    if let Some(Value::String(uri)) = payload.get_mut("uri")
                        && uri.starts_with("/v1/actors/")
                    {
                        *uri = uri.replacen(
                            "/v1/actors/",
                            &format!("/v1/stages/{}/actors/", target.stage_id),
                            1,
                        );
                    }
                    if let Some(Value::String(url)) = payload.get_mut("conversation_url")
                        && url.starts_with("/actors/")
                    {
                        *url = url.replacen(
                            "/actors/",
                            &format!("/stages/{}/actors/", target.stage_id),
                            1,
                        );
                    }
                }
                for (name, item) in object.iter_mut() {
                    if !matches!(name.as_str(), "arguments" | "payload" | "call") {
                        walk(item, target, key);
                    }
                }
            }
            _ => {}
        }
    }
    walk(value, target, key);
}
async fn handle(state: &ServiceState, target: &ActorAddress) -> ApiResult<PeerHandle> {
    let runtime = stages::runtime(state)?;
    if target.stage_id == runtime.host.local {
        return Err(ApiError::invalid());
    }
    runtime.want(target.stage_id);
    runtime.handle(target.stage_id).map_err(stages::failure)
}
async fn authorize(
    state: &ServiceState,
    headers: &HeaderMap,
    target: &ActorAddress,
) -> ApiResult<PeerHandle> {
    owner(state, headers).await?;
    let handle = handle(state, target).await?;
    match handle
        .rpc(Body::Authorize {
            actor: target.clone(),
        })
        .await
        .map_err(stages::failure)?
    {
        Body::ActorAccess {
            actor, read: true, ..
        } if actor == *target => Ok(handle),
        _ => Err(ApiError::forbidden()),
    }
}
async fn read(
    state: &ServiceState,
    headers: &HeaderMap,
    target: ActorAddress,
    query: Read,
) -> ApiResult<Value> {
    let handle = authorize(state, headers, &target).await?;
    let cursor = Replica::new(state.system.store())
        .client_cursor()
        .map_err(stages::failure)?;
    let Body::Observation { actor, content } = handle
        .rpc(Body::ReadQuery {
            actor: target.clone(),
            query,
        })
        .await
        .map_err(stages::failure)?
    else {
        return Err(ApiError::internal());
    };
    if actor != target {
        return Err(ApiError::forbidden());
    }
    let bytes = handle
        .content(target.clone(), content)
        .await
        .map_err(stages::failure)?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| ApiError::internal())?;
    if target.actor_id == crate::stage::shared::ACTOR_ID
        && let Some(head) = value.get("event_head")
    {
        let head = serde_json::from_value(head.clone()).map_err(|_| ApiError::internal())?;
        Replica::new(state.system.store())
            .start_core_tail(&target, &head)
            .map_err(stages::failure)?;
    }
    qualify(&mut value, &target);
    if let Some(object) = value.as_object_mut()
        && object.contains_key("items")
    {
        object.insert("event_cursor".into(), cursor.into());
    }
    Ok(value)
}
pub(super) fn core_source(state: &ServiceState) -> Option<ActorAddress> {
    Some(ActorAddress {
        stage_id: state.system.shared_core()?.read_source()?,
        actor_id: crate::stage::shared::ACTOR_ID.into(),
    })
}
pub(super) async fn core_read(
    state: &ServiceState,
    headers: &HeaderMap,
    actor: &str,
    query: Read,
) -> ApiResult<Option<Value>> {
    if actor != crate::stage::shared::ACTOR_ID {
        return Ok(None);
    }
    let Some(source) = core_source(state) else {
        return Ok(None);
    };
    let mut value = read(state, headers, source.clone(), query).await?;
    qualify_core(&mut value, &source);
    Ok(Some(value))
}
pub(super) async fn core_control(
    state: &ServiceState,
    headers: &HeaderMap,
    actor: &str,
    action: crate::stage::protocol::ConversationAction,
) -> ApiResult<Option<Value>> {
    if actor != crate::stage::shared::ACTOR_ID {
        return Ok(None);
    }
    let Some(source) = core_source(state) else {
        return Ok(None);
    };
    if let crate::stage::protocol::ConversationAction::Amend { message, .. } = &action {
        let original = read(
            state,
            headers,
            source.clone(),
            Read::Message {
                id: message.clone(),
            },
        )
        .await?;
        let original = serde_json::from_value(original).map_err(|_| ApiError::internal())?;
        if crate::conversation_import::is_import(&original) {
            return Err(ApiError::invalid());
        }
    }
    let mut value = control(state, headers, source.clone(), action).await?;
    qualify_core(&mut value, &source);
    Ok(Some(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    before: Option<i64>,
    after: Option<i64>,
    limit: Option<usize>,
    q: Option<String>,
}
fn limit(query: &List) -> ApiResult<u8> {
    Ok(super::bounded(query.limit, 32)? as u8)
}
async fn actor(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id)): Path<(StageId, String)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(read(&s, &h, address(stage, id)?, Read::Actor).await?))
}
async fn messages(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id)): Path<(StageId, String)>,
    Query(q): Query<List>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(
            &s,
            &h,
            address(stage, id)?,
            Read::Messages {
                before: q.before,
                limit: limit(&q)?,
            },
        )
        .await?,
    ))
}
async fn queue(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id)): Path<(StageId, String)>,
    Query(q): Query<List>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(
            &s,
            &h,
            address(stage, id)?,
            Read::Queue {
                after: q.after,
                limit: limit(&q)?,
            },
        )
        .await?,
    ))
}
async fn search(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id)): Path<(StageId, String)>,
    Query(q): Query<List>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(
            &s,
            &h,
            address(stage, id)?,
            Read::Search {
                text: q.q.clone().ok_or_else(ApiError::invalid)?,
                before: q.before,
                limit: limit(&q)?,
            },
        )
        .await?,
    ))
}
async fn message(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id, m)): Path<(StageId, String, String)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(&s, &h, address(stage, id)?, Read::Message { id: m }).await?,
    ))
}
async fn results(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id, m)): Path<(StageId, String, String)>,
    Query(q): Query<List>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(
            &s,
            &h,
            address(stage, id)?,
            Read::Results {
                message: m,
                before: q.before,
                limit: limit(&q)?,
            },
        )
        .await?,
    ))
}
async fn activity(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id, m)): Path<(StageId, String, String)>,
    Query(q): Query<List>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(
            &s,
            &h,
            address(stage, id)?,
            Read::Activity {
                message: m,
                after: q.after.unwrap_or(0),
                limit: limit(&q)?,
            },
        )
        .await?,
    ))
}
async fn result(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id, m)): Path<(StageId, String, String)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(&s, &h, address(stage, id)?, Read::Result { id: m }).await?,
    ))
}
async fn approvals(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id)): Path<(StageId, String)>,
    Query(q): Query<List>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(
            &s,
            &h,
            address(stage, id)?,
            Read::Approvals {
                before: q.before,
                limit: limit(&q)?,
            },
        )
        .await?,
    ))
}
async fn approval(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id, m)): Path<(StageId, String, String)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        read(&s, &h, address(stage, id)?, Read::Approval { id: m }).await?,
    ))
}
struct Watch {
    handle: PeerHandle,
    actor: String,
    id: uuid::Uuid,
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.handle.unwatch(&self.actor, self.id);
    }
}
struct Feed {
    ended: bool,
    core_alias: bool,
    state: ServiceState,
    headers: HeaderMap,
    target: ActorAddress,
    cursor: i64,
    queue: VecDeque<Value>,
    _watch: Watch,
}
async fn events(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id)): Path<(StageId, String)>,
    Query(q): Query<List>,
) -> ApiResult<Response> {
    let target = address(stage, id)?;
    event_feed(s, h, target, q.after, false).await
}
pub(super) async fn core_stream(
    state: ServiceState,
    headers: HeaderMap,
    target: ActorAddress,
    after: Option<i64>,
) -> ApiResult<Response> {
    event_feed(state, headers, target, after, true).await
}
async fn event_feed(
    s: ServiceState,
    h: HeaderMap,
    target: ActorAddress,
    after: Option<i64>,
    core_alias: bool,
) -> ApiResult<Response> {
    let handle = authorize(&s, &h, &target).await?;
    let view = uuid::Uuid::new_v4();
    handle
        .watch(
            &target.actor_id,
            view,
            if core_alias {
                SubscriptionLevel::Summary
            } else {
                SubscriptionLevel::Detail
            },
        )
        .map_err(stages::failure)?;
    let after = h
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::parse::<i64>)
        .transpose()
        .map_err(|_| ApiError::invalid())?
        .unwrap_or(after.unwrap_or(0));
    if after < 0 {
        return Err(ApiError::invalid());
    }
    let feed = Feed {
        ended: false,
        core_alias,
        state: s,
        headers: h,
        target: target.clone(),
        cursor: after,
        queue: VecDeque::new(),
        _watch: Watch {
            handle,
            actor: target.actor_id,
            id: view,
        },
    };
    Ok(Sse::new(futures_util::stream::unfold(
        feed,
        |mut feed| async move {
            loop {
                if feed.ended {
                    return None;
                }
                if feed.state.system.is_stopping() {
                    return None;
                }
                if feed.core_alias && core_source(&feed.state).as_ref() != Some(&feed.target) {
                    let permitted = owner(&feed.state, &feed.headers).await.is_ok();
                    let has_local_view = feed.state.system.shared_core().is_some_and(|core| core.status()["synchronized"] == true);
                    let kind = if !permitted { "access_revoked" }
                        else if core_source(&feed.state).is_some() || has_local_view { "resync" }
                        else { "error" };
                    feed.ended = true;
                    return Some((Ok::<_, Infallible>(Event::default().event(kind).data("{}")), feed));
                }
                if let Err(error) = authorize(&feed.state, &feed.headers, &feed.target).await {
                    feed.ended = true;
                    return Some((
                        Ok(Event::default()
                            .event(
                                if error.status == axum::http::StatusCode::FORBIDDEN
                                    || error.status == axum::http::StatusCode::UNAUTHORIZED
                                {
                                    "access_revoked"
                                } else {
                                    "error"
                                },
                            )
                            .data("{}")),
                        feed,
                    ));
                }
                if let Some(mut value) = feed.queue.pop_front() {
                    let cursor = value["seq"].as_i64().unwrap_or(feed.cursor);
                    feed.cursor = cursor;
                    if feed.core_alias {
                        qualify_core(&mut value, &feed.target);
                    } else {
                        qualify(&mut value, &feed.target);
                    }
                    let kind = if value.get("resync").is_some() {
                        "resync"
                    } else {
                        "activity"
                    };
                    return Some((
                        Ok(Event::default()
                            .event(kind)
                            .id(cursor.to_string())
                            .data(value.to_string())),
                        feed,
                    ));
                }
                let changed = feed.state.system.store().changed();
                let wake = changed.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                match Replica::new(feed.state.system.store())
                    .client_events(&feed.target, feed.cursor)
                {
                    Ok(events) if !events.is_empty() => {
                        feed.queue.extend(events);
                        continue;
                    }
                    Err(_) => return None,
                    _ => {}
                }
                tokio::select! {_=wake=>{},_=tokio::time::sleep(Duration::from_secs(5))=>{
                    if authorize(&feed.state,&feed.headers,&feed.target).await.is_err(){feed.ended=true;return Some((Ok(Event::default().event("error").data("{}")),feed));}
                    return Some((Ok(Event::default().comment("keepalive")),feed));
                }}
            }
        },
    )).into_response())
}
async fn resource(
    State(s): State<ServiceState>,
    h: HeaderMap,
    Path((stage, id, res)): Path<(StageId, String, String)>,
) -> ApiResult<Response> {
    let target = address(stage, id)?;
    let value = read(&s, &h, target.clone(), Read::Resource { id: res }).await?;
    let resource: crate::resources::Resource =
        serde_json::from_value(value).map_err(|_| ApiError::internal())?;
    let handle = authorize(&s, &h, &target).await?;
    let bytes = handle
        .content(target.clone(), resource.content)
        .await
        .map_err(stages::failure)?;
    let size = bytes.len();
    let source = futures_util::stream::try_unfold(
        (s, h, target, bytes, 0usize),
        |(state, headers, target, bytes, offset)| async move {
            if offset >= bytes.len() {
                return Ok::<_, std::io::Error>(None);
            }
            authorize(&state, &headers, &target).await.map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Resource access revoked",
                )
            })?;
            let end = (offset + 32768).min(bytes.len());
            let chunk = bytes[offset..end].to_vec();
            Ok(Some((chunk, (state, headers, target, bytes, end))))
        },
    );
    let mut response = axum::body::Body::from_stream(source).into_response();
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        HeaderValue::from_str(&resource.media_type).map_err(|_| ApiError::invalid())?,
    );
    headers.insert(
        "content-length",
        HeaderValue::from_str(&size.to_string()).map_err(|_| ApiError::invalid())?,
    );
    headers.insert(
        "cache-control",
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("content-security-policy",HeaderValue::from_static("sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'self'"));
    Ok(response)
}
