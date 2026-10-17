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

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Auto,
    Ask,
    None,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Pending,
    Approved,
    Denied,
    Cancelled,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolPermissionRequest {
    pub readable_roots: Vec<PathBuf>,
    pub writable_roots: Vec<PathBuf>,
    /// App-observed facts for this specific action, such as an opaque browser
    /// reference's target. This is bounded data, not worker/model history.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub operation_details: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub seq: i64,
    pub id: String,
    pub actor_id: String,
    pub message_id: String,
    pub call: crate::model_protocol::AgentToolCall,
    pub requested: ToolPermissionRequest,
    pub mode: ApprovalMode,
    pub decision: ApprovalDecision,
    pub reason: String,
    pub created_at_ms: i64,
    pub resolved_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActorPermissions {
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    pub read_only: bool,
    pub readable_roots: Vec<PathBuf>,
    pub writable_roots: Vec<PathBuf>,
    /// Extra recipients beyond this Actor's parent and children.
    pub message_targets: Vec<String>,
    pub allow_children: bool,
    /// Action grants over the device's installed Apps; never an installation list.
    #[serde(default)]
    pub app_access: Option<Vec<String>>,
    /// Creation/delegation ceiling, independent of directly installed Apps.
    /// None restricts delegation to the Actor's own installed Apps.
    #[serde(default)]
    pub delegation_apps: Option<Vec<String>>,
    /// Null permits the tools of installed Apps, subject to read-only policy.
    pub tools: Option<Vec<String>>,
}

impl ActorPermissions {
    pub fn workspace(workspace: PathBuf, read_only: bool) -> Self {
        Self {
            approval_mode: ApprovalMode::Auto,
            read_only,
            readable_roots: vec![workspace.clone()],
            writable_roots: if read_only { vec![] } else { vec![workspace] },
            message_targets: vec![],
            allow_children: true,
            app_access: None,
            delegation_apps: None,
            tools: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActorConfig {
    pub name: String,
    pub workspace: PathBuf,
    pub apps: Vec<String>,
    pub model: Option<String>,
    pub instructions: String,
    pub permissions: ActorPermissions,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActorStatus {
    Idle,
    Running,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActorRecord {
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
    pub read_only: bool,
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
            read_only: self.config.permissions.read_only,
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
    Request,
    Result,
    Steering,
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
pub struct DelegationGrant {
    pub apps: Vec<String>,
    pub permissions: ActorPermissions,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteAuthority {
    pub peer: crate::stage::StageId,
    pub grant_actor: String,
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
    pub delegation_grant: Option<DelegationGrant>,
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
