//! Optional login rendezvous. OPAQUE and its encrypted result terminate on devices.
use super::network::{self, Attempt, Candidate, DeviceLogin, Network, Packet};
use super::public_connection::{PublicationError as Failure, PublicationState as State};
use crate::stage::{
    Result, StageError, StageId,
    identity::{DeviceIdentity, DeviceProfile, Enrollment, SignedProof},
    transport::websocket::{self, Socket},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
#[derive(Serialize)]
struct Registration {
    version: u16,
    challenge: uuid::Uuid,
    expires_at_ms: i64,
    name_hash: String,
    device: DeviceProfile,
    account_id: Option<String>,
}
#[derive(Deserialize)]
struct Peer {
    device: DeviceProfile,
    account_id: String,
    label: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    #[serde(skip)]
    Pong(Vec<u8>),
    Challenge {
        nonce: uuid::Uuid,
        expires_at_ms: i64,
    },
    Registered {
        peers: Vec<Peer>,
    },
    Forward {
        source: DeviceProfile,
        nonce: uuid::Uuid,
        packet: Box<Packet>,
    },
    Unreachable {
        nonce: uuid::Uuid,
    },
}
async fn receive(socket: &mut Socket) -> std::result::Result<Event, Failure> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) if text.len() <= 64 * 1024 => {
                return serde_json::from_str(&text).map_err(|_| Failure::Protocol);
            }
            Some(Ok(Message::Ping(bytes))) => {
                send(socket, Message::Pong(bytes)).await?;
            }
            Some(Ok(Message::Pong(bytes))) => return Ok(Event::Pong(bytes.to_vec())),
            Some(Ok(Message::Close(Some(frame)))) if u16::from(frame.code) == 1008 => {
                return Err(Failure::Rejected);
            }
            Some(Err(tokio_tungstenite::tungstenite::Error::Tls(_))) => return Err(Failure::Tls),
            _ => return Err(Failure::Transport),
        }
    }
}
async fn event(socket: &mut Socket) -> Result<Event> {
    loop {
        match receive(socket).await.map_err(Failure::stage)? {
            Event::Pong(_) => continue,
            event => return Ok(event),
        }
    }
}
async fn send(socket: &mut Socket, message: Message) -> std::result::Result<(), Failure> {
    tokio::time::timeout(Duration::from_secs(5), socket.send(message))
        .await
        .map_err(|_| Failure::Timeout)?
        .map_err(|_| Failure::Transport)
}

#[derive(Clone, Copy)]
struct Timings {
    registration: Duration,
    tick: Duration,
    heartbeat: Duration,
    pong: Duration,
    retry_cap: Duration,
}
const TIMINGS: Timings = Timings {
    registration: Duration::from_secs(10),
    tick: Duration::from_secs(5),
    heartbeat: Duration::from_secs(15),
    pong: Duration::from_secs(30),
    retry_cap: Duration::from_secs(10),
};

