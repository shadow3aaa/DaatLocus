use std::{
    net::Ipv4Addr,
    path::{Path as StdPath, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header::AUTHORIZATION},
    response::IntoResponse,
    routing::{get, post},
};
use axum::{
    body::Body,
    http::{
        Uri,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::Response,
};
use base64::Engine;
use futures_util::StreamExt;
use include_dir::{Dir, include_dir};
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sysinfo::{Pid, System};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::{
    config::{Config, ModelConfig, ProviderConfig, ThinkingBudget, load_config},
    daat_locus_paths::{daat_locus_paths, daat_locus_paths_sync},
    dashboard::{
        DashboardActivityHistoryStore, DashboardCommandRunner, DashboardControlCommand,
        DashboardIncomingAttachment, DashboardState, execute_remote_command,
        execute_remote_message,
    },
    events::EventStore,
    model_catalog::catalog_model_capacity,
    pending_work::PendingWorkQueue,
    sandbox::StrongFilesystemSandboxMode,
    telegram_acl::TelegramAclHandle,
};

mod auth;
mod logs;

pub use auth::{
    CreatedDaemonToken, DaemonAuthToken, DaemonTokenListEntry, DaemonTokenRegistryHandle,
    create_daemon_token, list_daemon_tokens, load_daemon_auth_token,
    load_or_create_daemon_token_registry, revoke_daemon_token, rotate_daemon_token,
};

pub const DAEMON_BIND_HOST: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
pub const DAEMON_CLIENT_HOST: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
pub const DAEMON_HOST_DISPLAY: &str = "0.0.0.0";
/// Daemon cold start can include browser runtime install plus hindsight/uv
/// first-run setup. Hindsight itself allows 10 minutes for daemon start, so the
/// outer readiness window must be longer than that inner startup budget.
const READY_TIMEOUT: Duration = Duration::from_secs(900);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);
const DAEMON_MAIN_LOG: &str = "daat-locus.log";
const DAEMON_STDERR_LOG: &str = "daemon-stderr.log";
pub const DAEMONIZE_ENV: &str = "DAAT_LOCUS_DAEMONIZE";
const MAX_COMMAND_ATTACHMENTS: usize = 4;
const MAX_COMMAND_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

static EMBEDDED_WEBUI_DIST: Dir<'_> = include_dir!("$OUT_DIR/webui-dist");

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub pid: u32,
    pub started_at_ms: i64,
    pub version: String,
    pub bind_host: String,
    pub port: u16,
    pub state: DaemonLifecycleState,
    pub connected_clients: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonLifecycleState {
    Initializing,
    Ready,
    Stopping,
    Failed,
}

impl DaemonLifecycleState {
    fn as_u8(self) -> u8 {
        match self {
            Self::Initializing => 0,
            Self::Ready => 1,
            Self::Stopping => 2,
            Self::Failed => 3,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Ready,
            2 => Self::Stopping,
            3 => Self::Failed,
            _ => Self::Initializing,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::Ready => "ready",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }

    fn allows_runtime_commands(self) -> bool {
        matches!(self, Self::Ready)
    }
}

impl std::fmt::Display for DaemonLifecycleState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct DaemonLifecycleHandle {
    state: Arc<AtomicU8>,
}

impl DaemonLifecycleHandle {
    pub fn new(state: DaemonLifecycleState) -> Self {
        Self {
            state: Arc::new(AtomicU8::new(state.as_u8())),
        }
    }

    pub fn get(&self) -> DaemonLifecycleState {
        DaemonLifecycleState::from_u8(self.state.load(Ordering::SeqCst))
    }

    pub fn set(&self, state: DaemonLifecycleState) {
        self.state.store(state.as_u8(), Ordering::SeqCst);
    }

    pub fn mark_ready(&self) {
        self.set(DaemonLifecycleState::Ready);
    }

    pub fn mark_stopping(&self) {
        self.set(DaemonLifecycleState::Stopping);
    }

