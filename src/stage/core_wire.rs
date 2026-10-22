//! Native account-core protocol. Model tools retain each App's own schema.
use super::{
    StageId,
    shared::{Acknowledgement, Head},
    shared_store::Batch,
};
use crate::{
    actors::{ApprovalMode, MessageRecord},
    app::{AppToolExecutionResult, AppToolSpec},
    model_protocol::AgentToolCall,
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Configuration {
    #[serde(default)]
    pub context: crate::actors::AgentContext,
    pub name: String,
    pub instructions: String,
    pub model: Option<String>,
    pub apps: Vec<String>,
    pub approval_mode: crate::actors::ApprovalMode,
}
impl Configuration {
    pub fn from_actor(actor: &crate::actors::ActorRecord) -> Self {
        let config = &actor.config;
        Self {
            context: actor.context.clone(),
            name: config.name.clone(),
            instructions: config.instructions.clone(),
            model: config.model.clone(),
            apps: config.apps.clone(),
            approval_mode: config.permissions.approval_mode,
        }
    }
    pub fn apply(&self, config: &mut crate::actors::ActorConfig) {
        config.name = self.name.clone();
        config.instructions = self.instructions.clone();
        config.model = self.model.clone();
        config.apps = vec!["actors".into()];
        config.permissions.approval_mode = self.approval_mode;
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub stage_id: StageId,
    pub device: String,
    pub app: String,
    pub revision: String,
    pub tools: Vec<AppToolSpec>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    SubmitApproval {
        epoch: u64,
        message: String,
        source: crate::actors::runtime_permissions::ApplicationSource,
        application: crate::actors::ReviewApplication,
    },
    ReviewApproval {
        epoch: u64,
        facts: String,
    },
    ApprovalAppeal {
        epoch: u64,
        approval_id: String,
    },
    BeginApprovalAppeal {
        epoch: u64,
        approval_id: String,
        explanation: String,
    },
    ApplyApprovalAppeal {
        epoch: u64,
        authorization: crate::actors::ApprovalAppealAuthorization,
    },
    RecoveryStatus,
    RecoveryArchives {
        after: Option<i64>,
    },
    FreezeRecovery,
    RecoverySnapshot {
        head: Head,
        token: Option<String>,
        page: u32,
    },
    InstallRecovery {
        source: StageId,
        head: Head,
        epoch: u64,
    },
    FinishRecovery {
        head: Head,
        epoch: u64,
    },
    Snapshot {
        head: Head,
        token: Option<String>,
        page: u32,
    },
    Resources {
        records: Vec<crate::resources::Resource>,
    },
    ClearQueue,
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
    Preferences {
        model_present: bool,
        model: Option<String>,
        approval_mode: Option<ApprovalMode>,
    },
    Catalog {
        epoch: u64,
    },
    Prepare {
        batch: Batch,
    },
    Publish {
        head: Head,
    },
    Journal {
        after: Head,
    },
    Input {
        message: Box<MessageRecord>,
    },
    Invoke {
        epoch: u64,
        message: String,
        app: String,
        revision: String,
        call: AgentToolCall,
        identity_read_only: bool,
    },
    ToolResult {
        message: String,
        call_id: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    ApprovalSubmitted {
        record: Box<crate::actors::ApprovalRecord>,
    },
    ApprovalRequired {
        reference: crate::actors::approval::ApprovalRejection,
    },
    ApprovalReview {
        verdict: std::result::Result<
            crate::actors::approval::ReviewVerdict,
            crate::actors::approval::ReviewFailure,
        >,
    },
    ApprovalAppeal {
        record: Box<crate::actors::ApprovalRecord>,
    },
    RecoveryArchives {
        entries: Vec<super::recovery::ArchivedChange>,
        next_cursor: Option<i64>,
    },
    RecoveryStatus {
        status: super::recovery::Status,
    },
    Snapshot {
        page: super::shared_snapshot::Page,
    },
    QueueCleared {
        count: usize,
    },
    Steered {
        message: Box<MessageRecord>,
    },
    Catalog {
        apps: Vec<Installation>,
    },
    Prepared {
        receipt: Acknowledgement,
    },
    Published,
    Journal {
        batches: Vec<Batch>,
    },
    InputAccepted,
    ToolResult {
        result: Option<AppToolExecutionResult>,
        uncertain: bool,
    },
    Error {
        code: String,
    },
    ApprovalDenied {
        rejection: crate::actors::approval::ApprovalRejection,
    },
}

#[cfg(test)]
mod preference_tests {
    use super::*;
    #[test]
    fn installation_exposes_only_tools_and_rejects_obsolete_docs() {
        let installation = Installation {
            stage_id: StageId(uuid::Uuid::new_v4()),
            device: "fixture device".into(),
            app: "files".into(),
            revision: "fixture revision".into(),
            tools: vec![],
        };
        let current = serde_json::to_value(&installation).unwrap();
        assert!(current.get("docs").is_none());
        let decoded: Installation = serde_json::from_value(current.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), current);

        let mut legacy = current.clone();
        legacy["docs"] = serde_json::json!("Obsolete App instructions must not survive.");
        assert!(serde_json::from_value::<Installation>(legacy).is_err());
    }

    #[test]
    fn account_wire_exposes_only_narrow_preferences_not_arbitrary_configuration() {
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"kind":"configure","configuration":{"apps":["terminal"]}})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<Request>(serde_json::json!({"kind":"preferences","model_present":false,"model":null,"approval_mode":"none","filesystem_roots":[]})).is_err());
        assert!(
            serde_json::from_value::<super::super::protocol::ActorAdminAction>(
                serde_json::json!({"kind":"configure","config":{}})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<super::super::protocol::ActorAdminAction>(
                serde_json::json!({"kind":"delete"})
            )
            .is_err()
        );
        let value = serde_json::json!({"kind":"preferences","model_present":true,"model":null,"approval_mode":null});
        assert!(matches!(
            serde_json::from_value::<Request>(value).unwrap(),
            Request::Preferences {
                model_present: true,
                model: None,
                approval_mode: None
            }
        ));
    }
}
