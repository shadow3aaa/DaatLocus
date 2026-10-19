//! Transport-independent bounded facts. Cryptographic identity comes from the channel.
use super::{ActorAddress, Result, StageError, StageId, digest};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const VERSION: u16 = 1;
pub const MAX_FRAME: usize = 64 * 1024;
pub const MAX_INVOCATION_LIFETIME_MS: i64 = 7 * 24 * 60 * 60 * 1000;
pub const CHUNK_BYTES: usize = 32 * 1024;

/// Creation time remains bound to the ID after detailed dedup records are collected.
pub fn invocation_id(now: i64) -> Result<Uuid> {
    if now < 0 {
        return Err(StageError::Invalid("Invalid invocation clock"));
    }
    Ok(Uuid::new_v7(uuid::Timestamp::from_unix(
        uuid::NoContext,
        now as u64 / 1000,
        (now as u64 % 1000) as u32 * 1_000_000,
    )))
}
fn invocation_created(id: Uuid) -> Result<i64> {
    if id.get_version_num() != 7 {
        return Err(StageError::Invalid(
            "Invocation must use a time-bound identifier",
        ));
    }
    let (seconds, nanos) = id
        .get_timestamp()
        .ok_or(StageError::Invalid("Invalid invocation timestamp"))?
        .to_unix();
    seconds
        .checked_mul(1000)
        .and_then(|value| value.checked_add((nanos / 1_000_000) as u64))
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(StageError::Invalid("Invalid invocation timestamp"))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub epoch: Uuid,
    pub sequence: u64,
}
impl Cursor {
    pub fn validate(&self) -> Result<()> {
        if self.epoch.is_nil() || self.sequence > i64::MAX as u64 {
            return Err(StageError::Invalid("Invalid Actor cursor"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    pub hash: String,
    pub size: u64,
    pub version: u64,
}
impl BlobRef {
    pub fn validate(&self) -> Result<()> {
        if self.hash.len() != 64
            || hex::decode(&self.hash).is_err()
            || i64::try_from(self.size).is_err()
            || self.version != 1
        {
            return Err(StageError::Invalid("Invalid content reference"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    User,
    Actor {
        actor: ActorAddress,
        request_id: Uuid,
        ceiling: RemoteCeiling,
    },
}
/// Sender-local paths never grant host filesystem access. These capabilities can
/// only narrow the host's Actor grant, and are captured by the sending service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCeiling {
    pub read_only: bool,
    pub approval_mode: crate::actors::ApprovalMode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationState {
    DurableReceived,
    Running,
    Completed,
    Failed,
    Cancelled,
    ResultUnknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Body {
    AccountWorkReset {
        record: Option<super::reset::Record>,
    },
    RecoveryStatus {
        status: super::recovery::Status,
    },
    DeviceName {
        label: String,
    },
    AccountCredentialsQuery {
        known_binding: Option<String>,
    },
    AccountCredentials {
        record: Option<crate::accounts::CredentialBundle>,
    },
    ActorAdmin {
        actor: ActorAddress,
        action: ActorAdminAction,
    },
    ActorAdminResult {
        actor: ActorAddress,
        content: Option<BlobRef>,
    },
    RevocationsQuery {
        after: Option<StageId>,
        limit: u8,
    },
    Revocations {
        records: Vec<super::revocation::Change>,
        next: Option<StageId>,
    },
    RevocationsPush {
        records: Vec<super::revocation::Change>,
    },
    ResourceUpload {
        actor: ActorAddress,
        resource: crate::resources::Resource,
        export: Uuid,
    },
    ResourceStored {
        actor: ActorAddress,
        resource: crate::resources::Resource,
    },
    ConversationControl {
        actor: ActorAddress,
        action: ConversationAction,
    },
    ConversationResult {
        actor: ActorAddress,
        record: Option<Box<crate::actors::MessageRecord>>,
        count: usize,
    },
    ApprovalsQuery {
        before: Option<i64>,
        limit: u8,
    },
    ApprovalQuery {
        id: String,
    },
    ApprovalResolve {
        id: String,
        decision: crate::actors::ApprovalDecision,
    },
    Approvals {
        records: Vec<crate::actors::ApprovalRecord>,
        next: Option<i64>,
    },
    Approval {
        record: Box<crate::actors::ApprovalRecord>,
    },
    Notice {
        record: crate::actors::EventRecord,
    },
    Authorize {
        actor: ActorAddress,
    },
    ActorAccess {
        actor: ActorAddress,
        read: bool,
        converse: bool,
    },
    CoreRequest {
        content: BlobRef,
    },
    CoordinationRequest {
        request: Box<super::coordination::Request>,
    },
    AccountConfigurationQuery {
        known_binding: Option<String>,
    },
    AccountConfiguration {
        record: Option<crate::accounts::configuration::Record>,
    },
    CoordinationResponse {
        response: Box<super::coordination::Response>,
    },
    CoreResponse {
        content: BlobRef,
    },
    ChannelHello {
        proof: super::identity::SignedProof,
    },
    ChannelCommit {
        phase: String,
        generation: u64,
        nonce: Uuid,
    },
    ReadQuery {
        actor: ActorAddress,
        query: super::observation::Query,
    },
    Observation {
        actor: ActorAddress,
        content: BlobRef,
    },
    DirectoryQuery {
        after: Option<String>,
        limit: u8,
    },
    Directory {
        actors: Vec<RemoteActor>,
        next: Option<String>,
    },
    MembersQuery {
        after: Option<StageId>,
        limit: u8,
    },
    Members {
        enrollments: Vec<super::identity::Enrollment>,
        #[serde(default)]
        revocations: Vec<super::revocation::Change>,
        next: Option<StageId>,
    },
    Delivery {
        invocation: Uuid,
        delivery: Uuid,
        actor: ActorAddress,
        state: InvocationState,
        content: BlobRef,
        initial: bool,
    },
    TransferAck {
        message: Uuid,
    },
    ActorEvents {
        actor: ActorAddress,
        page: super::journal::Page,
    },
    ActorSnapshot {
        actor: ActorAddress,
        page: super::journal::SnapshotPage,
    },
    SnapshotQuery {
        actor: ActorAddress,
        token: Option<Uuid>,
        page: u32,
    },
    BlobManifestQuery {
        access: BlobAccess,
        content: BlobRef,
    },
    BlobManifest {
        access: BlobAccess,
        manifest: super::blob::Manifest,
    },
    BlobRead {
        access: BlobAccess,
        content: BlobRef,
        chunk: u32,
    },
    Invoke {
        origin: Origin,
        target: ActorAddress,
        content: BlobRef,
        attachments: Vec<crate::resources::Resource>,
    },
    Received {
        invocation: Uuid,
    },
    Result {
        invocation: Uuid,
        state: InvocationState,
        content: Option<BlobRef>,
    },
    ResultQuery {
        invocation: Uuid,
    },
    Cancel {
        invocation: Uuid,
    },
    Subscribe {
        actor: ActorAddress,
        cursor: Option<Cursor>,
        level: SubscriptionLevel,
    },
    Unsubscribe {
        subscription: Uuid,
        actor: Option<ActorAddress>,
    },
    Progress {
        subscription: Uuid,
        durable_received: Cursor,
        contiguous_applied: Cursor,
    },
    BlobChunk {
        content: BlobRef,
        chunk: u32,
        chunk_hash: String,
        #[serde(with = "chunk_bytes")]
        bytes: Vec<u8>,
    },
    Error {
        request: Uuid,
        code: String,
    },
    Heartbeat,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationAction {
    Cancel {
        message: String,
    },
    Amend {
        message: String,
        expected: i64,
        content: Option<String>,
    },
    Steer {
        message: String,
        target: String,
        expected: i64,
    },
    ClearQueue,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActorAdminAction {
    Preferences {
        model_present: bool,
        model: Option<String>,
        approval_mode: Option<crate::actors::ApprovalMode>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteActor {
    pub address: ActorAddress,
    pub name: String,
    pub status: crate::actors::ActorStatus,
    pub apps: Vec<String>,
    pub read_only: bool,
    pub converse: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BlobAccess {
    Actor { actor: ActorAddress },
    Invocation { invocation: Uuid },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionLevel {
    Summary,
    Detail,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u16,
    pub source: StageId,
    pub target: StageId,
    pub generation: u64,
    pub message_id: Uuid,
    pub invocation_id: Option<Uuid>,
    pub in_reply_to: Option<Uuid>,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub body: Body,
}
impl Envelope {
    pub fn validate(&self, peer: StageId, local: StageId, generation: u64, now: i64) -> Result<()> {
        if self.version != VERSION {
            return Err(StageError::Invalid("Unsupported Stage protocol version"));
        }
        if self.source != peer
            || self.target != local
            || peer == local
            || peer.0.is_nil()
            || local.0.is_nil()
            || self.message_id.is_nil()
        {
            return Err(StageError::Forbidden(
                "Envelope does not match authenticated channel",
            ));
        }
        if self.generation != generation {
            return Err(StageError::Conflict("Stale transport generation"));
        }
        if self.created_at_ms < 0
            || self.created_at_ms > now.saturating_add(60_000)
            || self.expires_at_ms <= now
            || self.expires_at_ms <= self.created_at_ms
            || self
                .expires_at_ms
                .checked_sub(self.created_at_ms)
                .is_none_or(|duration| duration > MAX_INVOCATION_LIFETIME_MS)
        {
            return Err(StageError::Invalid("Expired or invalid operation lifetime"));
        }
        match &self.body {
            Body::DeviceName { label } if label.trim().is_empty() || label.len() > 100 => {
                return Err(StageError::Invalid("Invalid device name"));
            }
            Body::ActorAdmin { actor, .. } | Body::ActorAdminResult { actor, .. } => {
                actor.validate()?
            }
            Body::RevocationsQuery { limit, .. } if *limit == 0 || *limit > 32 => {
                return Err(StageError::Invalid("Invalid revocation page"));
            }
            Body::Revocations { records, .. } | Body::RevocationsPush { records }
                if records.len() > 32 =>
            {
                return Err(StageError::Capacity("Revocation page exceeds budget"));
            }
            Body::ResourceUpload {
                actor,
                resource,
                export,
            } => {
                actor.validate()?;
                resource.validate()?;
                if export.is_nil() {
                    return Err(StageError::Invalid("Invalid resource export"));
                }
            }
            Body::ResourceStored { actor, resource } => {
                actor.validate()?;
                resource.validate()?;
            }
            Body::ConversationControl { actor, .. } | Body::ConversationResult { actor, .. } => {
                actor.validate()?
            }
            Body::ApprovalsQuery { limit, .. } if *limit == 0 || *limit > 32 => {
                return Err(StageError::Invalid("Invalid approval page"));
            }
            Body::ApprovalQuery { id } | Body::ApprovalResolve { id, .. }
                if id.len() > 128 || id.is_empty() =>
            {
                return Err(StageError::Invalid("Invalid approval identifier"));
            }
            Body::Notice { record }
                if record.seq <= 0 || record.actor_id.is_empty() || record.actor_id.len() > 128 =>
            {
                return Err(StageError::Invalid("Invalid notice"));
            }
            Body::Authorize { actor } | Body::ActorAccess { actor, .. } => actor.validate()?,
            Body::CoreRequest { content } | Body::CoreResponse { content } => content.validate()?,
            Body::ReadQuery { actor, query } => {
                actor.validate()?;
                if actor.stage_id != local {
                    return Err(StageError::Forbidden("Read targets another host"));
                }
                query.validate()?;
            }
            Body::Observation { actor, content } => {
                actor.validate()?;
                content.validate()?;
                if actor.stage_id != peer {
                    return Err(StageError::Forbidden("Observation host mismatch"));
                }
            }
            Body::DirectoryQuery { after, limit }
                if *limit == 0
                    || *limit > 32
                    || after.as_ref().is_some_and(|id| id.len() > 128) =>
            {
                return Err(StageError::Invalid("Invalid directory query"));
            }
            Body::Directory { actors, .. } => {
                if actors.len() > 32 {
                    return Err(StageError::Capacity("Directory response exceeds budget"));
                }
                for actor in actors {
                    actor.address.validate()?;
                    if actor.address.stage_id != peer {
                        return Err(StageError::Forbidden(
                            "Invalid authoritative Actor directory",
                        ));
                    }
                }
            }
            Body::MembersQuery { limit, .. } if *limit == 0 || *limit > 8 => {
                return Err(StageError::Invalid("Invalid member query"));
            }
            Body::Members { enrollments, .. } if enrollments.len() > 8 => {
                return Err(StageError::Capacity("Member response exceeds budget"));
            }
            Body::Delivery {
                delivery,
                actor,
                content,
                ..
            } => {
                actor.validate()?;
                content.validate()?;
                if actor.stage_id != peer || invocation_created(*delivery)? != self.created_at_ms {
                    return Err(StageError::Forbidden("Invalid delivery origin or lifetime"));
                }
            }
            Body::TransferAck { message } if message.is_nil() => {
                return Err(StageError::Invalid("Invalid transfer acknowledgment"));
            }
            Body::ActorEvents { actor, page } => {
                actor.validate()?;
                page.from.validate()?;
                page.base.validate()?;
                page.head.validate()?;
                if actor.stage_id != peer {
                    return Err(StageError::Forbidden(
                        "Event is not from authoritative host",
                    ));
                }
            }
            Body::ActorSnapshot { actor, page } => {
                actor.validate()?;
                page.base.validate()?;
                if actor.stage_id != peer
                    || page.token.is_nil()
                    || page.page > 316
                    || page.entries.len() > 32
                {
                    return Err(StageError::Forbidden("Invalid authoritative snapshot"));
                }
            }
            Body::SnapshotQuery { actor, token, page } => {
                actor.validate()?;
                if actor.stage_id != local || token.is_none() && *page != 0 || *page > 316 {
                    return Err(StageError::Invalid("Invalid snapshot request"));
                }
            }
            Body::BlobManifestQuery { access, content }
            | Body::BlobRead {
                access, content, ..
            } => {
                content.validate()?;
                if let BlobAccess::Actor { actor } = access {
                    actor.validate()?;
                    if actor.stage_id != local {
                        return Err(StageError::Forbidden(
                            "Content request targets another host",
                        ));
                    }
                }
            }
            Body::BlobManifest { manifest, .. } => manifest.validate()?,
            Body::Invoke {
                origin,
                target,
                content,
                attachments,
            } => {
                if self.invocation_id.is_none() || target.stage_id != local {
                    return Err(StageError::Invalid("Invalid invocation routing"));
                }
                if invocation_created(
                    self.invocation_id
                        .ok_or(StageError::Invalid("Invocation identifier required"))?,
                )? != self.created_at_ms
                {
                    return Err(StageError::Invalid(
                        "Invocation creation time cannot change on retry",
                    ));
                }
                target.validate()?;
                content.validate()?;
                for attachment in attachments {
                    attachment.validate()?;
                }
                if let Origin::Actor { actor, .. } = origin {
                    actor.validate()?;
                    if actor.stage_id != peer {
                        return Err(StageError::Forbidden(
                            "Actor source does not belong to peer",
                        ));
                    }
                }
            }
            Body::Subscribe { actor, cursor, .. } => {
                actor.validate()?;
                if let Some(cursor) = cursor {
                    cursor.validate()?;
                }
                if actor.stage_id != local {
                    return Err(StageError::Forbidden("Subscription targets another host"));
                }
            }
            Body::BlobChunk {
                content,
                bytes,
                chunk_hash,
                chunk,
            } => {
                content.validate()?;
                let offset = *chunk as u64 * CHUNK_BYTES as u64;
                if offset >= content.size
                    || bytes.len() as u64 != (content.size - offset).min(CHUNK_BYTES as u64)
                    || digest(bytes) != *chunk_hash
                {
                    return Err(StageError::Invalid("Invalid content chunk"));
                }
            }
            _ => {}
        }
        if self.encode()?.len() > MAX_FRAME {
            return Err(StageError::Invalid("Stage frame exceeds budget"));
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| StageError::Invalid("Cannot encode Stage frame"))?;
        if bytes.len() > MAX_FRAME {
            return Err(StageError::Invalid("Stage frame exceeds budget"));
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_FRAME {
            return Err(StageError::Invalid("Stage frame exceeds budget"));
        }
        serde_json::from_slice(bytes).map_err(|_| StageError::Invalid("Invalid Stage frame"))
    }
    /// Excludes link generation/message ID so a retry on a replacement link retains identity.
    pub fn invocation_digest(&self) -> Result<String> {
        let bytes = serde_json::to_vec(&(
            self.source,
            self.target,
            self.invocation_id,
            self.in_reply_to,
            self.created_at_ms,
            self.expires_at_ms,
            &self.body,
        ))
        .map_err(|_| StageError::Invalid("Invalid invocation"))?;
        Ok(digest(&bytes))
    }
}

mod chunk_bytes {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        bytes: &[u8],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() > super::CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("Chunk exceeds budget"));
        }
        let bytes = STANDARD.decode(text).map_err(serde::de::Error::custom)?;
        if bytes.len() > super::CHUNK_BYTES {
            return Err(serde::de::Error::custom("Chunk exceeds budget"));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn link_generation_and_authenticated_origin_are_checked_but_retry_digest_is_stable() {
        let source = StageId::new();
        let target = StageId::new();
        let invocation = invocation_id(100).unwrap();
        let mut envelope = Envelope {
            version: 1,
            source,
            target,
            generation: 1,
            message_id: Uuid::new_v4(),
            invocation_id: Some(invocation),
            in_reply_to: None,
            created_at_ms: 100,
            expires_at_ms: 10000,
            body: Body::Invoke {
                origin: Origin::User,
                target: ActorAddress {
                    stage_id: target,
                    actor_id: "root".into(),
                },
                content: BlobRef {
                    hash: digest(b"hello"),
                    size: 5,
                    version: 1,
                },
                attachments: vec![],
            },
        };
        envelope.validate(source, target, 1, 200).unwrap();
        let hash = envelope.invocation_digest().unwrap();
        let decoded = Envelope::decode(&envelope.encode().unwrap()).unwrap();
        assert_eq!(decoded.invocation_digest().unwrap(), hash);
        envelope.generation = 2;
        envelope.message_id = Uuid::new_v4();
        assert_eq!(envelope.invocation_digest().unwrap(), hash);
        assert!(envelope.validate(source, target, 1, 200).is_err());
        assert!(envelope.validate(StageId::new(), target, 2, 200).is_err());
        assert!(envelope.validate(source, target, 2, 10000).is_err());
    }
}
