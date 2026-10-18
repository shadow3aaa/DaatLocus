//! Negotiated link generations, deterministic dial preference and bounded retry.
use super::{
    Result, StageError, StageId,
    identity::{DeviceIdentity, SignedProof},
    ledger::StageLedger,
    transport::tls::VerifiedPeer,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Lan,
    Ssh,
    P2p,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Disabled,
    Connecting,
    Online,
    Unreachable,
}
#[derive(Clone, Debug, Serialize)]
pub struct LinkState {
    pub status: ConnectionStatus,
    pub route: Option<Route>,
    pub generation: u64,
    pub retries: u32,
    pub retry_at_ms: i64,
    pub since_ms: i64,
}
impl Default for LinkState {
    fn default() -> Self {
        Self {
            status: ConnectionStatus::Disabled,
            route: None,
            generation: 0,
            retries: 0,
            retry_at_ms: 0,
            since_ms: 0,
        }
    }
}
impl LinkState {
    pub fn attempt(&mut self, route: Route, now: i64, explicit_ssh: bool) -> Result<()> {
        if explicit_ssh && route != Route::Ssh {
            return Err(StageError::Forbidden("Explicit SSH route must be retained"));
        }
        if self.status == ConnectionStatus::Online {
            return Err(StageError::Conflict("Healthy connection is already active"));
        }
        if now < self.retry_at_ms {
            return Err(StageError::Unavailable("Peer connection is backing off"));
        }
        self.status = ConnectionStatus::Connecting;
        self.route = Some(route);
        self.retry_at_ms = 0;
        self.since_ms = now;
        Ok(())
    }
    pub fn online(&mut self, generation: u64, now: i64) {
        self.status = ConnectionStatus::Online;
        self.generation = generation;
        self.since_ms = now;
        self.retries = 0;
        self.retry_at_ms = 0;
    }
    pub fn failed(&mut self, now: i64) {
        // Retry scheduling is independent of peer availability.
        self.status = ConnectionStatus::Unreachable;
        self.retries = self.retries.saturating_add(1);
        let delay = 1000i64.saturating_mul(1i64 << self.retries.min(6));
        self.retry_at_ms = now.saturating_add(delay.min(10_000));
    }
    pub fn can_migrate(&self, preferred: Route, now: i64, explicit_ssh: bool) -> bool {
        self.status == ConnectionStatus::Online
            && !explicit_ssh
            && self.route != Some(preferred)
            && now.saturating_sub(self.since_ms) >= 30_000
    }
}
pub fn automatic_dialer(local: StageId, remote: StageId) -> bool {
    local < remote
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    version: u16,
    source: StageId,
    target: StageId,
    nonce: Uuid,
    generation: u64,
    committed_nonce: Option<Uuid>,
    binding: String,
    expires_at_ms: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    phase: String,
    generation: u64,
    nonce: Uuid,
}
async fn write<S: AsyncWrite + Unpin, T: Serialize>(stream: &mut S, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| StageError::Invalid("Cannot encode link handshake"))?;
    if bytes.len() > 12 * 1024 {
        return Err(StageError::Capacity("Link handshake exceeds budget"));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}
