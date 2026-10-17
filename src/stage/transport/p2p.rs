//! Real ICE/DTLS/SCTP channels. Signed SDP binds the DTLS fingerprint to an
//! enrolled device and this negotiation; signaling conveys no Actor traffic.
use crate::stage::{
    Result, StageError, StageId,
    identity::{DeviceIdentity, DeviceProfile, SignedProof, TrustRoot},
    protocol::{Envelope, MAX_FRAME},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit, RTCDataChannelState},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState, RTCSdpType,
        RTCSessionDescription, SettingEngineBuilder,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Negotiation {
    pub version: u16,
    pub group_id: String,
    pub source: StageId,
    pub target: StageId,
    pub generation: u64,
    pub nonce: Uuid,
    pub expires_at_ms: i64,
    pub description: RTCSessionDescription,
    pub fingerprint: String,
}
#[derive(Clone, Copy, Debug)]
pub struct Attempt {
    pub generation: u64,
    pub nonce: Uuid,
}
pub fn verify_signal(
    proof: &SignedProof,
    peer: &DeviceProfile,
    root: &TrustRoot,
    local: StageId,
    attempt: Attempt,
    now: i64,
) -> Result<Negotiation> {
    peer.validate()?;
    let value: Negotiation = proof.verify("rtc-negotiation", &peer.public_key)?;
    if value.expires_at_ms <= now || value.expires_at_ms > now.saturating_add(120_000) {
        tracing::debug!(
            remaining_ms = value.expires_at_ms.saturating_sub(now),
            "RTC negotiation timestamp rejected"
        );
        return Err(StageError::Forbidden(
            "RTC negotiation timestamp is invalid",
        ));
    }
    if value.version != 1
        || value.group_id != root.group_id
        || value.source != peer.stage_id
        || value.target != local
        || value.generation != attempt.generation
        || value.nonce != attempt.nonce
        || attempt.nonce.is_nil()
        || value.description.sdp.len() > 6 * 1024
        || !matches!(
            value.description.sdp_type,
            RTCSdpType::Offer | RTCSdpType::Answer
        )
        || fingerprint(&value.description.sdp)? != value.fingerprint
    {
        return Err(StageError::Forbidden(
            "RTC negotiation is outside authenticated binding",
        ));
    }
    Ok(value)
}
fn fingerprint(sdp: &str) -> Result<String> {
    let mut fingerprints = sdp
        .lines()
        .filter_map(|line| line.trim().strip_prefix("a=fingerprint:sha-256 "));
    let first = fingerprints
        .next()
        .ok_or(StageError::Forbidden("RTC fingerprint is missing"))?;
    let raw = first.replace(':', "");
    if raw.len() != 64 || hex::decode(&raw).is_err() || fingerprints.any(|value| value != first) {
        return Err(StageError::Forbidden("RTC fingerprints are inconsistent"));
    }
    Ok(first.to_string())
}
fn rtc_error(_: webrtc::error::Error) -> StageError {
    StageError::Unavailable("RTC transport operation failed")
}
pub struct P2pChannel {
    pc: Arc<dyn PeerConnection>,
    control: Arc<dyn DataChannel>,
    bulk: Arc<dyn DataChannel>,
    incoming_control: mpsc::Receiver<Result<Vec<u8>>>,
    incoming_bulk: mpsc::Receiver<Result<Vec<u8>>>,
    ready: watch::Receiver<u8>,
    closed: watch::Receiver<bool>,
    peer: Option<DeviceProfile>,
    gathered: watch::Receiver<bool>,
    readers: Vec<tokio::task::JoinHandle<()>>,
}
impl P2pChannel {
    pub fn verified_peer(&self) -> Result<super::tls::VerifiedPeer> {
        if *self.ready.borrow() != 3 || *self.closed.borrow() {
            return Err(StageError::Forbidden(
                "DTLS channel is not authenticated and open",
            ));
        }
        let profile = self
            .peer
            .as_ref()
            .ok_or(StageError::Forbidden("RTC peer has not authenticated"))?;
        Ok(super::tls::VerifiedPeer::signed_dtls(profile))
    }
    pub fn sender(&self) -> P2pSender {
        P2pSender {
            control: self.control.clone(),
            bulk: self.bulk.clone(),
            pc: self.pc.clone(),
        }
    }
    pub async fn new(stun_urls: Vec<String>, loopback_only: bool) -> Result<Self> {
        if stun_urls
            .iter()
            .any(|url| url.len() > 200 || !url.starts_with("stun:") || url.contains('@'))
        {
            return Err(StageError::Invalid(
                "Only explicitly configured STUN endpoints are supported",
            ));
        }
        let mut setting = SettingEngineBuilder::default();
        if loopback_only {
            // The driver binds only 127.0.0.1 below, so no other interface is gathered.
            setting = setting.with_include_loopback_candidate(true);
        }
        let (ready_tx, ready) = watch::channel(0u8);
        let (closed_tx, closed) = watch::channel(false);
        let (gathered_tx, gathered) = watch::channel(false);
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_setting_engine(setting.build())
                .with_configuration(
                    RTCConfigurationBuilder::default()
                        .with_ice_servers(if stun_urls.is_empty() {
                            vec![]
                        } else {
                            vec![RTCIceServer {
                                urls: stun_urls,
                                ..Default::default()
                            }]
                        })
                        .build(),
                )
                .with_handler(Arc::new(ConnectionEvents {
                    closed: closed_tx.clone(),
                    gathered: gathered_tx,
                }))
                .with_udp_addrs(if loopback_only {
                    vec!["127.0.0.1:0"]
                } else {
                    vec!["0.0.0.0:0", "[::]:0"]
                })
                .with_data_channel_send_buffer_limit(256 * 1024)
                .build()
                .await
                .map_err(rtc_error)?,
        );
        let options = |id: u16| RTCDataChannelInit {
            ordered: true,
            negotiated: Some(id),
            protocol: "daat-locus-stage/1".into(),
            ..Default::default()
        };
        let control = pc
            .create_data_channel("control", Some(options(0)))
            .await
            .map_err(rtc_error)?;
        let bulk = pc
            .create_data_channel("content", Some(options(2)))
            .await
            .map_err(rtc_error)?;
        let (control_tx, incoming_control) = mpsc::channel(8);
        let (bulk_tx, incoming_bulk) = mpsc::channel(4);
        let mut readers = Vec::new();
        for (channel, tx, bit) in [
            (control.clone(), control_tx, 1u8),
            (bulk.clone(), bulk_tx, 2u8),
        ] {
            let closed_tx = closed_tx.clone();
            let mut closed = closed.clone();
            let ready_tx = ready_tx.clone();
            readers.push(tokio::spawn(async move {
                loop {
                    if *closed.borrow() {
                        break;
                    }
                    let event = tokio::select! {
                        _ = closed.changed() => break,
                        event = channel.poll() => event,
                    };
                    match event {
                        Some(DataChannelEvent::OnOpen) => {
                            ready_tx.send_modify(|value| *value |= bit);
                        }
                        Some(DataChannelEvent::OnMessage(message)) => {
                            let value = if message.is_string
                                || message.data.is_empty()
                                || message.data.len() > MAX_FRAME
                            {
                                Err(StageError::Invalid("RTC message exceeds protocol budget"))
                            } else {
                                Ok(message.data.to_vec())
                            };
                            // Each channel has bounded backpressure and wakes on connection shutdown.
                            tokio::select! {
                                _ = closed.changed() => break,
                                result = tx.send(value) => if result.is_err() { break; },
                            }
                        }
                        None
                        | Some(
                            DataChannelEvent::OnClosing
                            | DataChannelEvent::OnClose
                            | DataChannelEvent::OnError,
                        ) => break,
                        _ => {}
                    }
                }
                closed_tx.send_replace(true);
            }));
        }
        Ok(Self {
            pc,
            control,
            bulk,
            incoming_control,
            incoming_bulk,
            ready,
            closed,
            peer: None,
            gathered,
            readers,
        })
    }

    async fn local_signal(
        &self,
        identity: &DeviceIdentity,
        root: &TrustRoot,
        target: StageId,
        attempt: Attempt,
        offer: bool,
        now: i64,
    ) -> Result<SignedProof> {
        let mut gathered = self.gathered.clone();
        let description = if offer {
            self.pc.create_offer(None).await
        } else {
            self.pc.create_answer(None).await
        }
        .map_err(rtc_error)?;
        self.pc
            .set_local_description(description)
            .await
            .map_err(rtc_error)?;
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            gathered.wait_for(|complete| *complete),
        )
        .await
        .map_err(|_| StageError::Unavailable("RTC gathering timed out"))?
        .map_err(|_| StageError::Unavailable("RTC gathering stopped"))?;
        let description = self
            .pc
            .local_description()
            .await
            .ok_or(StageError::Unavailable("RTC description is unavailable"))?;
        if description.sdp.len() > 6 * 1024 {
            return Err(StageError::Capacity("RTC negotiation budget exceeded"));
        }
        let fingerprint = fingerprint(&description.sdp)?;
        identity.sign(
            "rtc-negotiation",
            &Negotiation {
                version: 1,
                group_id: root.group_id.clone(),
                source: identity.profile().stage_id,
                target,
                generation: attempt.generation,
                nonce: attempt.nonce,
                // Verifiers cap the future deadline at 120 seconds using their
                // own clock. Leave 30 seconds of skew headroom rather than
                // issuing exactly at that boundary. Expired proofs still fail.
                expires_at_ms: now.saturating_add(90_000),
                description,
                fingerprint,
            },
        )
    }
    pub async fn offer(
        &self,
        identity: &DeviceIdentity,
        root: &TrustRoot,
        target: StageId,
        attempt: Attempt,
        now: i64,
    ) -> Result<SignedProof> {
        self.local_signal(identity, root, target, attempt, true, now)
            .await
    }
    pub async fn apply_signal(
        &mut self,
        proof: &SignedProof,
        peer: &DeviceProfile,
        root: &TrustRoot,
        local: StageId,
        attempt: Attempt,
        now: i64,
    ) -> Result<()> {
        let value = verify_signal(proof, peer, root, local, attempt, now)?;
        self.pc
            .set_remote_description(value.description)
            .await
            .map_err(rtc_error)?;
        self.peer = Some(peer.clone());
        Ok(())
    }
    pub async fn answer(
        &self,
        identity: &DeviceIdentity,
        root: &TrustRoot,
        target: StageId,
        attempt: Attempt,
        now: i64,
    ) -> Result<SignedProof> {
        self.local_signal(identity, root, target, attempt, false, now)
            .await
    }
    pub async fn connected(&mut self) -> Result<()> {
        if self.peer.is_none() {
            return Err(StageError::Forbidden("RTC peer has not authenticated"));
        }
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while *self.ready.borrow() != 3 {
                if *self.closed.borrow() { return Err(StageError::Unavailable("RTC channel closed")); }
                tokio::select! {
                    result=self.ready.changed()=>result.map_err(|_| StageError::Unavailable("RTC channel closed"))?,
                    _=self.closed.changed()=>return Err(StageError::Unavailable("RTC channel closed")),
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| StageError::Unavailable("RTC connection timed out"))?
    }
    pub async fn send(&self, frame: &Envelope, bulk: bool) -> Result<usize> {
        if self.peer.is_none() {
            return Err(StageError::Forbidden("RTC peer has not authenticated"));
        }
        let channel = if bulk { &self.bulk } else { &self.control };
        send_frame(channel, frame).await
    }

    pub async fn receive(&mut self) -> Result<(Envelope, usize)> {
        if *self.closed.borrow() {
            return Err(StageError::Unavailable("RTC channel closed"));
        }
        let bytes = tokio::select! {biased;
            _=self.closed.changed()=>return Err(StageError::Unavailable("RTC channel closed")),
            value=self.incoming_control.recv()=>value,
            value=self.incoming_bulk.recv()=>value,
        }
        .ok_or(StageError::Unavailable("RTC channel closed"))??;
        Ok((Envelope::decode(&bytes)?, bytes.len()))
    }
    pub async fn close(&self) -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.pc.close())
            .await
            .map_err(|_| StageError::Unavailable("RTC close timed out"))?
            .map_err(rtc_error)
    }
}
#[derive(Clone)]
pub struct P2pSender {
    control: Arc<dyn DataChannel>,
    bulk: Arc<dyn DataChannel>,
    pc: Arc<dyn PeerConnection>,
}
impl Drop for P2pChannel {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let pc = self.pc.clone();
            runtime.spawn(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), pc.close()).await;
            });
        }
    }
}
impl P2pSender {
    pub async fn send(&self, frame: &Envelope, bulk: bool) -> Result<usize> {
        let channel = if bulk { &self.bulk } else { &self.control };
        send_frame(channel, frame).await
    }

