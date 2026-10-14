//! Public rendezvous verifies public CA chains and hostnames. Android uses
//! WebPKI because the JVM verifier's additional OCSP check rejects certificates
//! without an OCSP responder as revoked. Other platforms use the OS verifier.
//! Peer TLS keeps its separate identity pins.
#[cfg(not(target_os = "android"))]
use rustls_platform_verifier::BuilderVerifierExt;
use std::sync::Arc;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{Error, error::TlsError, protocol::WebSocketConfig},
};

pub(crate) type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// The relay issues a 30-second challenge using its own clock. Allow bounded
/// clock skew here; the signed deadline is unchanged and the relay still checks
/// it against its original nonce/deadline, so this does not extend its lifetime.
pub(crate) fn valid_challenge(nonce: uuid::Uuid, expires_at_ms: i64, now: i64) -> bool {
    const CLOCK_SKEW_MS: i64 = 30_000;
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
        #[cfg(target_os = "android")]
        let builder = builder.with_root_certificates(rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        });
        #[cfg(not(target_os = "android"))]
        let builder = builder
            .with_platform_verifier()
            .map_err(|error| Error::Tls(TlsError::Rustls(Box::new(error))))?;
        let tls = builder.with_no_client_auth();
        Some(Connector::Rustls(Arc::new(tls)))
    } else {
        None // Callers allow plaintext only for explicit local test fixtures.
    };
    let (socket, _) =
        tokio_tungstenite::connect_async_tls_with_config(endpoint, Some(config), true, connector)
            .await?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

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
