use std::{
    collections::HashMap,
    io::Write,
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
    routing::{delete, get, post},
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
use sysinfo::{Pid, Signal, System};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::{
    config::{Config, ModelConfig, ProviderConfig, load_config},
    config_setup::{
        self, ConfigReadinessReport, PendingSetupProviderAuthFlow, SetupConfigRequest,
        SetupDiscoverModelsRequest, SetupProviderAuthCompleteRequest, SetupProviderAuthRunRequest,
        SetupProviderAuthStartRequest,
    },
    daat_locus_paths::{daat_locus_paths, daat_locus_paths_sync},
    dashboard::{
        DashboardAction, DashboardActionResult, DashboardActivityHistoryPage,
        DashboardCommandAttachment, DashboardCommandRunner, DashboardControlCommand,
        DashboardHistoryLoader, DashboardIncomingAttachment, DashboardInputHistory,
        DashboardSessionTitle, DashboardState, dashboard_action_is_manager_owned,
        dashboard_command_is_manager_owned, execute_control_command, execute_dashboard_action,
    },
    model_catalog::catalog_model_capacity,
    sandbox::StrongFilesystemSandboxMode,
    telegram_acl::{PendingAccessRequest, TelegramAclHandle},
};

mod auth;
mod logs;
pub mod session;
pub mod session_ipc;

pub(crate) type SessionTokenStore = Arc<parking_lot::RwLock<HashMap<session::SessionId, String>>>;

pub use auth::{
    CreatedDaemonToken, DaemonAuthToken, DaemonTokenListEntry, DaemonTokenRegistryHandle,
    create_daemon_token, list_daemon_tokens, load_daemon_auth_token,
    load_or_create_daemon_token_registry, revoke_daemon_token, rotate_daemon_token,
};

pub const DAEMON_BIND_HOST: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
pub const DAEMON_CLIENT_HOST: &str = "localhost";
pub const DAEMON_HOST_DISPLAY: &str = "0.0.0.0";
/// Manager startup should be quick because runtime initialization belongs to
/// session workers. Keep this short so startup failures do not look like hangs.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);
const DAEMON_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const DAEMON_PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const SESSION_IPC_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const SESSION_PROCESS_TERM_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_PROCESS_KILL_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_DIRECTORY_REMOVE_ATTEMPTS: usize = 5;
const SESSION_DIRECTORY_REMOVE_RETRY_DELAY: Duration = Duration::from_millis(100);
const DAEMON_MAIN_LOG: &str = crate::logging::DAEMON_LOG_FILE_NAME;
const SESSION_LOG: &str = crate::logging::SESSION_LOG_FILE_NAME;
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

#[derive(Serialize)]
pub struct StatusSummaryResponse {
    pub loaded_at_ms: i64,
    pub daemon: StatusResponse,
    pub pending_access_requests: Vec<PendingAccessRequest>,
    pub sessions: Vec<StatusSessionSummary>,
}

#[derive(Serialize)]
pub struct StatusSessionSummary {
    pub session: session::SessionSummary,
    pub runtime_status: Option<StatusSessionRuntimeSummary>,
    pub dashboard: Option<session_ipc::SessionStatusDashboard>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct StatusSessionRuntimeSummary {
    pub ready: bool,
    pub pending_work_count: usize,
    pub active_runtime_turn: bool,
}

impl From<session_ipc::SessionRuntimeStatus> for StatusSessionRuntimeSummary {
    fn from(status: session_ipc::SessionRuntimeStatus) -> Self {
        Self {
            ready: status.ready,
            pending_work_count: status.pending_work_count,
            active_runtime_turn: status.active_runtime_turn,
        }
    }
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
        // Only transition from Initializing -> Ready.
        // If already Stopping (e.g. restart requested during init), don't overwrite.
        let _ = self.state.compare_exchange(
            DaemonLifecycleState::Initializing.as_u8(),
            DaemonLifecycleState::Ready.as_u8(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn mark_stopping(&self) {
        self.set(DaemonLifecycleState::Stopping);
    }

    #[cfg_attr(not(test), allow(dead_code))]
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
    #[serde(default = "default_command_origin")]
    pub origin: session_ipc::UserInputOrigin,
    #[serde(default)]
    pub session_id: Option<String>,
}

fn default_command_origin() -> session_ipc::UserInputOrigin {
    session_ipc::UserInputOrigin::WebUi
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DashboardActionRequest {
    pub action: DashboardAction,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DashboardActionResponse {
    pub result: DashboardActionResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendRequest {
    pub message: String,
    #[serde(default)]
    pub session_id: Option<String>,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct SendResponse {
    pub event_id: String,
    pub status: String,
    pub reply_message: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigReadinessResponse {
    pub readiness: ConfigReadinessReport,
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
    pub providers: Vec<SettingsProviderSummary>,
    pub models: Vec<SettingsModelSummary>,
    pub daemon: SettingsDaemonSummary,
    pub judge: SettingsJudgeSummary,
    pub sandbox: SettingsSandboxSummary,
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
    pub temperature: f64,
    pub thinking_budget: Option<String>,
    pub rpm: Option<u32>,
    pub request_timeout_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub context_window_tokens: usize,
    pub effective_context_window_percent: i64,
    pub effective_context_window_tokens: usize,
    pub auto_compact_token_limit: usize,
    pub reserved_output_tokens: usize,
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
pub struct SettingsTelegramSummary {
    pub enabled: bool,
    pub credential: SettingsCredentialSummary,
    pub has_real_credentials: bool,
    pub poll_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
struct DashboardStreamQuery {
    token: Option<String>,
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DashboardActivityHistoryQuery {
    before: Option<i64>,
    after: Option<i64>,
    limit: Option<usize>,
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DashboardInputHistoryQuery {
    limit: Option<usize>,
    session_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct DashboardActivityHistoryCountResponse {
    matching_items: usize,
    total_items: usize,
}

#[derive(Debug, Deserialize)]
struct DashboardSnapshotQuery {
    session_id: Option<String>,
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
    telegram_acl: TelegramAclHandle,
    dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    connected_clients: Arc<std::sync::atomic::AtomicUsize>,
    sessions: session::SessionRegistry,
    session_tokens: SessionTokenStore,
    setup_auth_flows: Arc<parking_lot::Mutex<HashMap<String, PendingSetupProviderAuthFlow>>>,
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
    pub telegram_acl: TelegramAclHandle,
    pub dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    pub daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    pub sessions: session::SessionRegistry,
    pub session_tokens: SessionTokenStore,
    pub shutdown_rx: oneshot::Receiver<()>,
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
        Self::acquire_at_path(path).await
    }

    pub async fn acquire_with_suffix(suffix: &str) -> Result<Self> {
        let paths = daat_locus_paths().await;
        let mut path = paths.daemon_lock_file();
        if let Some(parent) = path.parent() {
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let ext = path.extension().unwrap_or_default().to_string_lossy();
            path = if ext.is_empty() {
                parent.join(format!("{stem}-{suffix}"))
            } else {
                parent.join(format!("{stem}-{suffix}.{ext}"))
            };
        }
        Self::acquire_at_path(path).await
    }

    async fn acquire_at_path(path: PathBuf) -> Result<Self> {
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
        telegram_acl,
        dashboard_control_tx,
        daemon_control_tx,
        sessions,
        session_tokens,
        shutdown_rx,
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
        telegram_acl,
        dashboard_control_tx,
        daemon_control_tx,
        connected_clients: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        sessions,
        session_tokens,
        setup_auth_flows: Arc::new(parking_lot::Mutex::new(HashMap::new())),
    };

    let router = Router::new()
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/status/summary", get(status_summary_handler))
        .route("/dashboard/snapshot", get(snapshot_handler))
        .route("/dashboard/stream", get(stream_handler))
        .route("/dashboard/activity-history", get(activity_history_handler))
        .route("/dashboard/input-history", get(input_history_handler))
        .route(
            "/dashboard/activity-history/count",
            get(activity_history_count_handler),
        )
        .route("/dashboard/action", post(dashboard_action_handler))
        .route(
            "/dashboard/attachments/{encoded_path}",
            get(dashboard_attachment_handler),
        )
        .route("/settings/summary", get(settings_summary_handler))
        .route("/config/readiness", get(config_readiness_handler))
        .route("/config/setup", post(config_setup_handler))
        .route("/config/probe", post(config_probe_handler))
        .route(
            "/config/discover-models",
            post(config_discover_models_handler),
        )
        .route(
            "/config/provider-auth/run",
            post(config_provider_auth_run_handler),
        )
        .route(
            "/config/provider-auth/device/start",
            post(config_provider_auth_device_start_handler),
        )
        .route(
            "/config/provider-auth/device/complete",
            post(config_provider_auth_device_complete_handler),
        )
        .route("/logs/sources", get(logs::sources_handler))
        .route("/logs/read", get(logs::read_handler))
        .route("/commands/run", post(command_handler))
        .route("/send", post(send_handler))
        .route("/daemon/shutdown", post(shutdown_handler))
        .route("/daemon/restart", post(restart_handler))
        .route("/sessions", get(session_list_handler))
        .route("/sessions", post(session_create_handler))
        .route("/sessions/{session_id}", delete(session_delete_handler))
        .route("/sessions/{session_id}/title", post(session_title_handler))
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
        "commands"
            | "daemon"
            | "dashboard"
            | "config"
            | "health"
            | "logs"
            | "sessions"
            | "settings"
            | "status"
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
    Json(status_response(&state)).into_response()
}

async fn config_readiness_handler() -> impl IntoResponse {
    Json(ConfigReadinessResponse {
        readiness: config_setup::ensure_config_readiness().await,
    })
    .into_response()
}

async fn config_setup_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SetupConfigRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match config_setup::write_setup_config(request).await {
        Ok(readiness) => Json(ConfigReadinessResponse { readiness }).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

async fn config_probe_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SetupConfigRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match config_setup::preview_setup_config(request).await {
        Ok(readiness) => Json(ConfigReadinessResponse { readiness }).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

async fn config_discover_models_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SetupDiscoverModelsRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match config_setup::discover_setup_models(request).await {
        Ok(response) => Json(response).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

async fn config_provider_auth_run_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SetupProviderAuthRunRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match config_setup::run_setup_provider_auth(request).await {
        Ok(response) => Json(response).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

async fn config_provider_auth_device_start_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SetupProviderAuthStartRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match config_setup::start_setup_provider_auth(request).await {
        Ok((response, flow)) => {
            state
                .setup_auth_flows
                .lock()
                .insert(response.flow_id.clone(), flow);
            Json(response).into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

async fn config_provider_auth_device_complete_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SetupProviderAuthCompleteRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let flow = match state.setup_auth_flows.lock().get(&request.flow_id).cloned() {
        Some(flow) if !flow.is_expired() => flow,
        Some(_) => {
            state.setup_auth_flows.lock().remove(&request.flow_id);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "setup authentication flow expired" })),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "unknown setup authentication flow" })),
            )
                .into_response();
        }
    };

    match config_setup::complete_setup_provider_auth(request.clone(), flow).await {
        Ok(response) => {
            state.setup_auth_flows.lock().remove(&request.flow_id);
            Json(response).into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

fn status_response(state: &ServerState) -> StatusResponse {
    StatusResponse {
        pid: std::process::id(),
        started_at_ms: state.started_at_ms,
        version: env!("CARGO_PKG_VERSION").to_string(),
        bind_host: state.bind_host.clone(),
        port: state.port,
        state: state.lifecycle.get(),
        connected_clients: state
            .connected_clients
            .load(std::sync::atomic::Ordering::Relaxed),
    }
}

fn overlay_manager_owned_dashboard_state(
    telegram_acl: &TelegramAclHandle,
    snapshot: &mut DashboardState,
) {
    snapshot.pending_access_requests = telegram_acl.pending_requests();
}

async fn status_summary_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let session_tasks = state.sessions.list().into_iter().map(|info| {
        status_session_summary(state.sessions.clone(), state.session_tokens.clone(), info)
    });
    let sessions = futures_util::future::join_all(session_tasks).await;

    Json(StatusSummaryResponse {
        loaded_at_ms: chrono::Utc::now().timestamp_millis(),
        daemon: status_response(&state),
        pending_access_requests: state.telegram_acl.pending_requests(),
        sessions,
    })
    .into_response()
}

async fn status_session_summary(
    sessions: session::SessionRegistry,
    session_tokens: SessionTokenStore,
    mut info: session::SessionInfo,
) -> StatusSessionSummary {
    let mut runtime_status = None;
    let mut dashboard = None;
    let mut error = None;

    if info.status.is_process_backed() {
        match live_session_client(&session_tokens, &info.session_id, &info) {
            Ok(client) => {
                let client = client.with_timeout(Duration::from_secs(2));
                match client
                    .request(session_ipc::SessionIpcRequest::StatusSummary)
                    .await
                {
                    Ok(session_ipc::SessionIpcResponse::StatusSummary { summary }) => {
                        let summary = *summary;
                        let session_title = summary
                            .session_title
                            .clone()
                            .or_else(|| summary.dashboard.session_title.clone());
                        apply_session_title_update(&sessions, &mut info, session_title.as_ref())
                            .await;
                        runtime_status = Some(summary.runtime_status.into());
                        dashboard = Some(summary.dashboard);
                    }
                    Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => {
                        error = Some(message);
                    }
                    Ok(_) => {
                        error = Some("unexpected session IPC status summary response".to_string());
                    }
                    Err(err) => {
                        error = Some(format!("{err:?}"));
                        session_tokens.write().remove(&info.session_id);
                        let _ = sessions.mark_dead(&info.session_id).await;
                        info.pid = None;
                        info.status = session::SessionStatus::Dead;
                        info.ipc_name = None;
                        info.ipc_token_hash = None;
                        info.last_seen_at_ms = Some(chrono::Utc::now().timestamp_millis());
                    }
                }
            }
            Err(err) => {
                error = Some(format!("{err:?}"));
                session_tokens.write().remove(&info.session_id);
                let _ = sessions.mark_dead(&info.session_id).await;
                info.pid = None;
                info.status = session::SessionStatus::Dead;
                info.ipc_name = None;
                info.ipc_token_hash = None;
                info.last_seen_at_ms = Some(chrono::Utc::now().timestamp_millis());
            }
        }
    }

    StatusSessionSummary {
        session: session::SessionSummary::from(info),
        runtime_status,
        dashboard,
        error,
    }
}

async fn apply_session_title_update(
    sessions: &session::SessionRegistry,
    info: &mut session::SessionInfo,
    title: Option<&DashboardSessionTitle>,
) {
    let Some(title) = title
        .map(|title| title.title.trim())
        .filter(|title| !title.is_empty())
    else {
        return;
    };
    if info.title.as_deref().map(str::trim) == Some(title) {
        return;
    }
    match sessions
        .set_title(&info.session_id, title.to_string())
        .await
    {
        Ok(true) => {
            info.title = Some(title.to_string());
        }
        Ok(false) => {
            tracing::warn!(
                "session title update ignored for unknown session {}",
                info.session_id
            );
        }
        Err(err) => {
            tracing::warn!(
                "failed to persist title for session {}: {err:?}",
                info.session_id
            );
        }
    }
}

async fn snapshot_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<DashboardSnapshotQuery>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(session_id) = query.session_id.as_deref() {
        match session_client_for_request(&state, session_id).await {
            Ok(client) => match client
                .request(session_ipc::SessionIpcRequest::DashboardSnapshot)
                .await
            {
                Ok(session_ipc::SessionIpcResponse::DashboardSnapshot { state: snapshot }) => {
                    let mut snapshot = *snapshot;
                    overlay_manager_owned_dashboard_state(&state.telegram_acl, &mut snapshot);
                    sync_session_title_from_dashboard_state(&state.sessions, session_id, &snapshot)
                        .await;
                    Json(snapshot).into_response()
                }
                Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => {
                    (StatusCode::BAD_GATEWAY, message).into_response()
                }
                Ok(_) => (
                    StatusCode::BAD_GATEWAY,
                    "unexpected session IPC dashboard snapshot response",
                )
                    .into_response(),
                Err(err) => (StatusCode::BAD_GATEWAY, format!("{err:?}")).into_response(),
            },
            Err(err) => (StatusCode::NOT_FOUND, format!("{err:?}")).into_response(),
        }
    } else {
        (
            StatusCode::BAD_REQUEST,
            "session_id is required for dashboard snapshot",
        )
            .into_response()
    }
}

async fn activity_history_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<DashboardActivityHistoryQuery>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if let Some(session_id) = query.session_id.as_deref() {
        match session_client_for_request(&state, session_id).await {
            Ok(client) => match client
                .request(session_ipc::SessionIpcRequest::DashboardHistoryPage {
                    before: query.before,
                    after: query.after,
                    limit: query.limit.unwrap_or(80),
                })
                .await
            {
                Ok(session_ipc::SessionIpcResponse::DashboardHistoryPage { page }) => {
                    Json(page).into_response()
                }
                Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => {
                    (StatusCode::BAD_GATEWAY, message).into_response()
                }
                Ok(_) => (
                    StatusCode::BAD_GATEWAY,
                    "unexpected session IPC dashboard history response",
                )
                    .into_response(),
                Err(err) => (StatusCode::BAD_GATEWAY, format!("{err:?}")).into_response(),
            },
            Err(err) => (StatusCode::NOT_FOUND, format!("{err:?}")).into_response(),
        }
    } else {
        (
            StatusCode::BAD_REQUEST,
            "session_id is required for dashboard activity history",
        )
            .into_response()
    }
}

async fn input_history_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<DashboardInputHistoryQuery>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Some(session_id) = query.session_id.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            "session_id is required for dashboard input history",
        )
            .into_response();
    };

    match session_client_for_request(&state, session_id).await {
        Ok(client) => match client
            .request(session_ipc::SessionIpcRequest::DashboardInputHistory {
                limit: query.limit.unwrap_or(100),
            })
            .await
        {
            Ok(session_ipc::SessionIpcResponse::DashboardInputHistory { history }) => {
                Json(history).into_response()
            }
            Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => {
                (StatusCode::BAD_GATEWAY, message).into_response()
            }
            Ok(_) => (
                StatusCode::BAD_GATEWAY,
                "unexpected session IPC dashboard input history response",
            )
                .into_response(),
            Err(err) => (StatusCode::BAD_GATEWAY, format!("{err:?}")).into_response(),
        },
        Err(err) => (StatusCode::NOT_FOUND, format!("{err:?}")).into_response(),
    }
}

async fn activity_history_count_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<DashboardSnapshotQuery>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Some(session_id) = query.session_id.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            "session_id is required for dashboard activity history count",
        )
            .into_response();
    };

    match session_client_for_request(&state, session_id).await {
        Ok(client) => match client
            .request(session_ipc::SessionIpcRequest::DashboardHistoryCount)
            .await
        {
            Ok(session_ipc::SessionIpcResponse::DashboardHistoryCount { count }) => {
                Json(DashboardActivityHistoryCountResponse {
                    matching_items: count.matching_items,
                    total_items: count.total_items,
                })
                .into_response()
            }
            Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => {
                (StatusCode::BAD_GATEWAY, message).into_response()
            }
            Ok(_) => (
                StatusCode::BAD_GATEWAY,
                "unexpected session IPC dashboard history count response",
            )
                .into_response(),
            Err(err) => (StatusCode::BAD_GATEWAY, format!("{err:?}")).into_response(),
        },
        Err(err) => (StatusCode::NOT_FOUND, format!("{err:?}")).into_response(),
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
            StatusCode::SERVICE_UNAVAILABLE,
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
    if let Some(session_id) = query.session_id.as_deref() {
        let sessions = state.sessions.clone();
        let telegram_acl = state.telegram_acl.clone();
        let session_id = session_id.to_string();
        return match session_client_for_request(&state, &session_id).await {
            Ok(client) => ws.on_upgrade(move |socket| {
                session_dashboard_ws(socket, client, sessions, telegram_acl, session_id)
            }),
            Err(err) => (StatusCode::NOT_FOUND, format!("{err:?}")).into_response(),
        };
    }
    (
        StatusCode::BAD_REQUEST,
        "session_id is required for dashboard stream",
    )
        .into_response()
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
    if let Some(response) = config_not_ready_response().await {
        return response;
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
    if let Some(session_id) = request.session_id.as_deref() {
        let trimmed = request.command.trim();
        if let Some(command) = trimmed.strip_prefix('/')
            && dashboard_command_is_manager_owned(command)
        {
            if !attachments.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(CommandResponse {
                        output: "dashboard commands cannot include attachments".to_string(),
                    }),
                )
                    .into_response();
            }
            let snapshot = state.dashboard_rx.borrow().clone();
            let output =
                execute_control_command(command.trim(), &snapshot, &state.dashboard_control_tx);
            return Json(CommandResponse { output }).into_response();
        }
        let client = match session_client_for_request(&state, session_id).await {
            Ok(client) => client,
            Err(err) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(CommandResponse {
                        output: format!("{err:?}"),
                    }),
                )
                    .into_response();
            }
        };
        if let Some(command) = trimmed.strip_prefix('/') {
            if !attachments.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(CommandResponse {
                        output: "dashboard commands cannot include attachments".to_string(),
                    }),
                )
                    .into_response();
            }
            let response = client
                .request(session_ipc::SessionIpcRequest::DashboardCommand {
                    command: command.trim().to_string(),
                })
                .await;
            let output = match response {
                Ok(session_ipc::SessionIpcResponse::DashboardCommandResult { output }) => output,
                Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => message,
                Ok(_) => "unexpected session IPC dashboard command response".to_string(),
                Err(err) => format!("session dashboard command failed: {err:?}"),
            };
            return Json(CommandResponse { output }).into_response();
        }
        let response = client
            .request(session_ipc::SessionIpcRequest::SubmitUserInput {
                origin: request.origin,
                text: request.command,
                attachments: attachments
                    .into_iter()
                    .map(|attachment| session_ipc::InputAttachment {
                        media_type: attachment.media_type,
                        local_path: attachment.local_path,
                        description: attachment.description,
                    })
                    .collect(),
                wait_for_reply: false,
            })
            .await;
        let output = match response {
            Ok(session_ipc::SessionIpcResponse::Submitted { event_id, .. }) => {
                format!("queued session message as event {event_id}")
            }
            Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => message,
            Ok(_) => "unexpected session IPC command response".to_string(),
            Err(err) => format!("session command failed: {err:?}"),
        };
        return Json(CommandResponse { output }).into_response();
    }
    if !attachments.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(CommandResponse {
                output: "session_id is required for user input with attachments".to_string(),
            }),
        )
            .into_response();
    }
    let trimmed = request.command.trim();
    let Some(command) = trimmed.strip_prefix('/') else {
        return (
            StatusCode::BAD_REQUEST,
            Json(CommandResponse {
                output: "session_id is required for user input".to_string(),
            }),
        )
            .into_response();
    };
    let snapshot = state.dashboard_rx.borrow().clone();
    let output = execute_control_command(command.trim(), &snapshot, &state.dashboard_control_tx);
    Json(CommandResponse { output }).into_response()
}

