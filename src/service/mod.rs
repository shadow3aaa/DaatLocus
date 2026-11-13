//! Actor-scoped HTTP queries and resumable semantic event streams.
mod admin;
mod approvals;
mod apps;
mod auth;
mod conversation;
mod imports;
mod prompt;
mod remote;
mod remote_sessions;
mod reset;
mod resources;
mod sessions;
mod stages;
mod stream;
mod virtual_displays;
mod webui;
pub(crate) use auth::native_bootstrap;

use crate::{
    actors::{ActorRecord, ActorSystem, Attachment, MessageRecord, Page},
    config::{ProviderConfig, resolve_env_reference},
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
    /// Set only for bounded session operations by the OS-owner IPC dispatcher.
    control_owner: bool,
    /// Authenticated relay peer, set only by the native peer dispatcher.
    relay_owner: Option<crate::stage::StageId>,
}

impl ServiceState {
    pub fn new(system: ActorSystem, public_origin: String) -> miette::Result<Self> {
        let stage = crate::stage::host::Host::load(system.clone())
            .map_err(|_| miette::miette!("Cannot load private account state"))?;
        let authentication = auth::Authentication::new(&system)?;
        Ok(Self {
            stage: Some(crate::stage::runtime::Runtime::new(stage)),
            system,
            origins: Arc::new(trusted_origins(vec![public_origin])),
            auth_flows: Arc::new(Mutex::new(HashMap::new())),
            authentication: Arc::new(Mutex::new(authentication)),
            codex_imports: Arc::new(Default::default()),
            listen_address: None,
            bootstrap_owner: false,
            control_owner: false,
            relay_owner: None,
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
        .route("/v1/actors/{actor}/messages", get(messages))
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
        .merge(sessions::routes())
        .merge(admin::routes())
        .merge(virtual_displays::routes())
        .merge(apps::routes())
        .merge(imports::routes())
        .merge(prompt::routes())
        .merge(stages::routes())
        .merge(remote::routes())
        .merge(remote_sessions::routes())
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
    if let Some(peer) = state.relay_owner {
        let runtime = state.stage.as_ref().ok_or_else(ApiError::forbidden)?;
        return crate::stage::owner_http::authorize(&runtime.host, peer).map_err(stages::failure);
    }
    if state.control_owner {
        return Ok(());
    }
    if headers.contains_key(axum::http::header::AUTHORIZATION) {
        return Err(ApiError::unauthorized());
    }
    auth::authorize_session(state, headers).await
}

/// Route a bounded maintenance request from the OS-owner daemon control channel.
/// This bypass is never reachable from HTTP; the caller is the authenticated local IPC dispatcher.
pub(crate) async fn owner_control_request(
    mut state: ServiceState,
    method: axum::http::Method,
    uri: &str,
    body: Vec<u8>,
) -> miette::Result<(u16, Value)> {
    use tower::ServiceExt;
    state.control_owner = true;
    let origin = state
        .origins
        .first()
        .ok_or_else(|| miette::miette!("Local service origin is unavailable"))?;
    let request = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("origin", origin)
        .header("x-daat-request", "1")
        .header("idempotency-key", uuid::Uuid::new_v4().to_string())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .map_err(|_| miette::miette!("Invalid local session request"))?;
    let response = router(state).oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .map_err(|_| miette::miette!("Local session response exceeds its limit"))?;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| miette::miette!("Invalid local session response"))?
    };
    Ok((status, value))
}

pub(crate) async fn relay_request(
    host: Arc<crate::stage::host::Host>,
    peer: crate::stage::StageId,
    request: crate::stage::owner_http::Request,
) -> crate::stage::Result<crate::stage::owner_http::Reply> {
    use crate::stage::{StageError, owner_http};
    use tower::ServiceExt;
    owner_http::authorize(&host, peer)?;
    let path = request.uri.split('?').next().unwrap_or("");
    if !(path == "/v1/sessions" || path.starts_with("/v1/sessions/") || path == "/v1/events") {
        return Err(StageError::Forbidden(
            "Relay route is outside session authority",
        ));
    }
    if path.ends_with("/event-stream") || !request.uri.starts_with("/v1/") {
        return Err(StageError::Invalid("Use bounded event reads"));
    }
    let origin = "http://relay-session.invalid";
    let mut state = ServiceState::new(host.system.clone(), origin.into())
        .map_err(|_| StageError::Unavailable("Cannot prepare session service"))?;
    state.stage = Some(crate::stage::runtime::Runtime::new(host.clone()));
    state.relay_owner = Some(peer);
    let mut builder = axum::http::Request::builder()
        .method(request.method.as_str())
        .uri(&request.uri)
        .header("origin", origin)
        .header("x-daat-request", "1");
    for (name, value) in request.headers {
        if !owner_http::forwarded_header(&name) {
            return Err(StageError::Forbidden("Relay header is not permitted"));
        }
        builder = builder.header(name, value);
    }
    let input = builder
        .body(axum::body::Body::from(request.body))
        .map_err(|_| StageError::Invalid("Invalid relay request"))?;
    let response = router(state).oneshot(input).await.unwrap();
    owner_http::authorize(&host, peer)?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = axum::body::to_bytes(
        response.into_body(),
        crate::daemon::attachments::MAX_FILE_ATTACHMENT_BYTES + 1024 * 1024,
    )
    .await
    .map_err(|_| StageError::Capacity("Session response exceeds transport budget"))?
    .to_vec();
    Ok(owner_http::Reply {
        status,
        content_type,
        body,
    })
}

pub(crate) async fn access_actor(
    state: &ServiceState,
    headers: &HeaderMap,
    id: &str,
    converse: bool,
) -> ApiResult<()> {
    owner(state, headers).await?;
    let actor = state.system.actor(id).map_err(ApiError::from_report)?;
    if converse {
        if actor.role != crate::actors::ActorRole::Session {
            return Err(ApiError::forbidden());
        }
        if actor.status == crate::actors::ActorStatus::Archived {
            return Err(ApiError::from_report(
                crate::actors::ActorError::Conflict("Session is archived".into()).into(),
            ));
        }
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

pub(crate) fn redact(value: &mut Value, system: &ActorSystem) {
    let config = system.config();
    let mut secrets: Vec<String> = system
        .websearch_settings()
        .get()
        .keys
        .values()
        .map(|key| resolve_env_reference(key))
        .filter(|key| !key.is_empty())
        .collect();
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
    session: Option<String>,
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
        .actor_page(query.session.as_deref(), query.after.as_deref(), limit + 1)
        .map_err(ApiError::from_report)?;
    let next = (actors.len() > limit).then(|| actors[limit - 1].id.clone());
    actors.truncate(limit);
    let mut value = json!({"items": actors, "next_cursor": next, "event_cursor": cursor});
    if let Some(items) = value.get_mut("items").and_then(Value::as_array_mut) {
        for actor in items {
            actor_conversation_link(actor);
        }
    }
    redact(&mut value, &state.system);
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
    redact(&mut value, &state.system);
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorPreferencesPatch {
    #[serde(default, deserialize_with = "present_model")]
    model: Option<Option<String>>,
}
fn present_model<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
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
    if patch.model.is_none() {
        return Err(ApiError::invalid());
    }
    let record = state
        .system
        .patch_owned_preferences(&actor, patch.model)
        .await
        .map_err(ApiError::from_report)?;
    let mut value = public_actor(&record);
    redact(&mut value, &state.system);
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
    redact(&mut value, &state.system);
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
    if query.before.is_none() {
        value["active"] = serde_json::to_value(
            state
                .system
                .store()
                .current_message(&actor)
                .map_err(ApiError::from_report)?,
        )
        .map_err(|_| ApiError::internal())?;
    }
    redact(&mut value, &state.system);
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
    redact(&mut value, &state.system);
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
    redact(&mut value, &state.system);
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
    redact(&mut value, &state.system);
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    content: String,
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(default)]
    skills: Vec<crate::actors::SkillReference>,
}
async fn send_core_message(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path(actor): Path<String>,
    Json(request): Json<SendRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let actor = actor.as_str();
    access_actor(&state, &headers, actor, true).await?;
    if state.system.config().validate().is_err() {
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
    let key = key.ok_or_else(ApiError::invalid)?;
    let mut attachments = Vec::new();
    for id in request.attachments {
        let resource = state
            .system
            .store()
            .resource(actor, &id)
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
        .send_user_with_skills(
            actor,
            request.content,
            attachments,
            request.skills,
            Some(key),
        )
        .map_err(ApiError::from_report)?;
    let mut value = serde_json::to_value(record).map_err(|_| ApiError::internal())?;
    redact(&mut value, &state.system);
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
