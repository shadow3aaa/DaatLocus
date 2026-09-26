//! Temporary-session-share tunnel.
//!
//! `Tunnel::start` encapsulates the whole "connect to the Cloudflare edge →
//! register a quick tunnel → get a `*.trycloudflare.com` hostname → forward
//! public traffic to `127.0.0.1:<port>`" flow, transcribed from the open-source
//! `cloudflared` CLI. There is deliberately no user-visible control surface: no
//! subcommand, no dashboard action, no config file. The only entry point is
//! [`Tunnel::start`], and the only teardown is [`TunnelHandle::shutdown`].
//!
//! The tunnel runs as a tokio task inside the Manager process and must never
//! panic: every fallible step returns a [`TunnelError`].

pub mod config;
pub mod edge;
pub mod error;
pub mod listener;
pub mod mux;
pub mod register;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::{sync::watch, task::JoinHandle};

pub use config::TunnelConfig;
use config::TunnelMode;
use edge::EdgeAddress;
pub use error::TunnelError;

const USER_AGENT: &str = concat!("daat-locus/", env!("CARGO_PKG_VERSION"));
const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_millis(500);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
const MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Stateless entry point for the tunnel module.
pub struct Tunnel;

/// A running tunnel. Dropping the handle without calling [`Self::shutdown`]
/// leaves the supervisor task running until its owning runtime shuts down.
#[derive(Debug)]
pub struct TunnelHandle {
    hostname: String,
    shutdown_tx: watch::Sender<bool>,
    supervisor: JoinHandle<()>,
}

impl TunnelHandle {
    /// The assigned `*.trycloudflare.com` hostname.
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    /// Tear the tunnel down and wait for the supervisor to stop.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.supervisor.await;
    }
}

impl Tunnel {
    /// Register a quick tunnel and start forwarding to `config.target`.
    ///
    /// Fails closed: if no hostname can be obtained, no share is created.
    pub async fn start(config: TunnelConfig) -> Result<TunnelHandle, TunnelError> {
        // Only quick tunnels exist today; this exhaustive read keeps the mode
        // field load-bearing and fails to compile when a new mode is added.
        let TunnelMode::Quick = config.mode;

        if !config.enabled {
            return Err(TunnelError::TunnelDisabled(
                "tunnelling is disabled for this build".to_string(),
            ));
        }

        if config.edge.addresses.is_empty() {
            return Err(TunnelError::NoColoAvailable(
                "no edge addresses configured".to_string(),
            ));
        }

        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|err| TunnelError::Internal(format!("build http client failed: {err}")))?;

        let credentials = register::register_quick_tunnel(&client).await?;

        let endpoint = Arc::new(make_endpoint(&config)?);
        // Establish one connection eagerly so a broken edge surfaces as a
        // typed failure instead of a silently dead share.
        let (first, _address) = connect_any(&endpoint, &config).await?;
        register_control_stream(&first, &credentials.tunnel_id).await?;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let supervisor = tokio::spawn(run_supervisor(endpoint, config.clone(), shutdown_rx));

        Ok(TunnelHandle {
            hostname: credentials.hostname,
            shutdown_tx,
            supervisor,
        })
    }
}

/// Send the framed `registerTunnel` control message on a fresh bidirectional
/// stream and wait for the edge's framed acknowledgement.
///
/// cloudflared associates a QUIC connection with the tunnel it is proxying for
/// by registering over the connection's first stream; the framing keeps the
/// control plane parse bounded.
async fn register_control_stream(
    connection: &quinn::Connection,
    tunnel_id: &str,
) -> Result<(), TunnelError> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| TunnelError::Internal(format!("open control stream: {err}")))?;

    let payload = serde_json::json!({ "type": "registerTunnel", "tunnelId": tunnel_id });
    let payload = serde_json::to_vec(&payload)
        .map_err(|err| TunnelError::Internal(format!("encode control message: {err}")))?;
    mux::send_frame(&mut send, &payload).await?;

    let _ack = tokio::time::timeout(Duration::from_secs(10), mux::read_frame(&mut recv))
        .await
        .map_err(|_| {
            TunnelError::ConnectTimeout(
                "edge did not acknowledge the tunnel registration".to_string(),
            )
        })??;
    let _ = send.finish();
    Ok(())
}

async fn run_supervisor(
    endpoint: Arc<quinn::Endpoint>,
    config: TunnelConfig,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut workers = Vec::with_capacity(config.connection_count);
    for _ in 0..config.connection_count {
        workers.push(tokio::spawn(maintain_connection(
            endpoint.clone(),
            config.clone(),
            shutdown_rx.clone(),
        )));
    }

    let _ = shutdown_rx.changed().await;
    for worker in workers {
        worker.abort();
    }
    endpoint.close(0u32.into(), b"shutdown");
}

