//! Owner session catalog and lifecycle. Conversation reads retain Actor URLs.
use super::*;

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/sessions", get(list).post(create))
        .route(
            "/v1/sessions/{session}",
            get(detail).patch(patch).delete(delete),
        )
        .route("/v1/sessions/{session}/archive", post(archive))
        .route("/v1/sessions/{session}/unarchive", post(unarchive))
        .route("/v1/sessions/{session}/actors", get(actors))
        .route("/v1/sessions/{session}/messages", post(send_core_message))
        .route(
            "/v1/sessions/{session}/events",
            get(crate::service::stream::actor_events),
        )
        .route(
            "/v1/sessions/{session}/attachments",
            post(resources::upload).layer(DefaultBodyLimit::max(
                crate::daemon::attachments::MAX_FILE_ATTACHMENT_BYTES,
            )),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    archived: Option<bool>,
    after: Option<String>,
    limit: Option<usize>,
}
async fn list(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<List>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let cursor = state
        .system
        .store()
        .event_cursor()
        .map_err(ApiError::from_report)?;
    let (items, next) = state
        .system
        .store()
        .session_page(
            query.archived.unwrap_or(false),
            query.after.as_deref(),
            bounded(query.limit, 100)?,
        )
        .map_err(ApiError::from_report)?;
    Ok(Json(
        json!({"items":items,"next_cursor":next,"event_cursor":cursor}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    title: Option<String>,
    model: Option<String>,
}
pub(super) fn idempotency(headers: &HeaderMap) -> ApiResult<Option<String>> {
    let key = headers
        .get("idempotency-key")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| ApiError::invalid())?;
    if key
        .as_ref()
        .is_some_and(|k| k.is_empty() || k.len() > 128 || !k.bytes().all(|b| b.is_ascii_graphic()))
    {
        return Err(ApiError::invalid());
    }
    Ok(key)
}
async fn create(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(input): Json<Create>,
) -> ApiResult<(StatusCode, Json<crate::actors::SessionSummary>)> {
    manage(&state, &headers).await?;
    let key = idempotency(&headers)?.ok_or_else(ApiError::invalid)?;
    let session = state
        .system
        .create_session(input.title, input.model, Some(&key))
        .await
        .map_err(ApiError::from_report)?;
    Ok((StatusCode::CREATED, Json(session)))
}
async fn detail(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::actors::SessionSummary>> {
    owner(&state, &headers).await?;
    Ok(Json(
        state
            .system
            .store()
            .session_summary(&id)
            .map_err(ApiError::from_report)?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    #[serde(default, deserialize_with = "present_model")]
    title: Option<Option<String>>,
    #[serde(default, deserialize_with = "present_model")]
    model: Option<Option<String>>,
}
async fn patch(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<Patch>,
) -> ApiResult<Json<crate::actors::SessionSummary>> {
    manage(&state, &headers).await?;
    if input.title.is_none() && input.model.is_none() {
        return Err(ApiError::invalid());
    }
    Ok(Json(
        state
            .system
            .patch_session(&id, input.title, input.model)
            .await
            .map_err(ApiError::from_report)?,
    ))
}
async fn archive(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::actors::SessionSummary>> {
    manage(&state, &headers).await?;
    Ok(Json(
        state
            .system
            .archive_session(&id, true)
            .await
            .map_err(ApiError::from_report)?,
    ))
}
async fn unarchive(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::actors::SessionSummary>> {
    manage(&state, &headers).await?;
    Ok(Json(
        state
            .system
            .archive_session(&id, false)
            .await
            .map_err(ApiError::from_report)?,
    ))
}
async fn delete(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    manage(&state, &headers).await?;
    state
        .system
        .delete_session(&id)
        .await
        .map_err(ApiError::from_report)?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Workers {
    status: Option<String>,
    after: Option<String>,
    limit: Option<usize>,
}
async fn actors(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<Workers>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    if query.status.as_deref().is_some_and(|s| s != "running") {
        return Err(ApiError::invalid());
    }
    let cursor = state
        .system
        .store()
        .event_cursor()
        .map_err(ApiError::from_report)?;
    let limit = bounded(query.limit, 100)?;
    let mut items = state
        .system
        .store()
        .session_actors(
            &id,
            query.after.as_deref(),
            query.status.is_some(),
            limit + 1,
        )
        .map_err(ApiError::from_report)?;
    let next = (items.len() > limit).then(|| items[limit - 1].id.clone());
    items.truncate(limit);
    Ok(Json(
        json!({"items":items,"next_cursor":next,"event_cursor":cursor}),
    ))
}
