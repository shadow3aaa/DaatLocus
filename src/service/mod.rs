//! Actor-scoped HTTP queries and resumable semantic event streams.
mod admin;
mod approvals;
mod apps;
mod auth;
mod computer_use;
mod conversation;
mod imports;
mod remote;
mod reset;
mod resources;
mod stages;
mod stream;
mod webui;
pub(crate) use auth::native_bootstrap;

use crate::{
    actors::{ActorRecord, ActorSystem, ApprovalMode, Attachment, MessageRecord, Page},
    config::{Config, ProviderConfig, resolve_env_reference},
    config_setup::PendingSetupProviderAuthFlow,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ServiceState {
    pub stage: Option<Arc<crate::stage::runtime::Runtime>>,
    pub system: ActorSystem,
    /// Trusted browser origins. Never derive authorization from Host or forwarded headers.
    origins: Arc<Vec<String>>,
    auth_flows: Arc<Mutex<HashMap<String, PendingSetupProviderAuthFlow>>>,
    authentication: Arc<Mutex<auth::Authentication>>,
    codex_imports: Arc<crate::conversation_import::sources::CodexSources>,
    listen_address: Option<std::net::SocketAddr>,
    /// Set only by the authenticated OS-owner IPC dispatcher, never by HTTP input.
    bootstrap_owner: bool,
}

impl ServiceState {
    pub fn new(system: ActorSystem, public_origin: String) -> miette::Result<Self> {
        let stage = crate::stage::host::Host::load(system.clone())
            .map_err(|_| miette::miette!("Cannot load private account state"))?;
        Ok(Self {
            stage: Some(crate::stage::runtime::Runtime::new(stage)),
            system,
            origins: Arc::new(trusted_origins(vec![public_origin])),
            auth_flows: Arc::new(Mutex::new(HashMap::new())),
            authentication: Arc::new(Mutex::new(auth::Authentication::new())),
            codex_imports: Arc::new(Default::default()),
            listen_address: None,
            bootstrap_owner: false,
        })
    }

    /// Actual bound endpoint used to verify local OS-owner bootstrap connections.
    pub fn with_listen_address(mut self, address: std::net::SocketAddr) -> Self {
        self.listen_address = Some(address);
        self
    }

    pub fn with_origins(mut self, origins: Vec<String>) -> Self {
        self.origins = Arc::new(trusted_origins(origins));
        self
    }
    /// Select the local Codex profile for an embedded service or isolated host fixture.
    pub fn with_codex_home(mut self, home: std::path::PathBuf) -> Self {
        self.codex_imports = Arc::new(crate::conversation_import::sources::CodexSources::new(
            Some(home),
        ));
        self
    }
}

fn trusted_origins(origins: Vec<String>) -> Vec<String> {
    origins
        .into_iter()
        .filter_map(|text| {
            let url = url::Url::parse(&text).ok()?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return None;
            }
            Some(url.origin().ascii_serialization())
        })
        .collect()
}

pub fn router(state: ServiceState) -> Router {
    Router::new()
        .route(
            "/v1/health",
            get(|| async { Json(json!({"protocol": 1, "version": env!("CARGO_PKG_VERSION")})) }),
        )
        .route("/v1/actors", get(list_actors))
        .route(
            "/v1/actors/{actor}",
            get(get_actor).patch(patch_actor_preferences),
        )
        .route(
            "/v1/actors/{actor}/messages",
            get(messages).post(send_message),
        )
        .route("/v1/actors/{actor}/messages/{message}", get(message))
        .route("/v1/actors/{actor}/messages/search", get(search_messages))
        .route(
            "/v1/actors/{actor}/messages/{message}/results",
            get(tool_results),
        )
        .route(
            "/v1/actors/{actor}/messages/{message}/cancel",
            post(cancel_message),
        )
        .route("/v1/actors/{actor}/results/{result}", get(tool_result))
        .route("/v1/events", get(stream::catalog_events))
        .route("/v1/actors/{actor}/events", get(stream::actor_events))
        .route("/v1/actors/{actor}/event-stream", get(stream::actor_stream))
        .route("/v1/event-stream", get(stream::catalog_stream))
        .route(
            "/v1/actors/{actor}/resources/{resource}",
            get(resources::download),
        )
        .route(
            "/v1/actors/{actor}/attachments",
            post(resources::upload).layer(DefaultBodyLimit::max(
                crate::daemon::attachments::MAX_FILE_ATTACHMENT_BYTES,
            )),
        )
        .merge(admin::routes())
        .merge(computer_use::routes())
        .merge(apps::routes())
        .merge(imports::routes())
        .merge(stages::routes())
        .merge(remote::routes())
        .merge(approvals::routes())
        .merge(auth::routes())
        .merge(reset::routes())
        .merge(conversation::routes())
        .fallback(webui::serve)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(response_contract))
        .with_state(state)
}

