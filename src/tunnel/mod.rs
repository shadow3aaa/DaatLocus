//! Temporary-session-share tunnel.
//!
//! `Tunnel::start` encapsulates the whole "register a Cloudflare quick tunnel
//! -> get a `*.trycloudflare.com` hostname -> forward public traffic to
//! `127.0.0.1:<port>`" flow. The edge protocol (QUIC + Cap'n Proto-RPC
//! `RegisterConnection`, SRV/DoT edge discovery, the Cloudflare-internal trust
//! roots, HTTP/1.1 + WebSocket proxying) is provided by the
//! `cloudflare-quick-tunnel` crate; this module is the thin in-process wrapper
//! that owns the lifecycle and maps failures onto the share API's error codes.
//!
//! There is deliberately no user-visible control surface: no subcommand, no
//! dashboard action, no config file. The only entry point is [`Tunnel::start`]
//! and the only teardown is [`TunnelHandle::shutdown`].
//!
//! The tunnel runs as a tokio task inside the Manager process and must never
//! panic: every fallible step returns a [`TunnelError`].

pub mod config;
pub mod error;

use std::sync::Arc;
use std::time::Duration;

use cloudflare_quick_tunnel::api::{self, request_tunnel};
use cloudflare_quick_tunnel::edge::{self, IpVersionFilter};
use cloudflare_quick_tunnel::pool::Pool;
use cloudflare_quick_tunnel::quic_dial::{build_endpoint, dial_any};
use cloudflare_quick_tunnel::rpc::{
    ConnectionOptions, ControlSession, TunnelAuth, register_connection,
};
use cloudflare_quick_tunnel::supervisor::{self, SupervisorExit, SupervisorMetrics};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

pub use config::TunnelConfig;
pub use error::TunnelError;

/// Backoff floor between reconnect attempts.
const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
/// Backoff ceiling between reconnect attempts.
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);
/// Ceiling on how many discovered edges a single dial fans out to.
const MAX_DIAL_EDGES: usize = 5;
/// Bound on how long `shutdown` waits for the supervisor to stop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(15);

/// Stateless entry point for the tunnel module.
pub struct Tunnel;

/// A running quick tunnel.
pub struct TunnelHandle {
    hostname: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl TunnelHandle {
    /// The assigned `*.trycloudflare.com` hostname (no scheme).
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    /// Tear the tunnel down and wait for the supervisor to stop.
    pub async fn shutdown(mut self) {
        self.stop().await;
    }

    async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, task).await;
        }
    }
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Tunnel {
    /// Register a quick tunnel and start forwarding to `config.target`.
    ///
    /// Fails closed: if no hostname can be obtained, no share is created.
    pub async fn start(config: TunnelConfig) -> Result<TunnelHandle, TunnelError> {
        if !config.enabled {
            return Err(TunnelError::TunnelDisabled(
                "tunnelling is disabled for this build".to_string(),
            ));
        }

        let tunnel = request_tunnel(api::DEFAULT_SERVICE_URL, api::DEFAULT_USER_AGENT)
            .await
            .map_err(map_error)?;
        let tunnel_id = Uuid::parse_str(&tunnel.id).map_err(|err| {
            TunnelError::Internal(format!("quick tunnel id is not a uuid: {err}"))
        })?;
        let hostname = normalize_hostname(&tunnel.hostname);
        let auth = TunnelAuth {
            account_tag: tunnel.account_tag.clone(),
            tunnel_secret: tunnel.secret.clone(),
        };

        let endpoint = build_endpoint().map_err(map_error)?;
        let (conn, control) = connect_and_register(&endpoint, &auth, tunnel_id, 0, false).await?;

        let metrics = SupervisorMetrics::default();
        let pool = Arc::new(Pool::new(config.target.port()));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let task = tokio::spawn(run_reactor(
            config.target.port(),
            endpoint,
            auth,
            tunnel_id,
            metrics,
            pool,
            conn,
            control,
            shutdown_rx,
        ));

        Ok(TunnelHandle {
            hostname,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        })
    }
}

async fn connect_and_register(
    endpoint: &quinn::Endpoint,
    auth: &TunnelAuth,
    tunnel_id: Uuid,
    conn_index: u8,
    replace_existing: bool,
) -> Result<(quinn::Connection, ControlSession), TunnelError> {
    let mut edges = edge::discover(IpVersionFilter::Auto)
        .await
        .map_err(map_error)?;
    if edges.is_empty() {
        return Err(TunnelError::NoColoAvailable(
            "edge discovery returned no addresses".to_string(),
        ));
    }
    edges.truncate(edges.len().min(MAX_DIAL_EDGES));

    let conn = dial_any(endpoint, &edges).await.map_err(map_error)?;
    let mut options = ConnectionOptions::default_for_quick_tunnel(api::DEFAULT_USER_AGENT);
    options.replace_existing = replace_existing;

    let (_details, control) = register_connection(&conn, auth, tunnel_id, conn_index, &options)
        .await
        .map_err(map_error)?;
    Ok((conn, control))
}

