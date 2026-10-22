//! Permission references for completed App results, independent of App arguments.
use super::{
    ActorControl, ActorRecord, ActorSystem, ApprovalDecision, ApprovalRecord, ApprovalScope,
    MessageRecord, ReviewApplication, ToolPermissionRequest,
};
use crate::{
    app::{AppToolExecutionContext, AppToolExecutionResult},
    model_protocol::AgentToolCall,
};
use miette::{Result, miette};
use serde::{Deserialize, Serialize};

/// A result reference is not itself an approval or an instruction to replay a call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionReference {
    pub tool_result_id: String,
    pub stage_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
}
impl PermissionReference {
    pub(crate) fn append_to(&self, content: &mut String) {
        content.push_str(&format!(
            "\n{}\n",
            serde_json::json!({"permission_reference":self})
        ));
        if self.approval_id.is_some() {
            content.push_str("If this scope is needed, use actors__request_approval with this approval_id and stage_id, null tool_result_id and roots, your current task and reason. Approval remembers permission only and never repeats the tool call.");
        } else {
            content.push_str("If the result shows a required filesystem scope is missing, use actors__request_approval with this tool_result_id and stage_id, null approval_id, the exact readable_roots/writable_roots you propose, your current task and reason. This result reference is not an approval or a detected denial. Approval remembers permission only and never repeats the tool call.");
        }
    }
}

/// Native wire shape, not a model-facing discriminated union.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationSource {
    Approval {
        approval_id: String,
    },
    ToolResult {
        tool_result_id: String,
        readable_roots: Vec<std::path::PathBuf>,
        writable_roots: Vec<std::path::PathBuf>,
    },
}

fn create_scope_application(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    base: &AppToolExecutionContext,
    requested: ToolPermissionRequest,
) -> Result<ApprovalRecord> {
    if requested.readable_roots.is_empty() && requested.writable_roots.is_empty() {
        return Err(miette!("Propose at least one exact filesystem root"));
    }
    if requested
        .readable_roots
        .iter()
        .chain(&requested.writable_roots)
        .any(|root| !root.is_absolute())
    {
        return Err(miette!("Permission roots must be explicit absolute paths"));
    }
    let mut requested = requested.canonicalized()?;
    requested
        .readable_roots
        .retain(|path| !base.sandbox_policy.is_path_readable(path));
    requested
        .writable_roots
        .retain(|path| !base.sandbox_policy.is_path_writable(path));
    if requested.readable_roots.is_empty() && requested.writable_roots.is_empty() {
        return Err(miette!(
            "The proposed filesystem scope is already covered. No additional permission is needed; investigate the original result instead of assuming a sandbox grant will fix it"
        ));
    }
    let effective = system.requested_tool_context(base, &requested)?;
    effective.sandbox_policy.filesystem.ensure_root_bindings()?;
    let mut scopes: Vec<ApprovalScope> = vec![];
    for (paths, write) in [
        (&requested.readable_roots, false),
        (&requested.writable_roots, true),
    ] {
        for path in paths {
            let scope = ApprovalScope::filesystem(path, write)?;
            if !scopes.iter().any(|saved| saved.covers(&scope)) {
                scopes.retain(|saved| !scope.covers(saved));
                scopes.push(scope);
            }
        }
    }
    system.store().create_approval(ApprovalRecord {
        seq: 0,
        id: uuid::Uuid::new_v4().to_string(),
        actor_id: actor.id.clone(),
        message_id: message.id.clone(),
        call: call.clone(),
        requested,
        review: None,
        scopes,
        mode: actor.config.permissions.approval_mode,
        decision: ApprovalDecision::ApplicationRequired,
        reason: "An approval application is required".into(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        resolved_at_ms: None,
        appeal: None,
        application: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn attach_reference(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    base: &AppToolExecutionContext,
    id: &str,
    result: &mut AppToolExecutionResult,
    error: Option<&miette::Report>,
) -> Result<()> {
    // Only a typed sandbox record identifies a denied target. Never parse output,
    // errno, command text, or an operating-system PermissionDenied diagnostic.
    let approval_id = error
        .and_then(|error| {
            error
                .downcast_ref::<crate::sandbox::AccessDenied>()
                .or_else(|| {
                    error
                        .chain()
                        .find_map(|cause| cause.downcast_ref::<crate::sandbox::AccessDenied>())
                })
        })
        .and_then(|denial| {
            let target = base.resolve_tool_path(&denial.record.path, None);
            let requested = ToolPermissionRequest {
                readable_roots: if denial.record.access == crate::sandbox::Access::Read {
                    vec![target.clone()]
                } else {
                    vec![]
                },
                writable_roots: if denial.record.access == crate::sandbox::Access::Write {
                    vec![target.clone()]
                } else {
                    vec![]
                },
                operation_details: serde_json::Value::Null,
            };
            // An unavailable or forbidden proposal must not erase the App result.
            create_scope_application(system, actor, message, call, base, requested)
                .ok()
                .map(|record| record.id)
        });
    result.permission_reference = Some(PermissionReference {
        tool_result_id: id.into(),
        stage_id: system.store().public_stage_id()?.to_string(),
        approval_id,
    });
    Ok(())
}

pub(crate) fn error_result(system: &ActorSystem, error: &miette::Report) -> AppToolExecutionResult {
    let error = system.safe_error(&error.to_string());
    let mut result = AppToolExecutionResult::new(
        "App call failed",
        serde_json::json!({"error":error}),
        Some(error.clone()),
    );
    result.failure = Some(error);
    result
}

pub(crate) async fn submit(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    base: &AppToolExecutionContext,
    source: ApplicationSource,
    application: ReviewApplication,
) -> Result<ApprovalRecord> {
    super::approval::validate_application(&application)?;
    super::approval::ensure_current(system, actor, message, control)?;
    let id = match source {
        ApplicationSource::Approval { approval_id } => approval_id,
        ApplicationSource::ToolResult {
            tool_result_id,
            readable_roots,
            writable_roots,
        } => {
            let native =
                system
                    .store()
                    .native_tool_result(&tool_result_id, &actor.id, &message.id)?;
            if native.read_only && !writable_roots.is_empty() {
                return Err(miette!(
                    "Original tool authority cannot obtain write access"
                ));
            }
            create_scope_application(
                system,
                actor,
                message,
                &native.call,
                base,
                ToolPermissionRequest {
                    readable_roots,
                    writable_roots,
                    operation_details: serde_json::Value::Null,
                },
            )?
            .id
        }
    };
    super::approval::submit_application(system, actor, message, control, base, &id, application)
        .await
}