async fn response_contract(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let status = response.status();
    if (status.is_client_error() || status.is_server_error())
        && !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"))
    {
        response = (
            status,
            Json(json!({"code": "invalid_request", "message": "Request could not be processed"})),
        )
            .into_response();
    }
    if !response.headers().contains_key("cache-control") {
        response.headers_mut().insert(
            "cache-control",
            axum::http::HeaderValue::from_static("private, no-store"),
        );
    }
    response.headers_mut().insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    response
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}
impl ApiError {
    pub(crate) fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "Authentication required",
        }
    }
    pub(crate) fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: "Operation is outside the granted scope",
        }
    }
    pub(crate) fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: "Resource not found",
        }
    }
    pub(crate) fn invalid() -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_request",
            message: "Request is invalid",
        }
    }
    pub(crate) fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "not_ready",
            message: "Agent configuration is not ready",
        }
    }
    pub(crate) fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: "Operation could not be completed",
        }
    }
    pub(crate) fn from_report(error: miette::Report) -> Self {
        // Errors from infrastructure/providers can include paths or secrets. Never return them verbatim.
        match error.downcast_ref::<crate::actors::ActorError>() {
            Some(crate::actors::ActorError::Forbidden(_)) => Self::forbidden(),
            Some(crate::actors::ActorError::NotFound(_)) => Self::not_found(),
            Some(crate::actors::ActorError::Conflict(_)) => Self {
                status: StatusCode::CONFLICT,
                code: "conflict",
                message: "Operation conflicts with current state",
            },
            Some(crate::actors::ActorError::Invalid(_)) => Self::invalid(),
            Some(crate::actors::ActorError::Unavailable(_)) => Self::unavailable(),
            None => Self::internal(),
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"code": self.code, "message": self.message})),
        )
            .into_response()
    }
}
pub(crate) type ApiResult<T> = Result<T, ApiError>;

pub(crate) async fn owner(state: &ServiceState, headers: &HeaderMap) -> ApiResult<()> {
    if headers.contains_key(axum::http::header::AUTHORIZATION)
        || !auth::authorize_session(state, headers).await
    {
        return Err(ApiError::unauthorized());
    }
    Ok(())
}

pub(crate) async fn access_actor(
    state: &ServiceState,
    headers: &HeaderMap,
    id: &str,
    converse: bool,
) -> ApiResult<()> {
    owner(state, headers).await?;
    state.system.actor(id).map_err(ApiError::from_report)?;
    if converse {
        mutation_origin(state, headers, true).await?;
    }
    Ok(())
}