async fn dashboard_action_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<DashboardActionRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.lifecycle.get().allows_runtime_commands() {
        return runtime_not_ready_response(state.lifecycle.get());
    }
    if let Some(response) = config_not_ready_dashboard_action_response().await {
        return response;
    }
    if dashboard_action_is_manager_owned(&request.action) {
        let result = execute_dashboard_action(request.action, &state.dashboard_control_tx);
        return Json(DashboardActionResponse { result }).into_response();
    }
    if let Some(session_id) = request.session_id.as_deref() {
        let client = match session_client_for_request(&state, session_id).await {
            Ok(client) => client,
            Err(err) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(DashboardActionResponse {
                        result: DashboardActionResult {
                            success: false,
                            message: format!("{err:?}"),
                            detail: None,
                        },
                    }),
                )
                    .into_response();
            }
        };
        let response = client
            .request(session_ipc::SessionIpcRequest::DashboardAction {
                action: request.action,
            })
            .await;
        let result = match response {
            Ok(session_ipc::SessionIpcResponse::DashboardActionResult { result }) => result,
            Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => DashboardActionResult {
                success: false,
                message,
                detail: None,
            },
            Ok(_) => DashboardActionResult {
                success: false,
                message: "unexpected session IPC dashboard action response".to_string(),
                detail: None,
            },
            Err(err) => DashboardActionResult {
                success: false,
                message: format!("session dashboard action failed: {err:?}"),
                detail: None,
            },
        };
        return Json(DashboardActionResponse { result }).into_response();
    }

    let result = execute_dashboard_action(request.action, &state.dashboard_control_tx);
    Json(DashboardActionResponse { result }).into_response()
}

