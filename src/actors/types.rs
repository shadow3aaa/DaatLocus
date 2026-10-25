use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum ActorError {
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    ApplicationRequired,
    Pending,
    Approved,
    Denied,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub seq: i64,
    pub id: String,
    pub message_id: String,
    pub call: crate::model_protocol::AgentToolCall,
    pub requested: crate::sandbox::SandboxScopeRequest,
    #[serde(default)]
    pub scopes: Vec<super::SandboxScope>,
    /// Immutable refusals for overlapping scopes of this same source event.
    /// A linked application keeps its own scope and never repeats their review.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_denials: Vec<String>,
    pub decision: ApprovalDecision,
    pub reason: String,
    pub created_at_ms: i64,
    pub resolved_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appeal: Option<ApprovalAppeal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<ReviewApplication>,
}

impl ApprovalRecord {
    pub(crate) fn requires_core_review(&self) -> bool {
        self.decision == ApprovalDecision::Denied
            || self.decision == ApprovalDecision::ApplicationRequired
                && !self.previous_denials.is_empty()
    }

    /// Provenance is resolved from the immutable input, never persisted as an
    /// Actor permission owner or used to partition the device's grants.
    pub fn source_actor(&self, store: &super::store::ActorStore) -> miette::Result<String> {
        Ok(store.message(&self.message_id)?.actor_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewApplication {
    pub task: String,
    pub reason: String,
}

/// The automatic denial stays immutable; an Owner appeal is a separate decision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalAppeal {
    pub core_message_id: String,
    pub request_message_id: String,
    pub explanation: String,
    pub decision: ApprovalDecision,
    pub owner_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_message_id: Option<String>,
    pub notification_id: Option<String>,
}

/// A decision by the active account Core, bound to its input and immutable scope.
/// The source device needs this bounded authorization, never Core's conversation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalAppealAuthorization {
    pub approval_id: String,
    pub core_message_id: String,
    pub decision_message_id: String,
    pub owner_message_id: Option<String>,
    pub scopes_digest: String,
    pub decision: ApprovalDecision,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActorConfig {
    pub name: String,
    pub workspace: PathBuf,
    pub apps: Vec<String>,
    pub model: Option<String>,
    pub instructions: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActorStatus {
    Idle,
    Running,
    Archived,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActorRecord {
    /// Execution-only restriction from authenticated remote authority; never Actor configuration.
    #[serde(skip)]
    pub remote_read_only: bool,
    pub id: String,
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub depth: u32,
    pub config: ActorConfig,
    #[serde(default)]
    pub context: super::AgentContext,
    pub status: ActorStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActorSummary {
    pub id: String,
    pub parent_id: Option<String>,
    pub conversation_url: String,
    pub name: String,
    pub status: ActorStatus,
    pub apps: Vec<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl ActorRecord {
    pub fn summary(&self) -> ActorSummary {
        ActorSummary {
            id: self.id.clone(),
            parent_id: self.parent_id.clone(),
            conversation_url: format!("/actors/{}", self.id),
            name: self.config.name.clone(),
            status: self.status,
            apps: self.config.apps.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageSource {
    User,
    Actor {
        actor_id: String,
    },
    RemoteUser {
        stage_id: crate::stage::StageId,
        invocation_id: uuid::Uuid,
    },
    RemoteActor {
        stage_id: crate::stage::StageId,
        actor_id: String,
        invocation_id: uuid::Uuid,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyTarget {
    User,
    Actor {
        actor_id: String,
        request_id: String,
    },
    Remote {
        stage_id: crate::stage::StageId,
        invocation_id: uuid::Uuid,
        user: bool,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    #[serde(alias = "steering")]
    Request,
    Result,
    /// An explicit outward message; never claimed as model input.
    Delivery,
    /// A collaborator's material update, without completing its request.
    Progress,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub uri: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteAuthority {
    pub read_only: bool,
    pub peer: crate::stage::StageId,
    pub origin_actor: String,
    #[serde(default)]
    pub reply_for: Option<uuid::Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageRecord {
    pub seq: i64,
    pub id: String,
    pub actor_id: String,
    pub source: MessageSource,
    pub reply_to: ReplyTarget,
    pub kind: MessageKind,
    pub in_reply_to: Option<String>,
    /// Immutable authority at delegation time; stored privately, never accepted
    /// from clients or repeated in conversation responses.
    #[serde(default, skip_serializing)]
    pub remote_authority: Option<RemoteAuthority>,
    pub content: String,
    pub attachments: Vec<Attachment>,
    pub status: MessageStatus,
    pub cancel_requested: bool,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
    #[serde(default)]
    pub final_generation: Option<u32>,
    pub reply: Option<String>,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActorEvent {
    ConversationRestored {
        archive_id: String,
    },
    ApprovalRequested {
        approval_id: String,
        message_id: String,
        call_id: String,
        name: String,
    },
    ApprovalResolved {
        approval_id: String,
        message_id: String,
        decision: ApprovalDecision,
        reason: String,
    },
    ActorChanged {
        actor: ActorSummary,
    },
    // Historical records remain readable; no operation emits Actor deletion.
    ActorDeleted {
        actor_id: String,
    },
    MessageAdded {
        message_id: String,
    },
    MessageUpdated {
        message_id: String,
        status: MessageStatus,
    },
    GenerationStarted {
        message_id: String,
        generation: u32,
    },
    GenerationDelta {
        message_id: String,
        generation: u32,
        channel: String,
        text: String,
    },
    ToolStarted {
        message_id: String,
        call_id: String,
        name: String,
    },
    ToolFinished {
        message_id: String,
        call_id: String,
        name: String,
        result_id: String,
        summary: String,
        error: Option<String>,
        #[serde(default)]
        started_at_ms: Option<i64>,
        #[serde(default)]
        finished_at_ms: Option<i64>,
        #[serde(default)]
        outcome: Option<Value>,
    },
    AttachmentAdded {
        attachment: Attachment,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: i64,
    pub actor_id: String,
    pub created_at_ms: i64,
    pub event: ActorEvent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResultRecord {
    pub id: String,
    pub actor_id: String,
    pub message_id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub payload: Value,
    pub model_content: String,
    pub summary: String,
    pub error: Option<String>,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResultSummary {
    pub seq: i64,
    pub id: String,
    pub message_id: String,
    pub call_id: String,
    pub name: String,
    pub summary: String,
    pub error: Option<String>,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    /// Small domain facts; the complete observation stays at the result endpoint.
    pub outcome: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredResource {
    pub id: String,
    pub actor_id: String,
    #[serde(skip_serializing)]
    pub path: PathBuf,
    pub name: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub uri: String,
}