async fn maintain_connection(
    endpoint: Arc<quinn::Endpoint>,
    config: TunnelConfig,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut backoff = INITIAL_RECONNECT_BACKOFF;
    loop {
        if *shutdown_rx.borrow() {
            return;
        }

        match connect_any(&endpoint, &config).await {
            Ok((connection, _address)) => {
                backoff = INITIAL_RECONNECT_BACKOFF;
                tokio::select! {
                    _ = listener::serve_connection(connection.clone(), config.target) => {}
                    _ = shutdown_rx.changed() => {
                        connection.close(0u32.into(), b"shutdown");
                        return;
                    }
                }
                connection.close(0u32.into(), b"reconnect");
            }
            Err(error) => {
                tracing::debug!("tunnel edge connection failed: {error}");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown_rx.changed() => return,
                }
                backoff = (backoff * 2).min(config.reconnect_backoff_cap);
            }
        }
    }
}

async fn connect_any(
    endpoint: &quinn::Endpoint,
    config: &TunnelConfig,
) -> Result<(quinn::Connection, EdgeAddress), TunnelError> {
    let mut last_error: Option<TunnelError> = None;

    for address in &config.edge.addresses {
        let resolved = match address.resolve() {
            Ok(resolved) => resolved,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        let server_name = config
            .edge
            .server_name
            .clone()
            .unwrap_or_else(|| address.server_name().to_string());

        for socket in resolved {
            let connecting = match endpoint.connect(socket, &server_name) {
                Ok(connecting) => connecting,
                Err(error) => {
                    last_error = Some(TunnelError::Internal(format!(
                        "start QUIC connection to {socket}: {error}"
                    )));
                    continue;
                }
            };

            match tokio::time::timeout(config.connect_timeout, connecting).await {
                Ok(Ok(connection)) => return Ok((connection, address.clone())),
                Ok(Err(error)) => {
                    last_error = Some(map_connection_error(error));
                }
                Err(_) => {
                    last_error = Some(TunnelError::ConnectTimeout(format!(
                        "QUIC handshake to {socket} timed out"
                    )));
                }
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| TunnelError::NoColoAvailable("no reachable edge address".to_string())))
}

fn map_connection_error(error: quinn::ConnectionError) -> TunnelError {
    match error {
        quinn::ConnectionError::TimedOut => {
            TunnelError::ConnectTimeout("QUIC handshake timed out".to_string())
        }
        quinn::ConnectionError::TransportError(ref transport) => {
            // `crypto_error` variants are surfaced by the transport as a TLS
            // alert during the handshake.
            TunnelError::TlsFailed(format!("edge transport error: {transport}"))
        }
        other => TunnelError::Internal(format!("QUIC connection failed: {other}")),
    }
}

fn make_endpoint(config: &TunnelConfig) -> Result<quinn::Endpoint, TunnelError> {
    let mut roots = quinn::rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let provider = Arc::new(quinn::rustls::crypto::ring::default_provider());
    let mut tls = quinn::rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&quinn::rustls::version::TLS13])
        .map_err(|err| TunnelError::TlsFailed(format!("build tls client config: {err}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    if !config.edge.alpn.is_empty() {
        tls.alpn_protocols = config.edge.alpn.clone();
    }

    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(Arc::new(tls))
        .map_err(|err| TunnelError::TlsFailed(format!("build QUIC client config: {err}")))?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic));

    let idle_timeout = quinn::IdleTimeout::try_from(MAX_IDLE_TIMEOUT)
        .map_err(|err| TunnelError::Internal(format!("idle timeout out of range: {err}")))?;
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(idle_timeout));
    transport.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    client_config.transport_config(Arc::new(transport));

    let bind: SocketAddr = "0.0.0.0:0"
        .parse()
        .map_err(|err| TunnelError::Internal(format!("parse bind address: {err}")))?;
    let mut endpoint = quinn::Endpoint::client(bind)
        .map_err(|err| TunnelError::Internal(format!("bind tunnel udp socket: {err}")))?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, str::FromStr, time::Duration};

    use super::{INITIAL_RECONNECT_BACKOFF, MAX_IDLE_TIMEOUT, TunnelConfig, config};
    use crate::tunnel::error::TunnelErrorCode;

    #[test]
    fn start_without_edge_addresses_fails_closed() {
        let target = SocketAddr::from_str("127.0.0.1:53825").expect("addr");
        let mut config = TunnelConfig::quick(target);
        config.edge.addresses.clear();

        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(super::Tunnel::start(config))
            .expect_err("must fail without edge addresses");
        assert_eq!(error.code(), TunnelErrorCode::NoColoAvailable);
    }

    #[test]
    fn reconnect_backoff_doubles_up_to_the_cap() {
        let cap = Duration::from_secs(4);
        let mut backoff = INITIAL_RECONNECT_BACKOFF;
        let mut observed = Vec::new();
        for _ in 0..6 {
            observed.push(backoff);
            backoff = (backoff * 2).min(cap);
        }
        assert_eq!(observed[0], INITIAL_RECONNECT_BACKOFF);
        assert_eq!(*observed.last().expect("non-empty"), cap);
    }

    #[test]
    fn idle_timeout_is_representable() {
        assert!(quinn::IdleTimeout::try_from(MAX_IDLE_TIMEOUT).is_ok());
        assert_eq!(config::DEFAULT_TUNNEL_CONNECTIONS, 4);
    }
}
