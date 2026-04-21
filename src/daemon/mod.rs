use std::{
    net::TcpListener as StdTcpListener,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
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
    daat_locus_paths::daat_locus_paths,
    dashboard::{
        DashboardCommandRunner, DashboardControlCommand, DashboardState, execute_remote_command,
    },
    events::EventStore,
    pending_work::PendingWorkQueue,
    telegram_acl::TelegramAclHandle,
};

const LOCALHOST: &str = "127.0.0.1";
const AUTH_HEADER: &str = "x-daat-locus-token";
const START_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonMetadata {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub started_at_ms: i64,
    pub version: String,
}

impl DaemonMetadata {
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn ws_url(&self) -> String {
        format!(
            "ws://{}:{}/dashboard/stream?token={}",
            self.host, self.port, self.token
        )
    }
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub pid: u32,
    pub started_at_ms: i64,
    pub version: String,
    pub host: String,
    pub port: u16,
    pub connected_clients: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResponse {
    pub output: String,
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[derive(Debug)]
pub enum DaemonControlCommand {
    ShutdownRequested,
}

#[derive(Clone)]
struct ServerState {
    metadata: Arc<DaemonMetadata>,
    dashboard_rx: watch::Receiver<DashboardState>,
    telegram_acl: TelegramAclHandle,
    events: EventStore,
    pending_work: PendingWorkQueue,
    dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    connected_clients: Arc<std::sync::atomic::AtomicUsize>,
}

pub struct DaemonServerHandle {
    pub metadata: DaemonMetadata,
    join: JoinHandle<()>,
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

pub async fn read_metadata() -> Result<DaemonMetadata> {
    let path = daat_locus_paths().await.daemon_metadata_file();
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|err| miette!("read daemon metadata {} failed: {err}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| miette!("decode daemon metadata {} failed: {err}", path.display()))
}

pub async fn write_metadata(metadata: &DaemonMetadata) -> Result<()> {
    let path = daat_locus_paths().await.daemon_metadata_file();
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(metadata)
        .map_err(|err| miette!("encode daemon metadata failed: {err}"))?;
    tokio::fs::write(&temp, bytes)
        .await
        .map_err(|err| miette!("write daemon metadata temp failed: {err}"))?;
    tokio::fs::rename(&temp, &path)
        .await
        .map_err(|err| miette!("replace daemon metadata failed: {err}"))?;
    Ok(())
}

pub async fn clear_metadata() {
    let path = daat_locus_paths().await.daemon_metadata_file();
    let _ = tokio::fs::remove_file(path).await;
}

fn random_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub async fn start_server(
    dashboard_rx: watch::Receiver<DashboardState>,
    telegram_acl: TelegramAclHandle,
    events: EventStore,
    pending_work: PendingWorkQueue,
    dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<DaemonServerHandle> {
    let std_listener = StdTcpListener::bind((LOCALHOST, 0))
        .map_err(|err| miette!("bind daemon listener failed: {err}"))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|err| miette!("set_nonblocking on daemon listener failed: {err}"))?;
    let local_addr = std_listener
        .local_addr()
        .map_err(|err| miette!("read daemon listener address failed: {err}"))?;
    let listener = TcpListener::from_std(std_listener)
        .map_err(|err| miette!("convert daemon listener failed: {err}"))?;

    let metadata = DaemonMetadata {
        pid: std::process::id(),
        host: LOCALHOST.to_string(),
        port: local_addr.port(),
        token: random_token(),
        started_at_ms: chrono::Utc::now().timestamp_millis(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    write_metadata(&metadata).await?;

    let app_state = ServerState {
        metadata: Arc::new(metadata.clone()),
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

    Ok(DaemonServerHandle { metadata, join })
}

async fn health_handler() -> impl IntoResponse {
    Json(HealthResponse { ok: true })
}

async fn status_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    query: Query<TokenQuery>,
) -> impl IntoResponse {
    if !authorized(&headers, query.token.as_deref(), &state.metadata.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(StatusResponse {
        pid: state.metadata.pid,
        started_at_ms: state.metadata.started_at_ms,
        version: state.metadata.version.clone(),
        host: state.metadata.host.clone(),
        port: state.metadata.port,
        connected_clients: state
            .connected_clients
            .load(std::sync::atomic::Ordering::Relaxed),
    })
    .into_response()
}

async fn snapshot_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    query: Query<TokenQuery>,
) -> impl IntoResponse {
    if !authorized(&headers, query.token.as_deref(), &state.metadata.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(state.dashboard_rx.borrow().clone()).into_response()
}

async fn stream_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
    headers: HeaderMap,
    query: Query<TokenQuery>,
) -> impl IntoResponse {
    if !authorized(&headers, query.token.as_deref(), &state.metadata.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| dashboard_ws(socket, state))
}

async fn command_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    query: Query<TokenQuery>,
    Json(request): Json<CommandRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, query.token.as_deref(), &state.metadata.token) {
        return StatusCode::UNAUTHORIZED.into_response();
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
    query: Query<TokenQuery>,
) -> impl IntoResponse {
    if !authorized(&headers, query.token.as_deref(), &state.metadata.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let _ = state
        .daemon_control_tx
        .send(DaemonControlCommand::ShutdownRequested);
    StatusCode::ACCEPTED.into_response()
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

fn authorized(headers: &HeaderMap, query_token: Option<&str>, expected: &str) -> bool {
    if let Some(value) = headers
        .get(AUTH_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        return value == expected;
    }
    query_token.is_some_and(|value| value == expected)
}

pub struct DaemonClient {
    metadata: DaemonMetadata,
    http: reqwest::Client,
}

impl DaemonClient {
    pub fn new(metadata: DaemonMetadata) -> Self {
        Self {
            metadata,
            http: reqwest::Client::new(),
        }
    }

    pub fn metadata(&self) -> &DaemonMetadata {
        &self.metadata
    }

    pub async fn health(&self) -> Result<()> {
        self.http
            .get(format!("{}/health", self.metadata.base_url()))
            .send()
            .await
            .map_err(|err| miette!("daemon health request failed: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("daemon health returned error: {err}"))?;
        Ok(())
    }

    pub async fn snapshot(&self) -> Result<DashboardState> {
        self.http
            .get(format!("{}/dashboard/snapshot", self.metadata.base_url()))
            .header(AUTH_HEADER, &self.metadata.token)
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
            .http
            .post(format!("{}/commands/run", self.metadata.base_url()))
            .header(AUTH_HEADER, &self.metadata.token)
            .json(&CommandRequest {
                command: command.to_string(),
            })
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
        self.http
            .post(format!("{}/daemon/shutdown", self.metadata.base_url()))
            .header(AUTH_HEADER, &self.metadata.token)
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
        let mut request = self
            .metadata
            .ws_url()
            .into_client_request()
            .map_err(|err| miette!("build dashboard ws request failed: {err}"))?;
        request
            .headers_mut()
            .insert(AUTH_HEADER, self.metadata.token.parse().unwrap());

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

pub async fn wait_for_daemon_ready() -> Result<DaemonMetadata> {
    let deadline = Instant::now() + START_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        match read_metadata().await {
            Ok(metadata) => {
                let client = DaemonClient::new(metadata.clone());
                match client.health().await {
                    Ok(()) => return Ok(metadata),
                    Err(err) => last_error = Some(err.to_string()),
                }
            }
            Err(err) => last_error = Some(err.to_string()),
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
    Err(miette!(
        "daemon did not become ready within {}s{}",
        START_TIMEOUT.as_secs(),
        last_error
            .as_deref()
            .map(|err| format!(": {err}"))
            .unwrap_or_default()
    ))
}

pub async fn wait_for_daemon_shutdown(pid: u32) -> Result<()> {
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return Ok(());
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
    Err(miette!(
        "daemon pid={} did not shut down within {}s",
        pid,
        START_TIMEOUT.as_secs()
    ))
}

pub async fn spawn_detached_daemon_process() -> Result<()> {
    let current_exe = std::env::current_exe()
        .map_err(|err| miette!("resolve current executable failed: {err}"))?;

    // 将 stderr 重定向到日志文件，方便排查 daemon 启动失败的原因。
    // stdout 仍然丢弃（emit_startup_progress 的 println! 已由 tracing 记录到文件）。
    let log_path = daat_locus_paths().await.logs_file("daemon-stderr.log");
    let stderr_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|err| miette!("open daemon stderr log {}: {err}", log_path.display()))?;

    std::process::Command::new(current_exe)
        .arg("daemon")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_file)
        .spawn()
        .map_err(|err| miette!("spawn daemon process failed: {err}"))?;
    Ok(())
}

pub async fn connect_existing_daemon() -> Result<DaemonClient> {
    let metadata = read_metadata().await?;
    if !process_exists(metadata.pid) {
        // pid 已不存在：daemon 非正常退出，留下了 stale metadata。
        tracing::info!(
            pid = metadata.pid,
            "daemon metadata found but pid is gone; clearing stale metadata"
        );
        clear_metadata().await;
        return Err(miette!("daemon pid={} is no longer running", metadata.pid));
    }
    let client = DaemonClient::new(metadata);
    client.health().await?;
    Ok(client)
}

pub async fn connect_or_start_daemon() -> Result<DaemonClient> {
    match connect_existing_daemon().await {
        Ok(client) => Ok(client),
        Err(_) => {
            spawn_detached_daemon_process().await?;
            let metadata = wait_for_daemon_ready().await?;
            let client = DaemonClient::new(metadata);
            client.health().await?;
            Ok(client)
        }
    }
}

pub fn status_summary(metadata: &DaemonMetadata) -> String {
    format!(
        "daemon pid={} {}:{} started_at_ms={} version={}",
        metadata.pid, metadata.host, metadata.port, metadata.started_at_ms, metadata.version
    )
}