    pub fn mark_failed_if_initializing(&self) {
        let _ = self.state.compare_exchange(
            DaemonLifecycleState::Initializing.as_u8(),
            DaemonLifecycleState::Failed.as_u8(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: String,
    #[serde(default)]
    pub attachments: Vec<CommandAttachmentRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandAttachmentRequest {
    pub name: String,
    pub media_type: String,
    pub data_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResponse {
    pub output: String,
}

#[derive(Debug, Serialize)]
pub struct SettingsSummaryResponse {
    pub loaded_at_ms: i64,
    pub home_path: String,
    pub config_path: String,
    pub locale: String,
    pub locale_label: String,
    pub main_model: String,
    pub judge_model: String,
    pub hindsight_model: String,
    pub providers: Vec<SettingsProviderSummary>,
    pub models: Vec<SettingsModelSummary>,
    pub daemon: SettingsDaemonSummary,
    pub judge: SettingsJudgeSummary,
    pub sandbox: SettingsSandboxSummary,
    pub hindsight: SettingsHindsightSummary,
    pub telegram: SettingsTelegramSummary,
}

#[derive(Debug, Serialize)]
pub struct SettingsProviderSummary {
    pub name: String,
    pub provider_type: &'static str,
    pub base_url: Option<String>,
    pub credential: SettingsCredentialSummary,
    pub auth_file: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SettingsCredentialSummary {
    pub status: SettingsCredentialStatus,
    pub source: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsCredentialStatus {
    Configured,
    EnvConfigured,
    EnvMissing,
    Missing,
    Placeholder,
    OauthFile,
}

#[derive(Debug, Serialize)]
pub struct SettingsModelSummary {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    pub is_main: bool,
    pub is_judge: bool,
    pub is_hindsight: bool,
    pub temperature: f64,
    pub thinking_budget: Option<&'static str>,
    pub rpm: Option<u32>,
    pub request_timeout_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub context_window_tokens: usize,
    pub effective_context_window_percent: i64,
    pub effective_context_window_tokens: usize,
    pub auto_compact_token_limit: usize,
    pub max_completion_tokens: usize,
    pub tool_output_max_tokens: usize,
    /// Whether the model accepts image/vision input in messages (resolved).
    pub supports_vision: bool,
}

#[derive(Debug, Serialize)]
pub struct SettingsDaemonSummary {
    pub bind_host: String,
    pub configured_port: u16,
    pub serving_port: u16,
}

#[derive(Debug, Serialize)]
pub struct SettingsJudgeSummary {
    pub enabled: bool,
    pub model: Option<String>,
    pub effective_model: String,
    pub max_pairwise_candidates: usize,
    pub max_pairwise_cases: usize,
}

#[derive(Debug, Serialize)]
pub struct SettingsSandboxSummary {
    pub enabled: bool,
    pub strong_filesystem: &'static str,
}

#[derive(Debug, Serialize)]
pub struct SettingsHindsightSummary {
    pub namespace: String,
    pub bank_id: String,
    pub request_timeout_secs: u64,
    pub profile: String,
    pub port: u16,
    pub model: Option<String>,
    pub effective_model: String,
}

#[derive(Debug, Serialize)]
pub struct SettingsTelegramSummary {
    pub enabled: bool,
    pub credential: SettingsCredentialSummary,
    pub has_real_credentials: bool,
    pub poll_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
struct DashboardStreamQuery {
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DashboardActivityHistoryQuery {
    before: Option<i64>,
    after: Option<i64>,
    limit: Option<usize>,
}

#[derive(Debug)]
pub enum DaemonControlCommand {
    ShutdownRequested { completion_tx: oneshot::Sender<()> },
    RestartRequested,
}

#[derive(Clone)]
struct ServerState {
    started_at_ms: i64,
    bind_host: String,
    port: u16,
    auth_registry: DaemonTokenRegistryHandle,
    lifecycle: DaemonLifecycleHandle,
    dashboard_rx: watch::Receiver<DashboardState>,
    dashboard_history: DashboardActivityHistoryStore,
    telegram_acl: TelegramAclHandle,
    events: EventStore,
    pending_work: PendingWorkQueue,
    dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    connected_clients: Arc<std::sync::atomic::AtomicUsize>,
    shutdown_notify: Arc<Notify>,
}

pub struct DaemonServerHandle {
    pub port: u16,
    join: JoinHandle<()>,
}

pub struct DaemonServerStartParams {
    pub port: u16,
    pub auth_registry: DaemonTokenRegistryHandle,
    pub lifecycle: DaemonLifecycleHandle,
    pub dashboard_rx: watch::Receiver<DashboardState>,
    pub dashboard_history: DashboardActivityHistoryStore,
    pub telegram_acl: TelegramAclHandle,
    pub events: EventStore,
    pub pending_work: PendingWorkQueue,
    pub dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    pub daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    pub shutdown_rx: oneshot::Receiver<()>,
    pub shutdown_notify: Arc<Notify>,
}

impl DaemonServerHandle {
    pub async fn shutdown(self) {
        let _ = self.join.await;
    }
}

#[derive(Serialize, Deserialize)]
struct LockFileState {
    pid: u32,
    started_at_ms: i64,
}

pub struct DaemonLock {
    path: PathBuf,
    acquired: bool,
}

impl DaemonLock {
    pub async fn acquire() -> Result<Self> {
        let paths = daat_locus_paths().await;
        let path = paths.daemon_lock_file();
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let pid = std::process::id();
        let state = LockFileState { pid, started_at_ms };

        loop {
            match tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .await
            {
                Ok(mut file) => {
                    let bytes = serde_json::to_vec(&state)
                        .map_err(|err| miette!("serialize daemon lock failed: {err}"))?;
                    tokio::io::AsyncWriteExt::write_all(&mut file, &bytes)
                        .await
                        .map_err(|err| miette!("write daemon lock failed: {err}"))?;
                    return Ok(Self {
                        path,
                        acquired: true,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if stale_lock_can_be_removed(&path).await? {
                        let _ = tokio::fs::remove_file(&path).await;
                        continue;
                    }
                    return Err(miette!("daemon already running"));
                }
                Err(err) => return Err(miette!("create daemon lock failed: {err}")),
            }
        }
    }

    pub fn release(&mut self) {
        if self.acquired {
            let _ = std::fs::remove_file(&self.path);
            self.acquired = false;
        }
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        if self.acquired {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn stale_lock_can_be_removed(path: &PathBuf) -> Result<bool> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(miette!("read daemon lock failed: {err}")),
    };
    let state: LockFileState = match serde_json::from_slice(&bytes) {
        Ok(state) => state,
        Err(_) => return Ok(true),
    };
    Ok(!process_exists(state.pid))
}

fn process_exists(pid: u32) -> bool {
    let system = System::new_all();
    system.process(Pid::from_u32(pid)).is_some()
}

pub async fn start_server(params: DaemonServerStartParams) -> Result<DaemonServerHandle> {
    let DaemonServerStartParams {
        port,
        auth_registry,
        lifecycle,
        dashboard_rx,
        dashboard_history,
        telegram_acl,
        events,
        pending_work,
        dashboard_control_tx,
        daemon_control_tx,
        shutdown_rx,
        shutdown_notify,
    } = params;

    let listener = TcpListener::bind((DAEMON_BIND_HOST, port))
        .await
        .map_err(|err| {
            miette!(
                "bind daemon listener on {}:{} failed: {err}",
                DAEMON_HOST_DISPLAY,
                port
            )
        })?;
    let local_addr = listener
        .local_addr()
        .map_err(|err| miette!("read daemon listener address failed: {err}"))?;
    let started_at_ms = chrono::Utc::now().timestamp_millis();

    let app_state = ServerState {
        started_at_ms,
        bind_host: DAEMON_HOST_DISPLAY.to_string(),
        port: local_addr.port(),
        auth_registry,
        lifecycle,
        dashboard_rx,
        dashboard_history,
        telegram_acl,
        events,
        pending_work,
        dashboard_control_tx,
        daemon_control_tx,
        connected_clients: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_notify,
    };

    let router = Router::new()
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/dashboard/snapshot", get(snapshot_handler))
        .route("/dashboard/stream", get(stream_handler))
        .route("/dashboard/activity-history", get(activity_history_handler))
        .route(
            "/dashboard/attachments/{encoded_path}",
            get(dashboard_attachment_handler),
        )
        .route("/settings/summary", get(settings_summary_handler))
        .route("/logs/sources", get(logs::sources_handler))
        .route("/logs/read", get(logs::read_handler))
        .route("/commands/run", post(command_handler))
        .route("/daemon/shutdown", post(shutdown_handler))
        .route("/daemon/restart", post(restart_handler))
        .with_state(app_state.clone());

    let router = router.fallback(get(embedded_webui_handler));

    let join = tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(err) = server.await {
            tracing::error!("daemon http server failed: {err}");
        }
    });

    Ok(DaemonServerHandle {
        port: local_addr.port(),
        join,
    })
}

async fn embedded_webui_handler(uri: Uri) -> Response {
    let request_path = uri.path().trim_start_matches('/');
    let asset_path = if request_path.is_empty() {
        "index.html"
    } else {
        request_path
    };

    if !is_safe_embedded_webui_path(asset_path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Some(response) = embedded_webui_asset_response(asset_path) {
        return response;
    }

    if is_daemon_api_path(asset_path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if !looks_like_static_asset_path(asset_path)
        && let Some(response) = embedded_webui_asset_response("index.html")
    {
        return response;
    }

    StatusCode::NOT_FOUND.into_response()
}

fn embedded_webui_asset_response(path: &str) -> Option<Response> {
    let file = EMBEDDED_WEBUI_DIST.get_file(path)?;
    let mut response = Response::new(Body::from(file.contents().to_vec()));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static(webui_content_type(path)),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(webui_cache_control(path)),
    );
    Some(response)
}

fn is_safe_embedded_webui_path(path: &str) -> bool {
    !path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
}

fn looks_like_static_asset_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.'))
}

fn is_daemon_api_path(path: &str) -> bool {
    matches!(
        path.split('/').next().unwrap_or_default(),
        "commands" | "daemon" | "dashboard" | "health" | "logs" | "settings" | "status"
    )
}

fn webui_content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "css" => "text/css; charset=utf-8",
        "gif" => "image/gif",
        "html" => "text/html; charset=utf-8",
        "ico" => "image/x-icon",
        "jpg" | "jpeg" => "image/jpeg",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn webui_cache_control(path: &str) -> &'static str {
    if path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    }
}

async fn dashboard_attachment_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<DashboardAttachmentQuery>,
    Path(encoded_path): Path<String>,
) -> impl IntoResponse {
    if !authorize_dashboard_attachment_request(&state, &headers, query.token.as_deref()).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Some(path) = decode_dashboard_attachment_path(&encoded_path) else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    if !is_allowed_dashboard_attachment_path(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Ok(bytes) = tokio::fs::read(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    Response::builder()
        .header(CONTENT_TYPE, webui_content_type(&path.to_string_lossy()))
        .header(CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn store_command_attachments(
    attachments: &[CommandAttachmentRequest],
) -> std::result::Result<Vec<DashboardIncomingAttachment>, String> {
    if attachments.len() > MAX_COMMAND_ATTACHMENTS {
        return Err(format!(
            "too many image attachments: maximum is {MAX_COMMAND_ATTACHMENTS}"
        ));
    }

    let mut stored = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        stored.push(store_command_attachment(attachment).await?);
    }
    Ok(stored)
}

async fn store_command_attachment(
    attachment: &CommandAttachmentRequest,
) -> std::result::Result<DashboardIncomingAttachment, String> {
    let media_type =
        normalize_dashboard_image_media_type(&attachment.media_type).ok_or_else(|| {
            "only PNG, JPEG, WebP, and GIF image attachments are supported".to_string()
        })?;
    let bytes = decode_image_data_url(&attachment.data_url, &media_type)?;
    if bytes.len() > MAX_COMMAND_ATTACHMENT_BYTES {
        return Err(format!(
            "image attachment is too large: maximum is {} MiB",
            MAX_COMMAND_ATTACHMENT_BYTES / 1024 / 1024
        ));
    }

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let digest_hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let original_name = attachment.name.trim();
    let display_name = if original_name.is_empty() {
        "webui-image"
    } else {
        original_name
    };
    let extension = extension_for_dashboard_image(display_name, &media_type);
    let file_name = format!(
        "dashboard-{}-{}.{}",
        chrono::Utc::now().timestamp_millis(),
        &digest_hex[..16],
        extension
    );
    let dir = daat_locus_paths_sync()
        .state_dir()
        .join("dashboard_attachments");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("failed to create dashboard attachment directory: {err}"))?;
    let path = dir.join(file_name);
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|err| format!("failed to write dashboard attachment: {err}"))?;

    Ok(DashboardIncomingAttachment {
        media_type,
        local_path: path.display().to_string(),
        description: Some(format!("webui image {display_name}")),
    })
}

fn decode_image_data_url(
    data_url: &str,
    expected_media_type: &str,
) -> std::result::Result<Vec<u8>, String> {
    let trimmed = data_url.trim();
    let payload = if let Some(rest) = trimmed.strip_prefix("data:") {
        let (metadata, payload) = rest
            .split_once(',')
            .ok_or_else(|| "invalid image data URL".to_string())?;
        let mut metadata_parts = metadata.split(';');
        let data_media_type = metadata_parts
            .next()
            .and_then(normalize_dashboard_image_media_type)
            .ok_or_else(|| "unsupported image data URL media type".to_string())?;
        if data_media_type != expected_media_type {
            return Err(
                "image data URL media type does not match attachment media type".to_string(),
            );
        }
        if !metadata_parts.any(|part| part.eq_ignore_ascii_case("base64")) {
            return Err("image data URL must be base64 encoded".to_string());
        }
        payload
    } else {
        trimmed
    };

    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|err| format!("invalid base64 image attachment: {err}"))
}

fn normalize_dashboard_image_media_type(media_type: &str) -> Option<String> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("image/png".to_string()),
        "image/jpeg" | "image/jpg" => Some("image/jpeg".to_string()),
        "image/webp" => Some("image/webp".to_string()),
        "image/gif" => Some("image/gif".to_string()),
        _ => None,
    }
}

fn extension_for_dashboard_image(file_name: &str, media_type: &str) -> &'static str {
    match StdPath::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "png",
        Some("jpg") | Some("jpeg") => "jpg",
        Some("webp") => "webp",
        Some("gif") => "gif",
        _ => match media_type {
            "image/png" => "png",
            "image/webp" => "webp",
            "image/gif" => "gif",
            _ => "jpg",
        },
    }
}

fn decode_dashboard_attachment_path(encoded_path: &str) -> Option<PathBuf> {
    if encoded_path.is_empty() || !encoded_path.len().is_multiple_of(2) {
        return None;
    }

    let mut bytes = Vec::with_capacity(encoded_path.len() / 2);
    let mut chars = encoded_path.as_bytes().chunks_exact(2);
    for pair in &mut chars {
        let pair = std::str::from_utf8(pair).ok()?;
        let byte = u8::from_str_radix(pair, 16).ok()?;
        bytes.push(byte);
    }
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

fn is_allowed_dashboard_attachment_path(path: &std::path::Path) -> bool {
    if !path.is_absolute() {
        return false;
    }

    let paths = daat_locus_paths_sync();
    let Ok(canonical_path) = path.canonicalize() else {
        return false;
    };
    ["telegram_attachments", "dashboard_attachments"]
        .iter()
        .filter_map(|dir| paths.state_dir().join(dir).canonicalize().ok())
        .any(|attachments_dir| canonical_path.starts_with(attachments_dir))
}

#[derive(Debug, Deserialize)]
struct DashboardAttachmentQuery {
    token: Option<String>,
}

async fn authorize_dashboard_attachment_request(
    state: &ServerState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> bool {
    if state.auth_registry.authorize_headers(headers).await {
        return true;
    }

    let Some(token) = query_token.map(str::trim).filter(|token| !token.is_empty()) else {
        return false;
    };
    state.auth_registry.authorize_token(token).await
}

async fn health_handler() -> impl IntoResponse {
    Json(HealthResponse { ok: true })
}

async fn status_handler(State(state): State<ServerState>) -> impl IntoResponse {
    Json(StatusResponse {
        pid: std::process::id(),
        started_at_ms: state.started_at_ms,
        version: env!("CARGO_PKG_VERSION").to_string(),
        bind_host: state.bind_host.clone(),
        port: state.port,
        state: state.lifecycle.get(),
        connected_clients: state
            .connected_clients
            .load(std::sync::atomic::Ordering::Relaxed),
    })
    .into_response()
}

async fn snapshot_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(state.dashboard_rx.borrow().clone()).into_response()
}

async fn activity_history_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<DashboardActivityHistoryQuery>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let result = if let Some(after) = query.after {
        state
            .dashboard_history
            .query_after(Some(after), query.limit.unwrap_or(80))
    } else {
        state
            .dashboard_history
            .query_before(query.before, query.limit.unwrap_or(80))
    };

    match result {
        Ok(page) => Json(page).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("dashboard activity history query failed: {err:?}"),
        )
            .into_response(),
    }
}

async fn settings_summary_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let paths = daat_locus_paths().await;
    match load_config().await {
        Ok(config) => Json(settings_summary_response(
            &config,
            paths.root().display().to_string(),
            paths.config_file("config.toml").display().to_string(),
            state.port,
        ))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to load config summary: {error}"),
        )
            .into_response(),
    }
}

async fn stream_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<DashboardStreamQuery>,
) -> impl IntoResponse {
    let authorized = if let Some(token) = query
        .token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        state.auth_registry.authorize_token(token).await
    } else {
        state.auth_registry.authorize_headers(&headers).await
    };

    if !authorized {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| dashboard_ws(socket, state))
}