async fn send_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SendRequest>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.lifecycle.get().allows_runtime_commands() {
        return runtime_not_ready_response(state.lifecycle.get());
    }
    if let Some(response) = config_not_ready_send_response().await {
        return response;
    }
    let message = request.message.trim();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(SendResponse {
                event_id: String::new(),
                status: "failed".to_string(),
                reply_message: None,
                note: Some("empty input".to_string()),
            }),
        )
            .into_response();
    }

    if let Some(session_id) = request.session_id.as_deref() {
        let client = match session_client_for_request(&state, session_id).await {
            Ok(client) => client,
            Err(err) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(SendResponse {
                        event_id: String::new(),
                        status: "failed".to_string(),
                        reply_message: None,
                        note: Some(format!("{err:?}")),
                    }),
                )
                    .into_response();
            }
        };
        return match client
            .request_without_response_timeout(session_ipc::SessionIpcRequest::SubmitUserInput {
                origin: session_ipc::UserInputOrigin::CliSend,
                text: message.to_string(),
                attachments: Vec::new(),
                wait_for_reply: true,
            })
            .await
        {
            Ok(session_ipc::SessionIpcResponse::Submitted {
                event_id,
                reply_message,
                terminal_status,
            }) => Json(SendResponse {
                event_id,
                status: "resolved".to_string(),
                reply_message,
                note: terminal_status,
            })
            .into_response(),
            Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => (
                StatusCode::BAD_GATEWAY,
                Json(SendResponse {
                    event_id: String::new(),
                    status: "failed".to_string(),
                    reply_message: None,
                    note: Some(message),
                }),
            )
                .into_response(),
            Ok(_) => (
                StatusCode::BAD_GATEWAY,
                Json(SendResponse {
                    event_id: String::new(),
                    status: "failed".to_string(),
                    reply_message: None,
                    note: Some("unexpected session IPC send response".to_string()),
                }),
            )
                .into_response(),
            Err(err) => (
                StatusCode::BAD_GATEWAY,
                Json(SendResponse {
                    event_id: String::new(),
                    status: "failed".to_string(),
                    reply_message: None,
                    note: Some(format!("{err:?}")),
                }),
            )
                .into_response(),
        };
    }

    (
        StatusCode::BAD_REQUEST,
        Json(SendResponse {
            event_id: String::new(),
            status: "failed".to_string(),
            reply_message: None,
            note: Some("session_id is required for /send".to_string()),
        }),
    )
        .into_response()
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

