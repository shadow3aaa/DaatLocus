//! Coordination, replication and device App routing around the existing Actor
//! kernel. This module does not implement a second Agent loop.
use super::{
    Result, StageError, StageId,
    blob::ContentStore,
    core_wire::{Installation, Request, Response},
    protocol::{BlobRef, Body},
    runtime::Runtime,
    shared::{self, Acknowledgement, Fence, Head, Operation, ReplicaAcknowledgement},
    shared_store::{self, Batch},
};
use crate::{
    actors::{ActorControl, MessageStatus},
    app::{AppManager, AppToolExecutionResult},
    builtin_apps,
    model_protocol::AgentToolCall,
};
use parking_lot::{Mutex, RwLock};
use rusqlite::{OptionalExtension, params};
use std::{
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;
mod approval;
pub struct Controller {
    runtime: Weak<Runtime>,
    fence: Mutex<Fence>,
    ready: AtomicBool,
    stopped: AtomicBool,
    draining: AtomicBool,
    epoch: std::sync::atomic::AtomicU64,
    checkpoint: tokio::sync::Mutex<()>,
    replica: tokio::sync::Mutex<()>,
    device_apps: tokio::sync::Mutex<Option<(String, AppManager)>>,
    wake: Notify,
    replica_wake: Notify,
    desired: RwLock<Option<shared::Reply>>,
    recovered_epoch: std::sync::atomic::AtomicU64,
    recovery: RwLock<Option<super::recovery::Branch>>,
    peers: RwLock<std::collections::HashMap<StageId, super::recovery::Status>>,
    frozen: Mutex<Option<(StageId, Instant)>>,
    failure: RwLock<Option<&'static str>>,
    last_confirmed: Mutex<Instant>,
}

impl Controller {
    pub fn new(runtime: &Arc<Runtime>) -> Result<Arc<Self>> {
        let account = runtime
            .host
            .account_id()
            .ok_or(StageError::Unavailable("Account has not been joined"))?;
        shared_store::initialize(runtime.host.system.store(), &account)?;
        super::recovery::initialize(runtime.host.system.store())?;
        let mut recovery = super::recovery::branch(runtime.host.system.store())?;
        if let Some(branch) = recovery.as_mut() {
            branch.epoch = branch
                .epoch
                .checked_add(1)
                .filter(|epoch| *epoch < 9_007_199_254_740_991)
                .ok_or(StageError::Capacity("Recovery epoch exhausted"))?;
            super::recovery::save_branch(runtime.host.system.store(), branch)?;
        }
        if runtime.host.identity()?.has_group_authority() {
            super::recovery::mark_synchronized(runtime.host.system.store())?;
        }
        runtime
            .host
            .system
            .ensure_core_actor()
            .map_err(|_| StageError::Unavailable("Cannot initialize account Actor"))?;
        super::ledger::initialize_account_ownership(runtime.host.system.store(), &account)?;
        runtime.host.system.store().stage_transaction(|tx|{
            tx.execute("INSERT INTO core_meta(key,value) VALUES('active','1') ON CONFLICT(key) DO UPDATE SET value='1'",[])?;
            tx.execute_batch("CREATE TABLE IF NOT EXISTS core_app_effects(message TEXT NOT NULL,call TEXT NOT NULL,digest TEXT NOT NULL,state TEXT NOT NULL,result BLOB,PRIMARY KEY(message,call));")?;Ok(())
        })?;
        Ok(Arc::new(Self {
            runtime: Arc::downgrade(runtime),
            fence: Mutex::new(Fence::default()),
            ready: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            epoch: std::sync::atomic::AtomicU64::new(0),
            checkpoint: tokio::sync::Mutex::new(()),
            replica: tokio::sync::Mutex::new(()),
            device_apps: tokio::sync::Mutex::new(None),
            wake: Notify::new(),
            replica_wake: Notify::new(),
            desired: RwLock::new(None),
            recovered_epoch: std::sync::atomic::AtomicU64::new(0),
            recovery: RwLock::new(recovery),
            peers: RwLock::new(std::collections::HashMap::new()),
            frozen: Mutex::new(None),
            failure: RwLock::new(None),
            last_confirmed: Mutex::new(Instant::now()),
        }))
    }
    fn runtime(&self) -> Result<Arc<Runtime>> {
        self.runtime
            .upgrade()
            .filter(|runtime| {
                runtime.host.account_id().is_some()
                    && runtime.host.enrollment().is_ok()
                    && !runtime.is_stopped()
                    && !self.stopped.load(Ordering::Acquire)
            })
            .ok_or(StageError::Unavailable(
                "Shared Actor connectivity is unavailable",
            ))
    }
    pub fn permitted(&self) -> bool {
        self.ready.load(Ordering::Acquire) && self.execution_permitted()
    }
    /// Readiness gates new input. An already running inference remains valid
    /// until its execution authority expires, is fenced, freezes or drains.
    pub(crate) fn execution_permitted(&self) -> bool {
        self.runtime().is_ok_and(|runtime| {
            !self.draining.load(Ordering::Acquire)
                && self.frozen.lock().is_none()
                && self.recovered_epoch.load(Ordering::Acquire) == self.epoch()
                && self.fence.lock().permits(
                    runtime.host.local,
                    self.epoch.load(Ordering::Acquire),
                    Instant::now(),
                )
        })
    }
    /// Called only after the previous model/App worker has fully stopped.
    /// Inference can recover even if a transient readiness loss kept the same
    /// epoch. The replica loop repairs uncertain tool protocol before activation.
    pub(crate) fn worker_stopped(&self) {
        self.ready.store(false, Ordering::Release);
        self.recovered_epoch.store(0, Ordering::Release);
        self.replica_wake.notify_one();
        self.wake.notify_one();
    }
    pub async fn model_apps(self: &Arc<Self>) -> miette::Result<AppManager> {
        let runtime = self.runtime().map_err(|error| miette::miette!("{error}"))?;
        let actor = runtime.host.system.actor(shared::ACTOR_ID)?;
        super::shared_apps::manager(self.clone(), actor, runtime.host.system.clone())
    }
    pub fn actor_devices(&self) -> Result<serde_json::Value> {
        let runtime = self.runtime()?;
        let mut devices = vec![
            serde_json::json!({"stage_id":runtime.host.local,"label":runtime.host.settings().label,"online":true,"current":true}),
        ];
        for peer in
            super::ledger::StageLedger::attach(runtime.host.system.store(), runtime.host.local)
                .peers(chrono::Utc::now().timestamp_millis())?
        {
            let id = peer.device.stage_id;
            devices.push(
                serde_json::json!({"stage_id":id,"online":runtime.connected(id),"current":false}),
            );
        }
        Ok(serde_json::json!({"devices":devices}))
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    pub async fn steer(
        &self,
        message: &str,
        target: &str,
        expected: i64,
    ) -> Result<crate::actors::MessageRecord> {
        let leader = self
            .fence
            .lock()
            .leader(Instant::now())
            .ok_or(StageError::Unavailable("Worker is reconnecting"))?;
        match self
            .request(
                leader,
                Request::Steer {
                    message: message.into(),
                    target: target.into(),
                    expected,
                },
            )
            .await?
        {
            Response::Steered { message } => Ok(*message),
            _ => Err(StageError::Unavailable(
                "Input was not applied to the active task",
            )),
        }
    }
    pub async fn amend(
        &self,
        message: &str,
        expected: i64,
        content: Option<String>,
    ) -> Result<crate::actors::MessageRecord> {
        let leader = self
            .fence
            .lock()
            .leader(Instant::now())
            .ok_or(StageError::Unavailable("Worker is reconnecting"))?;
        match self
            .request(
                leader,
                Request::Amend {
                    message: message.into(),
                    expected,
                    content,
                },
            )
            .await?
        {
            Response::Steered { message } => Ok(*message),
            _ => Err(StageError::Unavailable("Queued input was not changed")),
        }
    }
    pub async fn clear_queue(&self) -> Result<usize> {
        let leader = self
            .fence
            .lock()
            .leader(Instant::now())
            .ok_or(StageError::Unavailable("Worker is reconnecting"))?;
        match self.request(leader, Request::ClearQueue).await? {
            Response::QueueCleared { count } => Ok(count),
            _ => Err(StageError::Unavailable("Queue was not changed")),
        }
    }
    pub async fn preferences(
        &self,
        model: Option<Option<String>>,
        approval_mode: Option<crate::actors::ApprovalMode>,
    ) -> Result<()> {
        let reply = self.coordinate(Operation::Inspect, vec![]).await?;
        let lease = reply
            .lease
            .ok_or(StageError::Unavailable("Account worker is reconnecting"))?;
        match self
            .request(
                lease.leader,
                Request::Preferences {
                    model_present: model.is_some(),
                    model: model.flatten(),
                    approval_mode,
                },
            )
            .await?
        {
            Response::Published => Ok(()),
            _ => Err(StageError::Unavailable(
                "Configuration has not synchronized",
            )),
        }
    }
    pub fn notify(&self) {
        self.wake.notify_waiters();
    }
    pub(crate) fn recovery_status(&self) -> Result<super::recovery::Status> {
        let runtime = self.runtime()?;
        Ok(super::recovery::Status {
            branch: self.recovery.read().clone(),
            head: shared_store::head(runtime.host.system.store())?,
            epoch: self.epoch().max(self.fence.lock().epoch()),
            frozen: self.frozen.lock().is_some(),
            archives_pending: super::recovery::archives_pending(runtime.host.system.store())?,
        })
    }
    pub(crate) fn note_recovery(
        &self,
        peer: StageId,
        status: super::recovery::Status,
    ) -> Result<()> {
        status.head.validate()?;
        let runtime = self.runtime()?;
        let coordinator = runtime
            .coordination
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
        coordinator.authorize(peer)?;
        if let Some(branch) = &status.branch {
            branch.base.validate()?;
            if branch.device != peer
                || uuid::Uuid::parse_str(&branch.id).is_err()
                || branch.epoch > 9_007_199_254_740_991
            {
                return Err(StageError::Invalid("Invalid peer recovery branch"));
            }
        }
        let mut peers = self.peers.write();
        if peers
            .get(&peer)
            .is_some_and(|current| current.epoch > status.epoch)
        {
            return Ok(());
        }
        let changed = peers.insert(peer, status.clone()).as_ref() != Some(&status);
        drop(peers);
        if changed {
            self.wake.notify_one();
        }
        Ok(())
    }
    pub fn status(&self) -> serde_json::Value {
        let leader = self.fence.lock().leader(Instant::now());
        let worker = self.permitted();
        let synchronized = worker
            || self.runtime().is_ok_and(|runtime| {
                self.desired.read().as_ref().is_some_and(|desired| {
                    shared_store::head(runtime.host.system.store())
                        .is_ok_and(|head| head == desired.head)
                })
            });
        let model_ready = self.model_ready();
        let ready = self.accepts_input();
        let recovering = self.recovery.read().is_some();
        let frozen = self.frozen.lock().is_some();
        let failure = *self.failure.read();
        let config_revision = self.runtime().ok().and_then(|runtime| {
            runtime
                .host
                .configurations
                .revision(&runtime.host)
                .ok()
                .flatten()
        });
        let state = if matches!(
            failure,
            Some("recovery_storage_full" | "recovery_unauthorized" | "recovery_failed")
        ) {
            "recovery_blocked"
        } else if frozen || failure == Some("reconciling") {
            "reconciling"
        } else if !model_ready && !ready {
            "model_required"
        } else if worker && recovering {
            "local_recovery"
        } else if worker {
            "worker"
        } else if ready {
            "replica"
        } else if failure == Some("quorum_unavailable") {
            "recovering"
        } else if failure == Some("device_offline") {
            "offline"
        } else if leader.is_some() {
            "synchronizing"
        } else {
            "connecting"
        };
        serde_json::json!({"worker_stage_id":leader,"epoch":self.fence.lock().epoch(),"synchronized":synchronized,"ready":ready && !frozen,"model_ready":model_ready,"state":state,"error_code":failure,"config_revision":config_revision})
    }
    fn model_ready(&self) -> bool {
        self.runtime().is_ok_and(|runtime| {
            let config = runtime.host.system.config();
            config.validate().is_ok()
                && runtime
                    .host
                    .system
                    .actor(shared::ACTOR_ID)
                    .is_ok_and(|actor| {
                        config.models.contains_key(
                            actor.config.model.as_deref().unwrap_or(&config.main_model),
                        )
                    })
        })
    }
    pub fn accepts_input(&self) -> bool {
        if self.draining.load(Ordering::Acquire) || self.frozen.lock().is_some() {
            return false;
        }
        let Ok(runtime) = self.runtime() else {
            return false;
        };
        if self.permitted() {
            return self.model_ready();
        }
        self.desired.read().as_ref().is_some_and(|desired| {
            desired.lease.as_ref().is_some_and(|lease| {
                let valid = self
                    .fence
                    .lock()
                    .permits(lease.leader, lease.epoch, Instant::now());
                lease.leader != runtime.host.local && runtime.connected(lease.leader) && valid
            })
        })
    }
    /// Browsing and forwarding use the authenticated worker, independently of
    /// whether this device has hydrated its execution checkpoint.
    pub(crate) fn read_source(&self) -> Option<StageId> {
        if self.draining.load(Ordering::Acquire) || self.frozen.lock().is_some() {
            return None;
        }
        let runtime = self.runtime().ok()?;
        let source = self.fence.lock().leader(Instant::now())?;
        (source != runtime.host.local && runtime.connected(source)).then_some(source)
    }
    pub(crate) async fn forward_user_input(
        &self,
        message: crate::actors::MessageRecord,
    ) -> Result<()> {
        let leader = self
            .read_source()
            .ok_or(StageError::Unavailable("Worker is reconnecting"))?;
        self.push_resources(leader).await?;
        match self
            .request(
                leader,
                Request::Input {
                    message: Box::new(message),
                },
            )
            .await?
        {
            Response::InputAccepted => Ok(()),
            _ => Err(StageError::Unavailable(
                "Account input has not been accepted",
            )),
        }
    }
    fn invalidate(&self) {
        self.ready.store(false, Ordering::Release);
        self.fence.lock().invalidate();
    }
    pub(crate) async fn prepare_exit(&self) -> Result<()> {
        self.draining.store(true, Ordering::Release);
        let runtime = self.runtime()?;
        let coordinator = runtime
            .coordination
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
        let authority = coordinator
            .raft
            .current_leader()
            .await
            .map(StageId::from)
            .unwrap_or(runtime.host.local);
        self.freeze(authority).await?;
        if authority == runtime.host.local && !self.connected_peers(&runtime)?.is_empty() {
            self.reconcile().await?;
        }
        Ok(())
    }
    pub async fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.invalidate();
        self.wake.notify_waiters();
        if let Some(runtime) = self.runtime.upgrade() {
            let _ = runtime.host.system.store().stage_transaction(|tx| {
                tx.execute("UPDATE core_meta SET value='0' WHERE key='active'", [])?;
                Ok(())
            });
            runtime.host.system.wake(shared::ACTOR_ID);
        }
        if let Ok(mut resources) = self.device_apps.try_lock()
            && let Some((_, apps)) = resources.take()
        {
            let _ = apps.shutdown().await;
        }
    }
    async fn coordinate(
        &self,
        operation: Operation,
        receipts: Vec<Acknowledgement>,
    ) -> Result<shared::Reply> {
        let runtime = self.runtime()?;
        if self.frozen.lock().is_some() {
            return Err(StageError::Unavailable(
                "Conversation reconciliation is in progress",
            ));
        }
        let recovery = self.recovery.read().clone();
        if let Some(branch) = recovery {
            let coordinator = runtime
                .coordination
                .read()
                .clone()
                .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
            coordinator.authorize(runtime.host.local)?;
            if !self.recovery_authority_available(&runtime).await? {
                let sent = Instant::now();
                let head = shared_store::head(runtime.host.system.store())?;
                let (accepted, result_head) = match &operation {
                    Operation::Inspect | Operation::Advertise { .. } => (true, head.clone()),
                    Operation::Claim { head: expected } => (*expected == head, head.clone()),
                    Operation::Renew {
                        epoch,
                        head: expected,
                    } => (*epoch == branch.epoch && *expected == head, head.clone()),
                    Operation::Commit {
                        epoch,
                        previous,
                        head: next,
                    } => (*epoch == branch.epoch && *previous == head, next.clone()),
                    _ => (false, head.clone()),
                };
                let now = chrono::Utc::now().timestamp_millis();
                let reply = shared::Reply {
                    request_id: uuid::Uuid::new_v4(),
                    accepted,
                    server_time_ms: now,
                    head: result_head,
                    epoch: branch.epoch,
                    lease: Some(shared::Lease {
                        epoch: branch.epoch,
                        leader: runtime.host.local,
                        expires_at_ms: now + super::coordination::LEASE_MS,
                    }),
                    peers: vec![shared::Candidate {
                        stage_id: runtime.host.local,
                        head,
                        priority: runtime.host.settings().compute_priority,
                        eligible: true,
                    }],
                };
                self.fence.lock().observe(&reply, sent, Instant::now())?;
                return Ok(reply);
            }
            return Err(StageError::Unavailable(
                "Conversation reconciliation is in progress",
            ));
        }
        let (reply, sent, received) = runtime.coordinate(operation, receipts).await?;
        self.fence.lock().observe(&reply, sent, received)?;
        *self.last_confirmed.lock() = received;
        runtime.host.system.store().stage_transaction(|tx| {
            tx.execute("INSERT INTO core_meta VALUES('observed_epoch',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE CAST(value AS INTEGER)<CAST(excluded.value AS INTEGER)", [reply.epoch.to_string()])?;
            Ok(())
        })?;
        Ok(reply)
    }
    fn connected_peers(&self, runtime: &Runtime) -> Result<Vec<StageId>> {
        let coordinator = runtime.coordination.read().clone();
        Ok(
            super::ledger::StageLedger::attach(runtime.host.system.store(), runtime.host.local)
                .peers(chrono::Utc::now().timestamp_millis())?
                .into_iter()
                .map(|peer| peer.device.stage_id)
                .filter(|peer| {
                    runtime.connected(*peer)
                        && coordinator
                            .as_ref()
                            .is_some_and(|coordinator| coordinator.authorize(*peer).is_ok())
                })
                .collect(),
        )
    }
    async fn recovery_authority_available(&self, runtime: &Runtime) -> Result<bool> {
        let coordinator = runtime
            .coordination
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
        let Some(authority) = coordinator.raft.current_leader().await.map(StageId::from) else {
            return Ok(false);
        };
        if authority != runtime.host.local
            && (!runtime.connected(authority) || coordinator.authorize(authority).is_err())
        {
            return Ok(false);
        }
        let mut reachable = self.connected_peers(runtime)?;
        reachable.push(runtime.host.local);
        // Learner links cannot confirm account authority. Connected replicas
        // still need local branches when the actual voting quorum is absent.
        Ok(coordinator.durable_quorum(&reachable))
    }
    async fn require_worker(&self, leader: StageId, epoch: u64) -> Result<shared::Reply> {
        let reply = self.coordinate(Operation::Inspect, vec![]).await?;
        if !self.fence.lock().permits(leader, epoch, Instant::now()) {
            return Err(StageError::Forbidden(
                "Shared Actor worker lost its execution epoch",
            ));
        }
        Ok(reply)
    }
    pub async fn request(&self, peer: StageId, request: Request) -> Result<Response> {
        let label = match &request {
            Request::Catalog { .. } => "catalog",
            Request::Prepare { .. } => "prepare",
            Request::Publish { .. } => "publish",
            Request::Resources { .. } => "resources",
            Request::Input { .. } => "input",
            Request::FreezeRecovery => "freeze_recovery",
            Request::RecoverySnapshot { .. } => "recovery_snapshot",
            Request::SnapshotRange { .. } => "snapshot_range",
            Request::Snapshot { .. } => "snapshot",
            Request::Journal { .. } => "journal",
            Request::InstallRecovery { .. } => "install_recovery",
            Request::FinishRecovery { .. } => "finish_recovery",
            _ => "operation",
        };
        let began = Instant::now();
        let runtime = self.runtime()?;
        if peer == runtime.host.local {
            return Box::pin(self.handle(peer, request)).await;
        }
        let timeout = Duration::from_secs(
            if matches!(
                request,
                Request::Invoke { .. }
                    | Request::ReviewApproval { .. }
                    | Request::SubmitApproval { .. }
            ) {
                1200
            } else {
                20
            },
        );
        let handle = runtime.handle(peer)?;
        let bytes = super::core_transfer::encode_request(
            runtime.host.system.store(),
            &request,
            handle.packed_sync(),
        )?;
        let body = if handle.packed_sync() && bytes.len() <= super::core_transfer::INLINE_BYTES {
            Body::CoreRequestInline { bytes }
        } else {
            Body::CoreRequest {
                content: ContentStore::new(runtime.host.system.store())
                    .store_source(shared::ACTOR_ID, &bytes)?
                    .content,
            }
        };
        let response = handle.rpc_with_timeout(body, timeout).await?;
        let bytes = match response {
            Body::CoreResponse { content } => {
                handle
                    .content(
                        super::ActorAddress {
                            stage_id: peer,
                            actor_id: shared::ACTOR_ID.into(),
                        },
                        content,
                    )
                    .await?
            }
            Body::CoreResponseInline { bytes } if handle.packed_sync() => bytes,
            _ => return Err(StageError::Invalid("Invalid core reply")),
        };
        let response = super::core_transfer::decode_response(
            runtime.host.system.store(),
            &bytes,
            handle.packed_sync(),
        )?;
        tracing::debug!(peer=%peer,operation=label,elapsed_ms=began.elapsed().as_millis(),"Account device operation replied");
        if let Response::ApprovalDenied { rejection } = &response {
            let mut rejection = rejection.clone();
            rejection.reason = runtime.host.system.safe_error(&rejection.reason);
            return Err(StageError::ApprovalDenied(rejection));
        }
        if matches!(response, Response::Error { .. }) {
            return Err(StageError::Forbidden("Shared Actor operation was rejected"));
        }
        Ok(response)
    }
    pub async fn import_images(
        &self,
        peer: StageId,
        parts: &[crate::model_protocol::AgentContentPart],
    ) -> Result<()> {
        let runtime = self.runtime()?;
        for part in parts {
            if let crate::model_protocol::AgentContentPart::ResourceImage { resource, .. } = part {
                resource.validate()?;
                self.content(peer, &resource.content).await?;
                crate::resources::install(&runtime.host.system, shared::ACTOR_ID, resource)?;
            } else {
                return Err(StageError::Forbidden(
                    "Device image result must use a resource reference",
                ));
            }
        }
        Ok(())
    }
    async fn content(&self, peer: StageId, reference: &BlobRef) -> Result<Vec<u8>> {
        let runtime = self.runtime()?;
        if let Ok(bytes) = ContentStore::new(runtime.host.system.store()).verified_bytes(reference)
        {
            return Ok(bytes);
        }
        runtime
            .handle(peer)?
            .content(
                super::ActorAddress {
                    stage_id: peer,
                    actor_id: shared::ACTOR_ID.into(),
                },
                reference.clone(),
            )
            .await
    }
    pub async fn handle(&self, peer: StageId, request: Request) -> Result<Response> {
        let runtime = self.runtime()?;
        let system = &runtime.host.system;
        if peer != runtime.host.local {
            super::ledger::StageLedger::attach(system.store(), runtime.host.local)
                .peer(peer, chrono::Utc::now().timestamp_millis())?;
        }
        match request {
            Request::SubmitApproval {
                epoch,
                message,
                source,
                application,
            } => {
                self.handle_approval_application(peer, epoch, &message, source, application)
                    .await
            }
            Request::ReviewApproval { epoch, facts } => {
                self.handle_approval_review(epoch, facts).await
            }
            Request::ApprovalAppeal { epoch, approval_id } => {
                self.require_worker(peer, epoch).await?;
                if approval_id.is_empty() || approval_id.len() > 128 {
                    return Err(StageError::Invalid("Invalid approval reference"));
                }
                let record = system
                    .core_approval_appeal(&approval_id)
                    .map_err(|_| StageError::Forbidden("Appeal is outside account authority"))?;
                Ok(Response::ApprovalAppeal {
                    record: Box::new(record),
                })
            }
            Request::ApplyApprovalAppeal {
                epoch,
                authorization,
            } => {
                self.require_worker(peer, epoch).await?;
                let record = system
                    .apply_appeal_authorization(&authorization)
                    .await
                    .map_err(|_| StageError::Forbidden("Appeal decision could not be recorded"))?;
                Ok(Response::ApprovalAppeal {
                    record: Box::new(record),
                })
            }
            Request::RecoveryArchives { after } => {
                self.require_authority(peer).await?;
                self.touch_recovery(peer)?;
                let (entries, next_cursor) = super::recovery::archived_page(system.store(), after)?;
                Ok(Response::RecoveryArchives {
                    entries,
                    next_cursor,
                })
            }
            Request::RecoveryStatus => {
                let coordinator = runtime
                    .coordination
                    .read()
                    .clone()
                    .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
                coordinator.authorize(peer)?;
                if self
                    .frozen
                    .lock()
                    .as_ref()
                    .is_some_and(|(owner, _)| *owner == peer)
                {
                    self.touch_recovery(peer)?;
                }
                Ok(Response::RecoveryStatus {
                    status: self.recovery_status()?,
                })
            }
            Request::FreezeRecovery => {
                self.require_authority(peer).await?;
                self.freeze(peer).await?;
                Ok(Response::RecoveryStatus {
                    status: self.recovery_status()?,
                })
            }
            Request::RecoverySnapshot { head, token, page } => {
                let coordinator = runtime
                    .coordination
                    .read()
                    .clone()
                    .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
                coordinator.authorize(peer)?;
                if self.frozen.lock().is_none() || shared_store::head(system.store())? != head {
                    return Err(StageError::Conflict("Recovery snapshot is not frozen"));
                }
                // Replicas fetch from the frozen source on the authority's
                // behalf. Keep that source frozen while pages are consumed.
                if let Some(frozen) = self.frozen.lock().as_mut() {
                    frozen.1 = Instant::now();
                }
                Ok(Response::Snapshot {
                    page: super::shared_snapshot::produce(
                        system.store(),
                        &head,
                        token.as_deref(),
                        page,
                    )?,
                })
            }
            Request::InstallRecovery {
                source,
                head,
                epoch,
            } => {
                self.require_authority(peer).await?;
                let coordinator = runtime
                    .coordination
                    .read()
                    .clone()
                    .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
                coordinator.authorize(source)?;
                if self
                    .frozen
                    .lock()
                    .as_ref()
                    .is_none_or(|(authority, _)| *authority != peer)
                {
                    return Err(StageError::Conflict(
                        "Recovery was not frozen by this authority",
                    ));
                }
                self.touch_recovery(peer)?;
                let _guard = self.replica.lock().await;
                if shared_store::head(system.store())? != head {
                    if self.recovery.read().is_none() {
                        self.new_branch()?;
                    }
                    self.restore_recovery_snapshot(source, head.clone()).await?;
                }
                let receipt = self.head_receipt(peer, epoch, &head)?;
                Ok(Response::Prepared { receipt })
            }
            Request::FinishRecovery { head, epoch } => {
                self.require_authority(peer).await?;
                let (committed, _, _) = runtime.coordinate(Operation::Inspect, vec![]).await?;
                if committed.head != head
                    || committed.epoch < epoch
                    || shared_store::head(system.store())? != head
                {
                    return Err(StageError::Conflict("Recovery has not committed"));
                }
                super::recovery::clear_branch(system.store())?;
                super::recovery::mark_archives_merged(system.store())?;
                *self.recovery.write() = None;
                *self.frozen.lock() = None;
                *self.failure.write() = None;
                *self.desired.write() = None;
                self.recovered_epoch.store(0, Ordering::Release);
                self.epoch.store(epoch, Ordering::Release);
                system.store().stage_transaction(|tx| {
                    tx.execute("INSERT INTO core_meta VALUES('observed_epoch',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE CAST(value AS INTEGER)<CAST(excluded.value AS INTEGER)", [epoch.to_string()])?;
                    Ok(())
                })?;
                super::recovery::mark_synchronized(system.store())?;
                self.notify();
                Ok(Response::Published)
            }
            Request::Snapshot { head, token, page } => {
                let coordinated = self.coordinate(Operation::Inspect, vec![]).await?;
                if token.is_none() && coordinated.head != head {
                    return Err(StageError::Conflict("Snapshot head is not current"));
                }
                Ok(Response::Snapshot {
                    page: super::shared_snapshot::produce(
                        system.store(),
                        &head,
                        token.as_deref(),
                        page,
                    )?,
                })
            }
            Request::SnapshotRange {
                head,
                token,
                page,
                recovery,
            } => {
                if recovery {
                    let coordinator = runtime
                        .coordination
                        .read()
                        .clone()
                        .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
                    coordinator.authorize(peer)?;
                    if self.frozen.lock().is_none() || shared_store::head(system.store())? != head {
                        return Err(StageError::Conflict("Recovery snapshot is not frozen"));
                    }
                    if let Some(frozen) = self.frozen.lock().as_mut() {
                        frozen.1 = Instant::now();
                    }
                } else {
                    let coordinated = self.coordinate(Operation::Inspect, vec![]).await?;
                    if token.is_none() && coordinated.head != head {
                        return Err(StageError::Conflict("Snapshot head is not current"));
                    }
                }
                Ok(Response::SnapshotRange {
                    pages: super::shared_snapshot::produce_range(
                        system.store(),
                        &head,
                        token.as_deref(),
                        page,
                    )?,
                })
            }
            Request::Resources { records } => {
                tracing::debug!(stage=%runtime.host.local,peer=%peer,"Installing account resources");
                self.require_worker(runtime.host.local, self.epoch())
                    .await?;
                if records.len() > 32 {
                    return Err(StageError::Capacity("Resource page is too large"));
                }
                for record in records {
                    record.validate()?;
                    self.content(peer, &record.content).await?;
                    crate::resources::install(system, shared::ACTOR_ID, &record)?;
                }
                self.checkpoint().await?;
                tracing::debug!(stage=%runtime.host.local,"Account resources committed");
                Ok(Response::Published)
            }
            Request::ClearQueue => {
                self.require_worker(runtime.host.local, self.epoch())
                    .await?;
                let count = system
                    .store()
                    .clear_queued_users(shared::ACTOR_ID)
                    .map_err(|_| StageError::Unavailable("Queue could not be cleared"))?;
                self.checkpoint().await?;
                Ok(Response::QueueCleared { count })
            }
            Request::Amend {
                message,
                expected,
                content,
            } => {
                self.require_worker(runtime.host.local, self.epoch())
                    .await?;
                let original = system
                    .store()
                    .message(&message)
                    .map_err(|_| StageError::Forbidden("Queued input is unavailable"))?;
                let withdraw = content.is_none();
                let record = system
                    .store()
                    .amend_queued_user(
                        shared::ACTOR_ID,
                        &message,
                        expected,
                        content.unwrap_or_default(),
                        original.attachments,
                        withdraw,
                    )
                    .map_err(|_| StageError::Conflict("Queued input changed"))?;
                self.checkpoint().await?;
                Ok(Response::Steered {
                    message: Box::new(record),
                })
            }
            Request::Steer {
                message,
                target,
                expected,
            } => {
                self.require_worker(runtime.host.local, self.epoch())
                    .await?;
                let message = system
                    .store()
                    .steer_queued(shared::ACTOR_ID, &message, &target, expected)
                    .map_err(|_| {
                        tracing::debug!(expected,
                            input=?system.store().message(&message).ok().map(|m|(m.status,m.updated_at_ms,m.kind)),
                            target=?system.store().message(&target).ok().map(|m|(m.status,m.cancel_requested)),
                            "Guidance revision or execution changed");
                        StageError::Conflict("Queued input or target execution changed")
                    })?;
                self.checkpoint().await?;
                system.wake(shared::ACTOR_ID);
                Ok(Response::Steered {
                    message: Box::new(message),
                })
            }
            Request::Preferences {
                model_present,
                model,
                approval_mode,
            } => {
                self.require_worker(runtime.host.local, self.epoch())
                    .await?;
                system
                    .apply_owned_preferences(
                        shared::ACTOR_ID,
                        if model_present { Some(model) } else { None },
                        approval_mode,
                    )
                    .await
                    .map_err(|_| {
                        StageError::Invalid("Shared configuration is not valid on this worker")
                    })?;
                self.checkpoint().await?;
                Ok(Response::Published)
            }
            Request::Catalog { epoch } => {
                self.synchronize_worker_head(peer, epoch).await?;
                Ok(Response::Catalog {
                    apps: self.local_catalog()?,
                })
            }
            Request::Journal { after } => Ok(Response::Journal {
                batches: shared_store::batches(system.store(), &after, 8)?,
            }),
            Request::Prepare { batch } => {
                if batch.leader != peer {
                    return Err(StageError::Forbidden("Checkpoint sender is not its worker"));
                }
                let committed = self.require_worker(peer, batch.epoch).await?;
                let account = runtime
                    .host
                    .account_id()
                    .ok_or(StageError::Unavailable("Account is unavailable"))?;
                shared_store::validate(&batch, &account)?;
                let _replica = self.replica.lock().await;
                if committed.head != batch.previous && committed.head != batch.head {
                    return Err(StageError::Conflict(
                        "Checkpoint predecessor is not committed",
                    ));
                }
                // A returning voter can confirm execution metadata before its
                // business replica catches up. Install only the quorum's committed
                // predecessor before acknowledging the proposed immutable batch.
                let current = shared_store::head(system.store())?;
                if current != batch.previous && current != batch.head {
                    if current.sequence >= batch.previous.sequence {
                        return Err(StageError::Conflict("Checkpoint predecessor changed"));
                    }
                    if self.adopt(&batch.previous).is_err() {
                        self.restore_snapshot(peer, batch.previous.clone()).await?;
                    }
                }
                for change in &batch.changes {
                    self.content(peer, &change.content).await?;
                    self.install_resource_change(peer, change).await?;
                }
                self.require_worker(peer, batch.epoch).await?;
                shared_store::prepare(system.store(), &batch, &account)?;
                Ok(Response::Prepared {
                    receipt: self.receipt(&batch)?,
                })
            }
            Request::Publish { head } => {
                let _replica = self.replica.lock().await;
                let response = self.coordinate(Operation::Inspect, vec![]).await?;
                if response.head != head {
                    return Err(StageError::Conflict("Checkpoint has not been committed"));
                }
                self.adopt(&head)?;
                system.wake(shared::ACTOR_ID);
                Ok(Response::Published)
            }
            Request::Input { message } => {
                let epoch = self.epoch();
                self.require_worker(runtime.host.local, epoch).await?;
                if message.actor_id != shared::ACTOR_ID
                    || message.id.len() > 128
                    || message.content.len() > 64 * 1024
                    || message.status != MessageStatus::Queued
                    || message.remote_authority.is_some()
                    || message.delegation_grant.is_some()
                {
                    return Err(StageError::Forbidden("Invalid account Actor input"));
                }
                // Origin is accepted only from an authenticated native account device,
                // never from browser JSON. Attachments require the resource ledger.
                for attachment in &message.attachments {
                    let record =
                        crate::resources::descriptor(system, shared::ACTOR_ID, &attachment.id)?;
                    if record.name != attachment.name
                        || record.media_type != attachment.media_type
                        || record.content.size != attachment.size_bytes
                        || attachment.uri != format!("/v1/actors/core/resources/{}", attachment.id)
                    {
                        return Err(StageError::Forbidden("Attachment metadata changed"));
                    }
                }
                if let Ok(previous) = system.store().message(&message.id) {
                    if previous.content != message.content || previous.source != message.source {
                        return Err(StageError::Conflict("Input retry changed content"));
                    }
                } else {
                    system
                        .store()
                        .enqueue(*message, None)
                        .map_err(|_| StageError::Unavailable("Input was not persisted"))?;
                }
                self.checkpoint().await?;
                system.wake(shared::ACTOR_ID);
                Ok(Response::InputAccepted)
            }
            Request::Invoke {
                epoch,
                message,
                app,
                revision,
                call,
                identity_read_only,
            } => {
                self.invoke(
                    peer,
                    epoch,
                    &message,
                    &app,
                    &revision,
                    &call,
                    identity_read_only,
                )
                .await
            }
            Request::ToolResult { message, call_id } => self.effect_result(&message, &call_id),
        }
    }
    async fn snapshot_pages(
        &self,
        source: StageId,
        head: Head,
        token: Option<String>,
        page: u32,
        recovery: bool,
    ) -> Result<Vec<super::shared_snapshot::Page>> {
        let runtime = self.runtime()?;
        let packed = source != runtime.host.local && runtime.handle(source)?.packed_sync();
        let request = if packed {
            Request::SnapshotRange {
                head,
                token,
                page,
                recovery,
            }
        } else if recovery {
            Request::RecoverySnapshot { head, token, page }
        } else {
            Request::Snapshot { head, token, page }
        };
        match self.request(source, request).await? {
            Response::Snapshot { page } if !packed => Ok(vec![page]),
            Response::SnapshotRange { pages }
                if packed
                    && !pages.is_empty()
                    && pages.len() <= super::shared_snapshot::RANGE_PAGES =>
            {
                Ok(pages)
            }
            _ => Err(StageError::Invalid("Expected bounded Core snapshot pages")),
        }
    }
    async fn restore_snapshot(&self, source: StageId, head: Head) -> Result<()> {
        let runtime = self.runtime()?;
        let account = runtime
            .host
            .account_id()
            .ok_or(StageError::Unavailable("Account is unavailable"))?;
        let mut token = None;
        let mut page = 0;
        loop {
            for part in self
                .snapshot_pages(source, head.clone(), token.clone(), page, false)
                .await?
            {
                if part.head != head
                    || part.page != page
                    || token.as_ref().is_some_and(|token| *token != part.token)
                {
                    return Err(StageError::Conflict("Snapshot boundary changed"));
                }
                self.snapshot_content(source, &part.entries).await?;
                if super::shared_snapshot::receive(runtime.host.system.store(), &part, &account)? {
                    return Ok(());
                }
                token = Some(part.token);
                page += 1;
            }
        }
    }
    async fn snapshot_content(
        &self,
        source: StageId,
        entries: &[shared_store::Change],
    ) -> Result<()> {
        use futures_util::StreamExt;
        // Independent verified blobs can share the bounded peer download
        // window instead of paying two relay round trips for every record.
        let references = entries
            .iter()
            .map(|change| change.content.clone())
            .collect::<Vec<_>>();
        let mut downloads = futures_util::stream::iter(references)
            .map(|reference| async move { self.content(source, &reference).await })
            .buffer_unordered(8);
        while let Some(result) = downloads.next().await {
            result?;
            if let Some(frozen) = self.frozen.lock().as_mut() {
                frozen.1 = Instant::now();
            }
        }
        // Resource installation and the eventual page commit retain their
        // original order; a failed download never publishes a partial page.
        for change in entries {
            self.install_resource_change(source, change).await?;
            if let Some(frozen) = self.frozen.lock().as_mut() {
                frozen.1 = Instant::now();
            }
        }
        Ok(())
    }
    async fn require_authority(&self, peer: StageId) -> Result<()> {
        let runtime = self.runtime()?;
        let coordinator = runtime
            .coordination
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
        coordinator.authorize(peer)?;
        if coordinator.raft.current_leader().await.map(StageId::from) != Some(peer) {
            return Err(StageError::Forbidden(
                "Recovery must be directed by the account authority",
            ));
        }
        Ok(())
    }
    fn new_branch(&self) -> Result<super::recovery::Branch> {
        let runtime = self.runtime()?;
        let observed = runtime.host.system.store().stage_transaction(|tx| {
            let value: Option<String> = tx
                .query_row(
                    "SELECT value FROM core_meta WHERE key='observed_epoch'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(value
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0))
        })?;
        let epoch = self
            .epoch()
            .max(self.fence.lock().epoch())
            .max(observed)
            .checked_add(1)
            .filter(|epoch| *epoch < 9_007_199_254_740_991)
            .ok_or(StageError::Capacity("Recovery epoch exhausted"))?;
        let branch = super::recovery::Branch {
            id: uuid::Uuid::new_v4().to_string(),
            device: runtime.host.local,
            base: shared_store::head(runtime.host.system.store())?,
            epoch,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        super::recovery::save_branch(runtime.host.system.store(), &branch)?;
        self.epoch.store(epoch, Ordering::Release);
        *self.recovery.write() = Some(branch.clone());
        Ok(branch)
    }
    async fn freeze(&self, authority: StageId) -> Result<()> {
        let runtime = self.runtime()?;
        let epoch = self.epoch();
        let was_worker = self
            .fence
            .lock()
            .permits(runtime.host.local, epoch, Instant::now())
            && self.recovery.read().is_none();
        *self.frozen.lock() = Some((authority, Instant::now()));
        self.invalidate();
        runtime
            .host
            .system
            .quiesce_core()
            .await
            .map_err(|_| StageError::Unavailable("Core execution did not stop"))?;
        if was_worker {
            let _ = runtime
                .coordinate(Operation::Release { epoch }, vec![])
                .await;
        }
        let _checkpoint = tokio::time::timeout(Duration::from_secs(4), self.checkpoint.lock())
            .await
            .map_err(|_| StageError::Unavailable("Core checkpoint is stopping"))?;
        let _replica = tokio::time::timeout(Duration::from_secs(4), self.replica.lock())
            .await
            .map_err(|_| StageError::Unavailable("Core replica is stopping"))?;
        if shared_store::pending(runtime.host.system.store())? {
            let previous = self.recovery.read().clone();
            let branch = match previous {
                Some(branch) => branch,
                None => self.new_branch()?,
            };
            let account = runtime
                .host
                .account_id()
                .ok_or(StageError::Unavailable("Account is unavailable"))?;
            crate::resources::export_all(&runtime.host.system, shared::ACTOR_ID)?;
            while let Some(batch) = shared_store::capture(
                runtime.host.system.store(),
                &account,
                runtime.host.local,
                branch.epoch,
            )? {
                shared_store::commit(runtime.host.system.store(), &batch, runtime.host.local)?;
            }
        }
        Ok(())
    }
    fn touch_recovery(&self, authority: StageId) -> Result<()> {
        let mut frozen = self.frozen.lock();
        match frozen.as_mut() {
            Some((owner, touched)) if *owner == authority => {
                *touched = Instant::now();
                Ok(())
            }
            _ => Err(StageError::Conflict(
                "Recovery was not frozen by this authority",
            )),
        }
    }
    fn head_receipt(&self, authority: StageId, epoch: u64, head: &Head) -> Result<Acknowledgement> {
        let runtime = self.runtime()?;
        Ok(Acknowledgement {
            enrollment: runtime.host.enrollment()?,
            proof: runtime.host.identity()?.sign(
                "core-replica",
                &ReplicaAcknowledgement {
                    version: 1,
                    group_id: runtime
                        .host
                        .account_id()
                        .ok_or(StageError::Unavailable("Account is unavailable"))?,
                    stage_id: runtime.host.local,
                    leader: authority,
                    epoch,
                    head: head.clone(),
                    expires_at_ms: chrono::Utc::now().timestamp_millis() + 10000,
                },
            )?,
        })
    }
    async fn restore_recovery_snapshot(&self, source: StageId, head: Head) -> Result<()> {
        let runtime = self.runtime()?;
        let account = runtime
            .host
            .account_id()
            .ok_or(StageError::Unavailable("Account is unavailable"))?;
        let mut token = None;
        let mut page = 0;
        loop {
            if let Some(frozen) = self.frozen.lock().as_mut() {
                frozen.1 = Instant::now();
            }
            for part in self
                .snapshot_pages(source, head.clone(), token.clone(), page, true)
                .await?
            {
                if part.head != head
                    || part.page != page
                    || token.as_ref().is_some_and(|token| *token != part.token)
                {
                    return Err(StageError::Conflict("Recovery snapshot changed"));
                }
                self.snapshot_content(source, &part.entries).await?;
                let authority = self
                    .frozen
                    .lock()
                    .as_ref()
                    .map(|(authority, _)| *authority)
                    .ok_or(StageError::Conflict("Recovery is no longer frozen"))?;
                self.require_authority(authority).await?;
                let coordinator = runtime
                    .coordination
                    .read()
                    .clone()
                    .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
                coordinator.authorize(source)?;
                if super::shared_snapshot::receive_recovery(
                    runtime.host.system.store(),
                    &part,
                    &account,
                    runtime.host.local,
                )? {
                    return Ok(());
                }
                token = Some(part.token);
                page += 1;
            }
        }
    }
    async fn reconcile(&self) -> Result<bool> {
        let runtime = self.runtime()?;
        let connected = self.connected_peers(&runtime)?;
        let coordinator = runtime
            .coordination
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
        let needed = self.recovery.read().is_some()
            || super::recovery::archives_pending(runtime.host.system.store())?
            || self.peers.read().iter().any(|(peer, status)| {
                connected.contains(peer) && (status.branch.is_some() || status.archives_pending)
            });
        if !needed {
            return Ok(false);
        }
        if !self.recovery_authority_available(&runtime).await? {
            return Ok(false);
        }
        if coordinator.raft.current_leader().await.map(StageId::from) != Some(runtime.host.local) {
            if !connected.is_empty() {
                return Err(StageError::Unavailable(
                    "Conversation reconciliation is in progress",
                ));
            }
            return Ok(false);
        }
        // Freeze first. The metadata authority chooses a deterministic complete
        // branch; no model decides which effects to repeat or which rows to merge.
        tracing::debug!(authority=%runtime.host.local, "Conversation branch reconciliation started");
        self.freeze(runtime.host.local).await?;
        let mut frozen = vec![(runtime.host.local, self.recovery_status()?)];
        use futures_util::StreamExt;
        let mut freezes = futures_util::stream::iter(connected)
            .map(|peer| async move { (peer, self.request(peer, Request::FreezeRecovery).await) })
            .buffer_unordered(4);
        while let Some((peer, reply)) = freezes.next().await {
            if let Ok(Response::RecoveryStatus { status }) = reply {
                frozen.push((peer, status));
            }
        }
        drop(freezes);
        let keep_frozen = async {
            loop {
                futures_util::stream::iter(
                    frozen.iter().map(|(peer, _)| *peer).collect::<Vec<_>>(),
                )
                .map(|peer| async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        self.request(peer, Request::RecoveryStatus),
                    )
                    .await;
                })
                .buffer_unordered(4)
                .collect::<Vec<_>>()
                .await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        };
        let merge = async {
            let (source, selected) = frozen
                .iter()
                .min_by_key(|(peer, status)| (std::cmp::Reverse(status.head.sequence), *peer))
                .ok_or(StageError::Unavailable("No frozen recovery branch"))?;
            let source = *source;
            let mut head = selected.head.clone();
            tracing::debug!(authority=%runtime.host.local, source=%source, sequence=head.sequence, "Conversation recovery branch selected");
            let (current, _, _) = runtime.coordinate(Operation::Inspect, vec![]).await?;
            let epoch = frozen
                .iter()
                .map(|(_, status)| status.epoch)
                .max()
                .unwrap_or(0)
                .max(current.epoch)
                .checked_add(1)
                .filter(|epoch| *epoch < 9_007_199_254_740_991)
                .ok_or(StageError::Capacity("Recovery epoch exhausted"))?;
            // Collect losing conversation facts before any snapshot replacement.
            // A single authority builds and replicates one merged conversation;
            // model context remains the selected, valid tool protocol history.
            let mut branches = vec![];
            for (peer, status) in &frozen {
                let rows = self
                    .retained_conversation(*peer, status, *peer != source && status.head != head)
                    .await?;
                branches.push((*peer, rows));
            }
            if self.recovery.read().is_none() && branches.iter().any(|(_, rows)| !rows.is_empty()) {
                self.new_branch()?;
            }
            self.request(
                runtime.host.local,
                Request::InstallRecovery {
                    source,
                    head: head.clone(),
                    epoch,
                },
            )
            .await?;
            super::recovery::merge_conversation(runtime.host.system.store(), &branches)?;
            crate::resources::export_all(&runtime.host.system, shared::ACTOR_ID)?;
            let account = runtime
                .host
                .account_id()
                .ok_or(StageError::Unavailable("Account is unavailable"))?;
            while let Some(batch) = shared_store::capture(
                runtime.host.system.store(),
                &account,
                runtime.host.local,
                epoch,
            )? {
                shared_store::commit(runtime.host.system.store(), &batch, runtime.host.local)?;
            }
            head = shared_store::head(runtime.host.system.store())?;
            let source = runtime.host.local;
            let peers = frozen.iter().map(|(peer, _)| *peer).collect::<Vec<_>>();
            let mut installs = futures_util::stream::iter(peers.clone())
                .map(|peer| {
                    let head = head.clone();
                    async move {
                        (
                            peer,
                            self.request(
                                peer,
                                Request::InstallRecovery {
                                    source,
                                    head,
                                    epoch,
                                },
                            )
                            .await,
                        )
                    }
                })
                .buffer_unordered(4);
            let mut installed = vec![];
            while let Some((peer, result)) = installs.next().await {
                if matches!(result, Ok(Response::Prepared { .. })) {
                    installed.push(peer);
                }
            }
            drop(installs);
            tracing::debug!(authority=%runtime.host.local, replicas=installed.len(), "Conversation recovery branches installed");
            if !coordinator.durable_quorum(&installed) {
                return Err(StageError::Unavailable(
                    "Recovery replicas have not synchronized",
                ));
            }
            // Refresh voting receipts after all snapshot transfers, so an earlier
            // replica's short acknowledgement cannot expire during a large import.
            let mut receipts = vec![];
            for peer in installed
                .iter()
                .copied()
                .filter(|peer| coordinator.is_voter(*peer))
            {
                let Response::Prepared { receipt } = self
                    .request(
                        peer,
                        Request::InstallRecovery {
                            source,
                            head: head.clone(),
                            epoch,
                        },
                    )
                    .await?
                else {
                    return Err(StageError::Unavailable("Recovery receipt unavailable"));
                };
                receipts.push(receipt);
            }
            let (reply, _, _) = runtime
                .coordinate(
                    Operation::Reconcile {
                        epoch,
                        head: head.clone(),
                    },
                    receipts,
                )
                .await?;
            if !reply.accepted || reply.head != head {
                return Err(StageError::Unavailable(
                    "Recovery is waiting for the previous worker to stop",
                ));
            }
            // A lost Finish response is safe: committed metadata proves the chosen
            // head and epoch. A returning replica finishes the same import.
            for peer in installed {
                if self
                    .request(
                        peer,
                        Request::FinishRecovery {
                            head: head.clone(),
                            epoch,
                        },
                    )
                    .await
                    .is_ok()
                    && let Some(status) = self.peers.write().get_mut(&peer)
                    && status.epoch <= epoch
                {
                    status.branch = None;
                    status.head = head.clone();
                    status.epoch = epoch;
                    status.frozen = false;
                    status.archives_pending = false;
                }
            }
            Ok(true)
        };
        tokio::select! {
            result = merge => result,
            _ = keep_frozen => Err(StageError::Unavailable("Recovery renewal stopped")),
        }
    }

    async fn retained_conversation(
        &self,
        peer: StageId,
        status: &super::recovery::Status,
        active: bool,
    ) -> Result<Vec<super::recovery::RetainedRow>> {
        let mut rows = vec![];
        let mut after = None;
        let mut token = None;
        let mut page = 0;
        let mut archives = status.archives_pending;
        let mut snapshot = active;
        while archives || snapshot {
            if let Some(frozen) = self.frozen.lock().as_mut() {
                frozen.1 = Instant::now();
            }
            let entries = if archives {
                let Response::RecoveryArchives {
                    entries,
                    next_cursor,
                } = self
                    .request(peer, Request::RecoveryArchives { after })
                    .await?
                else {
                    return Err(StageError::Invalid(
                        "Expected archived conversation records",
                    ));
                };
                after = next_cursor;
                archives = next_cursor.is_some();
                entries
                    .into_iter()
                    .map(|entry| (entry.archive, entry.change))
                    .collect::<Vec<_>>()
            } else {
                let mut entries = vec![];
                for part in self
                    .snapshot_pages(peer, status.head.clone(), token.clone(), page, true)
                    .await?
                {
                    if part.head != status.head
                        || part.page != page
                        || token.as_ref().is_some_and(|token| *token != part.token)
                    {
                        return Err(StageError::Conflict("Recovery records changed"));
                    }
                    token = Some(part.token);
                    page += 1;
                    snapshot = !part.last;
                    entries.extend(
                        part.entries
                            .into_iter()
                            .map(|change| (format!("active/{}", status.head.hash), change)),
                    );
                }
                entries
            };
            for (branch, change) in entries {
                if !matches!(
                    change.entity,
                    shared_store::Entity::Message
                        | shared_store::Entity::Tool
                        | shared_store::Entity::Resource
                ) {
                    continue;
                }
                let bytes = self.content(peer, &change.content).await?;
                self.install_resource_change(peer, &change).await?;
                rows.push(super::recovery::RetainedRow {
                    branch,
                    entity: change.entity,
                    key: change.key,
                    bytes,
                });
            }
        }
        Ok(rows)
    }
    async fn start_local_recovery(&self) -> Result<bool> {
        let runtime = self.runtime()?;
        let coordinator = runtime
            .coordination
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
        coordinator.authorize(runtime.host.local)?;
        if self.draining.load(Ordering::Acquire)
            || self.recovery_authority_available(&runtime).await?
            || self.last_confirmed.lock().elapsed() < Duration::from_secs(6)
            || !super::recovery::synchronized(runtime.host.system.store())?
            || !runtime.host.system.workers_enabled()
            || runtime.host.system.config().validate().is_err()
            || !runtime
                .host
                .system
                .actor(shared::ACTOR_ID)
                .is_ok_and(|actor| runtime.host.system.provider(&actor).is_ok())
        {
            return Ok(false);
        }
        self.invalidate();
        runtime
            .host
            .system
            .quiesce_core()
            .await
            .map_err(|_| StageError::Unavailable("Core execution did not stop"))?;
        *self.frozen.lock() = None;
        if self.recovery.read().is_none() {
            self.new_branch()?;
        }
        self.advance_local().await?;
        Ok(true)
    }
    async fn advance_local(&self) -> Result<()> {
        let runtime = self.runtime()?;
        if !runtime
            .host
            .system
            .actor(shared::ACTOR_ID)
            .is_ok_and(|actor| runtime.host.system.provider(&actor).is_ok())
        {
            self.invalidate();
            return Err(StageError::Unavailable(
                "Local model credentials are unavailable",
            ));
        }
        let branch = self
            .recovery
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Recovery has not started"))?;
        let head = shared_store::head(runtime.host.system.store())?;
        self.coordinate(Operation::Claim { head: head.clone() }, vec![])
            .await?;
        self.epoch.store(branch.epoch, Ordering::Release);
        *self.desired.write() = Some(shared::Reply {
            request_id: uuid::Uuid::new_v4(),
            accepted: true,
            server_time_ms: chrono::Utc::now().timestamp_millis(),
            head,
            epoch: branch.epoch,
            lease: Some(shared::Lease {
                epoch: branch.epoch,
                leader: runtime.host.local,
                expires_at_ms: chrono::Utc::now().timestamp_millis()
                    + super::coordination::LEASE_MS,
            }),
            peers: vec![],
        });
        self.replica_wake.notify_waiters();
        if self.recovered_epoch.load(Ordering::Acquire) == branch.epoch {
            self.ready.store(true, Ordering::Release);
            runtime.host.system.wake(shared::ACTOR_ID);
        }
        Ok(())
    }
    fn receipt(&self, batch: &Batch) -> Result<Acknowledgement> {
        let runtime = self.runtime()?;
        let identity = runtime.host.identity()?;
        Ok(Acknowledgement {
            enrollment: runtime.host.enrollment()?,
            proof: identity.sign(
                "core-replica",
                &ReplicaAcknowledgement {
                    version: 1,
                    group_id: batch.account_id.clone(),
                    stage_id: runtime.host.local,
                    leader: batch.leader,
                    epoch: batch.epoch,
                    head: batch.head.clone(),
                    expires_at_ms: chrono::Utc::now().timestamp_millis() + 10000,
                },
            )?,
        })
    }
    async fn install_resource_change(
        &self,
        peer: StageId,
        change: &shared_store::Change,
    ) -> Result<()> {
        if change.entity == shared_store::Entity::Resource {
            let runtime = self.runtime()?;
            let bytes =
                ContentStore::new(runtime.host.system.store()).verified_bytes(&change.content)?;
            let resource: crate::resources::Resource = serde_json::from_slice(&bytes)
                .map_err(|_| StageError::Invalid("Invalid resource checkpoint"))?;
            resource.validate()?;
            self.content(peer, &resource.content).await?;
            crate::resources::install(&runtime.host.system, shared::ACTOR_ID, &resource)?;
        }
        Ok(())
    }
    async fn push_resources(&self, worker: StageId) -> Result<()> {
        let runtime = self.runtime()?;
        let resources = crate::resources::export_all(&runtime.host.system, shared::ACTOR_ID)?;
        if worker == runtime.host.local {
            return Ok(());
        }
        for page in resources.chunks(32) {
            let unsent=runtime.host.system.store().stage_transaction(|tx|{
                tx.execute_batch("CREATE TABLE IF NOT EXISTS core_resource_sent(peer TEXT NOT NULL,id TEXT NOT NULL,hash TEXT NOT NULL,PRIMARY KEY(peer,id));")?;
                page.iter().filter_map(|r|match tx.query_row("SELECT hash FROM core_resource_sent WHERE peer=?1 AND id=?2",params![worker.to_string(),r.id],|row|row.get::<_,String>(0)).optional(){Ok(Some(hash)) if hash==r.content.hash=>None,Ok(_)=>Some(Ok(r.clone())),Err(error)=>Some(Err(error.into()))}).collect::<Result<Vec<_>>>()
            })?;
            if unsent.is_empty() {
                continue;
            }
            self.request(
                worker,
                Request::Resources {
                    records: unsent.clone(),
                },
            )
            .await?;
            runtime.host.system.store().stage_transaction(|tx|{for record in unsent{tx.execute("INSERT INTO core_resource_sent VALUES(?1,?2,?3) ON CONFLICT(peer,id) DO UPDATE SET hash=excluded.hash",params![worker.to_string(),record.id,record.content.hash])?;}Ok(())})?;
        }
        Ok(())
    }
    fn adopt(&self, head: &Head) -> Result<()> {
        let runtime = self.runtime()?;
        if shared_store::head(runtime.host.system.store())? == *head {
            return Ok(());
        }
        let batch = shared_store::prepared(runtime.host.system.store(), head)?.ok_or(
            StageError::Unavailable("Committed Actor state is not yet available"),
        )?;
        shared_store::commit(runtime.host.system.store(), &batch, runtime.host.local)
    }
    pub async fn checkpoint(&self) -> Result<()> {
        let _guard = self.checkpoint.lock().await;
        let runtime = self.runtime()?;
        let epoch = self.epoch();
        let coordinated = self.require_worker(runtime.host.local, epoch).await?;
        if self.recovery.read().is_some() {
            let account = runtime
                .host
                .account_id()
                .ok_or(StageError::Unavailable("Account is unavailable"))?;
            crate::resources::export_all(&runtime.host.system, shared::ACTOR_ID)?;
            while let Some(batch) = shared_store::capture(
                runtime.host.system.store(),
                &account,
                runtime.host.local,
                epoch,
            )? {
                let reply = self
                    .coordinate(
                        Operation::Commit {
                            epoch,
                            previous: batch.previous.clone(),
                            head: batch.head.clone(),
                        },
                        vec![],
                    )
                    .await?;
                if !reply.accepted {
                    return Err(StageError::Conflict("Recovery checkpoint changed"));
                }
                shared_store::commit(runtime.host.system.store(), &batch, runtime.host.local)?;
            }
            return Ok(());
        }
        // Commit may have reached the device quorum even when its response was lost.
        // Publish that immutable prepared state before proposing another head.
        self.adopt(&coordinated.head)?;
        crate::resources::export_all(&runtime.host.system, shared::ACTOR_ID)?;
        let account = runtime
            .host
            .account_id()
            .ok_or(StageError::Unavailable("Account is unavailable"))?;
        while let Some(batch) = shared_store::capture(
            runtime.host.system.store(),
            &account,
            runtime.host.local,
            epoch,
        )? {
            tracing::debug!(stage=%runtime.host.local,sequence=batch.head.sequence,changes=batch.changes.len(),"Preparing account checkpoint");
            let mut receipts = vec![];
            let mut replicas = vec![];
            let peers =
                super::ledger::StageLedger::attach(runtime.host.system.store(), runtime.host.local)
                    .peers(chrono::Utc::now().timestamp_millis())?;
            use futures_util::StreamExt;
            let mut prepared = futures_util::stream::iter(
                peers
                    .into_iter()
                    .filter(|peer| runtime.connected(peer.device.stage_id)),
            )
            .map(|peer| {
                let batch = batch.clone();
                async move {
                    (
                        peer.device.stage_id,
                        self.request(peer.device.stage_id, Request::Prepare { batch })
                            .await,
                    )
                }
            })
            .buffer_unordered(4);
            let coordinator = runtime
                .coordination
                .read()
                .clone()
                .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
            // A metadata quorum also requires enough durable business replicas.
            while !coordinator.durable_quorum(&[runtime.host.local]) {
                let Some((peer, result)) = prepared.next().await else {
                    break;
                };
                if let Ok(Response::Prepared { receipt }) = result {
                    receipts.push(receipt);
                    replicas.push(peer);
                    let mut durable = replicas.clone();
                    durable.push(runtime.host.local);
                    if coordinator.durable_quorum(&durable) {
                        break;
                    }
                }
            }
            drop(prepared);
            self.require_worker(runtime.host.local, epoch).await?;
            receipts.push(self.receipt(&batch)?);
            let result = self
                .coordinate(
                    Operation::Commit {
                        epoch,
                        previous: batch.previous.clone(),
                        head: batch.head.clone(),
                    },
                    receipts,
                )
                .await?;
            if !result.accepted || result.head != batch.head {
                return Err(StageError::Unavailable(
                    "Actor state has not reached a safe replica",
                ));
            }
            shared_store::commit(runtime.host.system.store(), &batch, runtime.host.local).inspect_err(|error|tracing::debug!(stage=%runtime.host.local,sequence=batch.head.sequence,reason=%error,"Committed checkpoint publication deferred"))?;
            tracing::debug!(stage=%runtime.host.local,sequence=batch.head.sequence,"Shared Actor checkpoint committed");
            for peer in replicas {
                let _ = self
                    .request(
                        peer,
                        Request::Publish {
                            head: batch.head.clone(),
                        },
                    )
                    .await;
            }
            if !self
                .fence
                .lock()
                .permits(runtime.host.local, epoch, Instant::now())
            {
                return Err(StageError::Forbidden(
                    "Worker lease expired during checkpoint",
                ));
            }
        }
        Ok(())
    }
    async fn synchronize_worker_head(&self, peer: StageId, epoch: u64) -> Result<()> {
        let committed = self.require_worker(peer, epoch).await?;
        let runtime = self.runtime()?;
        let _replica = self.replica.lock().await;
        if shared_store::head(runtime.host.system.store())? != committed.head
            && self.adopt(&committed.head).is_err()
        {
            self.restore_snapshot(peer, committed.head).await?;
        }
        Ok(())
    }
    fn local_catalog(&self) -> Result<Vec<Installation>> {
        let runtime = self.runtime()?;
        let actor = runtime
            .host
            .system
            .core_device_actor()
            .map_err(|_| StageError::Unavailable("Device App policy is unavailable"))?;
        let manager = builtin_apps::create_apps(
            &actor,
            runtime.host.system.clone(),
            &runtime.host.system.config(),
        )
        .map_err(|_| StageError::Unavailable("Device Apps are not configured"))?;
        let revision = super::digest(
            &serde_json::to_vec(&serde_json::json!({"actor":actor.config,"configuration":runtime.host.system.config()}))
                .map_err(|_| StageError::Invalid("Cannot identify App configuration"))?,
        );
        Ok(manager
            .all_tool_specs()
            .into_iter()
            .map(|(id, tools)| Installation {
                stage_id: runtime.host.local,
                device: runtime.host.settings().label.clone(),
                app: id.to_string(),
                revision: revision.clone(),
                tools,
            })
            .collect())
    }
    #[allow(clippy::too_many_arguments)]
    async fn invoke(
        &self,
        peer: StageId,
        epoch: u64,
        message: &str,
        app: &str,
        revision: &str,
        call: &AgentToolCall,
        identity_read_only: bool,
    ) -> Result<Response> {
        let runtime = self.runtime()?;
        let system = &runtime.host.system;
        self.synchronize_worker_head(peer, epoch).await?;
        let mut actor = system
            .core_device_actor()
            .map_err(|_| StageError::Unavailable("Device Apps are unavailable"))?;
        let current = super::digest(
            &serde_json::to_vec(
                &serde_json::json!({"actor":actor.config,"configuration":system.config()}),
            )
            .map_err(|_| StageError::Invalid("Invalid App policy"))?,
        );
        if current != revision {
            return Err(StageError::Forbidden("Device App installation changed"));
        }
        let input = system
            .store()
            .message(message)
            .map_err(|_| StageError::Unavailable("Actor input has not synchronized"))?;
        if input.actor_id != shared::ACTOR_ID
            || input.status != MessageStatus::Running
            || input.cancel_requested
        {
            return Err(StageError::Forbidden("App input is not active"));
        }
        let digest=super::digest(&serde_json::to_vec(&serde_json::json!({"app":app,"revision":revision,"call":call,"identity_read_only":identity_read_only})).map_err(|_|StageError::Invalid("Invalid App invocation"))?);
        let exists = system.store().stage_transaction(|tx| {
            let previous: Option<String> = tx
                .query_row(
                    "SELECT digest FROM core_app_effects WHERE message=?1 AND call=?2",
                    params![message, call.id],
                    |r| r.get(0),
                )
                .optional()?;
            if previous
                .as_ref()
                .is_some_and(|previous| previous != &digest)
            {
                return Err(StageError::Conflict(
                    "App call retry changed its immutable arguments",
                ));
            }
            Ok(previous.is_some())
        })?;
        if exists {
            return self.effect_result(message, &call.id);
        }
        if identity_read_only {
            actor.remote_read_only = true;
            actor.config.permissions.writable_roots.clear();
        }
        let cache_revision = super::digest(
            &serde_json::to_vec(
                &serde_json::json!({"actor":actor.config,"configuration":system.config()}),
            )
            .map_err(|_| StageError::Invalid("Invalid effective App scope"))?,
        );
        let mut resources = self.device_apps.lock().await;
        if resources
            .as_ref()
            .is_none_or(|(revision, _)| revision != &cache_revision)
        {
            if let Some((_, old)) = resources.take() {
                let _ = old.shutdown().await;
            }
            *resources = Some((
                cache_revision.clone(),
                builtin_apps::create_apps(&actor, system.clone(), &system.config())
                    .map_err(|_| StageError::Unavailable("Device Apps could not start"))?,
            ));
        }
        let manager = &mut resources.as_mut().expect("device Apps installed").1;
        let mut context = system
            .execution_context_with_policy(&actor, &input, Some(actor.config.clone()))
            .map_err(|_| StageError::Forbidden("Device execution context is unavailable"))?;
        if let Some(caller) = &mut context.actor_call {
            caller.device_policy = Some(actor.config.clone());
        }
        let call = call.with_name(format!("{app}__{}", call.name));
        manager
            .validate_tool_call_for_approval(&call, &context)
            .map_err(|_| StageError::Forbidden("App call is outside its schema or grants"))?;
        let requested = manager
            .permission_request(&call, &context)
            .map_err(|_| StageError::Forbidden("App scope cannot be determined"))?;
        let control = Arc::new(ActorControl::default());
        let deadline = Instant::now() + Duration::from_secs(1190);
        let requirement = manager
            .review_requirement(&call, &context)
            .map_err(|_| StageError::Invalid("App review policy is unavailable"))?;
        let review = crate::actors::approval::authorize(
            system,
            &actor,
            &input,
            &call,
            &control,
            &context,
            &requested,
            requirement.as_ref(),
        );
        tokio::pin!(review);
        let approved = loop {
            tokio::select! {
                result=&mut review=>break result.map_err(|error| {
                    crate::actors::approval::rejection(&error).map(StageError::ApprovalDenied)
                        .unwrap_or(StageError::Forbidden("App authorization could not be completed"))
                })?,
                _=tokio::time::sleep(Duration::from_millis(25))=>{
                    if Instant::now() >= deadline || !self.fence.lock().permits(peer,epoch,Instant::now())
                        || system.store().message(message).is_ok_and(|input| input.cancel_requested || input.status != MessageStatus::Running) {
                        control.stop.store(true,Ordering::Release);
                        // Let authorize observe cancellation and resolve its own
                        // pending Ask or Auto record before dropping the wait.
                        let _ = (&mut review).await;
                        return Err(StageError::Forbidden("Worker changed during App approval"));
                    }
                },
            }
        };
        self.require_worker(peer, epoch).await?;
        let result_id = system
            .store()
            .shared_tool_started(&actor.id, message, &call, actor.remote_read_only, &digest)
            .map_err(|_| StageError::Unavailable("Cannot persist device execution provenance"))?;
        let mut future = Box::pin(manager.execute_tool(&call, &approved));
        let result = loop {
            tokio::select! {
                result=&mut future=>break Some(result),
                _=tokio::time::sleep(Duration::from_millis(25))=>{if Instant::now() >= deadline || !self.fence.lock().permits(peer,epoch,Instant::now()) || system.store().message(message).is_ok_and(|input| input.cancel_requested || input.status != MessageStatus::Running){break None;}},
            }
        };
        drop(future);
        let result = match result {
            Some(Ok(mut result)) => {
                crate::actors::runtime_permissions::attach_reference(
                    system,
                    &actor,
                    &input,
                    &call,
                    &approved,
                    &result_id,
                    &mut result,
                    None,
                )
                .map_err(|_| {
                    StageError::Unavailable("Cannot retain device permission reference")
                })?;
                Ok(result)
            }
            Some(Err(error)) => {
                if let Some(rejection) = crate::actors::approval::rejection(&error) {
                    Ok(AppToolExecutionResult::new(
                        "App operation was not approved",
                        rejection.payload(),
                        Some(rejection.model_content()),
                    ))
                } else {
                    let mut result =
                        crate::actors::runtime_permissions::error_result(system, &error);
                    crate::actors::runtime_permissions::attach_reference(
                        system,
                        &actor,
                        &input,
                        &call,
                        &approved,
                        &result_id,
                        &mut result,
                        Some(&error),
                    )
                    .map_err(|_| {
                        StageError::Unavailable("Cannot retain device permission reference")
                    })?;
                    Ok(result)
                }
            }
            None => Err(StageError::Unavailable("Device App result is uncertain")),
        };
        match result {
            Ok(mut result) => {
                result.model_images = crate::resources::capture_images(
                    system,
                    shared::ACTOR_ID,
                    result.model_images,
                )?;
                if app == "actors"
                    && let Some(payload) = result.payload.as_object_mut()
                {
                    if let Some(actors) = payload
                        .get_mut("actors")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        for actor in actors {
                            if let Some(id) = actor
                                .get("id")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                            {
                                actor["conversation_url"] = serde_json::json!(format!(
                                    "/stages/{}/actors/{id}",
                                    runtime.host.local
                                ));
                                actor["stage_id"] = serde_json::json!(runtime.host.local);
                            }
                        }
                    }
                    let id = payload
                        .get("actor_id")
                        .or_else(|| payload.get("id"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    if let Some(id) = id {
                        let url = format!("/stages/{}/actors/{id}", runtime.host.local);
                        payload.insert("conversation_url".into(), url.clone().into());
                        if let Some(content) = &mut result.model_content {
                            *content = content.replace(&format!("/actors/{id}"), &url);
                        }
                    }
                }
                let bytes = serde_json::to_vec(&result)
                    .map_err(|_| StageError::Invalid("Cannot persist App result"))?;
                system.store().stage_transaction(|tx|{tx.execute("UPDATE core_app_effects SET state='completed',result=?3 WHERE message=?1 AND call=?2",params![message,call.id,bytes])?;Ok(())})?;
                self.push_resources(peer).await?;
                Ok(Response::ToolResult {
                    result: Some(result),
                    uncertain: false,
                })
            }
            Err(error) => {
                system.store().stage_transaction(|tx| {
                    tx.execute(
                        "UPDATE core_app_effects SET state='unknown' WHERE message=?1 AND call=?2",
                        params![message, call.id],
                    )?;
                    Ok(())
                })?;
                Err(error)
            }
        }
    }
    fn effect_result(&self, message: &str, call: &str) -> Result<Response> {
        let runtime = self.runtime()?;
        runtime.host.system.store().stage_transaction(|tx| {
            let record: Option<(String, Option<Vec<u8>>)> = tx
                .query_row(
                    "SELECT state,result FROM core_app_effects WHERE message=?1 AND call=?2",
                    params![message, call],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((state, bytes)) = record else {
                return Ok(Response::ToolResult {
                    result: None,
                    uncertain: true,
                });
            };
            let result = bytes
                .map(|bytes| {
                    serde_json::from_slice::<AppToolExecutionResult>(&bytes)
                        .map_err(|_| StageError::Invalid("Invalid stored App result"))
                })
                .transpose()?;
            Ok(Response::ToolResult {
                result,
                uncertain: state != "completed",
            })
        })
    }
    pub async fn run(self: Arc<Self>) {
        let device = self.runtime.upgrade().map(|runtime| runtime.host.local);
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let sync = self.clone();
        let syncing = tokio::spawn(async move {
            let Some(runtime) = sync.runtime.upgrade() else {
                return;
            };
            let changes = runtime.host.system.store().changed();
            while !sync.stopped.load(Ordering::Acquire) {
                let changed = changes.notified();
                let coordinated = sync.replica_wake.notified();
                tokio::pin!(changed);
                tokio::pin!(coordinated);
                changed.as_mut().enable();
                coordinated.as_mut().enable();
                if let Err(error) = sync.synchronize().await {
                    tracing::debug!(reason=%error,"Shared Actor replica synchronization deferred");
                    tokio::select! {_=coordinated=>{},_=tokio::time::sleep(Duration::from_millis(500))=>{}}
                } else {
                    tokio::select! {_=changed=>{},_=coordinated=>{},_=tokio::time::sleep(Duration::from_secs(2))=>{}}
                }
            }
        });
        let flush = self.clone();
        let flushing = tokio::spawn(async move {
            let Some(runtime) = flush.runtime.upgrade() else {
                return;
            };
            let changes = runtime.host.system.store().changed();
            while !flush.stopped.load(Ordering::Acquire) {
                let changed = changes.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if flush.permitted()
                    && shared_store::pending(runtime.host.system.store()).unwrap_or(false)
                    && let Err(error) = flush.checkpoint().await
                {
                    tracing::debug!(reason=%error,"Shared Actor checkpoint deferred");
                }
                tokio::select! {_=changed=>{},_=tokio::time::sleep(Duration::from_secs(2))=>{}}
            }
        });
        while !self.stopped.load(Ordering::Acquire) {
            if let Err(error) = self.advance().await {
                self.invalidate();
                *self.failure.write() = Some(match &error {
                    StageError::Capacity(_) => "recovery_storage_full",
                    StageError::Forbidden(_) => "recovery_unauthorized",
                    StageError::Unavailable(reason) if reason.contains("reconciliation") => {
                        "reconciling"
                    }
                    StageError::Unavailable(_) => "quorum_unavailable",
                    _ => "recovery_failed",
                });
                if matches!(error, StageError::Unavailable(_))
                    && let Err(error) = self.start_local_recovery().await
                {
                    tracing::debug!(reason=%error,"Local recovery deferred");
                }
                tracing::debug!(device=?device,reason=%error,"Shared Actor worker coordination is reconnecting");
            } else {
                *self.failure.write() = None;
            }
            tokio::select! {_=interval.tick()=>{},_=self.wake.notified()=>{}}
        }
        syncing.abort();
        let _ = syncing.await;
        flushing.abort();
        let _ = flushing.await;
        self.invalidate();
    }
    async fn synchronize(&self) -> Result<()> {
        if self.frozen.lock().is_some() {
            return Ok(());
        }
        let Some(desired) = self.desired.read().clone() else {
            return Ok(());
        };
        let runtime = self.runtime()?;
        let system = &runtime.host.system;
        let head = shared_store::head(system.store())?;
        if let Some(lease) = &desired.lease
            && lease.leader != runtime.host.local
            && self
                .fence
                .lock()
                .permits(lease.leader, lease.epoch, Instant::now())
        {
            self.forward_inputs(lease.leader).await?;
        }
        if desired.head == head {
            super::recovery::mark_synchronized(system.store())?;
        }
        if desired.head != head {
            let _replica = self.replica.lock().await;
            // Another incoming checkpoint may have finished while we waited.
            let head = shared_store::head(system.store())?;
            if head.sequence >= desired.head.sequence {
                return Ok(());
            }
            if self.adopt(&desired.head).is_err() {
                let source = desired
                    .peers
                    .iter()
                    .filter(|peer| peer.stage_id != runtime.host.local && peer.head == desired.head)
                    .min_by_key(|peer| (peer.priority, peer.stage_id))
                    .map(|peer| peer.stage_id)
                    .or_else(|| {
                        desired
                            .lease
                            .as_ref()
                            .map(|lease| lease.leader)
                            .filter(|peer| *peer != runtime.host.local && runtime.connected(*peer))
                    })
                    .ok_or(StageError::Unavailable("Actor context is synchronizing"))?;
                runtime.want(source);
                let batches = match self.request(source, Request::Journal { after: head }).await {
                    Ok(Response::Journal { batches }) => batches,
                    _ => {
                        self.restore_snapshot(source, desired.head.clone()).await?;
                        return Ok(());
                    }
                };
                for batch in batches {
                    if batch.head.sequence > desired.head.sequence {
                        break;
                    }
                    for change in &batch.changes {
                        if self.content(source, &change.content).await.is_err() {
                            self.restore_snapshot(source, desired.head.clone()).await?;
                            return Ok(());
                        }
                        self.install_resource_change(source, change).await?;
                    }
                    let account = runtime
                        .host
                        .account_id()
                        .ok_or(StageError::Unavailable("Account is unavailable"))?;
                    shared_store::prepare(system.store(), &batch, &account)?;
                    shared_store::commit(system.store(), &batch, runtime.host.local)?;
                }
            }
            return Ok(());
        }
        let epoch = self.epoch();
        if self
            .fence
            .lock()
            .permits(runtime.host.local, epoch, Instant::now())
            && self.recovered_epoch.load(Ordering::Acquire) != epoch
        {
            if shared_store::local_history_pending(system.store())? {
                self.checkpoint().await?;
                shared_store::finish_local_history(system.store())?;
            }
            shared_store::resume(system.store())?;
            self.repair_protocol().await?;
            self.recovered_epoch.store(epoch, Ordering::Release);
            self.ready.store(true, Ordering::Release);
            system.wake(shared::ACTOR_ID);
        }
        Ok(())
    }
    async fn forward_inputs(&self, leader: StageId) -> Result<()> {
        let runtime = self.runtime()?;
        let system = &runtime.host.system;
        runtime.want(leader);
        let inputs=system.store().stage_transaction(|tx|{
            let data=tx.prepare("SELECT m.record FROM messages m WHERE m.actor_id='core' AND m.status='\"queued\"' AND NOT EXISTS(SELECT 1 FROM core_rows r WHERE r.entity='messages' AND r.remote_key=m.id) ORDER BY m.seq LIMIT 32")?.query_map([],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            data.into_iter().map(|row|serde_json::from_str::<crate::actors::MessageRecord>(&row).map_err(|_|StageError::Invalid("Invalid pending account input"))).collect::<Result<Vec<_>>>()
        })?;
        if inputs.is_empty() {
            return Ok(());
        }
        self.push_resources(leader).await?;
        for mut message in inputs {
            message.delegation_grant = None;
            message.remote_authority = None;
            match self
                .request(
                    leader,
                    Request::Input {
                        message: Box::new(message),
                    },
                )
                .await?
            {
                Response::InputAccepted => {}
                _ => {
                    return Err(StageError::Unavailable(
                        "Account input has not been accepted",
                    ));
                }
            }
        }
        Ok(())
    }
    async fn repair_protocol(&self) -> Result<()> {
        let runtime = self.runtime()?;
        let system = &runtime.host.system;
        let messages = system
            .store()
            .messages(shared::ACTOR_ID, None, 100)
            .map_err(|_| StageError::Unavailable("Cannot read resumable inputs"))?;
        for message in messages
            .items
            .into_iter()
            .filter(|message| message.status == MessageStatus::Queued)
        {
            if let Some(answer) = shared_store::completed_answer(system.store(), &message.id)? {
                system
                    .store()
                    .finish_message(&message.id, MessageStatus::Completed, Some(&answer), None)
                    .map_err(|_| StageError::Unavailable("Cannot deliver committed answer"))?;
                continue;
            }
            let mut recovered_images = Vec::new();
            for call in shared_store::incomplete_calls(system.store(), &message.id)? {
                if call.name == "actors__deliver"
                    && let Some(receipt) = system
                        .store()
                        .delivery_receipt(shared::ACTOR_ID, &message.id, &call.id)
                        .map_err(|_| StageError::Unavailable("Cannot read delivery receipt"))?
                {
                    system.store().publish_delivery(&receipt.id).map_err(|_| {
                        StageError::Unavailable("Cannot publish committed delivery")
                    })?;
                    let payload = serde_json::json!({"message_id":receipt.id,"finish_turn":call.arguments.get("finish_turn").and_then(serde_json::Value::as_bool).unwrap_or(false)});
                    system
                        .store()
                        .tool_finished(
                            shared::ACTOR_ID,
                            &message.id,
                            &call,
                            payload.clone(),
                            &payload.to_string(),
                            "Message delivered",
                            None,
                        )
                        .map_err(|_| {
                            StageError::Unavailable("Cannot recover delivery observation")
                        })?;
                    continue;
                }
                let mut devices =
                    super::ledger::StageLedger::attach(system.store(), runtime.host.local)
                        .peers(chrono::Utc::now().timestamp_millis())?
                        .into_iter()
                        .map(|peer| peer.device.stage_id)
                        .collect::<Vec<_>>();
                devices.push(runtime.host.local);
                let source = if call.name.starts_with("actors__") {
                    call.arguments
                        .get("stage_id")
                        .cloned()
                        .and_then(|value| serde_json::from_value::<StageId>(value).ok())
                        .filter(|id| devices.contains(id))
                } else {
                    devices.into_iter().find(|stage| {
                        call.name
                            .starts_with(&format!("d_{}_", &stage.0.simple().to_string()[..12]))
                    })
                };
                if let Some(source) = source
                    && let Ok(Response::ToolResult {
                        result: Some(result),
                        uncertain: false,
                    }) = self
                        .request(
                            source,
                            Request::ToolResult {
                                message: message.id.clone(),
                                call_id: call.id.clone(),
                            },
                        )
                        .await
                {
                    let content = result
                        .model_content
                        .unwrap_or_else(|| result.payload.to_string());
                    self.import_images(source, &result.model_images).await?;
                    let actor = system.actor(shared::ACTOR_ID).map_err(|_| {
                        StageError::Unavailable("Actor configuration is unavailable")
                    })?;
                    let config = system.config();
                    let limit = config
                        .models
                        .get(actor.config.model.as_ref().unwrap_or(&config.main_model))
                        .map_or(4096, |model| model.tool_output_max_tokens);
                    let mut content = crate::resources::bound_output(
                        system,
                        shared::ACTOR_ID,
                        &call.id,
                        &content,
                        limit,
                        result.overflow_continuation,
                    )?;
                    if let Some(reference) = &result.permission_reference {
                        reference.append_to(&mut content);
                    }
                    let images = crate::resources::capture_images(
                        system,
                        shared::ACTOR_ID,
                        result.model_images,
                    )?;
                    system
                        .store()
                        .tool_finished(
                            shared::ACTOR_ID,
                            &message.id,
                            &call,
                            result.payload,
                            &content,
                            &result.summary,
                            result.failure.as_deref(),
                        )
                        .map_err(|_| {
                            StageError::Unavailable("Cannot preserve confirmed App result")
                        })?;
                    if !images.is_empty() {
                        // Persist only portable references, after closing all call groups.
                        recovered_images.extend(images);
                    }
                    continue;
                }
                let text = "Execution changed devices before this call was confirmed. Effects may have occurred. Inspect the original device's App state; never automatically repeat this call.";
                system
                    .store()
                    .tool_finished(
                        shared::ACTOR_ID,
                        &message.id,
                        &call,
                        serde_json::Value::Null,
                        text,
                        "结果待确认",
                        Some(text),
                    )
                    .map_err(|_| {
                        StageError::Unavailable("Cannot preserve interrupted tool protocol")
                    })?;
            }
            if !recovered_images.is_empty() {
                let row = crate::model_protocol::AgentMessage::user_content(
                    crate::model_protocol::AgentContent::multimodal(
                        "Images returned by confirmed App calls",
                        recovered_images,
                    ),
                );
                system
                    .store()
                    .append_history(shared::ACTOR_ID, &message.id, &[row])
                    .map_err(|_| StageError::Unavailable("Cannot preserve recovered images"))?;
            }
        }
        Ok(())
    }
    async fn advance(&self) -> Result<()> {
        if self.draining.load(Ordering::Acquire) {
            return Err(StageError::Unavailable("Device is stopping"));
        }
        let runtime = self.runtime()?;
        let frozen = *self.frozen.lock();
        if let Some((authority, since)) = frozen {
            if since.elapsed() < Duration::from_secs(30)
                && (authority == runtime.host.local || runtime.connected(authority))
            {
                return Err(StageError::Unavailable(
                    "Conversation reconciliation is in progress",
                ));
            }
            *self.frozen.lock() = None;
        }
        if self.reconcile().await? {
            return Ok(());
        }
        if self.recovery.read().is_some() {
            return self.advance_local().await;
        }
        let system = &runtime.host.system;
        let head = shared_store::head(system.store())?;
        let eligible = system.workers_enabled()
            && system.config().validate().is_ok()
            && system
                .actor(shared::ACTOR_ID)
                .is_ok_and(|actor| system.provider(&actor).is_ok());
        let advertised = self
            .coordinate(
                Operation::Advertise {
                    head: head.clone(),
                    eligible,
                    priority: runtime.host.settings().compute_priority,
                },
                vec![],
            )
            .await?;
        // A checkpoint can commit while Advertise is in flight. Comparing its
        // reply to the pre-request head briefly revoked a healthy worker, left
        // its input running, and allowed later input to be claimed instead.
        let head = shared_store::head(system.store())?;
        *self.desired.write() = Some(advertised.clone());
        self.replica_wake.notify_waiters();
        // Account replicas exchange state even while the inbox is idle. A
        // replacement is already synchronized when the current worker is lost.
        for peer in &advertised.peers {
            if peer.stage_id != runtime.host.local {
                runtime.want(peer.stage_id);
            }
        }
        if advertised.head != head {
            let healthy = advertised.lease.as_ref().is_some_and(|lease| {
                self.fence
                    .lock()
                    .permits(runtime.host.local, lease.epoch, Instant::now())
            });
            if !(healthy
                && self.ready.load(Ordering::Acquire)
                && (head.sequence > advertised.head.sequence
                    || self.checkpoint.try_lock().is_err()))
            {
                self.ready.store(false, Ordering::Release);
            }
            return Ok(());
        }
        let own = advertised.lease.as_ref().is_some_and(|lease| {
            lease.leader == runtime.host.local
                && self
                    .fence
                    .lock()
                    .permits(runtime.host.local, lease.epoch, Instant::now())
        });
        if own && !eligible {
            self.coordinate(
                Operation::Release {
                    epoch: advertised.epoch,
                },
                vec![],
            )
            .await?;
            self.invalidate();
            return Ok(());
        }
        let granted = if own {
            self.coordinate(
                Operation::Renew {
                    epoch: advertised.epoch,
                    head: head.clone(),
                },
                vec![],
            )
            .await?
        } else {
            self.coordinate(Operation::Claim { head: head.clone() }, vec![])
                .await?
        };
        if granted.accepted
            && eligible
            && self
                .fence
                .lock()
                .permits(runtime.host.local, granted.epoch, Instant::now())
        {
            self.epoch.store(granted.epoch, Ordering::Release);
            *self.desired.write() = Some(granted);
            self.replica_wake.notify_waiters();
            if self.recovered_epoch.load(Ordering::Acquire) == self.epoch() {
                self.ready.store(true, Ordering::Release);
                system.wake(shared::ACTOR_ID);
            }
        } else {
            self.ready.store(false, Ordering::Release);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{actors::ActorSystem, app::AppToolExecutionContext, config::Config};
    use serde_json::json;

    async fn running_core() -> (
        tempfile::TempDir,
        Arc<Runtime>,
        Arc<Controller>,
        AppToolExecutionContext,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let system = ActorSystem::open_account_gated(
            directory.path().join("data"),
            directory.path().join("workspace"),
            Config::default(),
        )
        .await
        .unwrap();
        let host = super::super::host::Host::load(system.clone()).unwrap();
        host.initialize(true).unwrap();
        let runtime = Runtime::new(host);
        let core = Controller::new(&runtime).unwrap();
        system.install_shared_core(&core);
        grant(&core, runtime.host.local, Instant::now());
        let input = system
            .send_user(shared::ACTOR_ID, "Coordinate".into(), vec![], None)
            .unwrap();
        let running = system
            .store()
            .claim_next(shared::ACTOR_ID)
            .unwrap()
            .unwrap();
        assert_eq!(running.id, input.id);
        let context = system
            .execution_context(&system.actor(shared::ACTOR_ID).unwrap(), &running)
            .unwrap();
        (directory, runtime, core, context)
    }

    fn grant(core: &Controller, leader: StageId, now: Instant) {
        core.fence
            .lock()
            .observe(
                &shared::Reply {
                    request_id: uuid::Uuid::new_v4(),
                    accepted: true,
                    server_time_ms: 0,
                    head: Head::default(),
                    epoch: 1,
                    lease: Some(shared::Lease {
                        epoch: 1,
                        leader,
                        expires_at_ms: 6000,
                    }),
                    peers: vec![],
                },
                now,
                now,
            )
            .unwrap();
        core.epoch.store(1, Ordering::Release);
        core.recovered_epoch.store(1, Ordering::Release);
    }

    #[tokio::test]
    async fn core_tools_remain_authorized_during_sync() {
        let (_directory, runtime, core, context) = running_core().await;
        // A head mismatch pauses admission without revoking the current lease.
        core.ready.store(true, Ordering::Release);
        let mut apps = core.model_apps().await.unwrap();
        let call = AgentToolCall {
            id: "sync-devices".into(),
            name: "actors__devices".into(),
            arguments: json!({}),
        };
        apps.before_tool_call(&call, &context).unwrap();
        core.ready.store(false, Ordering::Release);
        assert!(
            !core.permitted(),
            "new input must still wait for synchronization"
        );
        let result = apps.execute_tool(&call, &context).await.unwrap();
        assert!(
            result.payload["devices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|device| {
                    device["stage_id"] == json!(runtime.host.local)
                        && device["online"] == true
                        && device["current"] == true
                })
        );
        let system = &runtime.host.system;
        let input = &context.actor_call.as_ref().unwrap().message_id;
        assert_eq!(
            system.store().message(input).unwrap().status,
            MessageStatus::Running
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn core_delivery_checks_execution_authority_independently_of_admission() {
        let (_directory, runtime, core, context) = running_core().await;
        let system = &runtime.host.system;
        let input = &context.actor_call.as_ref().unwrap().message_id;
        assert!(!core.permitted());
        let receipt = system
            .deliver_from_actor(shared::ACTOR_ID, input, "direct", "Still authorized")
            .unwrap();
        assert_eq!(receipt.reply.as_deref(), Some("Still authorized"));
        assert_eq!(receipt.in_reply_to.as_deref(), Some(input.as_str()));
        // This boundary persists locally; the Actors App checkpoints and
        // publishes the delivery separately before it becomes client-visible.
        let record: String = system
            .store()
            .stage_transaction(|tx| {
                Ok(tx.query_row(
                    "SELECT record FROM messages WHERE id=?1",
                    [&receipt.id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<crate::actors::MessageRecord>(&record).unwrap(),
            receipt
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn core_sync_does_not_bypass_revoked_execution_or_input_authority() {
        let (_directory, runtime, core, context) = running_core().await;
        let system = &runtime.host.system;
        let input = &context.actor_call.as_ref().unwrap().message_id;
        let mut apps = core.model_apps().await.unwrap();
        for reason in [
            "expired",
            "leader",
            "epoch",
            "unrecovered",
            "frozen",
            "draining",
            "stopped",
            "cancelled",
        ] {
            grant(&core, runtime.host.local, Instant::now());
            core.frozen.lock().take();
            core.draining.store(false, Ordering::Release);
            core.stopped.store(false, Ordering::Release);
            match reason {
                "expired" => grant(
                    &core,
                    runtime.host.local,
                    Instant::now() - Duration::from_secs(7),
                ),
                "leader" => grant(&core, StageId::new(), Instant::now()),
                "epoch" => core.epoch.store(2, Ordering::Release),
                "unrecovered" => core.recovered_epoch.store(0, Ordering::Release),
                "frozen" => *core.frozen.lock() = Some((StageId::new(), Instant::now())),
                "draining" => core.draining.store(true, Ordering::Release),
                "stopped" => core.stopped.store(true, Ordering::Release),
                "cancelled" => system.cancel(shared::ACTOR_ID, input).unwrap(),
                _ => unreachable!(),
            }
            let call = AgentToolCall {
                id: reason.into(),
                name: "actors__deliver".into(),
                arguments: json!({"content":"Must not be delivered","finish_turn":false}),
            };
            assert!(
                apps.execute_tool(&call, &context).await.is_err(),
                "{reason}"
            );
            assert!(
                system
                    .deliver_from_actor(shared::ACTOR_ID, input, reason, "Forbidden")
                    .is_err(),
                "{reason}"
            );
        }
        let deliveries: i64 = system.store().stage_transaction(|tx| {
            Ok(tx.query_row(
                "SELECT count(*) FROM messages WHERE actor_id='core' AND json_extract(record,'$.kind')='delivery'",
                [],
                |row| row.get(0),
            )?)
        }).unwrap();
        assert_eq!(
            deliveries, 0,
            "denied calls must not persist unpublished deliveries"
        );
        system.shutdown().await;
    }
    include!("shared_runtime/runtime_permission_tests.rs");
}