async fn command_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<CommandRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.lifecycle.get().allows_runtime_commands() {
        return runtime_not_ready_response(state.lifecycle.get());
    }
    let attachments = match store_command_attachments(&request.attachments).await {
        Ok(attachments) => attachments,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(CommandResponse { output: err }),
            )
                .into_response();
        }
    };
    if !attachments.is_empty() {
        let output = execute_remote_message(
            &request.command,
            attachments,
            &state.events,
            &state.pending_work,
        );
        return Json(CommandResponse { output }).into_response();
    }
    let snapshot = state.dashboard_rx.borrow().clone();
    let output = execute_remote_command(
        &request.command,
        &state.telegram_acl,
        &state.events,
        &state.pending_work,
        &snapshot,
        &state.dashboard_control_tx,
    );
    Json(CommandResponse { output }).into_response()
}

async fn shutdown_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    state.lifecycle.mark_stopping();
    let (completion_tx, completion_rx) = oneshot::channel();
    if state
        .daemon_control_tx
        .send(DaemonControlCommand::ShutdownRequested { completion_tx })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match completion_rx.await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn restart_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    state.lifecycle.mark_stopping();
    if state
        .daemon_control_tx
        .send(DaemonControlCommand::RestartRequested)
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    (StatusCode::ACCEPTED, "daemon restart scheduled").into_response()
}

