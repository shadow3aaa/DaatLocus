//! Device-owned execution metadata. Raft runs on authenticated device channels
//! through the relay; the relay never owns votes, leases or App payloads.
mod network;
mod store;
use crate::stage::{Result, StageError, StageId, runtime::Runtime, shared};
use openraft::{EmptyNode, Raft};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Cursor,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

pub const LEASE_MS: i64 = 6000;
pub const MAX_SNAPSHOT: usize = 128 * 1024;
/// Same persistent device UUID; Raft requires a stable minimum default sentinel.
/// StageId::default creates a new identity and must never be used as that sentinel.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct NodeId(pub uuid::Uuid);
impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl From<StageId> for NodeId {
    fn from(value: StageId) -> Self {
        Self(value.0)
    }
}
impl From<NodeId> for StageId {
    fn from(value: NodeId) -> Self {
        Self(value.0)
    }
}
openraft::declare_raft_types!(pub Types: D = Proposal, R = Outcome, NodeId = NodeId, Node = EmptyNode, SnapshotData = Cursor<Vec<u8>>);
pub type DeviceRaft = Raft<Types>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    #[serde(default)]
    work_generation: Option<crate::stage::reset::Generation>,
    caller: StageId,
    operation: shared::Operation,
    acknowledgements: Vec<StageId>,
    time: i64,
    claim_ready: bool,
    instance: uuid::Uuid,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub reply: shared::Reply,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Retire,
    Append(openraft::raft::AppendEntriesRequest<Types>),
    Vote(openraft::raft::VoteRequest<NodeId>),
    Snapshot(openraft::raft::InstallSnapshotRequest<Types>),
    Command {
        operation: shared::Operation,
        receipts: Vec<shared::Acknowledgement>,
        instance: uuid::Uuid,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Retired,
    Append(openraft::raft::AppendEntriesResponse<NodeId>),
    Vote(openraft::raft::VoteResponse<NodeId>),
    Snapshot(openraft::raft::InstallSnapshotResponse<NodeId>),
    Command(shared::Reply),
    Forward { leader: Option<StageId> },
    Rejected,
}
impl Clone for Response {
    fn clone(&self) -> Self {
        use openraft::raft::AppendEntriesResponse as Append;
        match self {
            Self::Retired => Self::Retired,
            Self::Append(value) => Self::Append(match value {
                Append::Success => Append::Success,
                Append::PartialSuccess(id) => Append::PartialSuccess(*id),
                Append::Conflict => Append::Conflict,
                Append::HigherVote(vote) => Append::HigherVote(*vote),
            }),
            Self::Vote(value) => Self::Vote(value.clone()),
            Self::Snapshot(value) => {
                Self::Snapshot(openraft::raft::InstallSnapshotResponse { vote: value.vote })
            }
            Self::Command(value) => Self::Command(value.clone()),
            Self::Forward { leader } => Self::Forward { leader: *leader },
            Self::Rejected => Self::Rejected,
        }
    }
}
pub struct Coordinator {
    runtime: Weak<Runtime>,
    pub raft: DeviceRaft,
    leadership: Mutex<Option<(u64, Instant, i64)>>,
    admission: tokio::sync::Mutex<()>,
    instance: uuid::Uuid,
    retiring: std::sync::atomic::AtomicBool,
}
impl Coordinator {
    pub async fn new(runtime: &Arc<Runtime>) -> Result<Arc<Self>> {
        let storage = store::Store::open(
            runtime.host.system.clone(),
            crate::stage::shared_store::head(runtime.host.system.store())?,
        )?;
        let config = openraft::Config {
            cluster_name: runtime
                .host
                .account_id()
                .ok_or(StageError::Unavailable("Account is unavailable"))?,
            // OpenRaft also uses this interval as the Append RPC deadline.
            // Durable writes and authenticated multiplexed channels cannot
            // reliably round-trip in 250ms under normal replication load.
            heartbeat_interval: 1000,
            election_timeout_min: 2500,
            election_timeout_max: 3500,
            max_payload_entries: 8,
            snapshot_policy: openraft::SnapshotPolicy::LogsSinceLast(128),
            snapshot_max_chunk_size: 4096,
            max_in_snapshot_log_to_keep: 128,
            purge_batch_size: 64,
            ..Default::default()
        }
        .validate()
        .map_err(|_| StageError::Invalid("Invalid device consensus configuration"))?;
        let raft = Raft::new(
            NodeId::from(runtime.host.local),
            Arc::new(config),
            network::Factory(Arc::downgrade(runtime)),
            storage.clone(),
            storage,
        )
        .await
        .map_err(|_| StageError::Unavailable("Cannot start device consensus"))?;
        Ok(Arc::new(Self {
            runtime: Arc::downgrade(runtime),
            raft,
            leadership: Mutex::new(None),
            admission: tokio::sync::Mutex::new(()),
            instance: uuid::Uuid::new_v4(),
            retiring: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    fn runtime(&self) -> Result<Arc<Runtime>> {
        self.runtime
            .upgrade()
            .filter(|runtime| !runtime.is_stopped())
            .ok_or(StageError::Unavailable("Device consensus is stopped"))
    }
    /// Transport authentication alone does not grant private coordination access.
    pub fn authorize(&self, peer: StageId) -> Result<()> {
        let runtime = self.runtime()?;
        let own = runtime.host.enrollment()?;
        let removed = crate::stage::revocation::removed(runtime.host.system.store())?;
        let now = chrono::Utc::now().timestamp_millis();
        let local =
            crate::stage::identity::verify_enrollment(&own.root, &own.chain, now, &removed)?;
        if !local.can_enroll {
            return Err(StageError::Forbidden(
                "Device cannot coordinate account execution",
            ));
        }
        if peer != runtime.host.local {
            let remote = crate::stage::ledger::StageLedger::attach(
                runtime.host.system.store(),
                runtime.host.local,
            )
            .peer(peer, now)?;
            let member = crate::stage::identity::verify_enrollment(
                &own.root,
                &remote.enrollment.chain,
                now,
                &removed,
            )?;
            if remote.enrollment.root != own.root || !member.can_enroll {
                return Err(StageError::Forbidden(
                    "Peer cannot coordinate account execution",
                ));
            }
        }
        Ok(())
    }
    pub async fn bootstrap(&self) -> Result<()> {
        let runtime = self.runtime()?;
        self.authorize(runtime.host.local)?;
        // A fresh joining device is a learner. Only the original account authority
        // can initialize one cluster; restart never invents a singleton quorum.
        if !self
            .raft
            .is_initialized()
            .await
            .map_err(|_| StageError::Unavailable("Cannot inspect device consensus"))?
            && runtime.host.identity()?.has_group_authority()
        {
            let nodes = BTreeMap::from([(NodeId::from(runtime.host.local), EmptyNode {})]);
            self.raft
                .initialize(nodes)
                .await
                .map_err(|_| StageError::Unavailable("Cannot initialize device quorum"))?;
        }
        Ok(())
    }
    /// Account replicas remain learners; joining does not add a permanent
    /// availability dependency. Old memberships narrow through their quorum.
    pub async fn admit_connected(&self) -> Result<()> {
        if self.retiring.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let Ok(_guard) = self.admission.try_lock() else {
            return Ok(());
        };
        let runtime = self.runtime()?;
        if self.raft.current_leader().await.map(StageId::from) != Some(runtime.host.local) {
            return Ok(());
        }
        let metrics = self.raft.metrics().borrow().clone();
        let voters = metrics
            .membership_config
            .membership()
            .voter_ids()
            .collect::<std::collections::BTreeSet<_>>();
        for peer in crate::stage::ledger::StageLedger::attach(
            runtime.host.system.store(),
            runtime.host.local,
        )
        .peers(chrono::Utc::now().timestamp_millis())?
        {
            let id = peer.device.stage_id;
            if metrics
                .membership_config
                .membership()
                .get_node(&NodeId::from(id))
                .is_some()
                || !runtime.connected(id)
                || self.authorize(id).is_err()
            {
                continue;
            }
            self.raft
                .add_learner(NodeId::from(id), EmptyNode {}, false)
                .await
                .map_err(|_| StageError::Unavailable("Device metadata has not synchronized"))?;
        }
        if voters.len() > 1 {
            self.raft
                .change_membership(
                    std::collections::BTreeSet::from([NodeId::from(runtime.host.local)]),
                    true,
                )
                .await
                .map_err(|_| StageError::Unavailable("Device membership has not committed"))?;
        }
        Ok(())
    }
    pub async fn command(
        &self,
        operation: shared::Operation,
        receipts: Vec<shared::Acknowledgement>,
    ) -> Result<shared::Reply> {
        let runtime = self.runtime()?;
        self.authorize(runtime.host.local)?;
        let leader = self
            .raft
            .current_leader()
            .await
            .map(StageId::from)
            .ok_or(StageError::Unavailable("Waiting for a device quorum"))?;
        let request = Request::Command {
            operation,
            receipts,
            instance: self.instance,
        };
        let response = if leader == runtime.host.local {
            self.handle(runtime.host.local, request).await?
        } else {
            network::rpc(&runtime, leader, request).await?
        };
        match response {
            Response::Command(reply) => Ok(reply),
            _ => Err(StageError::Unavailable(
                "Account execution has no confirmed device quorum",
            )),
        }
    }
    pub async fn handle(&self, peer: StageId, request: Request) -> Result<Response> {
        self.authorize(peer)?;
        let runtime = self.runtime()?;
        match request {
            Request::Retire => {
                if self.raft.current_leader().await.map(StageId::from) != Some(runtime.host.local) {
                    return Ok(Response::Forward {
                        leader: self.raft.current_leader().await.map(StageId::from),
                    });
                }
                self.retire(peer).await?;
                Ok(Response::Retired)
            }
            Request::Append(request) => {
                if request.vote.leader_id.node_id != NodeId::from(peer) || request.entries.len() > 8
                {
                    return Err(StageError::Forbidden("Invalid consensus sender"));
                }
                Ok(Response::Append(
                    self.raft
                        .append_entries(request)
                        .await
                        .map_err(|_| StageError::Unavailable("Device append rejected"))?,
                ))
            }
            Request::Vote(request) => {
                if request.vote.leader_id.node_id != NodeId::from(peer) {
                    return Err(StageError::Forbidden("Invalid vote sender"));
                }
                Ok(Response::Vote(self.raft.vote(request).await.map_err(
                    |_| StageError::Unavailable("Device vote rejected"),
                )?))
            }
            Request::Snapshot(request) => {
                if request.vote.leader_id.node_id != NodeId::from(peer)
                    || request.offset.saturating_add(request.data.len() as u64)
                        > MAX_SNAPSHOT as u64
                    || request.data.len() > 4096
                {
                    return Err(StageError::Capacity("Device snapshot budget exceeded"));
                }
                Ok(Response::Snapshot(
                    self.raft
                        .install_snapshot(request)
                        .await
                        .map_err(|_| StageError::Unavailable("Device snapshot rejected"))?,
                ))
            }
            Request::Command {
                operation,
                receipts,
                instance,
            } => {
                if instance.is_nil() {
                    return Err(StageError::Invalid("Invalid execution instance"));
                }
                match &operation {
                    shared::Operation::Advertise { head, .. }
                    | shared::Operation::Claim { head }
                    | shared::Operation::Renew { head, .. } => head.validate()?,
                    shared::Operation::Commit { head, previous, .. } => {
                        head.validate()?;
                        previous.validate()?;
                    }
                    shared::Operation::Reconcile { head, .. } => head.validate()?,
                    shared::Operation::Inspect | shared::Operation::Release { .. } => {}
                }
                if self.raft.current_leader().await.map(StageId::from) != Some(runtime.host.local) {
                    return Ok(Response::Forward {
                        leader: self.raft.current_leader().await.map(StageId::from),
                    });
                }
                let verification_time = chrono::Utc::now().timestamp_millis();
                let term = self.raft.metrics().borrow().current_term;
                let (time, claim_ready) = {
                    let mut leadership = self.leadership.lock();
                    if leadership.as_ref().is_none_or(|(old, _, _)| *old != term) {
                        *leadership = Some((term, Instant::now(), verification_time));
                    }
                    let (_, since, base) = leadership.as_ref().unwrap();
                    (
                        base.saturating_add(
                            since.elapsed().as_millis().min(i64::MAX as u128) as i64
                        ),
                        since.elapsed() >= Duration::from_millis(LEASE_MS as u64 + 250),
                    )
                };
                let mut acknowledgements = vec![];
                if let shared::Operation::Commit {
                    epoch, ref head, ..
                }
                | shared::Operation::Reconcile { epoch, ref head } = operation
                {
                    if receipts.len() > 64 {
                        return Err(StageError::Capacity("Receipt budget exceeded"));
                    }
                    let own = runtime.host.enrollment()?;
                    let removed = crate::stage::revocation::removed(runtime.host.system.store())?;
                    for receipt in receipts {
                        let member = crate::stage::identity::verify_enrollment(
                            &own.root,
                            &receipt.enrollment.chain,
                            verification_time,
                            &removed,
                        )?;
                        self.authorize(member.device.stage_id)?;
                        let ack: shared::ReplicaAcknowledgement = receipt
                            .proof
                            .verify("core-replica", &member.device.public_key)?;
                        if receipt.enrollment.root != own.root
                            || ack.version != 1
                            || ack.group_id != own.root.group_id
                            || ack.stage_id != member.device.stage_id
                            || ack.leader != peer
                            || ack.epoch != epoch
                            || ack.head != *head
                            || ack.expires_at_ms <= verification_time
                            || ack.expires_at_ms > verification_time + 30000
                        {
                            return Err(StageError::Forbidden("Invalid durable replica receipt"));
                        }
                        if !acknowledgements.contains(&ack.stage_id) {
                            acknowledgements.push(ack.stage_id);
                        }
                    }
                }
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    self.raft.client_write(Proposal {
                        work_generation: crate::stage::reset::generation(&runtime.host)?,
                        caller: peer,
                        operation,
                        acknowledgements,
                        time,
                        claim_ready,
                        instance,
                    }),
                )
                .await
                .map_err(|_| StageError::Unavailable("Device quorum confirmation timed out"))?
                .map_err(|_| {
                    StageError::Unavailable("Device quorum did not commit execution metadata")
                })?;
                Ok(Response::Command(result.data.reply))
            }
        }
    }
    pub async fn leave(&self) -> Result<()> {
        let runtime = self.runtime()?;
        if !self.is_voter(runtime.host.local) {
            return Ok(());
        }
        self.retiring
            .store(true, std::sync::atomic::Ordering::Release);
        let leader = self
            .raft
            .current_leader()
            .await
            .map(StageId::from)
            .ok_or(StageError::Unavailable("Account authority is unavailable"))?;
        tokio::time::timeout(Duration::from_secs(8), async {
            let response = if leader == runtime.host.local {
                self.handle(runtime.host.local, Request::Retire).await?
            } else {
                network::rpc(&runtime, leader, Request::Retire).await?
            };
            if matches!(response, Response::Retired) {
                Ok(())
            } else {
                Err(StageError::Unavailable("Device handoff did not commit"))
            }
        })
        .await
        .map_err(|_| StageError::Unavailable("Device handoff timed out"))?
    }
    async fn retire(&self, peer: StageId) -> Result<()> {
        let _admission = self.admission.lock().await;
        let runtime = self.runtime()?;
        let metrics = self.raft.metrics().borrow().clone();
        let mut voters = metrics
            .membership_config
            .membership()
            .voter_ids()
            .filter(|id| *id != NodeId::from(peer))
            .collect::<std::collections::BTreeSet<_>>();
        if voters.is_empty() {
            let core = runtime
                .host
                .core
                .read()
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .ok_or(StageError::Unavailable(
                    "Account conversation is unavailable",
                ))?;
            let head = crate::stage::shared_store::head(runtime.host.system.store())?;
            let mut peers = crate::stage::ledger::StageLedger::attach(
                runtime.host.system.store(),
                runtime.host.local,
            )
            .peers(chrono::Utc::now().timestamp_millis())?;
            peers.sort_by_key(|peer| peer.device.stage_id);
            let mut target = None;
            for record in peers {
                let id = record.device.stage_id;
                if id == peer || !runtime.connected(id) || self.authorize(id).is_err() {
                    continue;
                }
                if let Ok(crate::stage::core_wire::Response::RecoveryStatus { status }) = core
                    .request(id, crate::stage::core_wire::Request::RecoveryStatus)
                    .await
                    && status.head == head
                    && status.branch.is_none()
                {
                    target = Some(id);
                    break;
                }
            }
            let target = target.ok_or(StageError::Unavailable("No synchronized handoff device"))?;
            // This catch-up wait is bounded by leave's deadline, outside renewal.
            self.raft
                .add_learner(NodeId::from(target), EmptyNode {}, true)
                .await
                .map_err(|_| StageError::Unavailable("Handoff device has not synchronized"))?;
            voters.insert(NodeId::from(target));
        }
        self.raft
            .change_membership(voters, true)
            .await
            .map_err(|_| StageError::Unavailable("Device handoff did not commit"))?;
        Ok(())
    }
    pub async fn stop(&self) {
        let _ = self.raft.shutdown().await;
    }
    pub fn durable_quorum(&self, devices: &[StageId]) -> bool {
        self.raft
            .metrics()
            .borrow()
            .membership_config
            .membership()
            .get_joint_config()
            .iter()
            .all(|voters| {
                devices
                    .iter()
                    .filter(|device| voters.contains(&NodeId::from(**device)))
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    > voters.len() / 2
            })
    }
    pub fn is_voter(&self, device: StageId) -> bool {
        self.raft
            .metrics()
            .borrow()
            .membership_config
            .membership()
            .voter_ids()
            .any(|id| id == NodeId::from(device))
    }
}
