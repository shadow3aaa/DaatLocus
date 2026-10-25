//! Authenticated Stage input boundary. Transport adapters cannot manufacture Owner authority.
use super::{
    Result, StageError,
    blob::{ContentStore, Manifest},
    ledger::Receipt,
    protocol::{BlobAccess, Body, Envelope},
    transport::tls::VerifiedPeer,
};
use crate::actors::ActorSystem;

pub struct StageReceiver {
    system: ActorSystem,
}
impl StageReceiver {
    pub fn new(system: ActorSystem) -> Self {
        Self { system }
    }
    /// Control and resource requests bypass the Actor's serial work mailbox.
    /// Responses contain facts/references; the socket controller handles queues.
    pub fn handle_request(&self, peer: &VerifiedPeer, frame: &Envelope) -> Result<Envelope> {
        let now = chrono::Utc::now().timestamp_millis();
        let local = self.system.store().stage_local_id()?;
        let current = self
            .system
            .store()
            .stage_peer_for_authority(peer.stage_id())?;
        if current.device.tls_sha256 != peer.certificate_hash() {
            return Err(StageError::Forbidden("Peer identity changed"));
        }
        frame.validate(peer.stage_id(), local, current.generation, now)?;
        let ledger = super::ledger::StageLedger::attach(self.system.store(), local);
        let content = ContentStore::new(self.system.store());
        let body = match &frame.body {
            Body::ConversationControl { actor, action } => {
                if actor.stage_id != local
                    || !ledger.manages_actor(peer.stage_id(), &actor.actor_id, now)?
                {
                    return Err(StageError::Forbidden(
                        "Device cannot control this conversation",
                    ));
                }
                use super::protocol::ConversationAction;
                let mut count = 0;
                let record = match action {
                    ConversationAction::Cancel { message } => {
                        self.system
                            .cancel(&actor.actor_id, message)
                            .map_err(|_| StageError::Conflict("Input cannot be cancelled"))?;
                        Some(
                            self.system
                                .store()
                                .message(message)
                                .map_err(|_| StageError::Unavailable("Input is unavailable"))?,
                        )
                    }
                    ConversationAction::Amend {
                        message,
                        expected,
                        content,
                    } => {
                        let input = self
                            .system
                            .store()
                            .message(message)
                            .map_err(|_| StageError::Unavailable("Input is unavailable"))?;
                        Some(
                            self.system
                                .store()
                                .amend_queued_user(
                                    &actor.actor_id,
                                    message,
                                    *expected,
                                    content.clone().unwrap_or_default(),
                                    input.attachments,
                                    content.is_none(),
                                )
                                .map_err(|_| StageError::Conflict("Queued input changed"))?,
                        )
                    }
                    ConversationAction::ClearQueue => {
                        count = self
                            .system
                            .store()
                            .clear_queued_users(&actor.actor_id)
                            .map_err(|_| StageError::Conflict("Queue changed"))?;
                        None
                    }
                };
                self.system.wake(&actor.actor_id);
                Body::ConversationResult {
                    actor: actor.clone(),
                    record: record.map(Box::new),
                    count,
                }
            }
            Body::ApprovalsQuery { before, limit } => {
                let page = self
                    .system
                    .store()
                    .pending_approvals(None, *before, *limit as usize)
                    .map_err(|_| StageError::Unavailable("Cannot read pending approvals"))?;
                let mut records = vec![];
                for record in page.items {
                    if self
                        .system
                        .store()
                        .native_approval(&record.id)
                        .unwrap_or(false)
                        && ledger.manages_actor(peer.stage_id(), &record.actor_id, now)?
                    {
                        records.push(record);
                    }
                }
                Body::Approvals {
                    records,
                    next: page.next_cursor,
                }
            }
            Body::ApprovalQuery { id } => {
                let record = self
                    .system
                    .store()
                    .approval(id)
                    .map_err(|_| StageError::Unavailable("Approval is unavailable"))?;
                if !ledger.manages_actor(peer.stage_id(), &record.actor_id, now)?
                    || !self.system.store().native_approval(id).unwrap_or(false)
                {
                    return Err(StageError::Forbidden(
                        "Approval is outside device authority",
                    ));
                }
                Body::Approval {
                    record: Box::new(record),
                }
            }
            Body::ApprovalResolve { id, decision } => {
                let record = self
                    .system
                    .store()
                    .approval(id)
                    .map_err(|_| StageError::Unavailable("Approval is unavailable"))?;
                if !ledger.manages_actor(peer.stage_id(), &record.actor_id, now)?
                    || !self.system.store().native_approval(id).unwrap_or(false)
                {
                    return Err(StageError::Forbidden("Device cannot resolve this approval"));
                }
                if record.mode != crate::actors::ApprovalMode::Ask
                    || record.decision != crate::actors::ApprovalDecision::Pending
                    || !matches!(
                        decision,
                        crate::actors::ApprovalDecision::Approved
                            | crate::actors::ApprovalDecision::Denied
                    )
                {
                    return Err(StageError::Conflict(
                        "Approval does not await an owner decision",
                    ));
                }
                let input = self
                    .system
                    .store()
                    .message(&record.message_id)
                    .map_err(|_| StageError::Unavailable("Approval request is unavailable"))?;
                if input.cancel_requested || input.status != crate::actors::MessageStatus::Running {
                    return Err(StageError::Conflict("Execution is no longer active"));
                }
                let record = self
                    .system
                    .store()
                    .resolve_approval(id, *decision, "Resolved by account Owner on another device")
                    .map_err(|_| StageError::Conflict("Approval changed"))?;
                Body::Approval {
                    record: Box::new(record),
                }
            }
            Body::Authorize { actor } => {
                if actor.stage_id != local {
                    return Err(StageError::Forbidden("Authorization targets another host"));
                }
                self.system
                    .actor(&actor.actor_id)
                    .map_err(|_| StageError::Forbidden("Actor is unavailable"))?;
                let access = ledger.actor_grant(peer.stage_id(), &actor.actor_id, now)?;
                Body::ActorAccess {
                    actor: actor.clone(),
                    read: access.read,
                    converse: access.converse,
                }
            }
            Body::ReadQuery { actor, query } => Body::Observation {
                actor: actor.clone(),
                content: super::observation::reference(
                    &self.system,
                    peer.stage_id(),
                    actor,
                    query,
                    now,
                )?,
            },
            Body::DirectoryQuery { after, limit } => {
                let mut actors = vec![];
                for actor in self
                    .system
                    .list_visible()
                    .map_err(|_| StageError::Unavailable("Actor directory is unavailable"))?
                    .into_iter()
                    .filter(|actor| after.as_ref().is_none_or(|after| &actor.id > after))
                {
                    if let Ok(grant) = ledger.actor_grant(peer.stage_id(), &actor.id, now)
                        && grant.read
                    {
                        actors.push(super::protocol::RemoteActor {
                            address: super::ActorAddress {
                                stage_id: local,
                                actor_id: actor.id,
                            },
                            name: actor.name,
                            status: actor.status,
                            apps: actor.apps,
                            read_only: grant.read_only,
                            converse: grant.converse,
                        });
                    }
                }
                actors.sort_by(|a, b| a.address.actor_id.cmp(&b.address.actor_id));
                let next = (actors.len() > *limit as usize)
                    .then(|| actors[*limit as usize - 1].address.actor_id.clone());
                actors.truncate(*limit as usize);
                Body::Directory { actors, next }
            }
            Body::RevocationsQuery { after, limit } => {
                let mut records =
                    super::revocation::page(self.system.store(), *after, *limit as usize + 1)?;
                let next = if records.len() > *limit as usize {
                    Some(super::revocation::target(&records[*limit as usize - 1])?)
                } else {
                    None
                };
                records.truncate(*limit as usize);
                Body::Revocations { records, next }
            }
            Body::MembersQuery { after, limit } => {
                let mut members = ledger
                    .peers(now)?
                    .into_iter()
                    .filter(|member| after.is_none_or(|after| member.device.stage_id > after))
                    .collect::<Vec<_>>();
                members.sort_by_key(|member| member.device.stage_id);
                let next = (members.len() > *limit as usize)
                    .then(|| members[*limit as usize - 1].device.stage_id);
                members.truncate(*limit as usize);
                Body::Members {
                    revocations: super::revocation::page(self.system.store(), None, 32)?,
                    enrollments: members.into_iter().map(|peer| peer.enrollment).collect(),
                    next,
                }
            }
            Body::TransferAck { message } => {
                super::delivery::Deliveries::new(self.system.store(), local).acknowledge(
                    peer.stage_id(),
                    *message,
                    now,
                )?;
                Body::TransferAck { message: *message }
            }
            Body::ResultQuery { invocation } => {
                let result = ledger.result(peer.stage_id(), *invocation, now)?;
                Body::Result {
                    invocation: *invocation,
                    state: result.state,
                    content: result.result,
                }
            }
            Body::Cancel { invocation } => {
                let result = ledger.result(peer.stage_id(), *invocation, now)?;
                if let Some(message) = result.message_id {
                    self.system
                        .cancel(&result.actor_id, &message)
                        .map_err(|_| StageError::Conflict("Invocation cancellation failed"))?;
                }
                let result = ledger.result(peer.stage_id(), *invocation, now)?;
                Body::Result {
                    invocation: *invocation,
                    state: result.state,
                    content: result.result,
                }
            }
            Body::Subscribe {
                actor,
                cursor,
                level,
            } => Body::ActorEvents {
                actor: actor.clone(),
                page: super::journal::Journal::new(self.system.store()).page(
                    peer.stage_id(),
                    &actor.actor_id,
                    cursor.as_ref(),
                    *level,
                    now,
                    100,
                )?,
            },
            Body::SnapshotQuery { actor, token, page } => {
                let journal = super::journal::Journal::new(self.system.store());
                let page = if let Some(token) = token {
                    journal.snapshot_page(peer.stage_id(), &actor.actor_id, *token, *page, now)?
                } else {
                    journal.create_snapshot(peer.stage_id(), &actor.actor_id, now)?
                };
                Body::ActorSnapshot {
                    actor: actor.clone(),
                    page,
                }
            }
            Body::BlobManifestQuery {
                access,
                content: reference,
            } => {
                let manifest = match access {
                    BlobAccess::Actor { actor } => {
                        content.manifest(peer.stage_id(), &actor.actor_id, reference, now)?
                    }
                    BlobAccess::Invocation { invocation } => {
                        ledger.exported_manifest(peer.stage_id(), *invocation, reference, now)?
                    }
                };
                Body::BlobManifest {
                    access: access.clone(),
                    manifest,
                }
            }
            Body::BlobRead {
                access,
                content: reference,
                chunk,
            } => {
                let bytes = match access {
                    BlobAccess::Actor { actor } => {
                        content.chunk(peer.stage_id(), &actor.actor_id, reference, *chunk, now)?
                    }
                    BlobAccess::Invocation { invocation } => ledger.exported_chunk(
                        peer.stage_id(),
                        *invocation,
                        reference,
                        *chunk,
                        now,
                    )?,
                };
                Body::BlobChunk {
                    content: reference.clone(),
                    chunk: *chunk,
                    chunk_hash: super::digest(&bytes),
                    bytes,
                }
            }
            Body::Heartbeat => Body::Heartbeat,
            _ => return Err(StageError::Invalid("Unsupported Stage control request")),
        };
        Ok(Envelope {
            version: 1,
            source: local,
            target: peer.stage_id(),
            generation: current.generation,
            message_id: uuid::Uuid::new_v4(),
            invocation_id: frame.invocation_id,
            in_reply_to: Some(frame.message_id),
            created_at_ms: now,
            expires_at_ms: now.saturating_add(60_000),
            body,
        })
    }
    fn check_input(&self, peer: &VerifiedPeer, frame: &Envelope) -> Result<()> {
        let current = self
            .system
            .store()
            .stage_peer_for_authority(peer.stage_id())?;
        let local = self.system.store().stage_local_id()?;
        frame.validate(
            peer.stage_id(),
            local,
            current.generation,
            chrono::Utc::now().timestamp_millis(),
        )?;
        if current.device.tls_sha256 != peer.certificate_hash() {
            return Err(StageError::Forbidden("Peer identity changed"));
        }
        let Body::Invoke {
            target,
            origin,
            attachments,
            content,
            ..
        } = &frame.body
        else {
            return Err(StageError::Invalid("Expected invocation input"));
        };
        if (content.size == 0 && attachments.is_empty()) || content.size > 64 * 1024 {
            return Err(StageError::Invalid(
                "Invocation text exceeds admission budget",
            ));
        }
        let grant = super::ledger::StageLedger::attach(self.system.store(), local).actor_grant(
            peer.stage_id(),
            &target.actor_id,
            chrono::Utc::now().timestamp_millis(),
        )?;
        if !grant.converse {
            return Err(StageError::Forbidden("Device cannot invoke this Actor"));
        }
        if matches!(origin, super::protocol::Origin::Actor { .. }) && !grant.delegate {
            return Err(StageError::Forbidden("Device cannot delegate"));
        }
        for resource in attachments {
            if crate::resources::descriptor(&self.system, &target.actor_id, &resource.id)?
                != *resource
            {
                return Err(StageError::Forbidden(
                    "Attachment is outside the Actor delivery ledger",
                ));
            }
        }
        Ok(())
    }
    pub fn prepare_content(
        &self,
        peer: &VerifiedPeer,
        frame: &Envelope,
        manifest: &Manifest,
    ) -> Result<Vec<u32>> {
        let content = self.expected_content(peer, frame)?;
        if manifest.content != content {
            return Err(StageError::Forbidden("Content is outside invocation"));
        }
        ContentStore::new(self.system.store()).begin_download(
            peer.stage_id(),
            manifest,
            chrono::Utc::now().timestamp_millis(),
        )
    }
    pub fn receive_content_chunk(
        &self,
        peer: &VerifiedPeer,
        frame: &Envelope,
        index: u32,
        bytes: &[u8],
    ) -> Result<bool> {
        let content = self.expected_content(peer, frame)?;
        ContentStore::new(self.system.store()).receive_chunk(
            peer.stage_id(),
            &content.hash,
            index,
            bytes,
            chrono::Utc::now().timestamp_millis(),
        )
    }
    pub fn accept_cached_input(&self, peer: &VerifiedPeer, frame: &Envelope) -> Result<Receipt> {
        self.check_input(peer, frame)?;
        let Body::Invoke { content, .. } = &frame.body else {
            return Err(StageError::Invalid("Expected invocation input"));
        };
        let bytes = ContentStore::new(self.system.store()).verified_bytes(content)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| StageError::Invalid("Invocation text must be UTF-8"))?;
        self.receive_verified_content(peer, frame, text)
    }
    fn expected_content(
        &self,
        peer: &VerifiedPeer,
        frame: &Envelope,
    ) -> Result<super::protocol::BlobRef> {
        match &frame.body {
            Body::Invoke { content, .. } => {
                self.check_input(peer, frame)?;
                Ok(content.clone())
            }
            Body::Delivery { .. } => {
                let local = self.system.store().stage_local_id()?;
                super::delivery::Deliveries::new(self.system.store(), local).expected_reply(
                    peer.stage_id(),
                    frame,
                    chrono::Utc::now().timestamp_millis(),
                )
            }
            _ => Err(StageError::Invalid("Expected referenced input")),
        }
    }
    pub fn accept_cached_reply(&self, peer: &VerifiedPeer, frame: &Envelope) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let local = self.system.store().stage_local_id()?;
        let deliveries = super::delivery::Deliveries::new(self.system.store(), local);
        let content = deliveries.expected_reply(peer.stage_id(), frame, now)?;
        let bytes = ContentStore::new(self.system.store()).verified_bytes(&content)?;
        let text =
            String::from_utf8(bytes).map_err(|_| StageError::Invalid("Reply must be UTF-8"))?;
        if let Some(id) = deliveries.accept_reply(peer.stage_id(), frame, text, now)? {
            let input = self
                .system
                .store()
                .message(&id)
                .map_err(|_| StageError::Unavailable("Reply input is unavailable"))?;
            self.system.wake(&input.actor_id);
        }
        Ok(())
    }
    pub fn receive_verified_content(
        &self,
        peer: &VerifiedPeer,
        frame: &Envelope,
        content: String,
    ) -> Result<Receipt> {
        let current = self
            .system
            .store()
            .stage_peer_for_authority(peer.stage_id())?;
        if current.device.tls_sha256 != peer.certificate_hash() || frame.source != peer.stage_id() {
            return Err(StageError::Forbidden(
                "Input is not bound to the authenticated peer",
            ));
        }
        self.system
            .receive_stage_input(peer.stage_id(), frame, content)
            .map_err(|_| StageError::Forbidden("Remote Actor input was not accepted"))
    }
}