fn runtime_not_ready_response(state: DaemonLifecycleState) -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!("daemon is {state}; runtime commands are accepted only when ready"),
    )
        .into_response()
}

fn settings_summary_response(
    config: &Config,
    home_path: String,
    config_path: String,
    serving_port: u16,
) -> SettingsSummaryResponse {
    let judge_effective_model = config
        .judge
        .model
        .clone()
        .unwrap_or_else(|| config.main_model.clone());
    let hindsight_effective_model = config
        .hindsight
        .model
        .clone()
        .unwrap_or_else(|| config.main_model.clone());

    let mut providers = config
        .providers
        .iter()
        .map(|(name, provider)| settings_provider_summary(name, provider))
        .collect::<Vec<_>>();
    providers.sort_by(|a, b| a.name.cmp(&b.name));

    let mut models = config
        .models
        .iter()
        .map(|(name, model)| {
            settings_model_summary(
                name,
                model,
                &config.main_model,
                &judge_effective_model,
                &hindsight_effective_model,
            )
        })
        .collect::<Vec<_>>();
    models.sort_by(|a, b| a.name.cmp(&b.name));

    SettingsSummaryResponse {
        loaded_at_ms: chrono::Utc::now().timestamp_millis(),
        home_path,
        config_path,
        locale: config.locale.as_str().to_string(),
        locale_label: config.locale.display_name().to_string(),
        main_model: config.main_model.clone(),
        judge_model: judge_effective_model.clone(),
        hindsight_model: hindsight_effective_model.clone(),
        providers,
        models,
        daemon: SettingsDaemonSummary {
            bind_host: DAEMON_HOST_DISPLAY.to_string(),
            configured_port: config.daemon.port,
            serving_port,
        },
        judge: SettingsJudgeSummary {
            enabled: config.judge.enabled,
            model: config.judge.model.clone(),
            effective_model: judge_effective_model,
            max_pairwise_candidates: config.judge.max_pairwise_candidates,
            max_pairwise_cases: config.judge.max_pairwise_cases,
        },
        sandbox: SettingsSandboxSummary {
            enabled: config.sandbox.enabled,
            strong_filesystem: strong_filesystem_mode_label(config.sandbox.strong_filesystem),
        },
        hindsight: SettingsHindsightSummary {
            namespace: config.hindsight.namespace.clone(),
            bank_id: config.hindsight.bank_id.clone(),
            request_timeout_secs: config.hindsight.request_timeout_secs,
            profile: config.hindsight.profile.clone(),
            port: config.hindsight.port,
            model: config.hindsight.model.clone(),
            effective_model: hindsight_effective_model,
        },
        telegram: SettingsTelegramSummary {
            enabled: config.telegram.enabled,
            credential: credential_summary(
                &config.telegram.bot_token,
                Some("your-telegram-bot-token"),
            ),
            has_real_credentials: config.telegram.has_real_credentials(),
            poll_timeout_secs: config.telegram.poll_timeout_secs,
        },
    }
}

