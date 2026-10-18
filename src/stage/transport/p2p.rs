//! Real ICE/DTLS/SCTP channels. Signed SDP binds the DTLS fingerprint to an
//! enrolled device and this negotiation; signaling conveys no Actor traffic.
use crate::stage::{
    Result, StageError, StageId,
    identity::{DeviceIdentity, DeviceProfile, SignedProof, TrustRoot},
    protocol::{Envelope, MAX_FRAME},
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit, RTCDataChannelState},
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
        RTCIceCandidateInit, RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState,
        RTCSdpType, RTCSessionDescription, SettingEngineBuilder,
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
    traversal: Arc<super::punch::Traversal>,
    compact: Arc<AtomicBool>,
    remote: Arc<tokio::sync::Mutex<Option<RTCSessionDescription>>>,
    trickle: Arc<AtomicBool>,
    remote_ready: Arc<tokio::sync::Notify>,
    peer_ready: watch::Sender<bool>,
    signaled: AtomicBool,
}
const TRICKLE: &str = "a=x-daat-trickle:1";
const UPDATE: &str = "a=x-daat-update:1";
const READY: &str = "a=x-daat-ready:1";

#[derive(Clone)]
pub(crate) struct CandidateUpdates {
    pc: Arc<dyn PeerConnection>,
    traversal: Arc<super::punch::Traversal>,
    remote: Arc<tokio::sync::Mutex<Option<RTCSessionDescription>>>,
    trickle: Arc<AtomicBool>,
    remote_ready: Arc<tokio::sync::Notify>,
    ready: watch::Receiver<u8>,
    closed: watch::Receiver<bool>,
    peer_ready: watch::Sender<bool>,
}
fn fixed_sdp(sdp: &str) -> Vec<&str> {
    sdp.lines()
        .filter(|line| {
            !line.starts_with("a=candidate:")
                && *line != "a=end-of-candidates"
                && !line.starts_with("a=x-daat-punch:")
                && *line != UPDATE
                && *line != READY
        })
        .collect()
}
impl CandidateUpdates {
    pub async fn apply(&self, value: Negotiation) -> Result<()> {
        let mut remote = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let ready = self.remote_ready.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                let remote = self.remote.lock().await;
                if remote.is_some() {
                    break remote;
                }
                drop(remote);
                ready.await;
            }
        })
        .await
        .map_err(|_| StageError::Forbidden("ICE update preceded remote SDP"))?;
        let previous = remote.as_ref().unwrap();
        if !self.trickle.load(Ordering::Relaxed)
            || value.description.sdp_type != previous.sdp_type
            || fixed_sdp(&value.description.sdp) != fixed_sdp(&previous.sdp)
        {
            return Err(StageError::Forbidden(
                "ICE update changed authenticated transport",
            ));
        }
        let mut updated = previous.clone();
        let previous_candidates = previous
            .sdp
            .lines()
            .filter(|line| line.starts_with("a=candidate:"))
            .collect::<std::collections::HashSet<_>>();
        let mid = previous
            .sdp
            .lines()
            .find_map(|line| line.strip_prefix("a=mid:"))
            .unwrap_or("0")
            .to_owned();
        for line in value
            .description
            .sdp
            .lines()
            .filter(|line| line.starts_with("a=candidate:"))
        {
            if !previous_candidates.contains(line) {
                if updated
                    .sdp
                    .lines()
                    .filter(|line| line.starts_with("a=candidate:"))
                    .count()
                    >= 32
                {
                    return Err(StageError::Capacity("ICE candidate budget exceeded"));
                }
                self.pc
                    .add_ice_candidate(RTCIceCandidateInit {
                        candidate: line[2..].to_owned(),
                        sdp_mid: Some(mid.clone()),
                        sdp_mline_index: Some(0),
                        ..Default::default()
                    })
                    .await
                    .map_err(rtc_error)?;
                updated.sdp.push_str(&format!("{line}\r\n"));
            }
        }
        self.traversal.remote(&value.description);
        if value.description.sdp.lines().any(|line| line == READY) {
            self.peer_ready.send_replace(true);
        }
        *remote = Some(updated);
        Ok(())
    }
    pub async fn publish(
        self,
        identity: Arc<DeviceIdentity>,
        root: TrustRoot,
        target: StageId,
        attempt: Attempt,
        tx: mpsc::Sender<crate::stage::signaling::SignalCommand>,
    ) {
        let mut last = String::new();
        let started = std::time::Instant::now();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
        while started.elapsed() < std::time::Duration::from_secs(20) {
            interval.tick().await;
            if *self.closed.borrow() {
                break;
            }
            if !self.trickle.load(Ordering::Relaxed) {
                continue;
            }
            let Some(mut description) = self.pc.local_description().await else {
                break;
            };
            self.traversal.local(&mut description);
            description.sdp.push_str(&format!(
                "{}\r\n{TRICKLE}\r\n{UPDATE}\r\n",
                super::wire::SDP
            ));
            let ready = *self.ready.borrow() == 3;
            if ready {
                description.sdp.push_str(&format!("{READY}\r\n"));
            }
            if description.sdp == last || description.sdp.len() > 6144 {
                continue;
            }
            last = description.sdp.clone();
            let Ok(fingerprint) = fingerprint(&description.sdp) else {
                break;
            };
            let Ok(proof) = identity.sign(
                "rtc-negotiation",
                &Negotiation {
                    version: 1,
                    group_id: root.group_id.clone(),
                    source: identity.profile().stage_id,
                    target,
                    generation: attempt.generation,
                    nonce: attempt.nonce,
                    expires_at_ms: chrono::Utc::now().timestamp_millis() + 90_000,
                    description,
                    fingerprint,
                },
            ) else {
                break;
            };
            if !matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    tx.send(crate::stage::signaling::SignalCommand::Offer(proof))
                )
                .await,
                Ok(Ok(()))
            ) {
                break;
            }
            if ready {
                break;
            }
        }
    }
}
impl P2pChannel {
    pub(crate) fn signal_updates(&self) -> CandidateUpdates {
        self.signaled.store(true, Ordering::Relaxed);
        self.updates()
    }
    pub(crate) fn updates(&self) -> CandidateUpdates {
        CandidateUpdates {
            pc: self.pc.clone(),
            traversal: self.traversal.clone(),
            remote: self.remote.clone(),
            trickle: self.trickle.clone(),
            remote_ready: self.remote_ready.clone(),
            ready: self.ready.clone(),
            closed: self.closed.clone(),
            peer_ready: self.peer_ready.clone(),
        }
    }
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
            compact: self.compact.clone(),
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
        // Defaults exhaust a candidate after about 1.6 seconds, before a slow
        // answer has finished gathering. Keep checks alive through gathering
        // and the bounded connection window instead of waiting without probes.
        setting = setting
            .with_ice_connection_attempts(Some(std::time::Duration::from_millis(200)), Some(125));
        if loopback_only {
            // The driver binds only 127.0.0.1 below, so no other interface is gathered.
            setting = setting.with_include_loopback_candidate(true);
        }
        let (ready_tx, ready) = watch::channel(0u8);
        let (closed_tx, closed) = watch::channel(false);
        let (gathered_tx, gathered) = watch::channel(false);
        let traversal = super::punch::Traversal::new()?;
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_runtime(traversal.runtime())
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
                    traversal: traversal.clone(),
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
                            tracing::trace!(
                                channel = bit,
                                bytes = message.data.len(),
                                "RTC frame received"
                            );
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
            traversal,
            compact: Arc::new(AtomicBool::new(false)),
            remote: Arc::new(tokio::sync::Mutex::new(None)),
            trickle: Arc::new(AtomicBool::new(false)),
            remote_ready: Arc::new(tokio::sync::Notify::new()),
            peer_ready: watch::channel(false).0,
            signaled: AtomicBool::new(false),
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
        // One unreachable interface can keep STUN gathering active past the
        // negotiation budget. Send the candidates already gathered instead of
        // discarding working routes while waiting for every STUN transaction.
        let started = std::time::Instant::now();
        let complete = loop {
            if *gathered.borrow() {
                break true;
            }
            let current = self.pc.local_description().await;
            let usable = current.as_ref().is_some_and(|sdp| {
                sdp.sdp.lines().any(|line| {
                    let fields = line.split_whitespace().collect::<Vec<_>>();
                    line.starts_with("a=candidate:")
                        && fields.len() >= 8
                        && (fields[7] == "srflx"
                            || fields[4].parse::<std::net::Ipv6Addr>().is_ok_and(|ip| {
                                let first = ip.segments()[0];
                                (0x2000..0x4000).contains(&first)
                            })
                            || self.trickle.load(Ordering::Relaxed))
                })
            });
            if started.elapsed() >= std::time::Duration::from_millis(300) && usable {
                break false;
            }
            if started.elapsed() >= std::time::Duration::from_secs(5) {
                break false;
            }
            tokio::select! {
                result = gathered.changed() => { result.map_err(|_| StageError::Unavailable("RTC gathering stopped"))?; },
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
            }
        };
        let mut description = self
            .pc
            .local_description()
            .await
            .ok_or(StageError::Unavailable("RTC description is unavailable"))?;
        let candidates = description
            .sdp
            .lines()
            .filter(|line| line.starts_with("a=candidate:"))
            .count();
        if candidates == 0 {
            return Err(StageError::Unavailable("RTC has no local candidates"));
        }
        if !complete {
            tracing::debug!(
                candidates,
                "RTC gathering budget reached; using collected candidates"
            );
        }
        self.traversal.local(&mut description);
        description
            .sdp
            .push_str(&format!("{}\r\n", super::wire::SDP));
        description.sdp.push_str(&format!("{TRICKLE}\r\n"));
        tracing::debug!(
            phase = if offer {
                "offer_candidates"
            } else {
                "answer_candidates"
            },
            elapsed_ms = started.elapsed().as_millis() as u64,
            candidates,
            complete,
            "RTC connection phase completed"
        );
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
        self.compact.store(
            value
                .description
                .sdp
                .lines()
                .any(|line| line == super::wire::SDP),
            Ordering::Relaxed,
        );
        self.pc
            .set_remote_description(value.description.clone())
            .await
            .map_err(rtc_error)?;
        self.traversal.remote(&value.description);
        self.peer = Some(peer.clone());
        self.trickle.store(
            value.description.sdp.lines().any(|line| line == TRICKLE),
            Ordering::Relaxed,
        );
        *self.remote.lock().await = Some(value.description);
        self.remote_ready.notify_waiters();
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
        let started = std::time::Instant::now();
        if self.peer.is_none() {
            return Err(StageError::Forbidden("RTC peer has not authenticated"));
        }
        let mut peer_ready = self.peer_ready.subscribe();
        let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while *self.ready.borrow() != 3 || (self.signaled.load(Ordering::Relaxed) && self.trickle.load(Ordering::Relaxed) && !*peer_ready.borrow()) {
                if *self.closed.borrow() { return Err(StageError::Unavailable("RTC channel closed")); }
                tokio::select! {
                    result=self.ready.changed()=>result.map_err(|_| StageError::Unavailable("RTC channel closed"))?,
                    _=self.closed.changed()=>return Err(StageError::Unavailable("RTC channel closed")),
                    result=peer_ready.changed()=>result.map_err(|_| StageError::Unavailable("RTC readiness signal stopped"))?,
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| StageError::Unavailable("RTC connection timed out"))?;
        tracing::debug!(
            phase = "ice_dtls_sctp",
            elapsed_ms = started.elapsed().as_millis() as u64,
            success = result.is_ok(),
            "RTC connection phase completed"
        );
        result
    }
    pub async fn send(&self, frame: &Envelope, bulk: bool) -> Result<usize> {
        if self.peer.is_none() {
            return Err(StageError::Forbidden("RTC peer has not authenticated"));
        }
        let channel = if bulk { &self.bulk } else { &self.control };
        channel.writable().await.map_err(rtc_error)?;
        send_frame(channel, frame, self.compact.load(Ordering::Relaxed)).await
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
        let format = if self.compact.load(Ordering::Relaxed) {
            super::wire::Format::BinaryZstd
        } else {
            super::wire::Format::Json
        };
        Ok((format.decode(&bytes)?, bytes.len()))
    }
    pub async fn close(&self) -> Result<()> {
        self.traversal.stop();
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
    compact: Arc<AtomicBool>,
}
impl Drop for P2pChannel {
    fn drop(&mut self) {
        self.traversal.stop();
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
    pub(crate) async fn wait_writable(&self, bulk: bool) -> Result<()> {
        let channel = if bulk { &self.bulk } else { &self.control };
        channel.writable().await.map_err(rtc_error)
    }
    pub async fn send(&self, frame: &Envelope, bulk: bool) -> Result<usize> {
        let channel = if bulk { &self.bulk } else { &self.control };
        send_frame(channel, frame, self.compact.load(Ordering::Relaxed)).await
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
    traversal: Arc<super::punch::Traversal>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for ConnectionEvents {
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        tracing::debug!(?state, "RTC transport state changed");
        if matches!(
            state,
            RTCPeerConnectionState::Connected
                | RTCPeerConnectionState::Disconnected
                | RTCPeerConnectionState::Failed
                | RTCPeerConnectionState::Closed
        ) {
            self.traversal.stop();
        }
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            self.closed.send_replace(true);
        }
    }
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        self.gathered
            .send_replace(state == RTCIceGatheringState::Complete);
    }
}
async fn send_frame(
    channel: &Arc<dyn DataChannel>,
    frame: &Envelope,
    compact: bool,
) -> Result<usize> {
    if channel.ready_state().await.map_err(rtc_error)? != RTCDataChannelState::Open {
        return Err(StageError::Unavailable("RTC channel is not open"));
    }
    let format = if compact {
        super::wire::Format::BinaryZstd
    } else {
        super::wire::Format::Json
    };
    let bytes = format.encode(frame)?;
    channel
        .send(bytes.as_slice().into())
        .await
        .map_err(rtc_error)?;
    tracing::trace!(bytes = bytes.len(), "RTC frame accepted by send buffer");
    Ok(bytes.len())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::protocol::Body;
    #[tokio::test]
    async fn incremental_candidates_connect_without_replacing_authenticated_sdp() {
        let mut a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let root = a.initialize_group(1000, 100000).unwrap().root;
        let attempt = Attempt {
            generation: 1,
            nonce: Uuid::new_v4(),
        };
        let mut left = P2pChannel::new(vec![], true).await.unwrap();
        let mut right = P2pChannel::new(vec![], true).await.unwrap();
        let offer = left
            .offer(&a, &root, b.profile().stage_id, attempt, 1000)
            .await
            .unwrap();
        let strip = |proof: &SignedProof, identity: &DeviceIdentity| {
            let mut value: Negotiation = proof
                .verify("rtc-negotiation", &identity.profile().public_key)
                .unwrap();
            value.description.sdp = value
                .description
                .sdp
                .lines()
                .filter(|line| !line.starts_with("a=candidate:") && *line != "a=end-of-candidates")
                .map(|line| format!("{line}\r\n"))
                .collect();
            identity.sign("rtc-negotiation", &value).unwrap()
        };
        right
            .apply_signal(
                &strip(&offer, &a),
                &a.profile(),
                &root,
                b.profile().stage_id,
                attempt,
                1000,
            )
            .await
            .unwrap();
        let answer = right
            .answer(&b, &root, a.profile().stage_id, attempt, 1000)
            .await
            .unwrap();
        left.apply_signal(
            &strip(&answer, &b),
            &b.profile(),
            &root,
            a.profile().stage_id,
            attempt,
            1000,
        )
        .await
        .unwrap();
        let update = |proof: &SignedProof, identity: &DeviceIdentity| {
            let mut value: Negotiation = proof
                .verify("rtc-negotiation", &identity.profile().public_key)
                .unwrap();
            value.description.sdp.push_str(&format!("{UPDATE}\r\n"));
            value
        };
        let mut changed = update(&offer, &a);
        changed.description.sdp = changed
            .description
            .sdp
            .lines()
            .map(|line| {
                if line.starts_with("a=ice-pwd:") {
                    "a=ice-pwd:substituted-password\r\n".into()
                } else {
                    format!("{line}\r\n")
                }
            })
            .collect();
        assert!(right.updates().apply(changed).await.is_err());
        right.updates().apply(update(&offer, &a)).await.unwrap();
        left.updates().apply(update(&answer, &b)).await.unwrap();
        let (l, r) = tokio::join!(left.connected(), right.connected());
        l.unwrap();
        r.unwrap();
        let left_updates = left.signal_updates();
        let right_updates = right.signal_updates();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), left.connected())
                .await
                .is_err(),
            "local channel open must not substitute for peer readiness"
        );
        let mut ready_offer = update(&offer, &a);
        ready_offer
            .description
            .sdp
            .push_str(&format!("{READY}\r\n"));
        let mut ready_answer = update(&answer, &b);
        ready_answer
            .description
            .sdp
            .push_str(&format!("{READY}\r\n"));
        right_updates.apply(ready_offer).await.unwrap();
        left_updates.apply(ready_answer).await.unwrap();
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
        left.send(&frame, false).await.unwrap();
        let (received, _) =
            tokio::time::timeout(std::time::Duration::from_secs(3), right.receive())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(received.message_id, frame.message_id);
        left.close().await.unwrap();
        right.close().await.unwrap();
    }
    #[tokio::test]
    async fn real_loopback_channels_bind_signed_dtls_and_transfer_protocol() {
        check_channels(vec![]).await;
    }
    #[tokio::test]
    async fn unresponsive_stun_does_not_discard_working_candidates() {
        // A device can have one working interface and another whose STUN server
        // never responds. Gathering every address must not prevent signed SDP.
        let blackhole = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        check_channels(vec![format!("stun:{}", blackhole.local_addr().unwrap())]).await;
        let mut request = [0u8; 1500];
        blackhole
            .try_recv_from(&mut request)
            .expect("STUN was not attempted");
    }
    #[tokio::test]
    async fn delayed_answer_and_transient_loss_preserve_authenticated_channels() {
        use std::{
            net::SocketAddr,
            time::{Duration, Instant},
        };
        let mut a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let root = a.initialize_group(1000, 100000).unwrap().root;
        let mut left = P2pChannel::new(vec![], true).await.unwrap();
        let mut right = P2pChannel::new(vec![], true).await.unwrap();
        let attempt = Attempt {
            generation: 1,
            nonce: Uuid::new_v4(),
        };
        let offer = left
            .offer(&a, &root, b.profile().stage_id, attempt, 1000)
            .await
            .unwrap();
        let original: Negotiation = offer
            .verify("rtc-negotiation", &a.profile().public_key)
            .unwrap();
        let fields = original
            .description
            .sdp
            .lines()
            .find_map(|line| line.strip_prefix("a=candidate:"))
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>();
        let left_addr: SocketAddr = format!("{}:{}", fields[4], fields[5]).parse().unwrap();
        let proxy = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let available = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let forwarding_available = available.clone();
        let established = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let forwarding_established = established.clone();
        // Model two endpoint filters: packets pass only while both sides have
        // sent recently. A late answer must not arrive after the answerer's
        // checks have already stopped, even though connected() is still waiting.
        let forwarding = tokio::spawn(async move {
            let mut buffer = [0u8; 65536];
            let mut left_seen = None;
            let mut right_seen = None;
            let mut right_addr = None;
            loop {
                let (n, source) = proxy.recv_from(&mut buffer).await.unwrap();
                if !forwarding_available.load(std::sync::atomic::Ordering::Relaxed) {
                    continue;
                }
                let now = Instant::now();
                // A short initial filter catches exhausted setup checks. Once
                // established, retain the NAT mapping across the six-second
                // outage; requiring both independent consent timers to fire
                // within 400ms of one another models a different failure.
                let filter_lifetime = if forwarding_established.load(Ordering::Relaxed) {
                    Duration::from_secs(30)
                } else {
                    Duration::from_millis(400)
                };
                let target = if source == left_addr {
                    left_seen = Some(now);
                    right_addr.filter(|_| {
                        right_seen.is_some_and(|seen| now.duration_since(seen) < filter_lifetime)
                    })
                } else {
                    right_addr = Some(source);
                    right_seen = Some(now);
                    left_seen
                        .filter(|seen| now.duration_since(*seen) < filter_lifetime)
                        .map(|_| left_addr)
                };
                if let Some(target) = target {
                    proxy.send_to(&buffer[..n], target).await.unwrap();
                }
            }
        });
        struct Forwarder(tokio::task::JoinHandle<()>);
        impl Drop for Forwarder {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _forwarder = Forwarder(forwarding);
        let routed = |proof: &SignedProof, identity: &DeviceIdentity| {
            let mut signal: Negotiation = proof
                .verify("rtc-negotiation", &identity.profile().public_key)
                .unwrap();
            signal.description.sdp = signal
                .description
                .sdp
                .lines()
                .map(|line| {
                    if let Some(candidate) = line.strip_prefix("a=candidate:") {
                        let mut parts = candidate
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        parts[4] = proxy_addr.ip().to_string();
                        parts[5] = proxy_addr.port().to_string();
                        format!("a=candidate:{}\r\n", parts.join(" "))
                    } else {
                        format!("{line}\r\n")
                    }
                })
                .collect();
            identity.sign("rtc-negotiation", &signal).unwrap()
        };
        right
            .apply_signal(
                &routed(&offer, &a),
                &a.profile(),
                &root,
                b.profile().stage_id,
                attempt,
                1000,
            )
            .await
            .unwrap();
        let answer = right
            .answer(&b, &root, a.profile().stage_id, attempt, 1000)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
        left.apply_signal(
            &routed(&answer, &b),
            &b.profile(),
            &root,
            a.profile().stage_id,
            attempt,
            1000,
        )
        .await
        .unwrap();
        let (l, r) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(left.connected(), right.connected())
        })
        .await
        .unwrap();
        l.unwrap();
        r.unwrap();
        established.store(true, Ordering::Relaxed);
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
        left.send(&frame, false).await.unwrap();
        let (received, _) = tokio::time::timeout(Duration::from_secs(2), right.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received.message_id, frame.message_id);
        // ICE Disconnected is temporary, unlike Failed/Closed. Keep the same
        // signed DTLS/SCTP channel through a recoverable interruption rather
        // than turning the event into a permanent receiver error.
        available.store(false, std::sync::atomic::Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(6)).await;
        available.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut resumed = frame.clone();
        resumed.message_id = Uuid::new_v4();
        left.send(&resumed, false).await.unwrap();
        let (received, _) = tokio::time::timeout(Duration::from_secs(10), right.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received.message_id, resumed.message_id);
        left.close().await.unwrap();
        right.close().await.unwrap();
    }
    async fn check_channels(stun_urls: Vec<String>) {
        let mut a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let root = a.initialize_group(1000, 100000).unwrap().root;
        let mut left = P2pChannel::new(stun_urls, true).await.unwrap();
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
