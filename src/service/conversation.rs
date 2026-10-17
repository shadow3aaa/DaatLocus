//! Narrow conversation controls. Queued user changes never race into active execution.
use super::{ApiError, ApiResult, ServiceState, access_actor, bounded, redact};
use crate::actors::Attachment;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/actors/{actor}/messages/{message}/edit", post(edit))
        .route(
            "/v1/actors/{actor}/messages/{message}/withdraw",
            post(withdraw),
        )
        .route("/v1/actors/{actor}/queue/clear", post(clear))
        .route("/v1/actors/{actor}/queue", get(queue))
        .route("/v1/actors/{actor}/messages/{message}/steer", post(steer))
        .route(
            "/v1/actors/{actor}/messages/{message}/activity",
            get(activity),
        )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    content: String,
    attachments: Vec<String>,
    expected_updated_at_ms: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Withdraw {
    expected_updated_at_ms: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Activity {
    after: Option<i64>,
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Steer {
    target_message_id: String,
    expected_updated_at_ms: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Queue {
    after: Option<i64>,
    limit: Option<usize>,
}
async fn queue(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Query(query): Query<Queue>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    if query.after.is_some_and(|cursor| cursor < 0) {
        return Err(ApiError::invalid());
    }
    let page = state
        .system
        .store()
        .queued_messages(&actor, query.after, bounded(query.limit, 100)?)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(page).map_err(|_| ApiError::internal())?;
    value["active"] = serde_json::to_value(
        state
            .system
            .store()
            .running_message(&actor)
            .map_err(ApiError::from_report)?,
    )
    .map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn steer(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, message)): Path<(String, String)>,
    Json(input): Json<Steer>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, true).await?;
    reject_import_edit(&state, &actor, &message)?;
    if input.target_message_id.len() > 128 || input.expected_updated_at_ms < 0 {
        return Err(ApiError::invalid());
    }
    let record = state
        .system
        .steer_queued(
            &actor,
            &message,
            &input.target_message_id,
            input.expected_updated_at_ms,
        )
        .await
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(record).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}

async fn edit(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, message)): Path<(String, String)>,
    Json(input): Json<Edit>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, true).await?;
    reject_import_edit(&state, &actor, &message)?;
    if input.content.len() > 64 * 1024
        || input.content.trim().is_empty() && input.attachments.is_empty()
        || input.expected_updated_at_ms < 0
    {
        return Err(ApiError::invalid());
    }
    let mut attachments = Vec::new();
    for id in input.attachments {
        let resource = state
            .system
            .store()
            .resource(&actor, &id)
            .map_err(ApiError::from_report)?;
        attachments.push(Attachment {
            id: resource.id,
            name: resource.name,
            media_type: resource.media_type,
            size_bytes: resource.size_bytes,
            uri: resource.uri,
        });
    }
    if actor == crate::stage::shared::ACTOR_ID
        && let Some(core) = state.system.shared_core()
    {
        let original = state
            .system
            .store()
            .message(&message)
            .map_err(ApiError::from_report)?;
        if original.attachments != attachments {
            return Err(ApiError::invalid());
        }
        let record = core
            .amend(&message, input.expected_updated_at_ms, Some(input.content))
            .await
            .map_err(|_| ApiError::unavailable())?;
        let mut value = serde_json::to_value(record).map_err(|_| ApiError::internal())?;
        redact(&mut value, &state.system.config());
        return Ok(Json(value));
    }
    let record = state
        .system
        .store()
        .amend_queued_user(
            &actor,
            &message,
            input.expected_updated_at_ms,
            input.content,
            attachments,
            false,
        )
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(record).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
fn reject_import_edit(state: &ServiceState, actor: &str, message: &str) -> ApiResult<()> {
    let record = state
        .system
        .store()
        .message(message)
        .map_err(ApiError::from_report)?;
    if record.actor_id != actor {
        return Err(ApiError::not_found());
    }
    if crate::conversation_import::is_import(&record) {
        return Err(ApiError::invalid());
    }
    Ok(())
}
async fn withdraw(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, message)): Path<(String, String)>,
    Json(input): Json<Withdraw>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, true).await?;
    if actor == crate::stage::shared::ACTOR_ID
        && let Some(core) = state.system.shared_core()
    {
        let record = core
            .amend(&message, input.expected_updated_at_ms, None)
            .await
            .map_err(|_| ApiError::unavailable())?;
        let mut value = serde_json::to_value(record).map_err(|_| ApiError::internal())?;
        redact(&mut value, &state.system.config());
        return Ok(Json(value));
    }
    let record = state
        .system
        .store()
        .amend_queued_user(
            &actor,
            &message,
            input.expected_updated_at_ms,
            String::new(),
            vec![],
            true,
        )
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(record).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn clear(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, true).await?;
    if actor == crate::stage::shared::ACTOR_ID
        && let Some(core) = state.system.shared_core()
    {
        let count = core
            .clear_queue()
            .await
            .map_err(|_| ApiError::unavailable())?;
        return Ok(Json(json!({"cancelled_count":count})));
    }
    let count = state
        .system
        .store()
        .clear_queued_users(&actor)
        .map_err(ApiError::from_report)?;
    Ok(Json(json!({"cancelled_count":count})))
}
async fn activity(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, id)): Path<(String, String)>,
    Query(query): Query<Activity>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    let message = state
        .system
        .store()
        .message(&id)
        .map_err(ApiError::from_report)?;
    if message.actor_id != actor {
        return Err(ApiError::not_found());
    }
    if query.after.is_some_and(|value| value < 0) {
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
        .message_events(
            &actor,
            &id,
            query.after.unwrap_or(0),
            bounded(query.limit, 100)?,
        )
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(page).map_err(|_| ApiError::internal())?;
    value["event_cursor"] = Value::from(cursor);
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
