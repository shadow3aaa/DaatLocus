//! Public rendezvous verifies public CA chains and hostnames with the OS verifier.
//! Peer TLS keeps its separate identity pins.
use futures_util::{StreamExt, stream::FuturesUnordered};
use rustls_platform_verifier::BuilderVerifierExt;
use std::{collections::VecDeque, io, net::SocketAddr, sync::Arc, time::Duration};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{Error, error::TlsError, protocol::WebSocketConfig},
};

pub(crate) type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// The relay issues a 30-second challenge using its own clock. Allow bounded
/// clock skew here; the signed deadline is unchanged and the relay still checks
/// it against its original nonce/deadline, so this does not extend its lifetime.
pub(crate) fn valid_challenge(nonce: uuid::Uuid, expires_at_ms: i64, now: i64) -> bool {
    use super::super::identity::CLOCK_SKEW_MS;
    !nonce.is_nil()
        && expires_at_ms > now.saturating_sub(CLOCK_SKEW_MS)
        && expires_at_ms <= now.saturating_add(30_000 + CLOCK_SKEW_MS)
}

pub(crate) async fn connect(endpoint: &str, config: WebSocketConfig) -> Result<Socket, Error> {
    let connector = if endpoint.starts_with("wss://") {
        let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| Error::Tls(TlsError::Rustls(Box::new(error))))?;
        let builder = builder
            .with_platform_verifier()
            .map_err(|error| Error::Tls(TlsError::Rustls(Box::new(error))))?;
        let tls = builder.with_no_client_auth();
        Some(Connector::Rustls(Arc::new(tls)))
    } else {
        None // Callers allow plaintext only for explicit local test fixtures.
    };
    let url = url::Url::parse(endpoint)
        .map_err(|error| Error::Io(io::Error::new(io::ErrorKind::InvalidInput, error)))?;
    let host = match url.host() {
        Some(url::Host::Domain(host)) => host.to_owned(),
        Some(url::Host::Ipv4(ip)) => ip.to_string(),
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        None => {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Missing WebSocket host",
            )));
        }
    };
    let port = url
        .port()
        .unwrap_or(if url.scheme() == "wss" { 443 } else { 80 });
    let addresses = tokio::net::lookup_host((host.as_str(), port)).await?;
    connect_addresses(endpoint, config, connector, addresses.collect()).await
}

/// Race complete TLS/WebSocket handshakes, not only TCP connects: a reachable
/// address can still stall during TLS. Dropping the losing futures closes them.
async fn connect_addresses(
    endpoint: &str,
    config: WebSocketConfig,
    connector: Option<Connector>,
    addresses: Vec<SocketAddr>,
) -> Result<Socket, Error> {
    let first_ipv6 = addresses.first().is_some_and(SocketAddr::is_ipv6);
    let mut families = [VecDeque::new(), VecDeque::new()];
    for address in addresses {
        let family = usize::from(address.is_ipv6() != first_ipv6);
        if families[family].len() < 4 && !families[family].contains(&address) {
            families[family].push_back(address);
        }
    }
    let mut ordered = Vec::new();
    while !families[0].is_empty() || !families[1].is_empty() {
        for family in &mut families {
            if let Some(address) = family.pop_front() {
                ordered.push(address);
            }
        }
    }
    let started = tokio::time::Instant::now();
    let mut attempts = FuturesUnordered::new();
    for (index, address) in ordered.into_iter().enumerate() {
        let connector = connector.clone();
        attempts.push(async move {
            tokio::time::sleep_until(started + Duration::from_millis(250 * index as u64)).await;
            let stream = tokio::net::TcpStream::connect(address).await?;
            stream.set_nodelay(true)?;
            tokio_tungstenite::client_async_tls_with_config(
                endpoint,
                stream,
                Some(config),
                connector,
            )
            .await
            .map(|(socket, _)| socket)
        });
    }
    let mut failure = Error::Io(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "No WebSocket addresses",
    ));
    while let Some(result) = attempts.next().await {
        match result {
            Ok(socket) => return Ok(socket),
            Err(error) => {
                let category = match &error {
                    Error::Io(_) => "tcp_or_io",
                    Error::Tls(_) => "tls",
                    Error::Http(_) => "http_upgrade",
                    _ => "websocket_protocol",
                };
                tracing::debug!(category, "Public WebSocket address attempt failed");
                failure = error;
            }
        }
    }
    Err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn stalled_handshake_does_not_block_another_resolved_address() {
        let stalled = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let healthy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addresses = vec![stalled.local_addr().unwrap(), healthy.local_addr().unwrap()];
        let (accepted, observed) = tokio::sync::oneshot::channel();
        let stalled_server = tokio::spawn(async move {
            let (_stream, _) = stalled.accept().await.unwrap();
            accepted.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let healthy_server = tokio::spawn(async move {
            let (stream, _) = healthy.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text("reachable".into()))
                .await
                .unwrap();
        });
        let connection = tokio::time::timeout(
            Duration::from_secs(5),
            connect_addresses(
                "ws://localhost/fixture",
                WebSocketConfig::default(),
                None,
                addresses,
            ),
        )
        .await;
        stalled_server.abort();
        let _ = stalled_server.await;
        let mut socket = connection
            .expect("a stalled address blocked the healthy address")
            .unwrap();
        observed.await.unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text("reachable".into())
        );
        healthy_server.await.unwrap();
    }

    #[test]
    fn relay_challenge_tolerates_bounded_clock_skew() {
        let nonce = uuid::Uuid::new_v4();
        let server_now = 100_000;
        let deadline = server_now + 30_000;
        for client_now in [server_now - 1_000, server_now, server_now + 1_000] {
            assert!(valid_challenge(nonce, deadline, client_now));
        }
        assert!(!valid_challenge(uuid::Uuid::nil(), deadline, server_now));
        assert!(!valid_challenge(nonce, server_now - 30_000, server_now));
        assert!(!valid_challenge(nonce, server_now + 60_001, server_now));
    }

    #[tokio::test]
    async fn local_fixture_keeps_the_websocket_protocol() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text("challenge".into()))
                .await
                .unwrap();
            socket.next().await.unwrap().unwrap()
        });
        let mut socket = connect(&endpoint, WebSocketConfig::default())
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Text("challenge".into())
        );
        socket.send(Message::Text("response".into())).await.unwrap();
        assert_eq!(server.await.unwrap(), Message::Text("response".into()));
    }

    #[tokio::test]
    async fn public_connection_rejects_untrusted_certificates() {
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("wss://localhost:{}", listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_rustls::TlsAcceptor::from(Arc::new(tls))
                .accept(stream)
                .await
                .is_err()
        });
        let connection = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connect(&endpoint, WebSocketConfig::default()),
        )
        .await
        .unwrap();
        assert!(
            connection.is_err(),
            "Untrusted certificates must never open a public channel"
        );
        assert!(server.await.unwrap());
    }
}
