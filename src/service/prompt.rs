//! Owner inspection of static instructions; no worker, model or App is started.
use super::{ApiError, ApiResult, ServiceState, bounded, owner, redact};
use crate::{actors::prompt::SystemPrompt, context_budget::approx_token_count};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/system-prompt", get(files))
        .route("/v1/system-prompt/{index}", get(file))
}

fn revision(prompt: &SystemPrompt) -> String {
    // Names and ordered bytes both identify the inspected assembly.
    crate::stage::digest(
        &serde_json::to_vec(
            &prompt
                .files
                .iter()
                .map(|file| (&file.name, &file.content))
                .collect::<Vec<_>>(),
        )
        .expect("strings serialize"),
    )
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum Role {
    #[default]
    Core,
    Others,
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
        "revision": revision(&prompt),
        "total_tokens": approx_token_count(&prompt.render()),
        "items": prompt.files.iter().enumerate().skip(start).take(limit).map(|(index, file)| json!({"index":index,"name":file.name,"bytes":file.content.len(),"tokens":approx_token_count(&file.content)})).collect::<Vec<_>>(),
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
    if revision(&prompt) != query.revision {
        return Err(ApiError {
            status: axum::http::StatusCode::CONFLICT,
            code: "conflict",
            message: "System instructions changed; refresh the file list",
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
