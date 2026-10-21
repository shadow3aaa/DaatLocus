//! TLS 1.3 with enrollment-pinned device certificates and native handshake signatures.
use crate::stage::{
    Result, StageError, StageId, digest,
    identity::{DeviceIdentity, DeviceProfile},
};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use std::{collections::HashSet, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Clone)]
pub struct VerifiedPeer {
    stage: StageId,
    certificate_hash: String,
}
impl VerifiedPeer {
    #[cfg(test)]
    pub(crate) fn test_peer(profile: &DeviceProfile) -> Self {
        Self {
            stage: profile.stage_id,
            certificate_hash: profile.tls_sha256.clone(),
        }
    }
    pub fn stage_id(&self) -> StageId {
        self.stage
    }
    pub fn certificate_hash(&self) -> &str {
        &self.certificate_hash
    }
}
pub fn identify_server<S>(
    stream: &tokio_rustls::client::TlsStream<S>,
    expected: &DeviceProfile,
) -> Result<VerifiedPeer> {
    let certificate = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .ok_or(StageError::Forbidden("Peer certificate unavailable"))?;
    let hash = digest(certificate.as_ref());
    if hash != expected.tls_sha256 {
        return Err(StageError::Forbidden("Peer certificate identity changed"));
    }
    Ok(VerifiedPeer {
        stage: expected.stage_id,
        certificate_hash: hash,
    })
}
pub fn identify_client<S>(
    stream: &tokio_rustls::server::TlsStream<S>,
    allowed: &[DeviceProfile],
) -> Result<VerifiedPeer> {
    let certificate = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .ok_or(StageError::Forbidden("Peer certificate unavailable"))?;
    let hash = digest(certificate.as_ref());
    let mut matches = allowed.iter().filter(|profile| profile.tls_sha256 == hash);
    let profile = matches
        .next()
        .ok_or(StageError::Forbidden("Peer certificate is not enrolled"))?;
    if matches.next().is_some() {
        return Err(StageError::Conflict(
            "Peer certificate has duplicate identities",
        ));
    }
    Ok(VerifiedPeer {
        stage: profile.stage_id,
        certificate_hash: hash,
    })
}

#[derive(Debug)]
struct Pins {
    hashes: HashSet<String>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}
