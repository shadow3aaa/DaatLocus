use std::{
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
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

const LOCALHOST: &str = "127.0.0.1";
const START_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);

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

#[derive(Debug)]
pub enum DaemonControlCommand {
    ShutdownRequested { completion_tx: oneshot::Sender<()> },
}

#[derive(Clone)]
struct ServerState {
    started_at_ms: i64,
    port: u16,
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

pub async fn start_server(
    port: u16,
    dashboard_rx: watch::Receiver<DashboardState>,
    telegram_acl: TelegramAclHandle,
    events: EventStore,
    pending_work: PendingWorkQueue,
    dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<DaemonServerHandle> {
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
        connected_clients: state
            .connected_clients
            .load(std::sync::atomic::Ordering::Relaxed),
    })
    .into_response()
}

async fn snapshot_handler(State(state): State<ServerState>) -> impl IntoResponse {
    Json(state.dashboard_rx.borrow().clone()).into_response()
}

async fn stream_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| dashboard_ws(socket, state))
}

async fn command_handler(
    State(state): State<ServerState>,
    Json(request): Json<CommandRequest>,
) -> impl IntoResponse {
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

async fn shutdown_handler(State(state): State<ServerState>) -> impl IntoResponse {
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

pub struct DaemonClient {
    port: u16,
    http: reqwest::Client,
}

impl DaemonClient {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            http: reqwest::Client::new(),
        }
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
        self.http
            .get(format!("{}/dashboard/snapshot", self.base_url()))
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
            .post(format!("{}/commands/run", self.base_url()))
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
            .post(format!("{}/daemon/shutdown", self.base_url()))
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
        let request = self
            .ws_url()
            .into_client_request()
            .map_err(|err| miette!("build dashboard ws request failed: {err}"))?;

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
    let deadline = Instant::now() + START_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        match client.status().await {
            Ok(status) => return Ok(status),
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

pub async fn wait_for_daemon_shutdown(port: u16) -> Result<()> {
    let client = DaemonClient::new(port);
    let deadline = Instant::now() + START_TIMEOUT;
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
    let port = configured_daemon_port().await?;
    let client = DaemonClient::new(port);
    client.status().await?;
    Ok(client)
}

pub async fn connect_or_start_daemon() -> Result<DaemonClient> {
    match connect_existing_daemon().await {
        Ok(client) => Ok(client),
        Err(_) => {
            spawn_detached_daemon_process().await?;
            let status = wait_for_daemon_ready().await?;
            let client = DaemonClient::new(status.port);
            Ok(client)
        }
    }
}

pub fn status_summary(status: &StatusResponse) -> String {
    format!(
        "daemon pid={} {}:{} started_at_ms={} version={} connected_clients={}",
        status.pid,
        LOCALHOST,
        status.port,
        status.started_at_ms,
        status.version,
        status.connected_clients,
    )
}