#[allow(clippy::too_many_arguments)]
async fn run_reactor(
    local_port: u16,
    endpoint: quinn::Endpoint,
    auth: TunnelAuth,
    tunnel_id: Uuid,
    metrics: SupervisorMetrics,
    pool: Arc<Pool>,
    conn: quinn::Connection,
    control: ControlSession,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut conn = conn;
    let mut control = Some(control);
    let mut backoff = INITIAL_RECONNECT_BACKOFF;

    loop {
        let (supervisor_shutdown_tx, supervisor_shutdown_rx) = oneshot::channel();
        let exit = tokio::select! {
            biased;
            _ = &mut shutdown_rx => {
                let _ = supervisor_shutdown_tx.send(());
                SupervisorExit::Shutdown
            }
            exit = supervisor::run(
                conn.clone(),
                local_port,
                metrics.clone(),
                pool.clone(),
                supervisor_shutdown_rx,
            ) => exit,
        };
        conn.close(0u32.into(), b"rotate");
        drop(control.take());

        if matches!(exit, SupervisorExit::Shutdown) {
            return;
        }

        // The edge dropped us: reconnect with exponential backoff until a new
        // registration succeeds or shutdown is requested.
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => return,
                _ = tokio::time::sleep(backoff) => {}
            }
            match connect_and_register(&endpoint, &auth, tunnel_id, 0, true).await {
                Ok((new_conn, new_control)) => {
                    conn = new_conn;
                    control = Some(new_control);
                    backoff = INITIAL_RECONNECT_BACKOFF;
                    break;
                }
                Err(error) => {
                    tracing::warn!("tunnel reconnect failed: {error}");
                    backoff = (backoff * 2).min(MAX_RECONNECT_BACKOFF);
                }
            }
        }
    }
}

/// Strip any scheme/trailing slash so callers always get a bare hostname.
fn normalize_hostname(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

/// Map the crate's error model onto the share API's stable codes.
fn map_error(error: cloudflare_quick_tunnel::TunnelError) -> TunnelError {
    use cloudflare_quick_tunnel::TunnelError as CrateError;
    match error {
        CrateError::Api(err) => {
            TunnelError::RegisterRejected(format!("quick tunnel request failed: {err}"))
        }
        CrateError::ApiBusiness(errors) => {
            TunnelError::RegisterRejected(format!("cloudflare rejected the tunnel: {errors:?}"))
        }
        CrateError::ApiNonJson {
            status,
            body_snippet,
        } => TunnelError::RegisterRejected(format!(
            "cloudflare returned HTTP {status}: {body_snippet}"
        )),
        CrateError::Discovery(message) => TunnelError::DnsFailed(message),
        CrateError::QuicDial { attempts, last } => {
            let message = format!("after {attempts} attempt(s): {last}");
            let lower = last.to_ascii_lowercase();
            if lower.contains("certificate") || lower.contains("tls") {
                TunnelError::TlsFailed(message)
            } else {
                TunnelError::ConnectTimeout(message)
            }
        }
        CrateError::Register(message) => TunnelError::RegisterRejected(message),
        CrateError::PermanentFailure(attempts) => {
            TunnelError::ConnectTimeout(format!("supervisor gave up after {attempts} attempts"))
        }
        CrateError::Shutdown => TunnelError::Internal("tunnel was shut down".to_string()),
        CrateError::Internal(message) => TunnelError::Internal(message),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_hostname;

    #[test]
    fn normalize_hostname_strips_scheme_and_slash() {
        assert_eq!(
            normalize_hostname("https://a.trycloudflare.com"),
            "a.trycloudflare.com"
        );
        assert_eq!(
            normalize_hostname("http://a.trycloudflare.com/"),
            "a.trycloudflare.com"
        );
        assert_eq!(
            normalize_hostname(" a.trycloudflare.com "),
            "a.trycloudflare.com"
        );
    }

    #[tokio::test]
    #[ignore = "requires live Cloudflare edge access"]
    async fn live_quick_tunnel_serves_local_http() {
        use std::net::SocketAddr;
        use std::str::FromStr;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // A tiny local HTTP server that answers any request with `ok`.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local server");
        let local = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let body = b"ok";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        let target = SocketAddr::from_str(&local.to_string()).expect("target addr");
        let handle = super::Tunnel::start(super::TunnelConfig::quick(target))
            .await
            .expect("tunnel start");
        let hostname = handle.hostname().to_string();
        println!("LIVE_URL=https://{hostname}");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("client");
        let mut served = false;
        for _ in 0..10 {
            if let Ok(response) = client.get(format!("https://{hostname}/")).send().await {
                if let Ok(text) = response.text().await {
                    if text.contains("ok") {
                        served = true;
                        break;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }

        handle.shutdown().await;
        server.abort();
        assert!(served, "tunnel did not serve the local response");
    }
}
