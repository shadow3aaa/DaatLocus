use super::{ApiError, ApiResult, ServiceState, manage, owner};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/apps", get(list).post(install))
        .route("/v1/apps/{id}", get(detail))
        .route("/v1/apps/{id}/state", post(set_state))
        .route("/v1/apps/{id}/timeouts", post(set_timeouts))
        .route("/v1/apps/installations/{id}", delete(cancel_installation))
}
#[derive(Deserialize)]
struct ListQuery {
    after: Option<String>,
    limit: Option<usize>,
    search: Option<String>,
    enabled: Option<bool>,
}
async fn list(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let registry = state.system.app_registry();
    let limit = query.limit.unwrap_or(24).clamp(1, 64);
    if query.search.as_ref().is_some_and(|value| value.len() > 512)
        || query
            .after
            .as_ref()
            .is_some_and(|value| !crate::mcp::is_mcp_id(value))
    {
        return Err(ApiError::invalid());
    }
    let mut entries = registry.list_public(
        query.after.as_deref(),
        query.search.as_deref(),
        query.enabled,
        limit + 1,
    );
    let next = if entries.len() > limit {
        entries.truncate(limit);
        entries
            .last()
            .and_then(|entry| entry["id"].as_str())
            .map(str::to_owned)
    } else {
        None
    };
    Ok(Json(
        json!({"items":entries,"next_cursor":next,"revision":registry.revision(),"stdio_supported":!cfg!(target_os="android")}),
    ))
}
#[derive(Deserialize)]
struct DetailQuery {
    tool: Option<String>,
}
async fn detail(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<DetailQuery>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let entry = state
        .system
        .app_registry()
        .get(&id)
        .map_err(ApiError::from_report)?;
    let mut value = entry.public(false);
    value["tools"] = json!(
        entry
            .tools
            .iter()
            .map(|tool| json!({"name":tool.name,"description":tool.description}))
            .collect::<Vec<_>>()
    );
    if let Some(name) = query.tool {
        value["tool"] = json!(
            entry
                .tools
                .iter()
                .find(|t| t.name == name)
                .ok_or_else(ApiError::invalid)?
        );
    }
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRequest {
    request_id: Option<String>,
    name: String,
    #[serde(default)]
    description: String,
    source: crate::mcp::Source,
}
async fn install(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(request): Json<InstallRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    request.source.validate().map_err(|_| ApiError::invalid())?;
    if request.name.trim().is_empty()
        || request.name.chars().count() > 100
        || request.description.chars().count() > 500
    {
        return Err(ApiError::invalid());
    }
    let context = state
        .system
        .owner_app_context()
        .map_err(ApiError::from_report)?;
    let registry = state.system.app_registry();
    let entry = registry
        .install_cancellable(
            request
                .request_id
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            request.name,
            request.description,
            request.source,
            &context,
        )
        .await
        .map_err(ApiError::from_report)?;
    Ok(Json(entry.public(false)))
}
async fn cancel_installation(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    uuid::Uuid::parse_str(&id).map_err(|_| ApiError::invalid())?;
    state.system.app_registry().cancel_installation(&id);
    Ok(Json(json!({"status":"cancel_requested"})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateRequest {
    expected_revision: u64,
    enabled: bool,
}
async fn set_state(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<StateRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    let entry = state
        .system
        .app_registry()
        .set_enabled(&id, request.expected_revision, request.enabled)
        .await
        .map_err(ApiError::from_report)?;
    Ok(Json(entry.public(false)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeoutsRequest {
    expected_revision: u64,
    timeouts: crate::mcp::Timeouts,
}
async fn set_timeouts(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<TimeoutsRequest>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    request
        .timeouts
        .validate()
        .map_err(|_| ApiError::invalid())?;
    let entry = state
        .system
        .app_registry()
        .set_timeouts(&id, request.expected_revision, request.timeouts)
        .await
        .map_err(ApiError::from_report)?;
    Ok(Json(entry.public(false)))
}
