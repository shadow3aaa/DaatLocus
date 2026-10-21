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
    Relay,
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
    pub fn attempt(&mut self, route: Route, now: i64) -> Result<()> {
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
/// `binding` is a TLS exporter of the end-to-end device stream.
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
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
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
        assert!(state.attempt(Route::Relay, 2199).is_err());
        state.attempt(Route::Relay, 2200).unwrap();
        assert_eq!(state.status, ConnectionStatus::Connecting);
        state.failed(2300);
        assert_eq!(state.status, ConnectionStatus::Unreachable);
        state.online(2, 2400);
        assert_eq!(state.status, ConnectionStatus::Online);
        assert_eq!(state.retry_at_ms, 0);
        assert_eq!(state.retries, 0);
    }
}