async fn session_list_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(
        state
            .sessions
            .list()
            .into_iter()
            .map(session::SessionSummary::from)
            .collect::<Vec<_>>(),
    )
    .into_response()
}

#[derive(Debug, Deserialize)]
struct SessionCreateBody {
    project_dir: Option<PathBuf>,
    title: Option<String>,
}

async fn session_create_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(body): Json<SessionCreateBody>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(response) = config_not_ready_json_response().await {
        return response;
    }
    let scope = match body.project_dir {
        Some(project_dir) => match std::fs::canonicalize(&project_dir) {
            Ok(project_dir) => session::SessionScope::Project { project_dir },
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("cannot canonicalize project_dir: {err}")
                    })),
                )
                    .into_response();
            }
        },
        None => session::SessionScope::General,
    };
    match state.sessions.create(scope, body.title).await {
        Ok(info) => (
            StatusCode::CREATED,
            Json(session::SessionSummary::from(info)),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct SessionTitleBody {
    title: String,
}

async fn session_title_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(body): Json<SessionTitleBody>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let session_id = match session::SessionId::from_string(session_id) {
        Ok(session_id) => session_id,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("{err:?}") })),
            )
                .into_response();
        }
    };
    match state.sessions.set_title(&session_id, body.title).await {
        Ok(true) => Json(serde_json::json!({ "session_id": session_id.as_str() })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

async fn session_delete_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let session_id = match session::SessionId::from_string(session_id) {
        Ok(session_id) => session_id,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("{err:?}") })),
            )
                .into_response();
        }
    };
    match delete_session_by_id(
        &state.sessions,
        &state.session_tokens,
        &session_id,
        "session deleted",
    )
    .await
    {
        Ok(true) => Json(serde_json::json!({ "deleted": session_id.as_str() })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{err:?}") })),
        )
            .into_response(),
    }
}

pub(crate) async fn delete_session_by_id(
    sessions: &session::SessionRegistry,
    session_tokens: &SessionTokenStore,
    session_id: &session::SessionId,
    reason: &str,
) -> Result<bool> {
    if let Some(info) = sessions.get(session_id)
        && info.status.is_process_backed()
    {
        terminate_process_backed_session(
            sessions.clone(),
            session_tokens.clone(),
            info,
            reason.to_string(),
        )
        .await?;
    }
    session_tokens.write().remove(session_id);
    let removed = sessions.remove(session_id).await?.is_some();
    if removed {
        let session_dir = session::session_state_paths(session_id)
            .root()
            .to_path_buf();
        if session_dir.exists()
            && let Err(err) = remove_session_directory(&session_dir).await
        {
            tracing::warn!("{err:?}");
        }
    }
    Ok(removed)
}

async fn remove_session_directory(session_dir: &StdPath) -> Result<()> {
    let mut last_error = None;
    for attempt in 1..=SESSION_DIRECTORY_REMOVE_ATTEMPTS {
        match tokio::fs::remove_dir_all(session_dir).await {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                last_error = Some(err);
                if attempt < SESSION_DIRECTORY_REMOVE_ATTEMPTS {
                    tokio::time::sleep(SESSION_DIRECTORY_REMOVE_RETRY_DELAY).await;
                }
            }
        }
    }
    let err = last_error.expect("remove_dir_all should have produced an error");
    Err(miette!(
        "failed to remove session directory {} after {} attempt(s): {err}",
        session_dir.display(),
        SESSION_DIRECTORY_REMOVE_ATTEMPTS
    ))
}

pub(crate) async fn terminate_process_backed_sessions(
    sessions: &session::SessionRegistry,
    session_tokens: &SessionTokenStore,
    reason: &str,
) -> Result<()> {
    let targets = sessions
        .list()
        .into_iter()
        .filter(|info| info.status.is_process_backed())
        .collect::<Vec<_>>();
    let results = futures_util::future::join_all(targets.into_iter().map(|info| {
        terminate_process_backed_session(
            sessions.clone(),
            session_tokens.clone(),
            info,
            reason.to_string(),
        )
    }))
    .await;
    let errors = results
        .into_iter()
        .filter_map(std::result::Result::err)
        .map(|err| format!("{err:?}"))
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(miette!(
            "failed to terminate session processes for {reason}: {}",
            errors.join("; ")
        ))
    }
}

async fn terminate_process_backed_session(
    sessions: session::SessionRegistry,
    session_tokens: SessionTokenStore,
    info: session::SessionInfo,
    reason: String,
) -> Result<()> {
    let session_id = info.session_id.clone();
    let ipc_token = session_tokens.read().get(&session_id).cloned();
    if let (Some(ipc_name), Some(ipc_token)) = (info.ipc_name.clone(), ipc_token) {
        let client = session_ipc::SessionIpcClient::new(session_id.clone(), ipc_name, ipc_token)
            .with_timeout(SESSION_IPC_SHUTDOWN_TIMEOUT);
        if let Err(err) = client
            .request(session_ipc::SessionIpcRequest::Shutdown {
                reason: reason.clone(),
            })
            .await
        {
            tracing::warn!("session `{session_id}` IPC shutdown request failed: {err:?}");
        }
    }

    if let Some(pid) = info.pid
        && !wait_for_process_exit(pid, SESSION_PROCESS_TERM_TIMEOUT).await
    {
        tracing::warn!(
            "session `{session_id}` pid {pid} did not exit after IPC shutdown; sending SIGTERM"
        );
        let _ = signal_process(pid, Signal::Term);
        if !wait_for_process_exit(pid, SESSION_PROCESS_TERM_TIMEOUT).await {
            tracing::warn!(
                "session `{session_id}` pid {pid} did not exit after SIGTERM; sending SIGKILL"
            );
            let _ = signal_process(pid, Signal::Kill);
            if !wait_for_process_exit(pid, SESSION_PROCESS_KILL_TIMEOUT).await {
                return Err(miette!(
                    "session `{session_id}` pid {pid} did not exit after shutdown request"
                ));
            }
        }
    }

    session_tokens.write().remove(&session_id);
    sessions.mark_dead(&session_id).await?;
    Ok(())
}

async fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
    !process_exists(pid)
}

fn signal_process(pid: u32, signal: Signal) -> bool {
    let system = System::new_all();
    system
        .process(Pid::from_u32(pid))
        .and_then(|process| process.kill_with(signal))
        .unwrap_or(false)
}

fn open_stdio_log_pair(
    path: PathBuf,
    marker: &str,
) -> Result<(PathBuf, std::fs::File, std::fs::File)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| miette!("create log dir {} failed: {err}", parent.display()))?;
    }
    {
        let mut marker_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| miette!("open log {} failed: {err}", path.display()))?;
        writeln!(
            marker_file,
            "\n--- {marker} at {} ---",
            chrono::Utc::now().to_rfc3339()
        )
        .map_err(|err| miette!("write log marker {} failed: {err}", path.display()))?;
    }
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| miette!("open stdout log {} failed: {err}", path.display()))?;
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| miette!("open stderr log {} failed: {err}", path.display()))?;
    Ok((path, stdout, stderr))
}

async fn session_log_path(session_id: &session::SessionId) -> PathBuf {
    session::session_state_paths(session_id).logs_file(SESSION_LOG)
}

async fn open_session_log(
    session_id: &session::SessionId,
) -> Result<(PathBuf, std::fs::File, std::fs::File)> {
    let path = session_log_path(session_id).await;
    let marker = format!("session `{}` starting", session_id);
    open_stdio_log_pair(path, &marker)
}

fn spawn_session_exit_watcher(
    sessions: session::SessionRegistry,
    session_tokens: SessionTokenStore,
    session_id: session::SessionId,
    pid: u32,
    mut child: std::process::Child,
    log_path: PathBuf,
) {
    tokio::spawn(async move {
        let wait_result = tokio::task::spawn_blocking(move || child.wait()).await;
        match wait_result {
            Ok(Ok(status)) if status.success() => {
                tracing::info!(
                    "session `{session_id}` pid {pid} exited successfully with status {status}; log={}",
                    log_path.display()
                );
            }
            Ok(Ok(status)) => {
                tracing::warn!(
                    "session `{session_id}` pid {pid} exited with status {status}; log={}",
                    log_path.display()
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    "failed to wait for session `{session_id}` pid {pid}: {err}; log={}",
                    log_path.display()
                );
            }
            Err(err) => {
                tracing::warn!(
                    "session `{session_id}` pid {pid} wait task failed: {err}; log={}",
                    log_path.display()
                );
            }
        }

        if let Some(info) = sessions.get(&session_id)
            && info.pid == Some(pid)
            && info.status.is_process_backed()
        {
            session_tokens.write().remove(&session_id);
            if let Err(err) = sessions.mark_dead(&session_id).await {
                tracing::warn!(
                    "failed to mark exited session `{session_id}` pid {pid} dead: {err:?}"
                );
            }
        }
    });
}