pub(crate) async fn mutation_origin(
    state: &ServiceState,
    headers: &HeaderMap,
    cookie: bool,
) -> ApiResult<()> {
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    if cookie
        && (origin.is_none()
            || headers.get("x-daat-request").and_then(|v| v.to_str().ok()) != Some("1"))
    {
        return Err(ApiError::forbidden());
    }
    if let Some(origin) = origin
        && !state.origins.iter().any(|o| o == origin)
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

pub(crate) async fn manage(state: &ServiceState, headers: &HeaderMap) -> ApiResult<()> {
    owner(state, headers).await?;
    if state
        .stage
        .as_ref()
        .is_some_and(|runtime| crate::stage::reset::blocked(&runtime.host))
    {
        return Err(ApiError::unavailable());
    }
    mutation_origin(state, headers, true).await
}

pub(crate) fn public_actor(actor: &ActorRecord) -> Value {
    let mut value = serde_json::to_value(actor).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("context");
    }
    actor_conversation_link(&mut value);
    value
}

pub(crate) fn actor_conversation_link(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        object.remove("depth");
        if let Some(id) = object.get("id").and_then(Value::as_str).map(str::to_owned) {
            object.insert(
                "conversation_url".into(),
                Value::String(format!("/actors/{id}")),
            );
        }
    }
}

