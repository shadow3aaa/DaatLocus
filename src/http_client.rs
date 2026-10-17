//! Shared HTTP defaults keep TLS on the same ring provider as device transports.
pub fn builder() -> reqwest::ClientBuilder {
    static TLS: std::sync::Once = std::sync::Once::new();
    TLS.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::Client::builder()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn https_keeps_certificate_and_hostname_verification_with_ring() {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let certificate = certified.cert.der().clone();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let mut accepted = 0;
            for _ in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                if let Ok(mut stream) = acceptor.accept(stream).await {
                    let mut request = [0; 1024];
                    if stream.read(&mut request).await.unwrap_or(0) > 0 {
                        stream
                            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                            .await
                            .unwrap();
                        accepted += 1;
                    }
                }
            }
            accepted
        });
        let trusted = || {
            builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .add_root_certificate(reqwest::Certificate::from_der(certificate.as_ref()).unwrap())
        };
        let untrusted = builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        assert!(
            untrusted
                .get(format!("https://localhost:{}", address.port()))
                .send()
                .await
                .is_err()
        );
        let response = trusted()
            .build()
            .unwrap()
            .get(format!("https://localhost:{}", address.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        assert!(
            trusted()
                .resolve("wrong.example", address)
                .build()
                .unwrap()
                .get(format!("https://wrong.example:{}", address.port()))
                .send()
                .await
                .is_err()
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap(),
            1
        );
    }
}