async fn spawn_session_process(
    sessions: session::SessionRegistry,
    session_tokens: SessionTokenStore,
    session_id: session::SessionId,
    info: session::SessionInfo,
) -> Result<u32> {
    let ipc_name = session::session_ipc_name(&session_id);
    let ipc_token = session::generate_ipc_token();
    let binary = std::env::current_exe()
        .map_err(|err| miette!("resolve current executable failed: {err}"))?;
    let (session_log_path, stdout_log, stderr_log) = open_session_log(&session_id).await?;
    let mut command = std::process::Command::new(binary);
    command
        .arg("--session-id")
        .arg(session_id.as_str())
        .arg("--ipc-name")
        .arg(&ipc_name)
        .arg("--ipc-token")
        .arg(&ipc_token);
    if let Some(project_dir) = info.project_dir.as_ref() {
        command.arg("--session-project-dir").arg(project_dir);
    }
    command
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(stdout_log)
        .stderr(stderr_log);
    crate::process_spawn::apply_no_window(&mut command);
    let child = command
        .spawn()
        .map_err(|err| miette!("spawn session process failed: {err}"))?;
    let pid = child.id();
    tracing::info!(
        "spawned session `{session_id}` pid {pid}; log={}",
        session_log_path.display()
    );
    session_tokens
        .write()
        .insert(session_id.clone(), ipc_token.clone());
    session::store_session_ipc_token(&session_id, &ipc_token).await?;
    sessions
        .mark_starting(&session_id, pid, ipc_name.clone(), &ipc_token)
        .await?;
    spawn_session_exit_watcher(
        sessions.clone(),
        session_tokens.clone(),
        session_id.clone(),
        pid,
        child,
        session_log_path,
    );

    let client = session_ipc::SessionIpcClient::new(session_id.clone(), ipc_name, ipc_token)
        .with_timeout(Duration::from_secs(2));
    let startup_title = match wait_for_session_ready(&client).await {
        Ok(title) => title,
        Err(err) => {
            tracing::warn!("session `{session_id}` did not become ready; terminating pid {pid}");
            let _ = signal_process(pid, Signal::Term);
            if !wait_for_process_exit(pid, SESSION_PROCESS_TERM_TIMEOUT).await {
                let _ = signal_process(pid, Signal::Kill);
                let _ = wait_for_process_exit(pid, SESSION_PROCESS_KILL_TIMEOUT).await;
            }
            let _ = sessions.mark_dead(&session_id).await;
            return Err(err);
        }
    };
    sessions.mark_ready(&session_id).await?;
    if let Some(startup_title) = startup_title
        && let Some(mut info) = sessions.get(&session_id)
    {
        apply_session_title_update(&sessions, &mut info, Some(&startup_title)).await;
    }
    Ok(pid)
}

async fn wait_for_session_ready(
    client: &session_ipc::SessionIpcClient,
) -> Result<Option<DashboardSessionTitle>> {
    const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(30);
    let deadline = Instant::now() + SESSION_READY_TIMEOUT;
    while Instant::now() < deadline {
        match client
            .request(session_ipc::SessionIpcRequest::StatusSummary)
            .await
        {
            Ok(session_ipc::SessionIpcResponse::StatusSummary { summary })
                if summary.runtime_status.ready =>
            {
                return Ok(summary
                    .session_title
                    .clone()
                    .or_else(|| summary.dashboard.session_title.clone()));
            }
            Ok(session_ipc::SessionIpcResponse::StatusSummary { .. }) => {}
            Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => {
                tracing::warn!("session status returned error during startup: {message}");
            }
            Ok(_) => {}
            Err(err) => {
                tracing::debug!("session status probe failed during startup: {err:?}");
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(miette!(
        "session did not become ready within {}s",
        SESSION_READY_TIMEOUT.as_secs()
    ))
}

fn runtime_not_ready_response(state: DaemonLifecycleState) -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!("daemon is {state}; runtime commands are accepted only when ready"),
    )
        .into_response()
}

async fn config_not_ready_response() -> Option<axum::response::Response> {
    let readiness = config_setup::ensure_config_readiness().await;
    (!readiness.is_complete()).then(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(CommandResponse {
                output: readiness.agent_unavailable_message(),
            }),
        )
            .into_response()
    })
}

async fn config_not_ready_send_response() -> Option<axum::response::Response> {
    let readiness = config_setup::ensure_config_readiness().await;
    (!readiness.is_complete()).then(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(SendResponse {
                event_id: String::new(),
                status: "failed".to_string(),
                reply_message: None,
                note: Some(readiness.agent_unavailable_message()),
            }),
        )
            .into_response()
    })
}

async fn config_not_ready_dashboard_action_response() -> Option<axum::response::Response> {
    let readiness = config_setup::ensure_config_readiness().await;
    (!readiness.is_complete()).then(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(DashboardActionResponse {
                result: DashboardActionResult {
                    success: false,
                    message: readiness.agent_unavailable_message(),
                    detail: None,
                },
            }),
        )
            .into_response()
    })
}

async fn config_not_ready_json_response() -> Option<axum::response::Response> {
    let readiness = config_setup::ensure_config_readiness().await;
    (!readiness.is_complete()).then(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": readiness.agent_unavailable_message(),
                "readiness": readiness,
            })),
        )
            .into_response()
    })
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
            settings_model_summary(name, model, &config.main_model, &judge_effective_model)
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
        ProviderConfig::OpenaiCodexOauth { base_url } => {
            let auth_file = crate::providers::codex_oauth_auth_file(name)
                .to_string_lossy()
                .to_string();
            SettingsProviderSummary {
                name: name.to_string(),
                provider_type: "openai-codex-oauth",
                base_url: Some(
                    base_url
                        .clone()
                        .unwrap_or_else(|| "https://chatgpt.com/backend-api/codex".to_string()),
                ),
                credential: SettingsCredentialSummary {
                    status: SettingsCredentialStatus::OauthFile,
                    source: Some(auth_file.clone()),
                },
                auth_file: Some(auth_file),
            }
        }
        ProviderConfig::OpenaiCompatible {
            base_url, api_key, ..
        } => SettingsProviderSummary {
            name: name.to_string(),
            provider_type: "openai-compatible",
            base_url: Some(base_url.clone()),
            credential: credential_summary(api_key, Some("your-api-key")),
            auth_file: None,
        },
        ProviderConfig::Ollama { host, api_key, .. } => {
            let is_cloud = api_key.is_some();
            SettingsProviderSummary {
                name: name.to_string(),
                provider_type: if is_cloud { "ollama-cloud" } else { "ollama" },
                base_url: Some(host.clone().unwrap_or_else(|| {
                    if is_cloud {
                        "https://ollama.com".to_string()
                    } else {
                        "http://127.0.0.1:11434".to_string()
                    }
                })),
                credential: if is_cloud {
                    credential_summary(
                        api_key.as_deref().unwrap_or(""),
                        Some("your-ollama-api-key"),
                    )
                } else {
                    SettingsCredentialSummary {
                        status: SettingsCredentialStatus::Placeholder,
                        source: None,
                    }
                },
                auth_file: None,
            }
        }
    }
}

