use std::{
    path::PathBuf,
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
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header::AUTHORIZATION},
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::StreamExt;
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::{
    config::load_config,
    daat_locus_paths::daat_locus_paths,
    dashboard::{
        DashboardCommandRunner, DashboardControlCommand, DashboardState, execute_remote_command,
    },
    events::EventStore,
    pending_work::PendingWorkQueue,
    telegram_acl::TelegramAclHandle,
};

mod auth;

pub use auth::{
    CreatedDaemonToken, DaemonAuthToken, DaemonTokenListEntry, DaemonTokenRegistryHandle,
    create_daemon_token, list_daemon_tokens, load_daemon_auth_token,
    load_or_create_daemon_token_registry, revoke_daemon_token, rotate_daemon_token,
};

const LOCALHOST: &str = "127.0.0.1";
/// Daemon cold start can include browser runtime install plus hindsight/uv
/// first-run setup. Hindsight itself allows 10 minutes for daemon start, so the
/// outer readiness window must be longer than that inner startup budget.
const READY_TIMEOUT: Duration = Duration::from_secs(900);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);
const DAEMON_MAIN_LOG: &str = "daat-locus.log";
const DAEMON_STDERR_LOG: &str = "daemon-stderr.log";
pub const DAEMONIZE_ENV: &str = "DAAT_LOCUS_DAEMONIZE";

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub pid: u32,
    pub started_at_ms: i64,
    pub version: String,
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResponse {
    pub output: String,
}

#[derive(Debug)]
pub enum DaemonControlCommand {
    ShutdownRequested { completion_tx: oneshot::Sender<()> },
}

#[derive(Clone)]
struct ServerState {
    started_at_ms: i64,
    port: u16,
    auth_registry: DaemonTokenRegistryHandle,
    lifecycle: DaemonLifecycleHandle,
    dashboard_rx: watch::Receiver<DashboardState>,
    telegram_acl: TelegramAclHandle,
    events: EventStore,
    pending_work: PendingWorkQueue,
    dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    connected_clients: Arc<std::sync::atomic::AtomicUsize>,
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
    pub events: EventStore,
    pub pending_work: PendingWorkQueue,
    pub dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    pub daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
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
        events,
        pending_work,
        dashboard_control_tx,
        daemon_control_tx,
        shutdown_rx,
    } = params;

    let listener = TcpListener::bind((LOCALHOST, port))
        .await
        .map_err(|err| miette!("bind daemon listener failed: {err}"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|err| miette!("read daemon listener address failed: {err}"))?;
    let started_at_ms = chrono::Utc::now().timestamp_millis();

    let app_state = ServerState {
        started_at_ms,
        port: local_addr.port(),
        auth_registry,
        lifecycle,
        dashboard_rx,
        telegram_acl,
        events,
        pending_work,
        dashboard_control_tx,
        daemon_control_tx,
        connected_clients: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };

    let router = Router::new()
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/dashboard/snapshot", get(snapshot_handler))
        .route("/dashboard/stream", get(stream_handler))
        .route("/commands/run", post(command_handler))
        .route("/daemon/shutdown", post(shutdown_handler))
        .with_state(app_state.clone());

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

async fn health_handler() -> impl IntoResponse {
    Json(HealthResponse { ok: true })
}

async fn status_handler(State(state): State<ServerState>) -> impl IntoResponse {
    Json(StatusResponse {
        pid: std::process::id(),
        started_at_ms: state.started_at_ms,
        version: env!("CARGO_PKG_VERSION").to_string(),
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

async fn stream_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
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

fn runtime_not_ready_response(state: DaemonLifecycleState) -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!("daemon is {state}; runtime commands are accepted only when ready"),
    )
        .into_response()
}

async fn dashboard_ws(mut socket: WebSocket, state: ServerState) {
    state
        .connected_clients
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = state.dashboard_rx.clone();
    while rx.changed().await.is_ok() {
        let snapshot = rx.borrow().clone();
        match serde_json::to_string(&snapshot) {
            Ok(payload) => {
                if socket.send(Message::Text(payload.into())).await.is_err() {
                    break;
                }
            }
            Err(err) => {
                tracing::error!("encode dashboard ws snapshot failed: {err}");
                break;
            }
        }
    }
    state
        .connected_clients
        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
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
            http: reqwest::Client::new(),
            auth_token: None,
        }
    }

    pub async fn authenticated(port: u16) -> Result<Self> {
        Ok(Self {
            port,
            http: reqwest::Client::new(),
            auth_token: Some(load_daemon_auth_token().await?),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    fn base_url(&self) -> String {
        format!("http://{}:{}", LOCALHOST, self.port)
    }

    fn ws_url(&self) -> String {
        format!("ws://{}:{}/dashboard/stream", LOCALHOST, self.port)
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
        LOCALHOST,
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
        LOCALHOST,
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
