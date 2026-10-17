//! Account-owned Actor identity and conservative execution fencing. Devices are
//! temporary workers and App providers, never owners of this Actor's identity.
use super::{
    Result, StageError, StageId,
    identity::{Enrollment, SignedProof},
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use uuid::Uuid;
pub const ACTOR_ID: &str = "core";
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub sequence: u64,
    pub hash: String,
}
impl Default for Head {
    fn default() -> Self {
        Self {
            sequence: 0,
            hash: "0".repeat(64),
        }
    }
}
impl Head {
    pub fn validate(&self) -> Result<()> {
        if self.sequence > 9_007_199_254_740_991
            || self.hash.len() != 64
            || hex::decode(&self.hash).is_err()
        {
            return Err(StageError::Invalid("Invalid shared Actor state head"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Advertise {
        head: Head,
        eligible: bool,
        priority: u16,
    },
    Claim {
        head: Head,
    },
    Renew {
        epoch: u64,
        head: Head,
    },
    Commit {
        epoch: u64,
        previous: Head,
        head: Head,
    },
    Release {
        epoch: u64,
    },
    Reconcile {
        epoch: u64,
        head: Head,
    },
    Inspect,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acknowledgement {
    pub enrollment: Enrollment,
    pub proof: SignedProof,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaAcknowledgement {
    pub version: u16,
    pub group_id: String,
    pub stage_id: StageId,
    pub leader: StageId,
    pub epoch: u64,
    pub head: Head,
    pub expires_at_ms: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub epoch: u64,
    pub leader: StageId,
    pub expires_at_ms: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub stage_id: StageId,
    pub head: Head,
    pub priority: u16,
    pub eligible: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub request_id: Uuid,
    pub accepted: bool,
    pub server_time_ms: i64,
    pub head: Head,
    pub epoch: u64,
    pub lease: Option<Lease>,
    pub peers: Vec<Candidate>,
}
#[derive(Default)]
pub struct Fence {
    epoch: u64,
    leader: Option<StageId>,
    valid_until: Option<Instant>,
}
impl Fence {
    /// Use the full request round trip as a conservative uncertainty bound.
    /// Wall-clock skew never lengthens a grant; restart never restores a lease.
    pub fn observe(&mut self, reply: &Reply, sent: Instant, received: Instant) -> Result<()> {
        reply.head.validate()?;
        if reply.epoch < self.epoch {
            return Err(StageError::Forbidden("Coordinator response is stale"));
        }
        self.epoch = reply.epoch;
        self.leader = None;
        self.valid_until = None;
        if let Some(lease) = &reply.lease {
            if lease.epoch != reply.epoch || lease.leader.0.is_nil() {
                return Err(StageError::Forbidden("Invalid execution lease"));
            }
            let remaining = lease.expires_at_ms.saturating_sub(reply.server_time_ms);
            if remaining > 6000 {
                return Err(StageError::Forbidden("Execution lease exceeds its bound"));
            }
            let remaining = Duration::from_millis(remaining.max(0) as u64)
                .saturating_sub(received.saturating_duration_since(sent))
                .saturating_sub(Duration::from_millis(50));
            if !remaining.is_zero() {
                self.leader = Some(lease.leader);
                self.valid_until = received.checked_add(remaining);
            }
        }
        Ok(())
    }
    pub fn permits(&self, stage: StageId, epoch: u64, now: Instant) -> bool {
        self.epoch == epoch
            && self.leader == Some(stage)
            && self.valid_until.is_some_and(|end| now < end)
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn leader(&self, now: Instant) -> Option<StageId> {
        self.leader
            .filter(|_| self.valid_until.is_some_and(|end| now < end))
    }
    pub fn invalidate(&mut self) {
        self.leader = None;
        self.valid_until = None;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fencing_is_monotonic_conservative_and_never_survives_restart() {
        let first = StageId::new();
        let second = StageId::new();
        let sent = Instant::now();
        let received = sent + Duration::from_secs(2);
        let mut fence = Fence::default();
        let mut reply = Reply {
            request_id: Uuid::new_v4(),
            accepted: true,
            server_time_ms: -1_000_000,
            head: Head::default(),
            epoch: 1,
            lease: Some(Lease {
                epoch: 1,
                leader: first,
                expires_at_ms: -994_000,
            }),
            peers: vec![],
        };
        fence.observe(&reply, sent, received).unwrap();
        assert!(fence.permits(first, 1, received));
        assert!(!fence.permits(first, 1, received + Duration::from_secs(4)));
        reply.epoch = 2;
        reply.lease = Some(Lease {
            epoch: 2,
            leader: second,
            expires_at_ms: -994_000,
        });
        fence.observe(&reply, sent, received).unwrap();
        assert!(!fence.permits(first, 1, received));
        assert!(fence.permits(second, 2, received));
        reply.epoch = 1;
        assert!(fence.observe(&reply, sent, received).is_err());
        assert!(!Fence::default().permits(second, 2, received));
        fence.invalidate();
        assert!(!fence.permits(second, 2, received));
    }
}
