//! Explicit Owner bootstrap from an authenticated, idle recovery branch.
//! This never initializes Raft, changes membership, or certifies a global commit.
use super::*;
use crate::stage::recovery::{self, ForkExport, ForkReceipt, ForkRequest};

struct Importing<'a>(&'a Controller);
impl Drop for Importing<'_> {
    fn drop(&mut self) {
        self.0.forking.store(false, Ordering::Release);
        self.0.notify();
    }
}

impl Controller {
    fn fork_authorize(&self, peer: StageId) -> Result<()> {
        let runtime = self.runtime()?;
        let coordinator = runtime
            .coordination
            .read()
            .clone()
            .ok_or(StageError::Unavailable("Device consensus is unavailable"))?;
        coordinator.authorize(peer)?;
        if peer != runtime.host.local {
            let ledger = super::super::ledger::StageLedger::attach(
                runtime.host.system.store(),
                runtime.host.local,
            );
            let now = chrono::Utc::now().timestamp_millis();
            let grant = ledger.authorize_actor(peer, shared::ACTOR_ID, now)?;
            if grant.read_only
                || !grant.read
                || !grant.converse
                || !grant.delegate
                || !ledger.manages_actor(peer, shared::ACTOR_ID, now)?
            {
                return Err(StageError::Forbidden(
                    "Recovery requires account management authority",
                ));
            }
        }
        Ok(())
    }
    fn pending_fork_work(&self) -> Result<bool> {
        self.runtime()?
            .host
            .system
            .store()
            .stage_transaction(recovery::has_pending_inputs)
    }
    fn check_fork_request(&self, request: &ForkRequest) -> Result<()> {
        let runtime = self.runtime()?;
        if uuid::Uuid::parse_str(&request.id).is_err()
            || request.id == request.source_branch.id
            || request.target == request.source
            || request.source_branch.device != request.source
            || runtime.host.account_id().as_deref() != Some(request.account.as_str())
            || super::super::reset::generation(&runtime.host)? != request.work_generation
            || super::super::reset::blocked(&runtime.host)
        {
            return Err(StageError::Conflict(
                "Recovery context changed; refresh before recovering",
            ));
        }
        request.source_head.validate()?;
        request.source_branch.base.validate()?;
        request.expected_local_head.validate()?;
        Ok(())
    }
    pub async fn recovery_fork_status(&self) -> Result<serde_json::Value> {
        use futures_util::StreamExt;
        let runtime = self.runtime()?;
        self.fork_authorize(runtime.host.local)?;
        let head = shared_store::head(runtime.host.system.store())?;
        let branch = recovery::branch(runtime.host.system.store())?;
        let eligible = head == Head::default()
            && branch.is_none()
            && !recovery::synchronized(runtime.host.system.store())?
            && !self.pending_fork_work()?
            && !self.recovery_authority_available(&runtime).await?;
        let mut sources = vec![];
        if eligible {
            let mut replies =
                futures_util::stream::iter(self.connected_peers(&runtime)?.into_iter().take(64))
                    .map(|peer| async move {
                        (
                            peer,
                            tokio::time::timeout(
                                Duration::from_secs(3),
                                self.request(peer, Request::RecoveryStatus),
                            )
                            .await,
                        )
                    })
                    .buffer_unordered(4);
            while let Some((peer, reply)) = replies.next().await {
                if self.fork_authorize(peer).is_err() {
                    continue;
                }
                if let Ok(Ok(Response::RecoveryStatus { status })) = reply {
                    self.note_recovery(peer, status.clone())?;
                    if status.branch.is_some() && !status.frozen {
                        sources.push(serde_json::json!({"source":peer,"label":runtime.host.device_label(peer).ok().flatten(),"branch":status.branch,"head":status.head}));
                    }
                }
            }
        }
        Ok(
            serde_json::json!({"account":runtime.host.account_id(),"target":runtime.host.local,"head":head,"branch":branch,"eligible":eligible,"sources":sources,"work_generation":super::super::reset::generation(&runtime.host)?}),
        )
    }
    /// The caller is the local Owner HTTP operation, never a model/App RPC.
    pub async fn fork_recovery(&self, request: ForkRequest) -> Result<ForkReceipt> {
        let runtime = self.runtime()?;
        let _lifecycle = runtime.host.lifecycle.lock().await;
        let _operation = self.fork_operation.lock().await;
        self.fork_authorize(runtime.host.local)?;
        self.check_fork_request(&request)?;
        if request.target != runtime.host.local {
            return Err(StageError::Forbidden("Recovery targets another device"));
        }
        if let Some(receipt) = recovery::fork_receipt(runtime.host.system.store(), &request.id)? {
            return if receipt.request == request {
                Ok(receipt)
            } else {
                Err(StageError::Conflict("Recovery retry changed"))
            };
        }
        self.fork_authorize(request.source)?;
        if shared_store::head(runtime.host.system.store())? != request.expected_local_head
            || request.expected_local_head != Head::default()
            || self.recovery.read().is_some()
            || recovery::synchronized(runtime.host.system.store())?
            || self.pending_fork_work()?
            || self.recovery_authority_available(&runtime).await?
            || self.draining.load(Ordering::Acquire)
        {
            return Err(StageError::Conflict(
                "Recovery requires a fresh, idle replica without account authority",
            ));
        }
        self.forking.store(true, Ordering::Release);
        let _importing = Importing(self);
        self.invalidate();
        let response = self
            .request(
                request.source,
                Request::BeginRecoveryFork {
                    request: Box::new(request.clone()),
                },
            )
            .await?;
        let export = match response {
            Response::RecoveryFork { export } => export,
            Response::RecoveryForkDeferred => {
                return Err(StageError::Conflict(
                    "Source work or recovery state changed; refresh and stop pending source tasks",
                ));
            }
            _ => return Err(StageError::Invalid("Expected a frozen recovery source")),
        };
        let result = async {
            if export.request != request
                || export.branch != request.source_branch
                || export.head.sequence < request.source_head.sequence
            {
                return Err(StageError::Conflict("Recovery source changed"));
            }
            let _replica = self.replica.lock().await;
            let mut token = None;
            let mut index = 0;
            loop {
                self.check_fork_request(&request)?;
                self.fork_authorize(request.source)?;
                let Response::Snapshot { page } = self
                    .request(
                        request.source,
                        Request::RecoveryForkPage {
                            id: request.id.clone(),
                            token: token.clone(),
                            page: index,
                        },
                    )
                    .await?
                else {
                    return Err(StageError::Invalid("Expected a recovery snapshot page"));
                };
                if page.head != export.head
                    || page.page != index
                    || token.as_ref().is_some_and(|token| *token != page.token)
                {
                    return Err(StageError::Conflict("Recovery snapshot changed"));
                }
                self.snapshot_content(request.source, &page.entries).await?;
                self.check_fork_request(&request)?;
                if super::super::shared_snapshot::receive_fork(
                    runtime.host.system.store(),
                    &page,
                    &request.account,
                    &export,
                )? {
                    break;
                }
                token = Some(page.token);
                index += 1;
            }
            let receipt = recovery::fork_receipt(runtime.host.system.store(), &request.id)?
                .ok_or(StageError::Invalid("Recovery receipt is unavailable"))?;
            *self.recovery.write() = Some(receipt.branch.clone());
            self.epoch.store(receipt.branch.epoch, Ordering::Release);
            self.recovered_epoch.store(0, Ordering::Release);
            *self.desired.write() = None;
            *self.failure.write() = None;
            self.replica_wake.notify_waiters();
            Ok(receipt)
        }
        .await;
        let _ = self
            .request(
                request.source,
                Request::ReleaseRecoveryFork { id: request.id },
            )
            .await;
        result
    }
    pub(super) async fn handle_fork(&self, peer: StageId, request: Request) -> Result<Response> {
        let runtime = self.runtime()?;
        self.fork_authorize(peer)?;
        match request {
            Request::BeginRecoveryFork { request } => {
                let _operation = self.fork_operation.lock().await;
                self.check_fork_request(&request)?;
                if request.source != runtime.host.local || request.target != peer {
                    return Err(StageError::Forbidden(
                        "Recovery source or recipient changed",
                    ));
                }
                let existing = self.fork_export.lock().clone();
                if let Some(export) = existing
                    && export.request == *request
                    && self
                        .frozen
                        .lock()
                        .as_ref()
                        .is_some_and(|(owner, _)| *owner == peer)
                {
                    return Ok(Response::RecoveryFork {
                        export: Box::new(export),
                    });
                }
                let branch = self.recovery.read().clone().ok_or(StageError::Conflict(
                    "Source is not a local recovery branch",
                ))?;
                if branch != request.source_branch
                    || shared_store::head(runtime.host.system.store())? != request.source_head
                    || self.frozen.lock().is_some()
                    || self.forking.load(Ordering::Acquire)
                    || self.draining.load(Ordering::Acquire)
                    || !self.permitted()
                    || self.pending_fork_work()?
                    || self.recovery_authority_available(&runtime).await?
                {
                    return Ok(Response::RecoveryForkDeferred);
                }
                // Check the inbox and fence admission while holding its database
                // lock. A task which started before this point must finish or be
                // explicitly stopped by the Owner; exporting must not cancel it.
                let idle = runtime.host.system.store().stage_transaction(|tx| {
                    if recovery::has_pending_inputs(tx)? {
                        return Ok(false);
                    }
                    *self.frozen.lock() = Some((peer, Instant::now()));
                    self.invalidate();
                    Ok(true)
                })?;
                if !idle {
                    return Ok(Response::RecoveryForkDeferred);
                }
                self.freeze(peer).await?;
                let frozen = (|| {
                    if self.pending_fork_work()? {
                        return Err(StageError::Conflict("Source work has not settled"));
                    }
                    let export = ForkExport {
                        request: *request,
                        branch,
                        head: shared_store::head(runtime.host.system.store())?,
                        epoch: self.epoch(),
                    };
                    *self.fork_export.lock() = Some(export.clone());
                    Ok(Response::RecoveryFork {
                        export: Box::new(export),
                    })
                })();
                if frozen.is_err() {
                    *self.frozen.lock() = None;
                    self.notify();
                }
                frozen
            }
            Request::RecoveryForkPage { id, token, page } => {
                let export = self
                    .fork_export
                    .lock()
                    .clone()
                    .ok_or(StageError::Conflict("Recovery export expired"))?;
                if export.request.id != id
                    || export.request.target != peer
                    || self.recovery.read().as_ref() != Some(&export.branch)
                    || shared_store::head(runtime.host.system.store())? != export.head
                {
                    return Err(StageError::Conflict("Recovery export changed"));
                }
                self.touch_recovery(peer)?;
                Ok(Response::Snapshot {
                    page: super::super::shared_snapshot::produce(
                        runtime.host.system.store(),
                        &export.head,
                        token.as_deref(),
                        page,
                    )?,
                })
            }
            Request::ReleaseRecoveryFork { id } => {
                let matches =
                    self.fork_export.lock().as_ref().is_some_and(|export| {
                        export.request.id == id && export.request.target == peer
                    });
                if matches {
                    self.fork_export.lock().take();
                    let mut frozen = self.frozen.lock();
                    if frozen.as_ref().is_some_and(|(owner, _)| *owner == peer) {
                        *frozen = None;
                    }
                    self.notify();
                }
                Ok(Response::Published)
            }
            _ => Err(StageError::Invalid("Invalid recovery operation")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::{ActorSystem, MessageKind, MessageRecord, MessageSource, ReplyTarget};
    async fn storage(path: &std::path::Path, name: &str) -> ActorSystem {
        let system = ActorSystem::open_account_gated(
            path.join("data"),
            path.join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        system.ensure_core_actor().unwrap();
        let mut actor = system.actor(shared::ACTOR_ID).unwrap();
        actor.config.instructions = name.into();
        system.store().save_actor(&actor).unwrap();
        shared_store::initialize(system.store(), "fork-account").unwrap();
        recovery::initialize(system.store()).unwrap();
        system
    }
    fn input() -> MessageRecord {
        MessageRecord {
            seq: 0,
            id: uuid::Uuid::new_v4().to_string(),
            actor_id: shared::ACTOR_ID.into(),
            kind: MessageKind::Request,
            source: MessageSource::User,
            reply_to: ReplyTarget::User,
            in_reply_to: None,
            remote_authority: None,
            content: "Retain the uncertain operation".into(),
            attachments: vec![],
            status: MessageStatus::Queued,
            cancel_requested: false,
            started_at_ms: None,
            finished_at_ms: None,
            final_generation: None,
            reply: None,
            error: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }
    fn export(source: &ActorSystem, target: &ActorSystem) -> ForkExport {
        let source_id = source.store().public_stage_id().unwrap();
        while let Some(batch) =
            shared_store::capture(source.store(), "fork-account", source_id, 1).unwrap()
        {
            shared_store::commit(source.store(), &batch, source_id).unwrap();
        }
        let head = shared_store::head(source.store()).unwrap();
        let branch = recovery::Branch {
            id: uuid::Uuid::new_v4().to_string(),
            device: source_id,
            base: Head::default(),
            epoch: 2,
            created_at_ms: 1,
        };
        ForkExport {
            request: ForkRequest {
                id: uuid::Uuid::new_v4().to_string(),
                account: "fork-account".into(),
                target: target.store().public_stage_id().unwrap(),
                source: source_id,
                source_branch: branch.clone(),
                source_head: head.clone(),
                expected_local_head: Head::default(),
                work_generation: None,
            },
            branch,
            head,
            epoch: 2,
        }
    }
    fn copied_page(
        source: &ActorSystem,
        target: &ActorSystem,
        export: &ForkExport,
    ) -> super::super::super::shared_snapshot::Page {
        let page =
            super::super::super::shared_snapshot::produce(source.store(), &export.head, None, 0)
                .unwrap();
        assert!(page.last);
        for change in &page.entries {
            let bytes = ContentStore::new(source.store())
                .verified_bytes(&change.content)
                .unwrap();
            ContentStore::new(target.store())
                .store_source(shared::ACTOR_ID, &bytes)
                .unwrap();
        }
        page
    }
    #[tokio::test]
    async fn recovery_fork_rejects_unsettled_or_corrupted_snapshots_atomically() {
        for corrupt in [false, true] {
            let source_dir = tempfile::tempdir().unwrap();
            let target_dir = tempfile::tempdir().unwrap();
            let source = storage(source_dir.path(), "source context").await;
            let target = storage(target_dir.path(), "target context").await;
            let message = source.store().enqueue(input(), None).unwrap();
            source.store().claim_next(shared::ACTOR_ID).unwrap();
            if corrupt {
                source
                    .store()
                    .finish_message(
                        &message.id,
                        MessageStatus::Completed,
                        Some("Completed outcome"),
                        None,
                    )
                    .unwrap();
            }
            let export = export(&source, &target);
            let page = copied_page(&source, &target, &export);
            if corrupt {
                target
                    .store()
                    .stage_transaction(|tx| {
                        tx.execute(
                            "UPDATE stage_blobs SET bytes=?1 WHERE hash=?2",
                            rusqlite::params![b"corrupt".as_slice(), page.entries[0].content.hash],
                        )?;
                        Ok(())
                    })
                    .unwrap();
            }
            assert!(
                super::super::super::shared_snapshot::receive_fork(
                    target.store(),
                    &page,
                    "fork-account",
                    &export
                )
                .is_err()
            );
            assert_eq!(shared_store::head(target.store()).unwrap(), Head::default());
            assert_eq!(
                target.actor(shared::ACTOR_ID).unwrap().config.instructions,
                "target context"
            );
            assert!(target.store().message(&message.id).is_err());
            assert!(recovery::branch(target.store()).unwrap().is_none());
            assert!(
                recovery::fork_receipt(target.store(), &export.request.id)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(recovery::archive_count(target.store()).unwrap(), 0);
            source.shutdown().await;
            target.shutdown().await;
        }
    }
    #[tokio::test]
    async fn recovery_fork_retains_uncertain_inputs_without_requeueing_their_effects() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let source = storage(source_dir.path(), "source context").await;
        let target = storage(target_dir.path(), "target context").await;
        let message = source.store().enqueue(input(), None).unwrap();
        source.store().claim_next(shared::ACTOR_ID).unwrap();
        let call = AgentToolCall {
            id: "uncertain-call".into(),
            name: "files__write_file".into(),
            arguments: serde_json::json!({"path":"effect.txt","content":"one effect"}),
        };
        source
            .store()
            .tool_started(shared::ACTOR_ID, &message.id, &call)
            .unwrap();
        source
            .store()
            .finish_message(
                &message.id,
                MessageStatus::Interrupted,
                None,
                Some("Effects may have occurred"),
            )
            .unwrap();
        let export = export(&source, &target);
        let page = copied_page(&source, &target, &export);
        assert!(
            super::super::super::shared_snapshot::receive_fork(
                target.store(),
                &page,
                "fork-account",
                &export
            )
            .unwrap()
        );
        shared_store::resume(target.store()).unwrap();
        let restored = target.store().message(&message.id).unwrap();
        assert_eq!(restored.status, MessageStatus::Interrupted);
        assert_eq!(restored.error.as_deref(), Some("Effects may have occurred"));
        assert!(
            target
                .store()
                .claim_next(shared::ACTOR_ID)
                .unwrap()
                .is_none()
        );
        let (done, record) = target.store().stage_transaction(|tx| {
            Ok(tx.query_row::<(i64, String), _, _>(
                "SELECT done,record FROM tools WHERE actor_id='core' AND message_id=?1 AND call_id=?2",
                rusqlite::params![message.id, call.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        }).unwrap();
        assert_eq!(
            done, 0,
            "unknown effects must not acquire a fabricated completion"
        );
        let intent: crate::actors::ToolResultRecord = serde_json::from_str(&record).unwrap();
        assert_eq!(intent.arguments, call.arguments);
        assert!(intent.finished_at_ms.is_none());
        assert_eq!(intent.payload, serde_json::Value::Null);
        assert!(recovery::branch(target.store()).unwrap().is_some());
        assert_eq!(
            recovery::fork_receipt(target.store(), &export.request.id)
                .unwrap()
                .unwrap()
                .head,
            export.head
        );
        assert!(recovery::archive_count(target.store()).unwrap() >= 1);
        source.shutdown().await;
        target.shutdown().await;
    }
}