async fn read<S: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(stream: &mut S) -> Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > 12 * 1024 {
        return Err(StageError::Invalid("Link handshake exceeds budget"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| StageError::Invalid("Invalid link handshake"))
}
/// Only the deterministic dialer chooses a path. The other device waits for
/// its signed preface before taking the commit lock, so crossed LAN/RTC races
/// cannot each hold a lock while waiting on a different physical connection.
pub(crate) async fn read_preface<S: AsyncRead + Unpin>(stream: &mut S) -> Result<SignedProof> {
    tokio::time::timeout(std::time::Duration::from_secs(10), read(stream))
        .await
        .map_err(|_| StageError::Unavailable("Peer path selection timed out"))?
}
pub(crate) async fn read_p2p_preface(
    channel: &mut super::transport::p2p::P2pChannel,
) -> Result<SignedProof> {
    let (frame, _) = tokio::time::timeout(std::time::Duration::from_secs(10), channel.receive())
        .await
        .map_err(|_| StageError::Unavailable("Peer path selection timed out"))??;
    let super::protocol::Body::ChannelHello { proof } = frame.body else {
        return Err(StageError::Forbidden(
            "Expected channel identity confirmation",
        ));
    };
    Ok(proof)
}
/// `binding` is a TLS exporter or the verified signed DTLS negotiation binding.
/// No business traffic is admitted until both sides confirm their durable commit.
pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    identity: &DeviceIdentity,
    ledger: &StageLedger<'_>,
    verified: &VerifiedPeer,
    binding: &str,
    now: i64,
    preface: Option<SignedProof>,
) -> Result<(u64, Uuid)> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let current = ledger.peer(verified.stage_id(), now)?;
        if current.device.tls_sha256 != verified.certificate_hash() || binding.len() != 64 {
            return Err(StageError::Forbidden("Link identity binding changed"));
        }
        let local = identity.profile().stage_id;
        let hello = Hello {
            version: 1,
            source: local,
            target: verified.stage_id(),
            nonce: Uuid::new_v4(),
            generation: current.generation,
            committed_nonce: current.connection_nonce,
            binding: binding.into(),
            expires_at_ms: now.saturating_add(30_000),
        };
        write(stream, &identity.sign("stage-channel", &hello)?).await?;
        let proof: SignedProof = match preface {
            Some(proof) => proof,
            None => read(stream).await?,
        };
        let foreign: Hello = proof.verify("stage-channel", &current.device.public_key)?;
        if foreign.version != 1
            || foreign.source != verified.stage_id()
            || foreign.target != local
            || foreign.nonce.is_nil()
            || foreign.binding != binding
            || foreign.expires_at_ms <= now
            || foreign.expires_at_ms > now.saturating_add(60_000)
        {
            return Err(StageError::Forbidden("Peer link handshake is inconsistent"));
        }
        let recovered = ledger.recover_connection(
            verified.stage_id(),
            foreign.generation,
            foreign.committed_nonce,
            now,
        )?;
        let next = recovered
            .max(foreign.generation)
            .checked_add(1)
            .ok_or(StageError::Capacity("Link generation exhausted"))?;
        let nonce = if automatic_dialer(local, verified.stage_id()) {
            hello.nonce
        } else {
            foreign.nonce
        };
        ledger.prepare_connection(verified.stage_id(), next, nonce, now)?;
        for phase in ["prepared", "committed"] {
            if phase == "committed" {
                ledger.commit_generation(verified.stage_id(), recovered, next, nonce, now)?;
            }
            write(
                stream,
                &Commit {
                    phase: phase.into(),
                    generation: next,
                    nonce,
                },
            )
            .await?;
            let reply: Commit = read(stream).await?;
            if reply.phase != phase || reply.generation != next || reply.nonce != nonce {
                return Err(StageError::Conflict("Peer did not confirm link generation"));
            }
        }
        Ok((next, nonce))
    })
    .await
    .map_err(|_| StageError::Unavailable("Link handshake timed out"))?
}
pub async fn handshake_p2p(
    channel: &mut super::transport::p2p::P2pChannel,
    identity: &DeviceIdentity,
    ledger: &StageLedger<'_>,
    attempt: super::transport::p2p::Attempt,
    now: i64,
    preface: Option<SignedProof>,
) -> Result<(u64, Uuid)> {
    use super::protocol::{Body, Envelope};
    let verified = channel.verified_peer()?;
    let current = ledger.peer(verified.stage_id(), now)?;
    let local = identity.profile().stage_id;
    let binding = super::digest(
        format!(
            "{}:{}:{}:{}",
            local.min(verified.stage_id()),
            local.max(verified.stage_id()),
            attempt.generation,
            attempt.nonce
        )
        .as_bytes(),
    );
    let hello = Hello {
        version: 1,
        source: local,
        target: verified.stage_id(),
        nonce: attempt.nonce,
        generation: current.generation,
        committed_nonce: current.connection_nonce,
        binding: binding.clone(),
        expires_at_ms: now + 30000,
    };
    let frame = |body| Envelope {
        version: 1,
        source: local,
        target: verified.stage_id(),
        generation: attempt.generation,
        message_id: Uuid::new_v4(),
        invocation_id: None,
        in_reply_to: None,
        created_at_ms: now,
        expires_at_ms: now + 60000,
        body,
    };
    tokio::time::timeout(std::time::Duration::from_secs(10),async {
        channel.send(&frame(Body::ChannelHello{proof:identity.sign("stage-channel",&hello)?}),false).await?;
        let proof=match preface {Some(proof)=>proof,None=>read_p2p_preface(channel).await?};
        let foreign:Hello=proof.verify("stage-channel",&current.device.public_key)?;
        if foreign.version!=1||foreign.source!=verified.stage_id()||foreign.target!=local||foreign.binding!=binding||foreign.nonce!=attempt.nonce||foreign.expires_at_ms<=now||foreign.expires_at_ms>now+60000{return Err(StageError::Forbidden("Invalid DTLS channel confirmation"));}
        let recovered=ledger.recover_connection(verified.stage_id(),foreign.generation,foreign.committed_nonce,now)?;
        let next=recovered.max(foreign.generation)+1;if next!=attempt.generation{return Err(StageError::Conflict("Negotiated generation requires recovery"));}
        ledger.prepare_connection(verified.stage_id(),next,attempt.nonce,now)?;
        for phase in ["prepared","committed"] {
            if phase=="committed"{ledger.commit_generation(verified.stage_id(),recovered,next,attempt.nonce,now)?;}
            channel.send(&frame(Body::ChannelCommit{phase:phase.into(),generation:next,nonce:attempt.nonce}),false).await?;
            let (response,_)=channel.receive().await?;
            if !matches!(response.body,Body::ChannelCommit{phase:received,generation,nonce}if received==phase&&generation==next&&nonce==attempt.nonce){return Err(StageError::Conflict("Peer did not confirm DTLS generation"));}
        }Ok((next,attempt.nonce))
    }).await.map_err(|_|StageError::Unavailable("DTLS channel confirmation timed out"))?
}

#[cfg(test)]
mod availability_tests {
    use super::*;
    #[test]
    fn disconnected_peer_is_offline_while_retry_remains_scheduled() {
        let mut state = LinkState::default();
        state.online(1, 100);
        state.failed(200);
        assert_eq!(state.status, ConnectionStatus::Unreachable);
        assert_eq!(state.retry_at_ms, 2200);
        assert!(state.attempt(Route::P2p, 2199, false).is_err());
        state.attempt(Route::P2p, 2200, false).unwrap();
        assert_eq!(state.status, ConnectionStatus::Connecting);
        state.failed(2300);
        assert_eq!(state.status, ConnectionStatus::Unreachable);
        state.online(2, 2400);
        assert_eq!(state.status, ConnectionStatus::Online);
        assert_eq!(state.retry_at_ms, 0);
        assert_eq!(state.retries, 0);
    }
}