fn settings_provider_summary(name: &str, provider: &ProviderConfig) -> SettingsProviderSummary {
    match provider {
        ProviderConfig::Openai { api_key, base_url } => SettingsProviderSummary {
            name: name.to_string(),
            provider_type: "openai",
            base_url: Some(
                base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            ),
            credential: credential_summary(api_key, Some("your-api-key")),
            auth_file: None,
        },
        ProviderConfig::GithubCopilot { github_token } => SettingsProviderSummary {
            name: name.to_string(),
            provider_type: "github-copilot",
            base_url: None,
            credential: credential_summary(github_token, None),
            auth_file: None,
        },
        ProviderConfig::OpenaiCodexOauth {
            auth_file,
            base_url,
        } => SettingsProviderSummary {
            name: name.to_string(),
            provider_type: "openai-codex-oauth",
            base_url: Some(
                base_url
                    .clone()
                    .unwrap_or_else(|| "https://chatgpt.com/backend-api/codex".to_string()),
            ),
            credential: SettingsCredentialSummary {
                status: SettingsCredentialStatus::OauthFile,
                source: auth_file.clone(),
            },
            auth_file: auth_file.clone(),
        },
        ProviderConfig::OpenaiCompatible { base_url, api_key } => SettingsProviderSummary {
            name: name.to_string(),
            provider_type: "openai-compatible",
            base_url: Some(base_url.clone()),
            credential: credential_summary(api_key, Some("your-api-key")),
            auth_file: None,
        },
    }
}

