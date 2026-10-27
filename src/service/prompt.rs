//! Owner-managed instruction files and inspection of the actual static assembly.
use super::{ApiError, ApiResult, ServiceState, bounded, manage, owner, redact};
use crate::{
    actors::prompt::SystemPrompt,
    context_budget::approx_token_count,
    instruction_layers::{Layer, Library, Role},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Sse, sse::Event},
    routing::{get, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/system-prompt", get(files))
        .route("/v1/system-prompt/event-stream", get(changes))
        .route("/v1/system-prompt/{index}", get(file))
        .route("/v1/system-prompt/layers", put(replace_layers))
        .route("/v1/instruction-layers", get(layers).post(create_layer))
        .route(
            "/v1/instruction-layers/{id}",
            get(layer).patch(update_layer),
        )
}

/// Invalidation only: clients fetch bounded metadata and the selected body after a change.
async fn changes(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> ApiResult<Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>> {
    owner(&state, &headers).await?;
    let after = headers
        .get("last-event-id")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(ApiError::invalid)
        })
        .transpose()?
        .unwrap_or(0);
    if after > state.system.instructions().event_cursor() {
        return Err(ApiError::invalid());
    }
    Ok(Sse::new(futures_util::stream::unfold(
        (state, headers, after, false),
        |(state, headers, mut after, ended)| async move {
            if ended {
                return None;
            }
            let system = state.system.clone();
            loop {
                let changed = system.instructions().changed().notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if state.system.is_stopping() {
                    return None;
                }
                if owner(&state, &headers).await.is_err() {
                    return Some((
                        Ok(Event::default().event("access_revoked").data("{}")),
                        (state, headers, after, true),
                    ));
                }
                let current = state.system.instructions().event_cursor();
                if current > after {
                    after = current;
                    return Some((
                        Ok(Event::default()
                            .event("cursor")
                            .id(current.to_string())
                            .data("{}")),
                        (state, headers, after, false),
                    ));
                }
                if tokio::time::timeout(std::time::Duration::from_secs(5), changed)
                    .await
                    .is_err()
                {
                    // Re-check access after every wait, including heartbeat emission.
                    if owner(&state, &headers).await.is_err() {
                        continue;
                    }
                    return Some((
                        Ok(Event::default().comment("keepalive")),
                        (state, headers, after, false),
                    ));
                }
            }
        },
    )))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    #[serde(default)]
    role: Role,
    after: Option<usize>,
    limit: Option<usize>,
}

