//! One authenticated online rendezvous socket per Stage. No Actor data is sent.
use crate::stage::{
    Result, StageError, StageId,
    identity::{DeviceIdentity, Enrollment, SignedProof},
    transport::websocket::{self, Socket},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use uuid::Uuid;
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SignalEvent {
    Revocations {
        records: Vec<crate::stage::revocation::Change>,
    },
    Challenge {
        nonce: Uuid,
        expires_at_ms: i64,
    },
    Registered {
        peers: Vec<StageId>,
    },
    Presence {
        stage_id: StageId,
        online: bool,
    },
    Signal {
        source: StageId,
        proof: SignedProof,
    },
    Unreachable {
        target: StageId,
        nonce: Uuid,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::{
        identity::DeviceIdentity,
        transport::p2p::{Attempt, P2pChannel},
    };

    #[tokio::test]
    async fn heartbeat_requires_the_matching_pong_even_with_other_traffic() {
        for matching in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let Message::Ping(nonce) = socket.next().await.unwrap().unwrap() else {
                    panic!("heartbeat required");
                };
                socket
                    .send(Message::Pong(if matching { nonce } else { vec![0].into() }))
                    .await
                    .unwrap();
                socket.send(Message::Text(serde_json::json!({"type":"presence","stage_id":StageId::new(),"online":true}).to_string().into())).await.unwrap();
                // Keep the connection open without further reads or auto-pongs.
                std::future::pending::<()>().await;
            });
            let mut socket = SignalSocket {
                socket: websocket::connect(&endpoint, WebSocketConfig::default())
                    .await
                    .unwrap(),
                last_confirmed: Instant::now() - Duration::from_secs(60),
                pending_ping: None,
                peers: vec![],
                sent_bytes: 0,
                received_bytes: 0,
            };
            socket.heartbeat().await.unwrap();
            assert!(matches!(
                socket.receive().await.unwrap(),
                SignalEvent::Presence { .. }
            ));
            if matching {
                assert!(socket.pending_ping.is_none());
                socket.heartbeat().await.unwrap();
            } else {
                socket.pending_ping.as_mut().unwrap().1 = Instant::now() - Duration::from_secs(31);
                assert!(socket.heartbeat().await.is_err());
            }
            server.abort();
            let _ = server.await;
        }
    }

    #[tokio::test]
    #[ignore = "requires DAAT_LOCUS_TEST_STAGE_SIGNALING with a WSS rendezvous"]
    async fn public_signaling_connects_authenticated_devices() {
        let endpoint = std::env::var("DAAT_LOCUS_TEST_STAGE_SIGNALING").unwrap();
        let mut a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let own = a.initialize_group(now, now + 600_000).unwrap();
        let other = a
            .authorize(
                &own,
                &b.join_request(now).unwrap(),
                now,
                now + 600_000,
                true,
            )
            .unwrap();
        let (left, right) = tokio::join!(
            SignalSocket::connect(&endpoint, &a, &own),
            SignalSocket::connect(&endpoint, &b, &other)
        );
        let mut left = left.unwrap();
        let mut right = right.unwrap();
        let mut outgoing = P2pChannel::new(vec![], true).await.unwrap();
        let mut incoming = P2pChannel::new(vec![], true).await.unwrap();
        let attempt = Attempt {
            generation: 1,
            nonce: Uuid::new_v4(),
        };
        let offer = outgoing
            .offer(
                &a,
                &own.root,
                b.profile().stage_id,
                attempt,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        left.send_negotiation(&offer).await.unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let SignalEvent::Signal { source, proof } = right.receive().await.unwrap() {
                    assert_eq!(source, a.profile().stage_id);
                    break proof;
                }
            }
        })
        .await
        .unwrap();
        incoming
            .apply_signal(
                &received,
                &a.profile(),
                &own.root,
                b.profile().stage_id,
                attempt,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        let answer = incoming
            .answer(
                &b,
                &own.root,
                a.profile().stage_id,
                attempt,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        right.send_negotiation(&answer).await.unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let SignalEvent::Signal { source, proof } = left.receive().await.unwrap() {
                    assert_eq!(source, b.profile().stage_id);
                    break proof;
                }
            }
        })
        .await
        .unwrap();
        outgoing
            .apply_signal(
                &received,
                &b.profile(),
                &own.root,
                a.profile().stage_id,
                attempt,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .unwrap();
        let (a_connected, b_connected) = tokio::join!(outgoing.connected(), incoming.connected());
        a_connected.unwrap();
        b_connected.unwrap();
        outgoing.close().await.unwrap();
        incoming.close().await.unwrap();
        left.close().await.unwrap();
        right.close().await.unwrap();
    }
}
#[derive(Serialize)]
struct Registration {
    challenge: Uuid,
    version: u16,
    group_id: String,
    stage_id: StageId,
    expires_at_ms: i64,
}
pub struct SignalSocket {
    socket: Socket,
    last_confirmed: Instant,
    pending_ping: Option<(Vec<u8>, Instant)>,
    pub peers: Vec<StageId>,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}