fn settings_model_summary(
    name: &str,
    model: &ModelConfig,
    main_model: &str,
    judge_model: &str,
    hindsight_model: &str,
) -> SettingsModelSummary {
    SettingsModelSummary {
        name: name.to_string(),
        provider: model.provider.clone(),
        model_id: model.model_id.clone(),
        is_main: name == main_model,
        is_judge: name == judge_model,
        is_hindsight: name == hindsight_model,
        temperature: model.temperature,
        thinking_budget: model.thinking_budget().map(thinking_budget_label),
        rpm: model.rpm,
        request_timeout_secs: model.request_timeout_secs(),
        stream_idle_timeout_secs: model.stream_idle_timeout_secs(),
        context_window_tokens: model.context_window_tokens(),
        effective_context_window_percent: model.effective_context_window_percent(),
        effective_context_window_tokens: model.effective_context_window_tokens(),
        auto_compact_token_limit: model.auto_compact_token_limit(),
        max_completion_tokens: model.max_completion_tokens(),
        tool_output_max_tokens: model.tool_output_max_tokens,
        supports_vision: resolve_supports_vision(model),
    }
}

/// Resolve vision support: explicit config wins, then catalog, then default to `true`.
fn resolve_supports_vision(model: &ModelConfig) -> bool {
    match model.supports_vision {
        Some(v) => v,
        None => catalog_model_capacity(&model.model_id)
            .map(|c| c.supports_vision)
            .unwrap_or(true),
    }
}

fn credential_summary(value: &str, placeholder: Option<&str>) -> SettingsCredentialSummary {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return SettingsCredentialSummary {
            status: SettingsCredentialStatus::Missing,
            source: None,
        };
    }

    if let Some(env_name) = env_reference_name(trimmed) {
        let is_configured = std::env::var(&env_name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
        return SettingsCredentialSummary {
            status: if is_configured {
                SettingsCredentialStatus::EnvConfigured
            } else {
                SettingsCredentialStatus::EnvMissing
            },
            source: Some(env_name),
        };
    }

    if placeholder.is_some_and(|placeholder| trimmed == placeholder) {
        return SettingsCredentialSummary {
            status: SettingsCredentialStatus::Placeholder,
            source: None,
        };
    }

    SettingsCredentialSummary {
        status: SettingsCredentialStatus::Configured,
        source: None,
    }
}

fn env_reference_name(value: &str) -> Option<String> {
    let name = if let Some(inner) = value
        .strip_prefix("${")
        .and_then(|inner| inner.strip_suffix('}'))
    {
        inner
    } else if let Some(inner) = value.strip_prefix("env:") {
        inner
    } else {
        value.strip_prefix('$')?
    };
    let name = name.trim();
    if is_valid_env_reference_name(name) {
        Some(name.to_string())
    } else {
        None
    }
}

fn is_valid_env_reference_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn thinking_budget_label(value: ThinkingBudget) -> &'static str {
    match value {
        ThinkingBudget::None => "none",
        ThinkingBudget::Minimal => "minimal",
        ThinkingBudget::Low => "low",
        ThinkingBudget::Medium => "medium",
        ThinkingBudget::High => "high",
        ThinkingBudget::Max => "max",
    }
}

fn strong_filesystem_mode_label(value: StrongFilesystemSandboxMode) -> &'static str {
    match value {
        StrongFilesystemSandboxMode::Off => "off",
        StrongFilesystemSandboxMode::Auto => "auto",
        StrongFilesystemSandboxMode::Required => "required",
    }
}