fn settings_model_summary(
    name: &str,
    model: &ModelConfig,
    main_model: &str,
    judge_model: &str,
) -> SettingsModelSummary {
    SettingsModelSummary {
        name: name.to_string(),
        provider: model.provider.clone(),
        model_id: model.model_id.clone(),
        is_main: name == main_model,
        is_judge: name == judge_model,
        temperature: model.temperature,
        thinking_budget: model
            .thinking_budget()
            .map(|budget| budget.as_str().to_string()),
        rpm: model.rpm,
        request_timeout_secs: model.request_timeout_secs(),
        stream_idle_timeout_secs: model.stream_idle_timeout_secs(),
        context_window_tokens: model.context_window_tokens(),
        effective_context_window_percent: model.effective_context_window_percent(),
        effective_context_window_tokens: model.effective_context_window_tokens(),
        auto_compact_token_limit: model.auto_compact_token_limit(),
        reserved_output_tokens: model.reserved_output_tokens(),
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

fn strong_filesystem_mode_label(value: StrongFilesystemSandboxMode) -> &'static str {
    match value {
        StrongFilesystemSandboxMode::Off => "off",
        StrongFilesystemSandboxMode::Auto => "auto",
        StrongFilesystemSandboxMode::Required => "required",
    }
}

async fn session_dashboard_ws(
    mut socket: WebSocket,
    client: session_ipc::SessionIpcClient,
    sessions: session::SessionRegistry,
    telegram_acl: TelegramAclHandle,
    session_id: String,
) {
    let mut stream = match client.subscribe_dashboard().await {
        Ok(stream) => stream,
        Err(err) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({
                        "runtime_status": format!("session dashboard stream failed: {err:?}")
                    })
                    .to_string()
                    .into(),
                ))
                .await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    loop {
        match session_ipc::read_stream_event(&mut stream).await {
            Ok(session_ipc::SessionIpcStreamEvent::DashboardSnapshot { state }) => {
                let mut snapshot = *state;
                overlay_manager_owned_dashboard_state(&telegram_acl, &mut snapshot);
                sync_session_title_from_dashboard_state(&sessions, &session_id, &snapshot).await;
                if send_dashboard_ws_snapshot(&mut socket, &snapshot)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Ok(session_ipc::SessionIpcStreamEvent::DashboardClosed { .. }) => break,
            Ok(session_ipc::SessionIpcStreamEvent::Error { message, .. }) => {
                let _ = socket.send(Message::Text(message.into())).await;
                break;
            }
            Err(_) => break,
        }
    }
}

async fn sync_session_title_from_dashboard_state(
    sessions: &session::SessionRegistry,
    session_id: &str,
    snapshot: &DashboardState,
) {
    if snapshot.session_title.is_none() {
        return;
    }
    let Ok(session_id) = session::SessionId::from_string(session_id.to_string()) else {
        tracing::warn!("dashboard snapshot carried invalid session id `{session_id}`");
        return;
    };
    let Some(mut info) = sessions.get(&session_id) else {
        return;
    };
    apply_session_title_update(sessions, &mut info, snapshot.session_title.as_ref()).await;
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

async fn session_client_for_request(
    state: &ServerState,
    session_id: &str,
) -> Result<session_ipc::SessionIpcClient> {
    session_client_for_id(&state.sessions, &state.session_tokens, session_id).await
}

pub(crate) async fn session_client_for_id(
    sessions: &session::SessionRegistry,
    session_tokens: &SessionTokenStore,
    session_id: &str,
) -> Result<session_ipc::SessionIpcClient> {
    let readiness = config_setup::ensure_config_readiness().await;
    if !readiness.is_complete() {
        return Err(miette!(readiness.agent_unavailable_message()));
    }
    let session_id = session::SessionId::from_string(session_id.to_string())?;
    let mut info = sessions
        .get(&session_id)
        .ok_or_else(|| miette!("session `{session_id}` not found"))?;

    if let Ok(client) = live_session_client(session_tokens, &session_id, &info) {
        return Ok(client);
    }

    if info.status.is_process_backed() {
        sessions.mark_dead(&session_id).await?;
        info.status = session::SessionStatus::Dead;
        info.pid = None;
        info.ipc_name = None;
        info.ipc_token_hash = None;
    }

    spawn_session_process(
        sessions.clone(),
        session_tokens.clone(),
        session_id.clone(),
        info,
    )
    .await?;
    let info = sessions
        .get(&session_id)
        .ok_or_else(|| miette!("session `{session_id}` not found after spawn"))?;
    live_session_client(session_tokens, &session_id, &info)
}

fn live_session_client(
    session_tokens: &SessionTokenStore,
    session_id: &session::SessionId,
    info: &session::SessionInfo,
) -> Result<session_ipc::SessionIpcClient> {
    let ipc_name = info
        .ipc_name
        .clone()
        .ok_or_else(|| miette!("session `{session_id}` is not running"))?;
    let ipc_token = session_tokens
        .read()
        .get(session_id)
        .cloned()
        .ok_or_else(|| miette!("session `{session_id}` has no live IPC token"))?;
    Ok(session_ipc::SessionIpcClient::new(
        session_id.clone(),
        ipc_name,
        ipc_token,
    ))
}

#[derive(Clone)]
pub struct DaemonClient {
    port: u16,
    http: reqwest::Client,
    auth_token: Option<DaemonAuthToken>,
    session_id: Option<String>,
    control_timeout: Duration,
}

fn daemon_http_client() -> std::result::Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(DAEMON_HTTP_CONNECT_TIMEOUT)
        .build()
}

impl DaemonClient {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            http: daemon_http_client().expect("build daemon http client"),
            auth_token: None,
            session_id: None,
            control_timeout: DAEMON_CONTROL_REQUEST_TIMEOUT,
        }
    }

    pub async fn authenticated(port: u16) -> Result<Self> {
        Ok(Self {
            port,
            http: daemon_http_client()
                .map_err(|err| miette!("build daemon http client failed: {err}"))?,
            auth_token: Some(load_daemon_auth_token().await?),
            session_id: None,
            control_timeout: DAEMON_CONTROL_REQUEST_TIMEOUT,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    #[cfg(test)]
    fn with_control_timeout(mut self, timeout: Duration) -> Self {
        self.control_timeout = timeout;
        self
    }

    fn base_url(&self) -> String {
        format!("http://{}:{}", DAEMON_CLIENT_HOST, self.port)
    }

    fn ws_url(&self) -> String {
        let mut url = format!("ws://{}:{}/dashboard/stream", DAEMON_CLIENT_HOST, self.port);
        if let Some(session_id) = self.session_id.as_deref() {
            url.push_str("?session_id=");
            url.push_str(session_id);
        }
        url
    }

    fn with_auth(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let token = self
            .auth_token
            .as_ref()
            .ok_or_else(|| miette!("daemon auth token is not loaded"))?;
        Ok(request.bearer_auth(token.as_str()))
    }

    fn control_request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.timeout(self.control_timeout)
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
            .timeout(self.control_timeout)
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
        let mut url = format!("{}/dashboard/snapshot", self.base_url());
        if let Some(session_id) = self.session_id.as_deref() {
            url.push_str("?session_id=");
            url.push_str(session_id);
        }
        self.with_auth(self.control_request(self.http.get(url)))?
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
        self.send_command_with_attachments(command, Vec::new())
            .await
    }

    fn tui_command_request(
        &self,
        command: &str,
        attachments: Vec<CommandAttachmentRequest>,
    ) -> CommandRequest {
        CommandRequest {
            command: command.to_string(),
            attachments,
            origin: session_ipc::UserInputOrigin::Tui,
            session_id: self.session_id.clone(),
        }
    }

    pub async fn send_command_with_attachments(
        &self,
        command: &str,
        attachments: Vec<DashboardCommandAttachment>,
    ) -> Result<String> {
        let attachments = command_attachment_requests(attachments).await?;
        let response = self
            .with_auth(
                self.control_request(
                    self.http
                        .post(format!("{}/commands/run", self.base_url()))
                        .json(&self.tui_command_request(command, attachments)),
                ),
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

    pub async fn send_dashboard_action(
        &self,
        action: DashboardAction,
    ) -> Result<DashboardActionResult> {
        let response = self
            .with_auth(
                self.control_request(
                    self.http
                        .post(format!("{}/dashboard/action", self.base_url()))
                        .json(&DashboardActionRequest {
                            action,
                            session_id: self.session_id.clone(),
                        }),
                ),
            )?
            .send()
            .await
            .map_err(|err| miette!("dashboard action request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("dashboard action returned error: {err}"))?
            .json::<DashboardActionResponse>()
            .await
            .map_err(|err| miette!("decode dashboard action response failed: {err}"))?;
        Ok(response.result)
    }

    pub async fn send_message(&self, message: &str) -> Result<SendResponse> {
        let response =
            self.with_auth(self.http.post(format!("{}/send", self.base_url())).json(
                &SendRequest {
                    message: message.to_string(),
                    session_id: self.session_id.clone(),
                },
            ))?
            .send()
            .await
            .map_err(|err| miette!("send request failed: {err}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|err| miette!("read send response failed: {err}"))?;
        let send_response = serde_json::from_str::<SendResponse>(&text).map_err(|err| {
            miette!(
                "decode send response failed with HTTP {}: {}{}",
                status,
                err,
                if text.trim().is_empty() {
                    String::new()
                } else {
                    format!("; body: {text}")
                }
            )
        })?;
        if status.is_success() {
            Ok(send_response)
        } else {
            let note = send_response
                .note
                .as_deref()
                .unwrap_or("send request failed");
            Err(miette!(
                "send request returned {} for event {} with status {}: {}",
                status,
                send_response.event_id,
                send_response.status,
                note
            ))
        }
    }

    pub async fn list_sessions(&self) -> Result<Vec<session::SessionSummary>> {
        self.with_auth(
            self.control_request(self.http.get(format!("{}/sessions", self.base_url()))),
        )?
        .send()
        .await
        .map_err(|err| miette!("list sessions request failed: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("list sessions returned error: {err}"))?
        .json::<Vec<session::SessionSummary>>()
        .await
        .map_err(|err| miette!("decode session list failed: {err}"))
    }

    pub async fn create_session(
        &self,
        project_dir: Option<&std::path::Path>,
        title: Option<&str>,
    ) -> Result<session::SessionSummary> {
        let body = serde_json::json!({
            "project_dir": project_dir.map(|path| path.display().to_string()),
            "title": title,
        });
        self.with_auth(
            self.control_request(
                self.http
                    .post(format!("{}/sessions", self.base_url()))
                    .json(&body),
            ),
        )?
        .send()
        .await
        .map_err(|err| miette!("create session request failed: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("create session returned error: {err}"))?
        .json::<session::SessionSummary>()
        .await
        .map_err(|err| miette!("decode create session response failed: {err}"))
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.with_auth(
            self.control_request(
                self.http
                    .delete(format!("{}/sessions/{session_id}", self.base_url())),
            ),
        )?
        .send()
        .await
        .map_err(|err| miette!("delete session request failed: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("delete session returned error: {err}"))?;
        Ok(())
    }

    pub async fn set_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        self.with_auth(
            self.control_request(
                self.http
                    .post(format!("{}/sessions/{session_id}/title", self.base_url()))
                    .json(&serde_json::json!({ "title": title })),
            ),
        )?
        .send()
        .await
        .map_err(|err| miette!("set session title request failed: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("set session title returned error: {err}"))?;
        Ok(())
    }

    pub async fn activity_history_before(
        &self,
        before: Option<i64>,
        limit: usize,
    ) -> Result<DashboardActivityHistoryPage> {
        let mut url = format!(
            "{}/dashboard/activity-history?limit={}",
            self.base_url(),
            limit
        );
        if let Some(before) = before {
            url.push_str(&format!("&before={}", before));
        }
        if let Some(session_id) = self.session_id.as_deref() {
            url.push_str("&session_id=");
            url.push_str(session_id);
        }
        self.with_auth(self.control_request(self.http.get(&url)))?
            .send()
            .await
            .map_err(|err| miette!("activity history request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("activity history returned error: {err}"))?
            .json::<DashboardActivityHistoryPage>()
            .await
            .map_err(|err| miette!("decode activity history response failed: {err}"))
    }

    pub async fn input_history(&self, limit: usize) -> Result<DashboardInputHistory> {
        let mut url = format!(
            "{}/dashboard/input-history?limit={}",
            self.base_url(),
            limit
        );
        if let Some(session_id) = self.session_id.as_deref() {
            url.push_str("&session_id=");
            url.push_str(session_id);
        }
        self.with_auth(self.control_request(self.http.get(&url)))?
            .send()
            .await
            .map_err(|err| miette!("input history request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("input history returned error: {err}"))?
            .json::<DashboardInputHistory>()
            .await
            .map_err(|err| miette!("decode input history response failed: {err}"))
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.with_auth(
            self.control_request(
                self.http
                    .post(format!("{}/daemon/shutdown", self.base_url())),
            ),
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
            self.control_request(
                self.http
                    .post(format!("{}/daemon/restart", self.base_url())),
            ),
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
        let connect_timeout = self.control_timeout;

        let (ws, _) = tokio::select! {
            result = tokio::time::timeout(connect_timeout, tokio_tungstenite::connect_async(request)) => {
                let result = result
                    .map_err(|_| miette!("connect dashboard ws timed out after {}ms", connect_timeout.as_millis()))?;
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
    async fn run_command(
        &self,
        command: &str,
        attachments: Vec<DashboardCommandAttachment>,
        _state: &DashboardState,
    ) -> String {
        let result = if attachments.is_empty() {
            self.send_command(command).await
        } else {
            self.send_command_with_attachments(command, attachments)
                .await
        };
        match result {
            Ok(output) => output,
            Err(err) => format!("command failed: {err}"),
        }
    }

    async fn run_action(
        &self,
        action: DashboardAction,
        _state: &DashboardState,
    ) -> DashboardActionResult {
        match self.send_dashboard_action(action).await {
            Ok(result) => result,
            Err(err) => DashboardActionResult {
                success: false,
                message: format!("dashboard action failed: {err}"),
                detail: None,
            },
        }
    }
}

async fn command_attachment_requests(
    attachments: Vec<DashboardCommandAttachment>,
) -> Result<Vec<CommandAttachmentRequest>> {
    let mut requests = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let bytes = tokio::fs::read(&attachment.path).await.map_err(|err| {
            miette!(
                "failed to read image attachment {}: {err}",
                attachment.path.display()
            )
        })?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        requests.push(CommandAttachmentRequest {
            name: attachment.name,
            media_type: attachment.media_type.clone(),
            data_url: format!("data:{};base64,{encoded}", attachment.media_type),
        });
    }
    Ok(requests)
}

#[async_trait::async_trait]
impl DashboardHistoryLoader for DaemonClient {
    async fn load_history_before(
        &self,
        before: Option<i64>,
        limit: usize,
    ) -> Result<DashboardActivityHistoryPage, String> {
        self.activity_history_before(before, limit)
            .await
            .map_err(|err| err.to_string())
    }

    async fn load_recent_user_inputs(&self, limit: usize) -> Result<DashboardInputHistory, String> {
        self.input_history(limit)
            .await
            .map_err(|err| err.to_string())
    }
}

async fn configured_daemon_port() -> Result<u16> {
    Ok(config_setup::read_manager_boot_config().await.port)
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

pub async fn wait_for_daemon_restarted(previous: &StatusResponse) -> Result<StatusResponse> {
    let client = DaemonClient::new(previous.port);
    let deadline = Instant::now() + READY_TIMEOUT + SHUTDOWN_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        match client.status().await {
            Ok(status) if is_restarted_ready_daemon(previous, &status) => return Ok(status),
            Ok(status) if status.state == DaemonLifecycleState::Failed => {
                return Err(miette!(
                    "daemon restart failed{}",
                    daemon_startup_log_tail_suffix().await
                ));
            }
            Ok(status) => {
                last_error = Some(format!(
                    "daemon pid={} started_at_ms={} is {}",
                    status.pid, status.started_at_ms, status.state
                ));
            }
            Err(err) => last_error = Some(err.to_string()),
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
    Err(miette!(
        "daemon did not restart within {}s{}{}",
        (READY_TIMEOUT + SHUTDOWN_TIMEOUT).as_secs(),
        last_error
            .as_deref()
            .map(|err| format!(": {err}"))
            .unwrap_or_default(),
        daemon_startup_log_tail_suffix().await
    ))
}

fn is_restarted_ready_daemon(previous: &StatusResponse, status: &StatusResponse) -> bool {
    status.state == DaemonLifecycleState::Ready
        && status.port == previous.port
        && (status.pid != previous.pid || status.started_at_ms > previous.started_at_ms)
}

pub async fn wait_for_daemon_shutdown(port: u16) -> Result<()> {
    let client = DaemonClient::new(port);
    let mut deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    let mut saw_stopping = false;
    while Instant::now() < deadline {
        match client.status().await {
            Ok(status) if status.state == DaemonLifecycleState::Stopping => {
                observe_daemon_shutdown_status(status.state, &mut saw_stopping, &mut deadline);
            }
            Ok(_) => {
                // Still running normally.
            }
            Err(_) => {
                // Connection refused — daemon has released the port.
                return Ok(());
            }
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

fn observe_daemon_shutdown_status(
    state: DaemonLifecycleState,
    saw_stopping: &mut bool,
    deadline: &mut Instant,
) {
    if state == DaemonLifecycleState::Stopping && !*saw_stopping {
        *saw_stopping = true;
        *deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    }
}

pub async fn spawn_detached_daemon_process() -> Result<()> {
    let current_exe = std::env::current_exe()
        .map_err(|err| miette!("resolve current executable failed: {err}"))?;
    let startup_mode = DetachedDaemonStartupMode::from_tray_enabled(
        crate::daemon_tray::should_attempt_daemon_tray(),
    );

    let (log_path, stdout_log, stderr_log) =
        open_stdio_log_pair(daemon_main_log_path().await, "daemon process starting")?;

    let mut command = std::process::Command::new(current_exe);
    configure_detached_daemon_command(&mut command, startup_mode);
    command.stdout(stdout_log).stderr(stderr_log);
    command
        .spawn()
        .map_err(|err| miette!("spawn daemon process failed: {err}"))?;
    tracing::info!(
        "spawned detached daemon process; log={}",
        log_path.display()
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetachedDaemonStartupMode {
    WithTray,
    Headless,
}

impl DetachedDaemonStartupMode {
    fn from_tray_enabled(enabled: bool) -> Self {
        if enabled {
            Self::WithTray
        } else {
            Self::Headless
        }
    }
}

fn configure_detached_daemon_command(
    command: &mut std::process::Command,
    startup_mode: DetachedDaemonStartupMode,
) {
    command.arg("serve").stdin(Stdio::null());

    match startup_mode {
        DetachedDaemonStartupMode::WithTray => {
            command.env(crate::daemon_tray::ENABLE_TRAY_ENV, "1");
            command.env_remove(DAEMONIZE_ENV);
        }
        DetachedDaemonStartupMode::Headless => {
            command.env(DAEMONIZE_ENV, "1");
            command.env_remove(crate::daemon_tray::ENABLE_TRAY_ENV);
        }
    }

    apply_detached_daemon_creation_flags(command, startup_mode);
}

#[cfg(windows)]
fn apply_detached_daemon_creation_flags(
    command: &mut std::process::Command,
    startup_mode: DetachedDaemonStartupMode,
) {
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

    let mut flags = CREATE_NEW_PROCESS_GROUP;
    if matches!(startup_mode, DetachedDaemonStartupMode::Headless) {
        flags |= DETACHED_PROCESS;
    }
    crate::process_spawn::apply_no_window_with_flags(command, flags);
}

#[cfg(not(windows))]
fn apply_detached_daemon_creation_flags(
    _command: &mut std::process::Command,
    _startup_mode: DetachedDaemonStartupMode,
) {
}

async fn daemon_main_log_path() -> PathBuf {
    daat_locus_paths().await.logs_file(DAEMON_MAIN_LOG)
}

async fn daemon_startup_log_tail_suffix() -> String {
    log_tail_section(daemon_main_log_path().await, "recent daemon log")
        .await
        .map(|section| format!("\n\n{section}"))
        .unwrap_or_default()
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
        Err(connect_err) => {
            let port = configured_daemon_port().await?;
            fail_if_daemon_port_accepts_without_status(port, &connect_err).await?;
            spawn_detached_daemon_process().await?;
            let status = wait_for_daemon_ready().await?;
            let client = DaemonClient::authenticated(status.port).await?;
            Ok(client)
        }
    }
}

async fn fail_if_daemon_port_accepts_without_status(
    port: u16,
    status_error: &miette::Report,
) -> Result<()> {
    let connected = tokio::time::timeout(
        DAEMON_PORT_PROBE_TIMEOUT,
        TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    if connected {
        return Err(miette!(
            "daemon port {}:{} accepts TCP connections but did not return a usable /status; refusing to start a second daemon on the same port. status error: {status_error}. Try stopping the stale listener, rebooting Windows, or changing [daemon].port in config.toml.",
            DAEMON_CLIENT_HOST,
            port
        ));
    }
    Ok(())
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

    #[test]
    fn daemon_client_connects_to_localhost_not_bind_unspecified_address() {
        let client = DaemonClient::new(53825);

        assert_eq!(client.base_url(), "http://localhost:53825");
        assert_eq!(client.ws_url(), "ws://localhost:53825/dashboard/stream");
    }

    #[test]
    fn detached_daemon_command_with_tray_marks_child_startup_intent() {
        let mut command = std::process::Command::new("daat-locus");

        configure_detached_daemon_command(&mut command, DetachedDaemonStartupMode::WithTray);

        assert_eq!(command_args(&command), vec!["serve".to_string()]);
        assert_eq!(
            command_env(&command, crate::daemon_tray::ENABLE_TRAY_ENV),
            Some(Some("1".to_string()))
        );
        assert_eq!(command_env(&command, DAEMONIZE_ENV), Some(None));
    }

    #[test]
    fn detached_daemon_command_headless_marks_daemonize_intent() {
        let mut command = std::process::Command::new("daat-locus");

        configure_detached_daemon_command(&mut command, DetachedDaemonStartupMode::Headless);

        assert_eq!(command_args(&command), vec!["serve".to_string()]);
        assert_eq!(
            command_env(&command, DAEMONIZE_ENV),
            Some(Some("1".to_string()))
        );
        assert_eq!(
            command_env(&command, crate::daemon_tray::ENABLE_TRAY_ENV),
            Some(None)
        );
    }

    fn command_args(command: &std::process::Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn command_env(command: &std::process::Command, key: &str) -> Option<Option<String>> {
        command
            .get_envs()
            .find(|(name, _)| *name == std::ffi::OsStr::new(key))
            .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
    }

    #[test]
    fn command_request_defaults_to_webui_origin_for_compatibility() {
        let request: CommandRequest = serde_json::from_value(serde_json::json!({
            "command": "hello"
        }))
        .expect("deserialize command request");

        assert_eq!(request.origin, session_ipc::UserInputOrigin::WebUi);
    }

    #[test]
    fn daemon_client_command_requests_use_tui_origin() {
        let request = DaemonClient::new(53825)
            .with_session("session-test")
            .tui_command_request("hello", Vec::new());

        assert_eq!(request.origin, session_ipc::UserInputOrigin::Tui);
        assert_eq!(request.session_id.as_deref(), Some("session-test"));
    }

    #[test]
    fn open_stdio_log_pair_writes_start_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("session.log");

        let (returned_path, stdout_log, stderr_log) =
            open_stdio_log_pair(path.clone(), "session `abc` starting").expect("open log pair");
        drop(stdout_log);
        drop(stderr_log);

        assert_eq!(returned_path, path);
        let contents = std::fs::read_to_string(&returned_path).expect("read log");
        assert!(contents.contains("--- session `abc` starting at "));
    }

    #[tokio::test]
    async fn daemon_status_times_out_when_port_accepts_without_response() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind hanging daemon test listener");
        let port = listener.local_addr().expect("listener addr").port();
        let accept_task = tokio::spawn(async move {
            if let Ok((_socket, _addr)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        let client = DaemonClient::new(port).with_control_timeout(Duration::from_millis(50));
        let started = Instant::now();
        let err = client.status().await.expect_err("status should time out");
        accept_task.abort();

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(err.to_string().contains("daemon status request failed"));
    }

    #[tokio::test]
    async fn daemon_port_probe_rejects_unresponsive_listener_before_spawn() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind unresponsive daemon test listener");
        let port = listener.local_addr().expect("listener addr").port();
        let accept_task = tokio::spawn(async move {
            if let Ok((_socket, _addr)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        let err = fail_if_daemon_port_accepts_without_status(
            port,
            &miette!("daemon status request failed: timeout"),
        )
        .await
        .expect_err("accepted but unresponsive port should be rejected");
        accept_task.abort();

        assert!(err.to_string().contains("accepts TCP connections"));
        assert!(err.to_string().contains("usable /status"));
        assert!(
            err.to_string()
                .contains("refusing to start a second daemon")
        );
    }

    #[tokio::test]
    async fn remove_session_directory_removes_non_empty_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let session_dir = temp.path().join("session");
        let nested_dir = session_dir.join("state");
        tokio::fs::create_dir_all(&nested_dir)
            .await
            .expect("create nested session dir");
        tokio::fs::write(nested_dir.join("events"), b"[]")
            .await
            .expect("write session file");

        remove_session_directory(&session_dir)
            .await
            .expect("remove session dir");

        assert!(!session_dir.exists());
    }

    #[test]
    fn restarted_ready_daemon_requires_new_process_identity() {
        let previous = StatusResponse {
            pid: 100,
            started_at_ms: 1_000,
            version: "0.1.1".to_string(),
            bind_host: DAEMON_HOST_DISPLAY.to_string(),
            port: 53825,
            state: DaemonLifecycleState::Ready,
            connected_clients: 0,
        };
        let same_process = StatusResponse {
            pid: 100,
            started_at_ms: 1_000,
            version: "0.1.1".to_string(),
            bind_host: DAEMON_HOST_DISPLAY.to_string(),
            port: 53825,
            state: DaemonLifecycleState::Ready,
            connected_clients: 0,
        };
        let restarted = StatusResponse {
            pid: 101,
            started_at_ms: 1_100,
            version: "0.1.1".to_string(),
            bind_host: DAEMON_HOST_DISPLAY.to_string(),
            port: 53825,
            state: DaemonLifecycleState::Ready,
            connected_clients: 0,
        };

        assert!(!is_restarted_ready_daemon(&previous, &same_process));
        assert!(is_restarted_ready_daemon(&previous, &restarted));
    }

    #[test]
    fn shutdown_wait_deadline_extends_once_for_stopping_status() {
        let mut deadline = Instant::now() + Duration::from_secs(1);
        let initial_deadline = deadline;
        let mut saw_stopping = false;

        observe_daemon_shutdown_status(
            DaemonLifecycleState::Ready,
            &mut saw_stopping,
            &mut deadline,
        );
        assert_eq!(deadline, initial_deadline);
        assert!(!saw_stopping);

        observe_daemon_shutdown_status(
            DaemonLifecycleState::Stopping,
            &mut saw_stopping,
            &mut deadline,
        );
        assert!(deadline > initial_deadline);
        assert!(saw_stopping);
        let stopping_deadline = deadline;

        observe_daemon_shutdown_status(
            DaemonLifecycleState::Stopping,
            &mut saw_stopping,
            &mut deadline,
        );
        assert_eq!(deadline, stopping_deadline);

        observe_daemon_shutdown_status(
            DaemonLifecycleState::Ready,
            &mut saw_stopping,
            &mut deadline,
        );
        assert_eq!(deadline, stopping_deadline);
    }
}