impl SignalSocket {
    pub async fn connect(
        endpoint: &str,
        identity: &DeviceIdentity,
        enrollment: &Enrollment,
    ) -> Result<Self> {
        tokio::time::timeout(
            Duration::from_secs(10),
            Self::connect_prepared(endpoint, identity, enrollment),
        )
        .await
        .map_err(|_| StageError::Unavailable("Signaling registration timed out"))?
    }
    async fn connect_prepared(
        endpoint: &str,
        identity: &DeviceIdentity,
        enrollment: &Enrollment,
    ) -> Result<Self> {
        let mut url = url::Url::parse(endpoint)
            .map_err(|_| StageError::Invalid("Invalid signaling endpoint"))?;
        if url.scheme() != "wss"
            && !(cfg!(test) && url.scheme() == "ws" && url.host_str() == Some("127.0.0.1"))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(StageError::Invalid(
                "Signaling requires an explicitly configured WSS origin",
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
            std::time::Duration::from_secs(10),
            websocket::connect(url.as_str(), config),
        )
        .await
        .map_err(|_| StageError::Unavailable("Signaling connection timed out"))?
        .map_err(|_| StageError::Unavailable("Signaling connection failed"))?;
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
            .map_err(|_| StageError::Unavailable("Signaling challenge timed out"))??;
        let SignalEvent::Challenge {
            nonce,
            expires_at_ms,
        } = event
        else {
            return Err(StageError::Forbidden(
                "Signaling authentication challenge required",
            ));
        };
        let now = chrono::Utc::now().timestamp_millis();
        if !websocket::valid_challenge(nonce, expires_at_ms, now) {
            return Err(StageError::Forbidden("Invalid signaling challenge"));
        }
        let proof = identity.sign(
            "signal-register",
            &Registration {
                version: 1,
                group_id: enrollment.root.group_id.clone(),
                stage_id: identity.profile().stage_id,
                challenge: nonce,
                expires_at_ms,
            },
        )?;
        value
            .send_json(
                &serde_json::json!({"type":"register","enrollment":enrollment,"proof":proof}),
            )
            .await?;
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), value.receive())
            .await
            .map_err(|_| StageError::Unavailable("Signaling registration timed out"))??;
        let SignalEvent::Registered { peers } = event else {
            return Err(StageError::Forbidden("Signaling registration rejected"));
        };
        if peers.len() > 64 {
            return Err(StageError::Capacity("Signaling device budget exceeded"));
        }
        value.peers = peers;
        Ok(value)
    }
    async fn send_json(&mut self, value: &serde_json::Value) -> Result<()> {
        let text = serde_json::to_string(value)
            .map_err(|_| StageError::Invalid("Cannot encode signaling message"))?;
        if text.len() > 64 * 1024 {
            return Err(StageError::Capacity("Signaling message exceeds budget"));
        }
        let length = text.len();
        self.send_frame(Message::Text(text.into())).await?;
        self.sent_bytes = self.sent_bytes.saturating_add(length as u64);
        Ok(())
    }
    pub async fn send_negotiation(&mut self, proof: &SignedProof) -> Result<()> {
        self.send_json(&serde_json::json!({"type":"signal","proof":proof}))
            .await
    }
    pub async fn send_revocation(
        &mut self,
        change: &crate::stage::revocation::Change,
    ) -> Result<()> {
        self.send_json(&serde_json::json!({"type":"revoke","record":change}))
            .await
    }
    pub async fn receive(&mut self) -> Result<SignalEvent> {
        loop {
            match self.socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    self.received_bytes = self.received_bytes.saturating_add(text.len() as u64);
                    return serde_json::from_str(&text)
                        .map_err(|_| StageError::Invalid("Invalid signaling response"));
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
                    tracing::debug!(code = ?frame.as_ref().map(|frame| frame.code), "Public signaling connection closed by service");
                    return Err(StageError::Unavailable("Signaling connection closed"));
                }
                _ => return Err(StageError::Unavailable("Signaling connection closed")),
            }
        }
    }
    async fn send_frame(&mut self, frame: Message) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), self.socket.send(frame))
            .await
            .map_err(|_| StageError::Unavailable("Signaling send timed out"))?
            .map_err(|_| StageError::Unavailable("Signaling send failed"))
    }
    pub async fn heartbeat(&mut self) -> Result<()> {
        self.heartbeat_with(Duration::from_secs(15), Duration::from_secs(30))
            .await
    }
    async fn heartbeat_with(&mut self, interval: Duration, timeout: Duration) -> Result<()> {
        if let Some((_, sent)) = &self.pending_ping {
            if sent.elapsed() >= timeout {
                return Err(StageError::Unavailable("Signaling heartbeat timed out"));
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
            .map_err(|_| StageError::Unavailable("Signaling close timed out"))?
            .map_err(|_| StageError::Unavailable("Signaling close failed"))
    }
}