async fn dashboard_ws(mut socket: WebSocket, state: ServerState) {
    state
        .connected_clients
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = state.dashboard_rx.clone();
    let shutdown_notify = state.shutdown_notify.clone();
    let initial_snapshot = rx.borrow_and_update().clone();
    if send_dashboard_ws_snapshot(&mut socket, &initial_snapshot)
        .await
        .is_ok()
    {
        loop {
            tokio::select! {
                result = rx.changed() => {
                    match result {
                        Ok(()) => {
                            let snapshot = rx.borrow().clone();
                            if send_dashboard_ws_snapshot(&mut socket, &snapshot)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                _ = shutdown_notify.notified() => {
                    break;
                }
            }
        }
    }
    state
        .connected_clients
        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
}

async fn send_dashboard_ws_snapshot(
    socket: &mut WebSocket,
    snapshot: &DashboardState,
) -> std::result::Result<(), ()> {
    let payload = serde_json::to_string(snapshot).map_err(|err| {
        tracing::error!("encode dashboard ws snapshot failed: {err}");
    })?;

    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|_| ())
}

#[derive(Clone)]
pub struct DaemonClient {
    port: u16,
    http: reqwest::Client,
    auth_token: Option<DaemonAuthToken>,
}

impl DaemonClient {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("build daemon http client"),
            auth_token: None,
        }
    }

    pub async fn authenticated(port: u16) -> Result<Self> {
        Ok(Self {
            port,
            http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .map_err(|err| miette!("build daemon http client failed: {err}"))?,
            auth_token: Some(load_daemon_auth_token().await?),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    fn base_url(&self) -> String {
        format!("http://{}:{}", DAEMON_CLIENT_HOST, self.port)
    }

    fn ws_url(&self) -> String {
        format!("ws://{}:{}/dashboard/stream", DAEMON_CLIENT_HOST, self.port)
    }

    fn with_auth(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let token = self
            .auth_token
            .as_ref()
            .ok_or_else(|| miette!("daemon auth token is not loaded"))?;
        Ok(request.bearer_auth(token.as_str()))
    }

    fn websocket_request(&self) -> Result<axum::http::Request<()>> {
        let token = self
            .auth_token
            .as_ref()
            .ok_or_else(|| miette!("daemon auth token is not loaded"))?;
        let mut request = self
            .ws_url()
            .into_client_request()
            .map_err(|err| miette!("build dashboard ws request failed: {err}"))?;
        let auth_value = HeaderValue::from_str(&token.bearer_value())
            .map_err(|err| miette!("build daemon auth header failed: {err}"))?;
        request.headers_mut().insert(AUTHORIZATION, auth_value);
        Ok(request)
    }

    pub async fn status(&self) -> Result<StatusResponse> {
        self.http
            .get(format!("{}/status", self.base_url()))
            .send()
            .await
            .map_err(|err| miette!("daemon status request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("daemon status returned error: {err}"))?
            .json::<StatusResponse>()
            .await
            .map_err(|err| miette!("decode daemon status failed: {err}"))
    }

    pub async fn snapshot(&self) -> Result<DashboardState> {
        self.with_auth(
            self.http
                .get(format!("{}/dashboard/snapshot", self.base_url())),
        )?
        .send()
        .await
        .map_err(|err| miette!("dashboard snapshot request failed: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("dashboard snapshot returned error: {err}"))?
        .json::<DashboardState>()
        .await
        .map_err(|err| miette!("decode dashboard snapshot failed: {err}"))
    }

    pub async fn send_command(&self, command: &str) -> Result<String> {
        let response = self
            .with_auth(
                self.http
                    .post(format!("{}/commands/run", self.base_url()))
                    .json(&CommandRequest {
                        command: command.to_string(),
                        attachments: Vec::new(),
                    }),
            )?
            .send()
            .await
            .map_err(|err| miette!("run command request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("run command returned error: {err}"))?
            .json::<CommandResponse>()
            .await
            .map_err(|err| miette!("decode run command response failed: {err}"))?;
        Ok(response.output)
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.with_auth(
            self.http
                .post(format!("{}/daemon/shutdown", self.base_url())),
        )?
        .send()
        .await
        .map_err(|err| miette!("daemon shutdown request failed: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("daemon shutdown returned error: {err}"))?;
        Ok(())
    }

    pub async fn restart(&self) -> Result<()> {
        self.with_auth(
            self.http
                .post(format!("{}/daemon/restart", self.base_url())),
        )?
        .send()
        .await
        .map_err(|err| miette!("daemon restart request failed: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("daemon restart returned error: {err}"))?;
        Ok(())
    }

    pub async fn stream_to(
        &self,
        tx: watch::Sender<DashboardState>,
        mut stop_rx: oneshot::Receiver<()>,
    ) -> Result<()> {
        let request = self.websocket_request()?;

        let (ws, _) = tokio::select! {
            result = tokio_tungstenite::connect_async(request) => {
                result.map_err(|err| miette!("connect dashboard ws failed: {err}"))?
            }
            _ = &mut stop_rx => {
                return Ok(());
            }
        };
        let (_, mut read) = ws.split();
        loop {
            tokio::select! {
                message = read.next() => {
                    let Some(message) = message else {
                        break;
                    };
                    let message = message.map_err(|err| miette!("dashboard ws read failed: {err}"))?;
                    match message {
                        tokio_tungstenite::tungstenite::Message::Text(payload) => {
                            let snapshot = serde_json::from_str::<DashboardState>(&payload)
                                .map_err(|err| miette!("decode dashboard ws snapshot failed: {err}"))?;
                            let _ = tx.send(snapshot);
                        }
                        tokio_tungstenite::tungstenite::Message::Close(_) => break,
                        _ => {}
                    }
                }
                _ = &mut stop_rx => break,
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl DashboardCommandRunner for DaemonClient {
    async fn run_command(&self, command: &str, _state: &DashboardState) -> String {
        match self.send_command(command).await {
            Ok(output) => output,
            Err(err) => format!("command failed: {err}"),
        }
    }
}

async fn configured_daemon_port() -> Result<u16> {
    Ok(load_config()
        .await
        .map_err(|err| miette!("failed to load daemon config: {err}"))?
        .daemon
        .port)
}

pub async fn wait_for_daemon_ready() -> Result<StatusResponse> {
    let port = configured_daemon_port().await?;
    let client = DaemonClient::new(port);
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        match client.status().await {
            Ok(status) if status.state == DaemonLifecycleState::Ready => return Ok(status),
            Ok(status) if status.state == DaemonLifecycleState::Failed => {
                return Err(miette!(
                    "daemon startup failed{}",
                    daemon_startup_log_tail_suffix().await
                ));
            }
            Ok(status) => {
                last_error = Some(format!("daemon is {}", status.state));
            }
            Err(err) => last_error = Some(err.to_string()),
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
    Err(miette!(
        "daemon did not become ready within {}s{}{}",
        READY_TIMEOUT.as_secs(),
        last_error
            .as_deref()
            .map(|err| format!(": {err}"))
            .unwrap_or_default(),
        daemon_startup_log_tail_suffix().await
    ))
}

pub async fn wait_for_daemon_shutdown(port: u16) -> Result<()> {
    let client = DaemonClient::new(port);
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    while Instant::now() < deadline {
        if client.status().await.is_err() {
            return Ok(());
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
    Err(miette!(
        "daemon on {}:{} did not stop accepting connections within {}s",
        DAEMON_HOST_DISPLAY,
        port,
        SHUTDOWN_TIMEOUT.as_secs()
    ))
}

pub async fn spawn_detached_daemon_process() -> Result<()> {
    let current_exe = std::env::current_exe()
        .map_err(|err| miette!("resolve current executable failed: {err}"))?;

    // Redirect stderr to the log file to simplify daemon startup failure diagnosis.
    // stdout is still discarded because emit_startup_progress println! output is
    // already recorded through tracing.
    let log_path = daemon_stderr_log_path().await;
    let stderr_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|err| miette!("open daemon stderr log {}: {err}", log_path.display()))?;

    let mut command = std::process::Command::new(current_exe);
    command
        .arg("daemon")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_file)
        .env(DAEMONIZE_ENV, "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .map_err(|err| miette!("spawn daemon process failed: {err}"))?;
    Ok(())
}

async fn daemon_stderr_log_path() -> PathBuf {
    daat_locus_paths().await.logs_file(DAEMON_STDERR_LOG)
}

async fn daemon_main_log_path() -> PathBuf {
    daat_locus_paths().await.logs_file(DAEMON_MAIN_LOG)
}

async fn daemon_startup_log_tail_suffix() -> String {
    let mut sections = Vec::new();
    if let Some(section) = log_tail_section(daemon_main_log_path().await, "recent daemon log").await
    {
        sections.push(section);
    }
    if let Some(section) =
        log_tail_section(daemon_stderr_log_path().await, "recent daemon stderr").await
    {
        sections.push(section);
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

async fn log_tail_section(path: PathBuf, title: &str) -> Option<String> {
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return None;
    };
    let tail = text
        .lines()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.trim().is_empty() {
        None
    } else {
        Some(format!("{title} ({}):\n{tail}", path.display()))
    }
}

pub fn daemonize_current_process_if_requested() -> Result<()> {
    if std::env::var_os(DAEMONIZE_ENV).is_none() {
        return Ok(());
    }
    // This marker is only for the top-level daemon child. If it survives in the
    // daemon environment, later helper processes such as workspace app workers
    // will daemonize themselves before running their actual subcommand.
    unsafe {
        std::env::remove_var(DAEMONIZE_ENV);
    }
    daemonize_current_process()
}

#[cfg(unix)]
fn daemonize_current_process() -> Result<()> {
    // Run before Tokio starts: daemonizing forks, so doing it after a
    // multi-thread runtime exists would duplicate only the calling thread.
    unsafe {
        fork_parent_exit("first daemon fork")?;
        if libc::setsid() == -1 {
            return Err(miette!(
                "daemonize setsid failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        fork_parent_exit("second daemon fork")?;
    }
    Ok(())
}

#[cfg(unix)]
unsafe fn fork_parent_exit(label: &str) -> Result<()> {
    match unsafe { libc::fork() } {
        -1 => Err(miette!(
            "{label} failed: {}",
            std::io::Error::last_os_error()
        )),
        0 => Ok(()),
        _ => unsafe { libc::_exit(0) },
    }
}

#[cfg(not(unix))]
fn daemonize_current_process() -> Result<()> {
    Ok(())
}

pub async fn connect_daemon_status() -> Result<DaemonClient> {
    let port = configured_daemon_port().await?;
    let client = DaemonClient::new(port);
    client.status().await?;
    Ok(client)
}

pub async fn connect_existing_daemon() -> Result<DaemonClient> {
    let status_client = connect_daemon_status().await?;
    DaemonClient::authenticated(status_client.port()).await
}

pub async fn connect_or_start_daemon() -> Result<DaemonClient> {
    match connect_existing_daemon().await {
        Ok(client) => Ok(client),
        Err(_) => {
            spawn_detached_daemon_process().await?;
            let status = wait_for_daemon_ready().await?;
            let client = DaemonClient::authenticated(status.port).await?;
            Ok(client)
        }
    }
}

pub fn status_summary(status: &StatusResponse) -> String {
    format!(
        "daemon pid={} {}:{} state={} started_at_ms={} version={} connected_clients={}",
        status.pid,
        status.bind_host,
        status.port,
        status.state,
        status.started_at_ms,
        status.version,
        status.connected_clients,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_lifecycle_state_serializes_as_snake_case() {
        let encoded = serde_json::to_string(&DaemonLifecycleState::Initializing).unwrap();
        assert_eq!(encoded, "\"initializing\"");
        let decoded: DaemonLifecycleState = serde_json::from_str("\"ready\"").unwrap();
        assert_eq!(decoded, DaemonLifecycleState::Ready);
    }

    #[test]
    fn daemon_lifecycle_handle_transitions_to_failed_only_during_initializing() {
        let lifecycle = DaemonLifecycleHandle::new(DaemonLifecycleState::Initializing);
        lifecycle.mark_failed_if_initializing();
        assert_eq!(lifecycle.get(), DaemonLifecycleState::Failed);

        lifecycle.set(DaemonLifecycleState::Ready);
        lifecycle.mark_failed_if_initializing();
        assert_eq!(lifecycle.get(), DaemonLifecycleState::Ready);
    }
}