fn rejected() -> Error {
    Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure)
}
impl Pins {
    fn check(&self, certificate: &CertificateDer<'_>) -> std::result::Result<(), Error> {
        if self.hashes.contains(&digest(certificate.as_ref())) {
            Ok(())
        } else {
            Err(rejected())
        }
    }
    fn verify12(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify13(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
}
impl ServerCertVerifier for Pins {
    fn verify_server_cert(
        &self,
        certificate: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, Error> {
        self.check(certificate)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        self.verify12(message, certificate, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        self.verify13(message, certificate, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
impl ClientCertVerifier for Pins {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        certificate: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> std::result::Result<ClientCertVerified, Error> {
        self.check(certificate)?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        self.verify12(message, certificate, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        self.verify13(message, certificate, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
fn key(identity: &DeviceIdentity) -> PrivateKeyDer<'static> {
    PrivatePkcs8KeyDer::from(identity.tls_private_key().to_vec()).into()
}
pub fn server(identity: &DeviceIdentity, allowed: &[DeviceProfile]) -> Result<TlsAcceptor> {
    if allowed.is_empty() {
        return Err(StageError::Invalid("TLS needs an authorized peer set"));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Pins {
        hashes: allowed
            .iter()
            .map(|profile| profile.tls_sha256.clone())
            .collect(),
        provider: provider.clone(),
    };
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| StageError::Unavailable("TLS 1.3 unavailable"))?
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(
            vec![CertificateDer::from(identity.tls_certificate().to_vec())],
            key(identity),
        )
        .map_err(|_| StageError::Invalid("Invalid Stage TLS identity"))?;
    config.alpn_protocols = vec![super::wire::ALPN.to_vec(), b"daat-locus-stage/1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}
pub fn client(identity: &DeviceIdentity, peer: &DeviceProfile) -> Result<TlsConnector> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Pins {
        hashes: HashSet::from([peer.tls_sha256.clone()]),
        provider: provider.clone(),
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| StageError::Unavailable("TLS 1.3 unavailable"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_client_auth_cert(
            vec![CertificateDer::from(identity.tls_certificate().to_vec())],
            key(identity),
        )
        .map_err(|_| StageError::Invalid("Invalid Stage TLS identity"))?;
    config.alpn_protocols = vec![super::wire::ALPN.to_vec(), b"daat-locus-stage/1".to_vec()];
    Ok(TlsConnector::from(Arc::new(config)))
}
pub fn server_name(peer: StageId) -> Result<ServerName<'static>> {
    ServerName::try_from(format!("{peer}.dl.invalid"))
        .map_err(|_| StageError::Invalid("Invalid Stage TLS name"))
}
pub async fn accept<S: AsyncRead + AsyncWrite + Unpin>(
    acceptor: &TlsAcceptor,
    stream: S,
) -> Result<tokio_rustls::server::TlsStream<S>> {
    let tls = tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(stream))
        .await
        .map_err(|_| StageError::Unavailable("Stage TLS handshake timed out"))??;
    if !matches!(
        tls.get_ref().1.alpn_protocol(),
        Some(super::wire::ALPN | b"daat-locus-stage/1")
    ) {
        return Err(StageError::Forbidden("Stage TLS protocol mismatch"));
    }
    Ok(tls)
}
pub async fn connect<S: AsyncRead + AsyncWrite + Unpin>(
    connector: &TlsConnector,
    peer: StageId,
    stream: S,
) -> Result<tokio_rustls::client::TlsStream<S>> {
    let tls = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        connector.connect(server_name(peer)?, stream),
    )
    .await
    .map_err(|_| StageError::Unavailable("Stage TLS handshake timed out"))??;
    if !matches!(
        tls.get_ref().1.alpn_protocol(),
        Some(super::wire::ALPN | b"daat-locus-stage/1")
    ) {
        return Err(StageError::Forbidden("Stage TLS protocol mismatch"));
    }
    Ok(tls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::protocol::{Body, Envelope};
    #[tokio::test]
    async fn stage_alpn_negotiates_compact_and_falls_back_for_either_legacy_side() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        for (legacy_server, legacy_client, expected) in [
            (false, false, super::super::wire::ALPN),
            (true, false, b"daat-locus-stage/1".as_slice()),
            (false, true, b"daat-locus-stage/1".as_slice()),
        ] {
            let acceptor = server(&a, &[b.profile()]).unwrap();
            let connector = client(&b, &a.profile()).unwrap();
            let mut server_config = acceptor.config().as_ref().clone();
            let mut client_config = connector.config().as_ref().clone();
            if legacy_server {
                server_config.alpn_protocols = vec![b"daat-locus-stage/1".to_vec()];
            }
            if legacy_client {
                client_config.alpn_protocols = vec![b"daat-locus-stage/1".to_vec()];
            }
            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let connector = TlsConnector::from(Arc::new(client_config));
            let (left, right) = tokio::io::duplex(65536);
            let (server, client) = tokio::join!(
                accept(&acceptor, left),
                connect(&connector, a.profile().stage_id, right)
            );
            assert_eq!(server.unwrap().get_ref().1.alpn_protocol(), Some(expected));
            assert_eq!(client.unwrap().get_ref().1.alpn_protocol(), Some(expected));
        }
    }
    #[tokio::test]
    async fn mutual_device_pins_bind_real_tls_and_reject_a_wrong_peer() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let stranger = DeviceIdentity::generate().unwrap();
        let server = server(&a, &[b.profile()]).unwrap();
        let connector = client(&b, &a.profile()).unwrap();
        let (left, right) = tokio::io::duplex(65536);
        let (server_result, client_result) = tokio::join!(
            accept(&server, left),
            connect(&connector, a.profile().stage_id, right)
        );
        let mut server_stream = server_result.unwrap();
        let mut client_stream = client_result.unwrap();
        let export_server = server_stream
            .get_ref()
            .1
            .export_keying_material([0; 32], b"EXPORTER-DaatLocus-Stage-v1", None)
            .unwrap();
        let export_client = client_stream
            .get_ref()
            .1
            .export_keying_material([0; 32], b"EXPORTER-DaatLocus-Stage-v1", None)
            .unwrap();
        assert_eq!(export_server, export_client);
        let frame = Envelope {
            version: 1,
            source: b.profile().stage_id,
            target: a.profile().stage_id,
            generation: 1,
            message_id: uuid::Uuid::new_v4(),
            invocation_id: None,
            in_reply_to: None,
            created_at_ms: 100,
            expires_at_ms: 10000,
            body: Body::Heartbeat,
        };
        let (sent, received) = tokio::join!(
            super::super::write_frame(&mut client_stream, &frame),
            super::super::read_frame(&mut server_stream)
        );
        assert_eq!(sent.unwrap(), received.unwrap().1);
        let bad_client = client(&stranger, &a.profile()).unwrap();
        let (left, right) = tokio::io::duplex(65536);
        let (server_result, client_result) = tokio::join!(
            accept(&server, left),
            connect(&bad_client, a.profile().stage_id, right)
        );
        assert!(server_result.is_err() || client_result.is_err());
    }
}