async fn files(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = bounded(query.limit, 100)?;
    owner(&state, &headers).await?;
    let prompt = SystemPrompt::for_role(&state.system, matches!(query.role, Role::Core))
        .map_err(ApiError::from_report)?;
    let start = query.after.unwrap_or(0);
    let mut value = json!({
        "revision": prompt.revision(),
        "editable_layers": true,
        "total_tokens": approx_token_count(&prompt.render()),
        "items": prompt.files.iter().enumerate().skip(start).take(limit).map(|(index, file)| json!({"index":index,"name":file.name,"bytes":file.content.len(),"tokens":approx_token_count(&file.content),"layer_id":file.layer_id})).collect::<Vec<_>>(),
        "next_cursor": (start.saturating_add(limit) < prompt.files.len()).then(|| start.saturating_add(limit))
    });
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileQuery {
    #[serde(default)]
    role: Role,
    revision: String,
    offset: Option<usize>,
}

async fn file(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(index): Path<usize>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let prompt = SystemPrompt::for_role(&state.system, matches!(query.role, Role::Core))
        .map_err(ApiError::from_report)?;
    if prompt.revision() != query.revision {
        return Err(ApiError {
            status: axum::http::StatusCode::CONFLICT,
            code: "conflict",
            message: "System instructions changed",
        });
    }
    let file = prompt.files.get(index).ok_or_else(ApiError::not_found)?;
    // Redact before paging so credentials cannot straddle a chunk boundary.
    let mut content = Value::String(file.content.clone());
    redact(&mut content, &state.system.config());
    let content = content.as_str().ok_or_else(ApiError::internal)?;
    let start = query.offset.unwrap_or(0);
    if !content.is_char_boundary(start) {
        return Err(ApiError::invalid());
    }
    let mut end = start.saturating_add(16 * 1024).min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let mut value = json!({"content": &content[start..end], "next_offset": (end < content.len()).then_some(end)});
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}

fn metadata(file: &Layer, library: &Library) -> Value {
    let roles: Vec<_> = [Role::Core, Role::Others]
        .into_iter()
        .filter(|role| library.ids(*role).contains(&file.id))
        .collect();
    json!({"id":file.id,"name":file.name,"bytes":file.content.len(),"tokens":approx_token_count(&file.content),"revision":file.revision(),"roles":roles})
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerQuery {
    #[serde(default)]
    q: String,
    after: Option<usize>,
    limit: Option<usize>,
}
async fn layers(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<LayerQuery>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let limit = bounded(query.limit, 50)?;
    let library = state.system.instructions().snapshot();
    let search = query.q.trim().to_lowercase();
    let mut files: Vec<_> = library
        .layers
        .iter()
        .filter(|file| {
            file.name.to_lowercase().contains(&search)
                || file.content.to_lowercase().contains(&search)
        })
        .collect();
    files.sort_by(|a, b| {
        b.content
            .len()
            .cmp(&a.content.len())
            .then_with(|| a.id.cmp(&b.id))
    });
    let start = query.after.unwrap_or(0);
    let mut value = json!({"items":files.iter().skip(start).take(limit).map(|file|metadata(file,&library)).collect::<Vec<_>>(),"next_cursor":(start.saturating_add(limit)<files.len()).then(||start.saturating_add(limit))});
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn layer(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let library = state.system.instructions().snapshot();
    let file = library
        .layers
        .iter()
        .find(|file| file.id == id)
        .ok_or_else(ApiError::not_found)?;
    let mut value = metadata(file, &library);
    value["content"] = file.content.clone().into();
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteLayer {
    name: String,
    content: String,
    revision: Option<String>,
}
async fn write_layer(
    state: &ServiceState,
    headers: &HeaderMap,
    id: Option<&str>,
    request: WriteLayer,
) -> ApiResult<Json<Value>> {
    manage(state, headers).await?;
    if id.is_none() && request.revision.is_some() {
        return Err(ApiError::invalid());
    }
    let file = state
        .system
        .instructions()
        .save(
            id,
            request.revision.as_deref(),
            &request.name,
            request.content,
        )
        .map_err(ApiError::from_report)?;
    let mut value = metadata(&file, &state.system.instructions().snapshot());
    value["content"] = file.content.into();
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn create_layer(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(request): Json<WriteLayer>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Ok((
        StatusCode::CREATED,
        write_layer(&state, &headers, None, request).await?,
    ))
}
async fn update_layer(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<WriteLayer>,
) -> ApiResult<Json<Value>> {
    write_layer(&state, &headers, Some(&id), request).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleQuery {
    #[serde(default)]
    role: Role,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceLayers {
    revision: String,
    layer_ids: Vec<String>,
}
async fn replace_layers(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<RoleQuery>,
    Json(request): Json<ReplaceLayers>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    let core = query.role == Role::Core;
    state
        .system
        .instructions()
        .replace(query.role, request.layer_ids, |library| {
            if SystemPrompt::for_role_with_layers(&state.system, core, library)?.revision()
                != request.revision
            {
                return Err(crate::actors::ActorError::Conflict(
                    "System instructions changed".into(),
                )
                .into());
            }
            Ok(())
        })
        .map_err(ApiError::from_report)?;
    let prompt = SystemPrompt::for_role(&state.system, core).map_err(ApiError::from_report)?;
    Ok(Json(json!({"revision":prompt.revision()})))
}