async fn connect(
    endpoint: &str,
    identity: &DeviceIdentity,
    name: &str,
    enrollment: Option<Enrollment>,
    label: String,
) -> Result<(Socket, Vec<Candidate>)> {
    register(
        endpoint,
        identity,
        name,
        enrollment,
        label,
        TIMINGS.registration,
    )
    .await
    .map_err(Failure::stage)
}
async fn register(
    endpoint: &str,
    identity: &DeviceIdentity,
    name: &str,
    enrollment: Option<Enrollment>,
    label: String,
    timeout: Duration,
) -> std::result::Result<(Socket, Vec<Candidate>), Failure> {
    tokio::time::timeout(
        timeout,
        register_inner(endpoint, identity, name, enrollment, label),
    )
    .await
    .map_err(|_| Failure::Timeout)?
}
async fn register_inner(
    endpoint: &str,
    identity: &DeviceIdentity,
    name: &str,
    enrollment: Option<Enrollment>,
    label: String,
) -> std::result::Result<(Socket, Vec<Candidate>), Failure> {
    let mut url = url::Url::parse(endpoint).map_err(|_| Failure::Protocol)?;
    if url.scheme() != "wss"
        && !(cfg!(test) && url.scheme() == "ws" && url.host_str() == Some("127.0.0.1"))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Failure::Protocol);
    }
    let hash = network::lookup(name);
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    url.set_path(&format!("/accounts/{hash}"));
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(64 * 1024);
    config.max_frame_size = Some(64 * 1024);
    config.max_write_buffer_size = 128 * 1024;
    config.write_buffer_size = 16 * 1024;
    let mut socket = tokio::time::timeout(
        Duration::from_secs(5),
        websocket::connect(url.as_str(), config),
    )
    .await
    .map_err(|_| Failure::Timeout)?
    .map_err(|error| match error {
        tokio_tungstenite::tungstenite::Error::Tls(_) => Failure::Tls,
        tokio_tungstenite::tungstenite::Error::Http(response)
            if response.status().as_u16() == 403 =>
        {
            Failure::Rejected
        }
        _ => Failure::Transport,
    })?;
    let Event::Challenge {
        nonce,
        expires_at_ms,
    } = receive(&mut socket).await?
    else {
        return Err(Failure::Protocol);
    };
    let now = chrono::Utc::now().timestamp_millis();
    if !websocket::valid_challenge(nonce, expires_at_ms, now) {
        return Err(Failure::Protocol);
    }
    let registration = Registration {
        version: 1,
        challenge: nonce,
        expires_at_ms,
        name_hash: hash.clone(),
        device: identity.profile(),
        account_id: enrollment.as_ref().map(|value| value.root.group_id.clone()),
    };
    let proof: SignedProof = identity
        .sign("account-rendezvous", &registration)
        .map_err(|_| Failure::Authority)?;
    send(&mut socket, Message::Text(serde_json::json!({"type":"register","device":identity.profile(),"enrollment":enrollment,"proof":proof,"label":label}).to_string().into())).await?;
    let Event::Registered { peers } = receive(&mut socket).await? else {
        return Err(Failure::Rejected);
    };
    if peers.len() > 64 {
        return Err(Failure::Protocol);
    }
    let mut candidates = vec![];
    for peer in peers {
        peer.device.validate().map_err(|_| Failure::Protocol)?;
        if peer.account_id.len() != 64
            || hex::decode(&peer.account_id).is_err()
            || peer.label.len() > 100
        {
            return Err(Failure::Protocol);
        }
        candidates.push(Candidate {
            profile: peer.device,
            account_id: peer.account_id,
            name_hash: hash.clone(),
            addresses: vec![],
            label: peer.label,
            relay: Some(endpoint.to_owned()),
            seen: Instant::now(),
        });
    }
    candidates.sort_by_key(|candidate| candidate.profile.stage_id);
    Ok((socket, candidates))
}
async fn forward(
    socket: &mut Socket,
    target: StageId,
    nonce: uuid::Uuid,
    packet: Packet,
) -> Result<()> {
    let value = serde_json::json!({"type":"forward","target":target,"nonce":nonce,"packet":packet})
        .to_string();
    if value.len() > 64 * 1024 {
        return Err(StageError::Capacity("Account relay packet too large"));
    }
    send(socket, Message::Text(value.into()))
        .await
        .map_err(Failure::stage)
}
pub async fn discover(
    endpoint: &str,
    identity: &DeviceIdentity,
    name: &str,
) -> Result<Vec<Candidate>> {
    let (mut socket, candidates) = connect(endpoint, identity, name, None, String::new()).await?;
    let _ = socket.close(None).await;
    Ok(candidates)
}
pub struct Client {
    socket: Socket,
    target: DeviceProfile,
    nonce: uuid::Uuid,
}
impl Client {
    #[allow(clippy::too_many_arguments)]
    pub async fn login(
        endpoint: &str,
        identity: &DeviceIdentity,
        candidate: &Candidate,
        name: String,
        request: String,
        device: &crate::stage::identity::JoinRequest,
        origin: String,
    ) -> Result<(Self, String, Vec<u8>, String)> {
        let (socket, _) = connect(endpoint, identity, &name, None, String::new()).await?;
        let mut client = Self {
            socket,
            target: candidate.profile.clone(),
            nonce: uuid::Uuid::new_v4(),
        };
        let login = DeviceLogin {
            name: String::new(),
            account_id: candidate.account_id.clone(),
            request,
            device: device.clone(),
            origin,
        };
        forward(
            &mut client.socket,
            client.target.stage_id,
            client.nonce,
            Packet::Start { login },
        )
        .await?;
        let Packet::Challenge { response, context } = client.reply().await? else {
            return Err(StageError::Forbidden("Account login failed"));
        };
        let context = URL_SAFE_NO_PAD
            .decode(context)
            .map_err(|_| StageError::Invalid("Invalid login context"))?;
        let binding = format!("relay:{}", client.nonce);
        Ok((client, response, context, binding))
    }
    async fn reply(&mut self) -> Result<Packet> {
        loop {
            match event(&mut self.socket).await? {
                Event::Forward {
                    source,
                    nonce,
                    packet,
                } if source == self.target && nonce == self.nonce => return Ok(*packet),
                Event::Unreachable { nonce } if nonce == self.nonce => {
                    return Err(StageError::Unavailable("Account device unavailable"));
                }
                _ => {}
            }
        }
    }
    pub async fn finish(&mut self, message: String) -> Result<Packet> {
        forward(
            &mut self.socket,
            self.target.stage_id,
            self.nonce,
            Packet::Finish { message },
        )
        .await?;
        self.reply().await
    }
}
pub async fn serve(network: Arc<Network>) {
    serve_with_timings(network, TIMINGS).await;
}
async fn serve_with_timings(network: Arc<Network>, timings: Timings) {
    let mut delay = Duration::from_secs(1).min(timings.retry_cap);
    let mut last_failure = None;
    loop {
        let Some(runtime) = network.runtime() else {
            return;
        };
        // Register wakes before reading configuration and credentials.
        let credentials_changed = runtime.host.accounts.changed().notified();
        let settings_changed = network.publication_wake.notified();
        tokio::pin!(credentials_changed, settings_changed);
        credentials_changed.as_mut().enable();
        settings_changed.as_mut().enable();
        let endpoint = runtime.host.settings().signaling_origin;
        let result = if let Some(endpoint) = &endpoint {
            match (
                runtime.host.accounts.current(),
                runtime.host.enrollment(),
                runtime.host.identity(),
            ) {
                (Ok(Some(record)), Ok(enrollment), Ok(identity)) => {
                    if network.publication_status().state != State::Reconnecting {
                        network.publish_status(
                            Some(endpoint),
                            State::Connecting,
                            None,
                            None,
                            false,
                        );
                    }
                    match register(
                        endpoint,
                        &identity,
                        &record.name,
                        Some(enrollment),
                        runtime.host.settings().label,
                        timings.registration,
                    )
                    .await
                    {
                        Ok((mut socket, _)) => {
                            last_failure = None;
                            delay = Duration::from_secs(1).min(timings.retry_cap);
                            network.publish_status(Some(endpoint), State::Online, None, None, true);
                            let outcome = published(
                                &network,
                                &runtime,
                                &mut socket,
                                endpoint,
                                &record,
                                timings,
                            )
                            .await;
                            // Closing a broken socket must not delay reconnect indefinitely.
                            let _ =
                                tokio::time::timeout(Duration::from_secs(1), socket.close(None))
                                    .await;
                            outcome
                        }
                        Err(error) => Err(error),
                    }
                }
                (Ok(None), _, _) => {
                    network.publish_status(
                        Some(endpoint),
                        State::WaitingForAccount,
                        None,
                        None,
                        false,
                    );
                    Ok(())
                }
                _ => Err(Failure::Authority),
            }
        } else {
            network.publish_status(None, State::NotConfigured, None, None, false);
            Ok(())
        };
        if let Err(error) = result {
            if last_failure != Some(error) {
                tracing::warn!(
                    code = error.code(),
                    "Public account login connection failed"
                );
                last_failure = Some(error);
            }
            network.publish_status(
                endpoint.as_deref(),
                State::Reconnecting,
                Some(error),
                Some(delay),
                false,
            );
        }
        tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = credentials_changed => {}, _ = settings_changed => {} }
        delay = (delay * 2).min(timings.retry_cap);
    }
}
async fn published(
    network: &Network,
    runtime: &crate::stage::runtime::Runtime,
    socket: &mut Socket,
    endpoint: &str,
    record: &super::CredentialBundle,
    timings: Timings,
) -> std::result::Result<(), Failure> {
    let label = runtime.host.settings().label;
    let mut attempts: HashMap<(StageId, uuid::Uuid), (Instant, Attempt)> = HashMap::new();
    let mut timer = tokio::time::interval(timings.tick);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next_ping = Instant::now();
    let mut pending: Option<(Vec<u8>, Instant)> = None;
    loop {
        let changed = runtime.host.accounts.changed().notified();
        let settings_changed = network.publication_wake.notified();
        tokio::pin!(changed, settings_changed);
        changed.as_mut().enable();
        settings_changed.as_mut().enable();
        let settings = runtime.host.settings();
        if runtime.host.enrollment().is_err() {
            return Err(Failure::Authority);
        }
        if settings.signaling_origin.as_deref() != Some(endpoint)
            || settings.label != label
            || !runtime
                .host
                .accounts
                .authorizes(&record.account_id, &record.binding())
        {
            return Ok(());
        }
        tokio::select! {
            incoming = receive(socket) => match incoming? {
                Event::Pong(payload) => {
                    if pending.as_ref().is_some_and(|(expected, _)| *expected == payload) {
                        pending = None;
                        network.publish_status(Some(endpoint), State::Online, None, None, true);
                    }
                }
                Event::Forward { source, nonce, packet } => {
                    attempts.retain(|_, (since, _)| since.elapsed() < Duration::from_secs(120));
                    let response = match *packet {
                        Packet::Start { mut login } if login.device.device == source && attempts.len() < 32 => {
                            login.name = record.name.clone();
                            match Attempt::start(runtime, login, format!("relay:{nonce}")) {
                                Ok((attempt, reply)) => { attempts.insert((source.stage_id, nonce), (Instant::now(), attempt)); reply }
                                Err(_) => Packet::Rejected,
                            }
                        }
                        Packet::Finish { message } => attempts.remove(&(source.stage_id, nonce))
                            .and_then(|(_, attempt)| attempt.finish(runtime, &message).ok()).unwrap_or(Packet::Rejected),
                        _ => Packet::Rejected,
                    };
                    forward(socket, source.stage_id, nonce, response).await.map_err(|_| Failure::Transport)?;
                }
                _ => return Err(Failure::Protocol),
            },
            _ = timer.tick() => {
                let now = Instant::now();
                if pending.as_ref().is_some_and(|(_, sent)| now.duration_since(*sent) >= timings.pong) {
                    return Err(Failure::Heartbeat);
                }
                if pending.is_none() && now >= next_ping {
                    let bytes = uuid::Uuid::new_v4().as_bytes().to_vec();
                    send(socket, Message::Ping(bytes.clone().into())).await?;
                    pending = Some((bytes, now));
                    next_ping = now + timings.heartbeat;
                }
            }
            _ = changed => {},
            _ = settings_changed => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::CredentialBundle, actors::ActorSystem, config::Config, service::ServiceState,
    };
    type ServerSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;
    async fn accept_registration(stream: tokio::net::TcpStream) -> ServerSocket {
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let challenge = serde_json::json!({"type":"challenge","nonce":uuid::Uuid::new_v4(),"expires_at_ms":chrono::Utc::now().timestamp_millis()+30_000});
        socket
            .send(Message::Text(challenge.to_string().into()))
            .await
            .unwrap();
        let registration = socket.next().await.unwrap().unwrap();
        let registration: serde_json::Value =
            serde_json::from_str(registration.to_text().unwrap()).unwrap();
        assert_eq!(registration["type"], "register");
        socket
            .send(Message::Text(r#"{"type":"registered","peers":[]}"#.into()))
            .await
            .unwrap();
        socket
    }
    #[tokio::test]
    async fn registration_cannot_wait_forever_for_a_challenge_or_acknowledgement() {
        for acknowledge in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                if acknowledge {
                    let challenge = serde_json::json!({"type":"challenge","nonce":uuid::Uuid::new_v4(),"expires_at_ms":chrono::Utc::now().timestamp_millis()+30_000});
                    socket
                        .send(Message::Text(challenge.to_string().into()))
                        .await
                        .unwrap();
                }
                std::future::pending::<()>().await;
                drop(socket);
            });
            let identity = DeviceIdentity::generate().unwrap();
            let result = register(
                &endpoint,
                &identity,
                "fixture",
                None,
                String::new(),
                Duration::from_millis(80),
            )
            .await;
            assert!(matches!(result, Err(Failure::Timeout)));
            server.abort();
            let _ = server.await;
        }
    }
    #[tokio::test]
    async fn policy_rejection_and_pong_frames_have_distinct_meanings() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server =
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = accept_registration(stream).await;
                socket
                    .send(Message::Pong(vec![1, 2, 3].into()))
                    .await
                    .unwrap();
                socket.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Policy,
                reason: "Rejected".into(),
            })).await.unwrap();
            });
        let identity = DeviceIdentity::generate().unwrap();
        let (mut socket, _) = register(
            &endpoint,
            &identity,
            "fixture",
            None,
            String::new(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert!(matches!(receive(&mut socket).await, Ok(Event::Pong(bytes)) if bytes == [1,2,3]));
        assert!(matches!(receive(&mut socket).await, Err(Failure::Rejected)));
        server.await.unwrap();
    }

    async fn publication_fixture(
        endpoint: String,
    ) -> (
        tempfile::TempDir,
        ActorSystem,
        Arc<crate::stage::runtime::Runtime>,
        Arc<Network>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let system = ActorSystem::open_account_gated(
            directory.path().join("actors"),
            directory.path().join("workspace"),
            Config::default(),
        )
        .await
        .unwrap();
        let runtime = ServiceState::new(system.clone(), "http://127.0.0.1:45825".into())
            .unwrap()
            .stage
            .unwrap();
        runtime.host.initialize(true).unwrap();
        let account = runtime.host.account_id().unwrap();
        let setup = daat_locus_auth::new_setup();
        let client =
            daat_locus_auth::RegistrationClient::start(b"isolated publication fixture".to_vec())
                .unwrap();
        let response = daat_locus_auth::registration_response(
            &setup,
            format!("DaatLocus.Account.v1/{account}/fixture").as_bytes(),
            &client.request(),
        )
        .unwrap();
        let file = daat_locus_auth::password_file(&client.finish(&response).unwrap()).unwrap();
        runtime
            .host
            .accounts
            .install(
                CredentialBundle::new(account.clone(), "fixture".into(), &setup, &file, None)
                    .unwrap(),
                &account,
            )
            .unwrap();
        let mut settings = runtime.host.settings();
        settings.listen = "127.0.0.1:0".into();
        settings.signaling_origin = Some(endpoint);
        runtime.host.save_settings(settings).unwrap();
        let network = Network::start_without_publication(&runtime).await.unwrap();
        (directory, system, runtime, network)
    }
    #[tokio::test]
    async fn silent_half_open_publication_reconnects_then_accepts_only_matching_pongs() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (_directory, system, runtime, network) = publication_fixture(endpoint).await;
        let (first_ready, first_seen) = tokio::sync::oneshot::channel();
        let (pong_seen, pong_ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Keep the TCP socket alive without reading or acknowledging heartbeats.
            let mut first = accept_registration(stream).await;
            first
                .send(Message::Pong(vec![9, 9, 9].into()))
                .await
                .unwrap();
            first_ready.send(()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut second = accept_registration(stream).await;
            if let Some(Ok(Message::Ping(bytes))) = second.next().await {
                second.send(Message::Pong(bytes)).await.unwrap();
                pong_seen.send(()).unwrap();
            }
            while let Some(Ok(message)) = second.next().await {
                if let Message::Ping(bytes) = message {
                    second.send(Message::Pong(bytes)).await.unwrap();
                }
            }
            drop(first);
        });
        let own = network.clone();
        let publisher = tokio::spawn(async move {
            serve_with_timings(
                own,
                Timings {
                    registration: Duration::from_secs(2),
                    tick: Duration::from_millis(10),
                    heartbeat: Duration::from_millis(20),
                    pong: Duration::from_millis(80),
                    retry_cap: Duration::from_millis(100),
                },
            )
            .await;
        });
        first_seen.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = network.publication_status();
                if status.error_code == Some(Failure::Heartbeat.code()) {
                    assert_eq!(status.state, State::Reconnecting);
                    assert!(status.retry_at_ms.is_some());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), pong_ready)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while network.publication_status().state != State::Online {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(160)).await;
        let status = network.publication_status();
        assert_eq!(status.state, State::Online);
        assert_eq!(status.error_code, None);
        assert!(status.last_confirmed_at_ms.is_some());
        let mut settings = runtime.host.settings();
        settings.signaling_origin = None;
        runtime.host.save_settings(settings).unwrap();
        network.publication_wake.notify_waiters();
        tokio::time::timeout(Duration::from_secs(2), async {
            while network.publication_status().state != State::NotConfigured {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        publisher.abort();
        let _ = publisher.await;
        server.abort();
        let _ = server.await;
        network.stop().await;
        system.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires DAAT_LOCUS_TEST_ACCOUNT_RELAY pointing to a local Worker"]
    async fn unpaired_password_login_crosses_a_real_local_relay_without_disclosing_credentials() {
        password_login_over_relay(false).await;
    }

    #[tokio::test]
    #[ignore = "requires DAAT_LOCUS_TEST_ACCOUNT_RELAY pointing to a local Worker"]
    async fn unreachable_lan_candidate_falls_back_to_relay_login() {
        password_login_over_relay(true).await;
    }

    async fn password_login_over_relay(unreachable_lan: bool) {
        let endpoint =
            std::env::var("DAAT_LOCUS_TEST_ACCOUNT_RELAY").expect("local Worker endpoint");
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            main_model: "sync-fixture".into(),
            ..Default::default()
        };
        config.providers.insert(
            "sync-fixture".into(),
            crate::config::ProviderConfig::OpenaiCompatible {
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: "isolated-test-key".into(),
            },
        );
        config.models.insert(
            "sync-fixture".into(),
            crate::config::ModelConfig {
                provider: "sync-fixture".into(),
                model_id: "sync-fixture".into(),
                ..Default::default()
            },
        );
        let a = ActorSystem::open_account_gated(
            a_dir.path().join("actors"),
            a_dir.path().join("workspace"),
            config,
        )
        .await
        .unwrap();
        let b = ActorSystem::open_account_gated(
            b_dir.path().join("actors"),
            b_dir.path().join("workspace"),
            Config::default(),
        )
        .await
        .unwrap();
        let source = ServiceState::new(a.clone(), "http://127.0.0.1:45825".into())
            .unwrap()
            .stage
            .unwrap();
        let target = ServiceState::new(b.clone(), "http://127.0.0.1:45826".into())
            .unwrap()
            .stage
            .unwrap();
        source.host.initialize(true).unwrap();
        let setup = daat_locus_auth::new_setup();
        let account = source.host.account_id().unwrap();
        let password = b"local relay account password";
        let registration = daat_locus_auth::RegistrationClient::start(password.to_vec()).unwrap();
        let response = daat_locus_auth::registration_response(
            &setup,
            format!("DaatLocus.Account.v1/{account}/alice").as_bytes(),
            &registration.request(),
        )
        .unwrap();
        let file =
            daat_locus_auth::password_file(&registration.finish(&response).unwrap()).unwrap();
        source
            .host
            .accounts
            .install(
                CredentialBundle::new(account.clone(), "alice".into(), &setup, &file, None)
                    .unwrap(),
                &account,
            )
            .unwrap();
        let mut settings = source.host.settings();
        settings.signaling_origin = Some(endpoint.clone());
        settings.interfaces = vec!["daat-locus-test-no-interface".into()];
        settings.label = "Login test desktop".into();
        source.host.save_settings(settings).unwrap();
        source.start().await.unwrap();
        a.activate_account_workers().await.unwrap();
        target.start().await.unwrap();
        let identity = target.host.identity().unwrap();
        let candidate = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(peers) = discover(&endpoint, &identity, "alice").await
                    && let Some(candidate) =
                        peers.into_iter().find(|peer| peer.account_id == account)
                {
                    break candidate;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .unwrap();
        assert!(candidate.addresses.is_empty());
        assert!(candidate.relay.is_some());
        let client = daat_locus_auth::LoginClient::start(password.to_vec()).unwrap();
        let login = if unreachable_lan {
            let mut settings = target.host.settings();
            settings.signaling_origin = Some(endpoint.clone());
            target.host.save_settings(settings).unwrap();
            let network = target.account_network().await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut lan = candidate.clone();
            lan.addresses = vec![listener.local_addr().unwrap()];
            lan.relay = None;
            drop(listener);
            network.add_test_candidate(lan);
            network
                .login(
                    "alice",
                    &URL_SAFE_NO_PAD.encode(client.request()),
                    "http://127.0.0.1:45826",
                    &[],
                )
                .await
                .unwrap()
        } else {
            crate::accounts::network::RemoteLogin::connect(
                &target,
                candidate,
                "alice".into(),
                URL_SAFE_NO_PAD.encode(client.request()),
                "http://127.0.0.1:45826".into(),
            )
            .await
            .unwrap()
        };
        let authenticated = client
            .authenticate(
                &URL_SAFE_NO_PAD.decode(&login.response).unwrap(),
                &login.context,
            )
            .unwrap();
        let encrypted = login
            .finish(URL_SAFE_NO_PAD.encode(&authenticated.message))
            .await
            .unwrap();
        assert!(!encrypted.contains("local relay account password"));
        let bytes = authenticated
            .channel
            .open(&URL_SAFE_NO_PAD.decode(encrypted).unwrap())
            .unwrap();
        let imported: crate::accounts::network::AccountImport =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(imported.credentials.account_id, account);
        assert_eq!(imported.credentials.name, "alice");
        assert_eq!(
            imported.enrollment.root,
            source.host.enrollment().unwrap().root
        );
        assert_eq!(
            crate::stage::identity::verify_enrollment(
                &imported.enrollment.root,
                &imported.enrollment.chain,
                chrono::Utc::now().timestamp_millis(),
                &Default::default()
            )
            .unwrap()
            .device,
            target.host.profile().unwrap()
        );
        // Login must lead to automatic private synchronization, without a
        // manual dial or an artificial signaling command from the test.
        target.host.join(imported.enrollment, &account).unwrap();
        target
            .host
            .accounts
            .install(imported.credentials, &account)
            .unwrap();
        target
            .host
            .record_device_label(source.host.local, &imported.label)
            .unwrap();
        let mut settings = target.host.settings();
        settings.signaling_origin = Some(endpoint.clone());
        settings.interfaces = vec!["daat-locus-test-no-interface".into()];
        settings.label = "Login test phone".into();
        target.host.save_settings(settings).unwrap();
        target.start().await.unwrap();
        target.want(source.host.local);
        tokio::time::timeout(Duration::from_secs(40), async {
            while source
                .host
                .device_label(target.host.local)
                .unwrap()
                .as_deref()
                != Some("Login test phone")
                || target.host.configurations.current().unwrap().is_none()
                || b.config().main_model != "sync-fixture"
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        b.activate_account_workers().await.unwrap();
        assert_eq!(
            source.state(target.host.local).route,
            Some(crate::stage::connection::Route::P2p)
        );
        assert_eq!(
            target.state(source.host.local).route,
            Some(crate::stage::connection::Route::P2p)
        );
        let message = a
            .send_user("core", "Private sync test message".into(), vec![], None)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            while b.store().message(&message.id).is_err() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            b.store().message(&message.id).unwrap().content,
            "Private sync test message"
        );
        source.stop().await;
        target.stop().await;
        a.shutdown().await;
        b.shutdown().await;
    }
}