    pub async fn close(&self) -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.pc.close())
            .await
            .map_err(|_| StageError::Unavailable("RTC close timed out"))?
            .map_err(rtc_error)
    }
}
struct ConnectionEvents {
    closed: watch::Sender<bool>,
    gathered: watch::Sender<bool>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for ConnectionEvents {
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if matches!(
            state,
            RTCPeerConnectionState::Disconnected
                | RTCPeerConnectionState::Failed
                | RTCPeerConnectionState::Closed
        ) {
            self.closed.send_replace(true);
        }
    }
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        self.gathered
            .send_replace(state == RTCIceGatheringState::Complete);
    }
}
async fn send_frame(channel: &Arc<dyn DataChannel>, frame: &Envelope) -> Result<usize> {
    if channel.ready_state().await.map_err(rtc_error)? != RTCDataChannelState::Open {
        return Err(StageError::Unavailable("RTC channel is not open"));
    }
    let bytes = frame.encode()?;
    channel
        .try_send(bytes.as_slice().into())
        .await
        .map_err(|error| {
            if matches!(error, webrtc::error::Error::ErrSendBufferFull) {
                StageError::Capacity("RTC send backlog is full")
            } else {
                rtc_error(error)
            }
        })?;
    Ok(bytes.len())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::protocol::Body;
    #[tokio::test]
    async fn real_loopback_channels_bind_signed_dtls_and_transfer_protocol() {
        let mut a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let root = a.initialize_group(1000, 100000).unwrap().root;
        let mut left = P2pChannel::new(vec![], true).await.unwrap();
        let mut right = P2pChannel::new(vec![], true).await.unwrap();
        let nonce = Uuid::new_v4();
        let offer = left
            .offer(
                &a,
                &root,
                b.profile().stage_id,
                Attempt {
                    generation: 1,
                    nonce,
                },
                1000,
            )
            .await
            .unwrap();
        assert!(
            verify_signal(
                &offer,
                &a.profile(),
                &root,
                b.profile().stage_id,
                Attempt {
                    generation: 1,
                    nonce
                },
                0,
            )
            .is_ok(),
            "a valid signed offer must tolerate the receiver clock being one second behind"
        );
        assert!(
            verify_signal(
                &offer,
                &a.profile(),
                &root,
                b.profile().stage_id,
                Attempt {
                    generation: 2,
                    nonce
                },
                1001
            )
            .is_err()
        );
        right
            .apply_signal(
                &offer,
                &a.profile(),
                &root,
                b.profile().stage_id,
                Attempt {
                    generation: 1,
                    nonce,
                },
                1001,
            )
            .await
            .unwrap();
        let answer = right
            .answer(
                &b,
                &root,
                a.profile().stage_id,
                Attempt {
                    generation: 1,
                    nonce,
                },
                1001,
            )
            .await
            .unwrap();
        left.apply_signal(
            &answer,
            &b.profile(),
            &root,
            a.profile().stage_id,
            Attempt {
                generation: 1,
                nonce,
            },
            1002,
        )
        .await
        .unwrap();
        let (l, r) = tokio::join!(left.connected(), right.connected());
        l.unwrap();
        r.unwrap();
        let frame = Envelope {
            version: 1,
            source: a.profile().stage_id,
            target: b.profile().stage_id,
            generation: 1,
            message_id: Uuid::new_v4(),
            invocation_id: None,
            in_reply_to: None,
            created_at_ms: 1000,
            expires_at_ms: 10000,
            body: Body::Heartbeat,
        };
        let sent = left.send(&frame, false).await.unwrap();
        let (received, count) =
            tokio::time::timeout(std::time::Duration::from_secs(5), right.receive())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(received.message_id, frame.message_id);
        assert_eq!(sent, count);
        let payload: Vec<u8> = (0..crate::stage::protocol::CHUNK_BYTES)
            .map(|index| (index % 251) as u8)
            .collect();
        let mut content = frame.clone();
        let hash = crate::stage::digest(&payload);
        content.body = Body::BlobChunk {
            content: crate::stage::protocol::BlobRef {
                hash: hash.clone(),
                size: payload.len() as u64,
                version: 1,
            },
            chunk: 0,
            chunk_hash: hash,
            bytes: payload.clone(),
        };
        let sent = right.sender().send(&content, true).await.unwrap();
        let (received, count) =
            tokio::time::timeout(std::time::Duration::from_secs(5), left.receive())
                .await
                .unwrap()
                .unwrap();
        let Body::BlobChunk { bytes, .. } = received.body else {
            panic!("expected bulk content")
        };
        assert_eq!(bytes, payload);
        assert_eq!(sent, count);
        left.control
            .send_text("not a binary protocol frame")
            .await
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), right.receive(),)
                .await
                .unwrap(),
            Err(StageError::Invalid(_))
        ));
        left.close().await.unwrap();
        let disconnected = tokio::time::timeout(std::time::Duration::from_secs(2), right.receive())
            .await
            .expect("closing RTC must wake the receiver without waiting for heartbeat expiry");
        assert!(disconnected.is_err());
        right.close().await.unwrap();
    }
}