pub(crate) fn redact(value: &mut Value, config: &Config) {
    let mut secrets = Vec::new();
    for provider in config.providers.values() {
        let key = match provider {
            ProviderConfig::Openai { api_key, .. }
            | ProviderConfig::AnthropicCompatible { api_key, .. }
            | ProviderConfig::OpenaiCompatible { api_key, .. } => Some(api_key.as_str()),
            ProviderConfig::GithubCopilot { github_token } => Some(github_token.as_str()),
            ProviderConfig::Ollama { api_key, .. } => api_key.as_deref(),
            _ => None,
        };
        if let Some(key) = key {
            let resolved = resolve_env_reference(key);
            if !resolved.is_empty() {
                secrets.push(resolved);
            }
        }
    }
    fn walk(value: &mut Value, secrets: &[String]) {
        match value {
            Value::String(text) => {
                for secret in secrets {
                    *text = text.replace(secret, "[redacted]");
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, secrets);
                }
            }
            Value::Object(map) => {
                map.remove("local_path");
                for (key, value) in map {
                    if matches!(
                        key.as_str(),
                        "api_key"
                            | "github_token"
                            | "access_token"
                            | "refresh_token"
                            | "authorization"
                            | "cookie"
                    ) {
                        *value = Value::Null;
                    } else {
                        walk(value, secrets);
                    }
                }
            }
            _ => {}
        }
    }
    walk(value, &secrets);
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorListQuery {
    after: Option<String>,
    limit: Option<usize>,
}
async fn list_actors(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Query(query): Query<ActorListQuery>,
) -> ApiResult<Json<Value>> {
    owner(&state, &headers).await?;
    let cursor = state
        .system
        .store()
        .event_cursor()
        .map_err(ApiError::from_report)?;
    let limit = bounded(query.limit, 100)?;
    let mut actors = state
        .system
        .store()
        .actor_page(query.after.as_deref(), limit + 1)
        .map_err(ApiError::from_report)?;
    let next = (actors.len() > limit).then(|| actors[limit - 1].id.clone());
    actors.truncate(limit);
    let mut value = json!({"items": actors, "next_cursor": next, "event_cursor": cursor});
    if let Some(items) = value.get_mut("items").and_then(Value::as_array_mut) {
        for actor in items {
            actor_conversation_link(actor);
        }
    }
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
pub(crate) fn bounded(value: Option<usize>, maximum: usize) -> ApiResult<usize> {
    let n = value.unwrap_or(50.min(maximum));
    if n == 0 || n > maximum {
        Err(ApiError::invalid())
    } else {
        Ok(n)
    }
}
async fn get_actor(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    let record = state.system.actor(&actor).map_err(ApiError::from_report)?;
    let mut value = public_actor(&record);
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorPreferencesPatch {
    #[serde(default, deserialize_with = "present_model")]
    model: Option<Option<String>>,
    #[serde(default, deserialize_with = "present_mode")]
    approval_mode: Option<ApprovalMode>,
}
fn present_model<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}
fn present_mode<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ApprovalMode>, D::Error> {
    ApprovalMode::deserialize(deserializer).map(Some)
}
async fn patch_actor_preferences(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Json(value): Json<Value>,
) -> ApiResult<Json<Value>> {
    manage(&state, &headers).await?;
    let patch: ActorPreferencesPatch =
        serde_json::from_value(value).map_err(|_| ApiError::invalid())?;
    if patch.model.is_none() && patch.approval_mode.is_none() {
        return Err(ApiError::invalid());
    }
    let record = state
        .system
        .patch_owned_preferences(&actor, patch.model, patch.approval_mode)
        .await
        .map_err(ApiError::from_report)?;
    let mut value = public_actor(&record);
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    before: Option<i64>,
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchQuery {
    q: String,
    before: Option<i64>,
    limit: Option<usize>,
}
async fn search_messages(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    let text = query.q.trim();
    if text.is_empty() || text.len() > 512 || query.before.is_some_and(|n| n <= 0) {
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
        .search_messages(&actor, text, query.before, bounded(query.limit, 100)?)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(page).map_err(|_| ApiError::internal())?;
    value["event_cursor"] = Value::from(cursor);
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn messages(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    // Captured before the query: changes racing the load are replayed, never lost.
    let cursor = state
        .system
        .store()
        .event_cursor()
        .map_err(ApiError::from_report)?;
    let page: Page<MessageRecord> = state
        .system
        .store()
        .messages(&actor, query.before, bounded(query.limit, 100)?)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(page).map_err(|_| ApiError::internal())?;
    value["event_cursor"] = Value::from(cursor);
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn message(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, id)): Path<(String, String)>,
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
    let mut value = serde_json::to_value(message).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
async fn tool_result(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    let result = state
        .system
        .store()
        .tool_result(&id)
        .map_err(ApiError::from_report)?;
    if result.actor_id != actor {
        return Err(ApiError::not_found());
    }
    let mut value = serde_json::to_value(result).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}

async fn tool_results(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, id)): Path<(String, String)>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    access_actor(&state, &headers, &actor, false).await?;
    if query.before.is_some_and(|before| before < 0) {
        return Err(ApiError::invalid());
    }
    let message = state
        .system
        .store()
        .message(&id)
        .map_err(ApiError::from_report)?;
    if message.actor_id != actor {
        return Err(ApiError::not_found());
    }
    let results = state
        .system
        .store()
        .tool_summaries(&actor, &id, query.before, bounded(query.limit, 100)?)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(results).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    content: String,
    #[serde(default)]
    attachments: Vec<String>,
}
async fn send_message(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Json(request): Json<SendRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    access_actor(&state, &headers, &actor, true).await?;
    if actor == crate::stage::shared::ACTOR_ID && !state.system.core_permitted() {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "core_not_ready",
            message: "Conversation is connecting or synchronizing; retry when ready",
        });
    }
    if actor != crate::stage::shared::ACTOR_ID && state.system.config().validate().is_err() {
        return Err(ApiError::unavailable());
    }
    if request.content.len() > 64 * 1024
        || (request.content.trim().is_empty() && request.attachments.is_empty())
    {
        return Err(ApiError::invalid());
    }
    let key = headers
        .get("idempotency-key")
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| ApiError::invalid())?;
    if key.as_ref().is_some_and(|key| {
        key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic())
    }) {
        return Err(ApiError::invalid());
    }
    let mut attachments = Vec::new();
    for id in request.attachments {
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
    let record = state
        .system
        .send_user(&actor, request.content, attachments, key)
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(record).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system.config());
    Ok((StatusCode::ACCEPTED, Json(value)))
}
async fn cancel_message(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, message)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    access_actor(&state, &headers, &actor, true).await?;
    let record = state
        .system
        .store()
        .message(&message)
        .map_err(ApiError::from_report)?;
    if record.actor_id != actor {
        return Err(ApiError::not_found());
    }
    state
        .system
        .cancel(&actor, &message)
        .map_err(ApiError::from_report)?;
    Ok(StatusCode::ACCEPTED)
}
