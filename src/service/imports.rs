//! Owner-selected Codex rollouts become immutable Core evidence before submission.
use super::{ApiError, ApiResult, ServiceState, manage, owner};
use crate::conversation_import::{self, Transcript};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn routes() -> Router<ServiceState> {
    Router::new()
        .route("/v1/imports", get(capabilities))
        .route("/v1/imports/codex", get(list).post(submit))
        .route("/v1/imports/codex/preview", post(preview))
}
async fn capabilities(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    Ok(Json(json!({"codex":state.codex_imports.supported()})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    after: Option<String>,
    search: Option<String>,
    limit: Option<usize>,
    refresh: Option<bool>,
}
async fn list(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<List>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    if !state.codex_imports.supported() {
        return Err(ApiError::not_found());
    }
    let limit = super::bounded(query.limit, 24)?;
    if query.search.as_ref().is_some_and(|value| value.len() > 512)
        || query.after.as_ref().is_some_and(|value| {
            value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(ApiError::invalid());
    }
    let sources = state.codex_imports.clone();
    let (items, next_cursor) = tokio::task::spawn_blocking(move || {
        sources.list(
            query.after.as_deref(),
            query.search.as_deref().unwrap_or(""),
            limit,
            query.refresh.unwrap_or(false),
        )
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(ApiError::from_report)?;
    owner(&state, &headers).await?;
    Ok(Json(json!({"items":items,"next_cursor":next_cursor})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedSource {
    source_id: String,
}
async fn preview(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(input): Json<SelectedSource>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    if !state.codex_imports.supported() {
        return Err(ApiError::not_found());
    }
    let sources = state.codex_imports.clone();
    let transcript = tokio::task::spawn_blocking(move || sources.load(&input.source_id))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|error| {
            if error.downcast_ref::<crate::actors::ActorError>().is_some() {
                ApiError::from_report(error)
            } else {
                invalid_codex()
            }
        })?;
    manage(&state, &headers).await?;
    let title = transcript.title();
    let bytes = serde_json::to_vec(&transcript).map_err(|_| ApiError::internal())?;
    // Owner-only staging is not an Actor resource and needs no preset identity.
    let resource = format!("r-{}", crate::stage::digest(&bytes));
    state.system.store().stage_transaction(|tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS owner_imports(id TEXT PRIMARY KEY,title TEXT NOT NULL,bytes BLOB NOT NULL);")?;
        tx.execute("INSERT OR IGNORE INTO owner_imports VALUES(?1,?2,?3)",params![resource,title,bytes])?;
        Ok(())
    }).map_err(super::stages::failure)?;
    Ok(Json(
        json!({"resource_id":resource,"title":title,"message_count":transcript.messages.len()}),
    ))
}
fn invalid_codex() -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_codex_conversation",
        message: "This Codex conversation could not be read",
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submit {
    resource_id: String,
    /// An existing local session; omission or null starts a new one.
    session_id: Option<String>,
}
async fn submit(
    State(state): State<ServiceState>,
    mut headers: HeaderMap,
    Json(input): Json<Submit>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    manage(&state, &headers).await?;
    if !state.codex_imports.supported() {
        return Err(ApiError::not_found());
    }
    if input.resource_id.len() > 128 {
        return Err(ApiError::invalid());
    }
    let (title,bytes) = state.system.store().stage_transaction(|tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS owner_imports(id TEXT PRIMARY KEY,title TEXT NOT NULL,bytes BLOB NOT NULL);")?;
        Ok(tx.query_row("SELECT title,bytes FROM owner_imports WHERE id=?1",[&input.resource_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Vec<u8>>(1)?))).optional()?)
    }).map_err(super::stages::failure)?.ok_or_else(ApiError::not_found)?;
    Transcript::decode(&bytes).map_err(|_| invalid_codex())?;
    let (session, key) = match input.session_id {
        Some(session) => {
            // Reject workers, unknown sessions and archived targets before publishing bytes.
            let summary = state
                .system
                .store()
                .session_summary(&session)
                .map_err(ApiError::from_report)?;
            if summary.status == crate::actors::ActorStatus::Archived {
                return Err(ApiError::from_report(
                    crate::actors::ActorError::Conflict("Session is archived".into()).into(),
                ));
            }
            let key = format!(
                "codex-import-{}",
                crate::stage::digest(format!("{}\n{session}", input.resource_id).as_bytes())
            );
            (session, key)
        }
        None => {
            let key = format!("codex-import-{}", input.resource_id);
            let session = state
                .system
                .create_session(Some(title.clone()), None, Some(&key))
                .await
                .map_err(ApiError::from_report)?
                .id;
            (session, key)
        }
    };
    let resource = crate::resources::publish(
        &state.system,
        &session,
        &title,
        conversation_import::MEDIA_TYPE,
        &bytes,
    )
    .map_err(super::stages::failure)?;
    headers.insert(
        "idempotency-key",
        key.parse().map_err(|_| ApiError::invalid())?,
    );
    super::send_core_message(
        State(state),
        headers,
        Path(session),
        Json(super::SendRequest {
            skills: Vec::new(),
            content: "Import the attached conversation.".into(),
            attachments: vec![resource.id],
        }),
    )
    .await
}
