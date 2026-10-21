//! Authenticated public relay socket. Business bytes remain inside device TLS.
use crate::stage::{
    Result, StageError, StageId,
    identity::{DeviceIdentity, Enrollment},
    transport::websocket::{self, Socket},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use uuid::Uuid;
pub const WINDOW: usize = 64 * 1024;
pub const CHUNK: usize = 16 * 1024;
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelayEvent {
    Challenge {
        nonce: Uuid,
        expires_at_ms: i64,
        protocol: u16,
    },
    Registered {
        peers: Vec<StageId>,
        protocol: u16,
    },
    Presence {
        stage_id: StageId,
        online: bool,
    },
    Revocations {
        records: Vec<crate::stage::revocation::Change>,
    },
    Incoming {
        channel: Uuid,
        source: StageId,
    },
    Opened {
        channel: Uuid,
    },
    Closed {
        channel: Uuid,
    },
    #[serde(skip)]
    Data {
        channel: Uuid,
        bytes: Vec<u8>,
    },
    #[serde(skip)]
    Credit {
        channel: Uuid,
        amount: usize,
    },
}
#[derive(Serialize)]
struct Registration {
    challenge: Uuid,
    version: u16,
    group_id: String,
    stage_id: StageId,
    expires_at_ms: i64,
}
pub struct RelaySocket {
    socket: Socket,
    last_confirmed: Instant,
    pending_ping: Option<(Vec<u8>, Instant)>,
    pub peers: Vec<StageId>,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}
impl RelaySocket {
    pub async fn connect(
        endpoint: &str,
        identity: &DeviceIdentity,
        enrollment: &Enrollment,
    ) -> Result<Self> {
        tokio::time::timeout(
            Duration::from_secs(40),
            Self::connect_prepared(endpoint, identity, enrollment),
        )
        .await
        .map_err(|_| StageError::Unavailable("Relay registration timed out"))?
    }
    async fn connect_prepared(
        endpoint: &str,
        identity: &DeviceIdentity,
        enrollment: &Enrollment,
    ) -> Result<Self> {
        let mut url =
            url::Url::parse(endpoint).map_err(|_| StageError::Invalid("Invalid relay endpoint"))?;
        if url.scheme() != "wss" && !(url.scheme() == "ws" && url.host_str() == Some("127.0.0.1"))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(StageError::Invalid(
                "Relay requires an explicitly configured WSS origin",
            ));
        }
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        url.set_path(&format!("/connect/{}", enrollment.root.group_id));
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(64 * 1024);
        config.max_frame_size = Some(64 * 1024);
        config.max_write_buffer_size = 128 * 1024;
        config.write_buffer_size = 16 * 1024;
        let socket = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            websocket::connect(url.as_str(), config),
        )
        .await
        .map_err(|_| StageError::Unavailable("Relay connection timed out"))?
        .map_err(|_| StageError::Unavailable("Relay connection failed"))?;
        let mut value = Self {
            socket,
            last_confirmed: Instant::now(),
            pending_ping: None,
            peers: vec![],
            sent_bytes: 0,
            received_bytes: 0,
        };
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), value.receive())
            .await
            .map_err(|_| StageError::Unavailable("Relay challenge timed out"))??;
        let RelayEvent::Challenge {
            nonce,
            expires_at_ms,
            protocol: 2,
        } = event
        else {
            return Err(StageError::Forbidden(
                "Relay authentication challenge required",
            ));
        };
        let now = chrono::Utc::now().timestamp_millis();
        if !websocket::valid_challenge(nonce, expires_at_ms, now) {
            return Err(StageError::Forbidden("Invalid relay challenge"));
        }
        let proof = identity.sign(
            "relay-register",
            &Registration {
                version: 2,
                group_id: enrollment.root.group_id.clone(),
                stage_id: identity.profile().stage_id,
                challenge: nonce,
                expires_at_ms,
            },
        )?;
        value
            .send_json(
                &serde_json::json!({"type":"register","protocol":2,"enrollment":enrollment,"proof":proof}),
            )
            .await?;
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), value.receive())
            .await
            .map_err(|_| StageError::Unavailable("Relay registration timed out"))??;
        let RelayEvent::Registered { peers, protocol: 2 } = event else {
            return Err(StageError::Forbidden("Relay registration rejected"));
        };
        value.peers = peers;
        Ok(value)
    }
    async fn send_json(&mut self, value: &serde_json::Value) -> Result<()> {
        let text = serde_json::to_string(value)
            .map_err(|_| StageError::Invalid("Cannot encode relay message"))?;
        if text.len() > 64 * 1024 {
            return Err(StageError::Capacity("Relay message exceeds budget"));
        }
        let length = text.len();
        self.send_frame(Message::Text(text.into())).await?;
        self.sent_bytes = self.sent_bytes.saturating_add(length as u64);
        Ok(())
    }
    pub async fn control(
        &mut self,
        kind: &str,
        channel: Uuid,
        target: Option<StageId>,
    ) -> Result<()> {
        let mut value = serde_json::json!({"type":kind,"channel":channel});
        if let Some(target) = target {
            value["target"] = serde_json::json!(target);
        }
        self.send_json(&value).await
    }
    pub async fn data(&mut self, channel: Uuid, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() || bytes.len() > CHUNK {
            return Err(StageError::Capacity("Relay chunk exceeds budget"));
        }
        let mut frame = vec![2, 0];
        frame.extend_from_slice(channel.as_bytes());
        frame.extend_from_slice(bytes);
        self.send_frame(Message::Binary(frame.into())).await
    }
    pub async fn credit(&mut self, channel: Uuid, amount: usize) -> Result<()> {
        if amount == 0 || amount > WINDOW {
            return Err(StageError::Invalid("Invalid relay credit"));
        }
        let mut frame = vec![2, 1];
        frame.extend_from_slice(channel.as_bytes());
        frame.extend_from_slice(&(amount as u32).to_be_bytes());
        self.send_frame(Message::Binary(frame.into())).await
    }
    pub async fn send_revocation(
        &mut self,
        change: &crate::stage::revocation::Change,
    ) -> Result<()> {
        self.send_json(&serde_json::json!({"type":"revoke","record":change}))
            .await
    }
    pub async fn receive(&mut self) -> Result<RelayEvent> {
        loop {
            match self.socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    self.received_bytes = self.received_bytes.saturating_add(text.len() as u64);
                    return serde_json::from_str(&text)
                        .map_err(|_| StageError::Invalid("Invalid relay response"));
                }
                Some(Ok(Message::Binary(bytes))) => {
                    if bytes.len() < 18 || bytes[0] != 2 {
                        return Err(StageError::Invalid("Invalid relay binary frame"));
                    }
                    let channel = Uuid::from_slice(&bytes[2..18])
                        .map_err(|_| StageError::Invalid("Invalid relay channel"))?;
                    return match bytes[1] {
                        0 if bytes.len() > 18 && bytes.len() <= CHUNK + 18 => {
                            Ok(RelayEvent::Data {
                                channel,
                                bytes: bytes[18..].to_vec(),
                            })
                        }
                        1 if bytes.len() == 22 => {
                            let amount =
                                u32::from_be_bytes(bytes[18..22].try_into().unwrap()) as usize;
                            if amount == 0 || amount > WINDOW {
                                return Err(StageError::Invalid("Invalid relay credit"));
                            }
                            Ok(RelayEvent::Credit { channel, amount })
                        }
                        _ => Err(StageError::Invalid("Invalid relay binary frame")),
                    };
                }
                Some(Ok(Message::Ping(bytes))) => {
                    self.send_frame(Message::Pong(bytes)).await?;
                }
                Some(Ok(Message::Pong(bytes))) => {
                    if self
                        .pending_ping
                        .as_ref()
                        .is_some_and(|(nonce, _)| nonce.as_slice() == bytes.as_ref())
                    {
                        self.pending_ping = None;
                        self.last_confirmed = Instant::now();
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    tracing::debug!(code = ?frame.as_ref().map(|frame| frame.code), "Public relay connection closed by service");
                    return Err(StageError::Unavailable("Relay connection closed"));
                }
                _ => return Err(StageError::Unavailable("Relay connection closed")),
            }
        }
    }
    async fn send_frame(&mut self, frame: Message) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), self.socket.send(frame))
            .await
            .map_err(|_| StageError::Unavailable("Relay send timed out"))?
            .map_err(|_| StageError::Unavailable("Relay send failed"))
    }
    pub async fn heartbeat(&mut self) -> Result<()> {
        self.heartbeat_with(Duration::from_secs(15), Duration::from_secs(30))
            .await
    }
    async fn heartbeat_with(&mut self, interval: Duration, timeout: Duration) -> Result<()> {
        if let Some((_, sent)) = &self.pending_ping {
            if sent.elapsed() >= timeout {
                return Err(StageError::Unavailable("Relay heartbeat timed out"));
            }
        } else if self.last_confirmed.elapsed() >= interval {
            let nonce = Uuid::new_v4().as_bytes().to_vec();
            self.send_frame(Message::Ping(nonce.clone().into())).await?;
            self.pending_ping = Some((nonce, Instant::now()));
        }
        Ok(())
    }
    pub async fn close(&mut self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(2), self.socket.close(None))
            .await
            .map_err(|_| StageError::Unavailable("Relay close timed out"))?
            .map_err(|_| StageError::Unavailable("Relay close failed"))
    }
}
