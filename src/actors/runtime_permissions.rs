//! Permission references for completed App results, independent of App arguments.
use super::{
    ActorControl, ActorRecord, ActorSystem, ApprovalDecision, ApprovalRecord, MessageRecord,
    ReviewApplication, SandboxScope,
};
use crate::sandbox::SandboxScopeRequest;
use crate::{
    app::{AppToolExecutionContext, AppToolExecutionResult},
    model_protocol::AgentToolCall,
};
use miette::{Result, miette};
use serde::{Deserialize, Serialize};

pub(crate) fn bind_access_broker(
    context: &mut AppToolExecutionContext,
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    control: &ActorControl,
) {
    context.access_broker = Some(std::sync::Arc::new(InvocationAccess {
        system: system.clone(),
        actor: actor.clone(),
        message: message.clone(),
        call: call.clone(),
        control: control.clone(),
    }));
}

struct InvocationAccess {
    system: ActorSystem,
    actor: ActorRecord,
    message: MessageRecord,
    call: AgentToolCall,
    control: ActorControl,
}

#[async_trait::async_trait]
impl crate::sandbox::access::SandboxAccessBroker for InvocationAccess {
    async fn authorize(
        &self,
        denial: crate::sandbox::DenialRecord,
    ) -> Result<crate::sandbox::RuntimeSandboxPolicy> {
        if denial.source != crate::sandbox::DenialSource::PolicyPreflight {
            return Err(miette!(
                "Native process denials cannot be resumed by the host filesystem broker"
            ));
        }
        super::approval::ensure_current(&self.system, &self.actor, &self.message, &self.control)?;
        let base = self.system.execution_context(&self.actor, &self.message)?;
        let write = denial.access == crate::sandbox::Access::Write;
        let requested = SandboxScopeRequest {
            readable_roots: if write {
                vec![]
            } else {
                vec![denial.path.clone()]
            },
            writable_roots: if write {
                vec![denial.path.clone()]
            } else {
                vec![]
            },
        };
        // Another concurrent request may already have granted this access.
        if if write {
            base.sandbox_policy.is_path_writable(&denial.path)
        } else {
            base.sandbox_policy.is_path_readable(&denial.path)
        } {
            return Ok(base.sandbox_policy);
        }
        let record =
            create_scope_application(&self.system, &self.message, &self.call, &base, requested)?;
        let application = record
            .application
            .clone()
            .unwrap_or_else(|| ReviewApplication {
                task: self.message.content.chars().take(1000).collect(),
                reason: format!(
                    "The suspended filesystem operation needs {} access to {}",
                    if write { "write" } else { "read" },
                    denial.path.display()
                ),
            });
        // This branch never forwards a refusal to Core. The original operation
        // returns its immutable refusal reference; only an explicit application
        // can subsequently ask Core, without running automatic review again.
        super::approval::submit_application(
            &self.system,
            &self.actor,
            &self.message,
            &self.control,
            &base,
            &record.id,
            application,
        )
        .await?;
        super::approval::ensure_current(&self.system, &self.actor, &self.message, &self.control)?;
        Ok(self
            .system
            .execution_context(&self.actor, &self.message)?
            .sandbox_policy)
    }
}

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
    message: &MessageRecord,
    call: &AgentToolCall,
    base: &AppToolExecutionContext,
    requested: SandboxScopeRequest,
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
    if let Some(record) =
        system
            .store()
            .sandbox_application_for_call(&message.id, &call.id, &requested)?
    {
        return Ok(record);
    }
    // A transient Sudo context does not mean the device remembered these roots.
    // Explicit permission controls still operate on durable device authority.
    let mut device_base = base.clone();
    if device_base.sandbox_policy.is_unrestricted() {
        let actor = system.execution_actor(message)?;
        device_base.sandbox_policy = system.execution_context(&actor, message)?.sandbox_policy;
    }
    requested
        .readable_roots
        .retain(|path| !device_base.sandbox_policy.is_path_readable(path));
    requested
        .writable_roots
        .retain(|path| !device_base.sandbox_policy.is_path_writable(path));
    if requested.readable_roots.is_empty() && requested.writable_roots.is_empty() {
        return Err(miette!(
            "The proposed filesystem scope is already covered. No additional permission is needed; investigate the original result instead of assuming a sandbox grant will fix it"
        ));
    }
    let effective = system.requested_tool_context(&device_base, &requested)?;
    effective.sandbox_policy.filesystem.ensure_root_bindings()?;
    let mut scopes: Vec<SandboxScope> = vec![];
    for (paths, write) in [
        (&requested.readable_roots, false),
        (&requested.writable_roots, true),
    ] {
        for path in paths {
            let scope = SandboxScope::filesystem(path, write)?;
            if !scopes.iter().any(|saved| saved.covers(&scope)) {
                scopes.retain(|saved| !scope.covers(saved));
                scopes.push(scope);
            }
        }
    }
    system.store().create_approval(ApprovalRecord {
        sudo: None,
        execution_claim: None,
        seq: 0,
        id: uuid::Uuid::new_v4().to_string(),
        message_id: message.id.clone(),
        call: call.clone(),
        requested,
        scopes,
        previous_denials: vec![],
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
    _actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    base: &AppToolExecutionContext,
    id: &str,
    result: &mut AppToolExecutionResult,
    error: Option<&miette::Report>,
) -> Result<()> {
    // Transport results retain the authenticated execution device's reference.
    // Observing them locally must never manufacture local execution evidence.
    if result.permission_reference.is_some() {
        return Ok(());
    }
    // Only typed sandbox records identify a denied target: a policy-preflight
    // denial carried by the error, or deny-and-record events the child's
    // backend captured. Never parse output, errno, command text, or an
    // operating-system PermissionDenied diagnostic.
    let mut proposed = SandboxScopeRequest {
        readable_roots: vec![],
        writable_roots: vec![],
    };
    if let Some(denial) = error.and_then(sandbox_denial) {
        push_denial_scope(
            &mut proposed,
            base,
            &denial.record.path,
            denial.record.access,
        );
    }
    for denial in result
        .sandbox
        .iter()
        .flat_map(|report| report.filesystem_denials())
    {
        push_denial_scope(&mut proposed, base, &denial.path, denial.access);
    }
    let approval_id = if proposed.readable_roots.is_empty() && proposed.writable_roots.is_empty() {
        None
    } else {
        // An unavailable or forbidden proposal must not erase the App result.
        create_scope_application(system, message, call, base, proposed)
            .ok()
            .map(|record| record.id)
    };
    result.permission_reference = Some(PermissionReference {
        tool_result_id: id.into(),
        stage_id: system.store().public_stage_id()?.to_string(),
        approval_id,
    });
    Ok(())
}

fn sandbox_denial(error: &miette::Report) -> Option<&crate::sandbox::AccessDenied> {
    error
        .downcast_ref::<crate::sandbox::AccessDenied>()
        .or_else(|| {
            error
                .chain()
                .find_map(|cause| cause.downcast_ref::<crate::sandbox::AccessDenied>())
        })
}

fn push_denial_scope(
    request: &mut SandboxScopeRequest,
    base: &AppToolExecutionContext,
    path: &std::path::Path,
    access: crate::sandbox::Access,
) {
    let target = base.resolve_tool_path(path, None);
    let roots = match access {
        crate::sandbox::Access::Read => &mut request.readable_roots,
        crate::sandbox::Access::Write => &mut request.writable_roots,
    };
    if !roots.iter().any(|existing| existing == &target) {
        roots.push(target);
    }
}

/// Appends a bounded, model-facing summary of captured native denials.
pub(crate) fn append_denial_summary(
    report: Option<&crate::sandbox::DenialReport>,
    content: &mut String,
) {
    let Some(report) = report else {
        return;
    };
    use crate::sandbox::CaptureStatus;
    content.push_str(&format!("\nNative sandbox diagnostics: backend={:?}, capture={:?}, observed_denials={}, retained_denials={}, truncated={}.\n",
        report.backend, report.status, report.observed_denials, report.denials.len(), report.truncated));
    for event in report.denials.iter().take(8) {
        content.push_str(&format!(
            "{} {}: {}\n",
            event.resource_type,
            event.access_type,
            event.resource.chars().take(512).collect::<String>()
        ));
    }
    if report.denials.len() > 8 {
        content.push_str("Additional native observations are preserved in this tool result's sandbox diagnostics.\n");
    }
    if matches!(
        report.status,
        CaptureStatus::Unavailable | CaptureStatus::Pending | CaptureStatus::Failed
    ) || report.truncated
    {
        content.push_str("Denial collection is incomplete or unavailable; an empty report does not prove the sandbox permitted every operation.\n");
    }
    if report
        .denials
        .iter()
        .any(|event| event.filesystem_denial().is_none())
    {
        content.push_str("Unclassified and non-filesystem observations are retained as facts. They do not authorize guessed read/write roots.\n");
    }
}

pub(crate) fn error_result(system: &ActorSystem, error: &miette::Report) -> AppToolExecutionResult {
    let diagnostics = error
        .downcast_ref::<crate::sandbox::ProcessFailure>()
        .map(|failure| failure.report.clone())
        .or_else(|| {
            error
                .chain()
                .find_map(|cause| cause.downcast_ref::<crate::sandbox::ProcessFailure>())
                .map(|failure| failure.report.clone())
        });
    let error = system.safe_error(&error.to_string());
    let mut result = AppToolExecutionResult::new(
        "App call failed",
        serde_json::json!({"error":error}),
        Some(error.clone()),
    );
    result.failure = Some(error);
    result.sandbox = diagnostics;
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
                message,
                &native.call,
                base,
                SandboxScopeRequest {
                    readable_roots,
                    writable_roots,
                },
            )?
            .id
        }
    };
    let record = system.store().approval(&id)?;
    if record.source_actor(system.store())? != actor.id || !system.store().native_approval(&id)? {
        return Err(miette!("Approval reference is outside the Actor authority"));
    }
    if record.decision == ApprovalDecision::Denied {
        return forward_to_core(system, actor, base, &id, &application.reason);
    }
    let reason = application.reason.clone();
    match super::approval::submit_application(
        system,
        actor,
        message,
        control,
        base,
        &id,
        application,
    )
    .await
    {
        Ok(record) => Ok(record),
        Err(error) if super::approval::rejection(&error).is_some() => {
            forward_to_core(system, actor, base, &id, &reason)
        }
        Err(error) => Err(error),
    }
}

fn forward_to_core(
    system: &ActorSystem,
    actor: &ActorRecord,
    base: &AppToolExecutionContext,
    id: &str,
    explanation: &str,
) -> Result<ApprovalRecord> {
    if actor.id == crate::stage::shared::ACTOR_ID {
        system.appeal_from_current_core(id, explanation)
    } else {
        let caller = base
            .actor_call
            .as_ref()
            .ok_or_else(|| miette!("Application requires an active request"))?;
        system.appeal_from_actor(caller, id, explanation)
    }
}

#[cfg(test)]
mod native_denial_tests {
    use super::*;
    use crate::sandbox::Access;

    fn base() -> AppToolExecutionContext {
        AppToolExecutionContext {
            restart_native: false,
            access_broker: None,
            execution_cwd: std::env::temp_dir(),
            sandbox_policy: crate::sandbox::RuntimeSandboxPolicy::disabled(),
            tool_output_max_tokens: 1024,
            turn_epoch: 0,
            actor_call: None,
        }
    }

    #[test]
    fn native_records_fill_the_requested_scope_for_both_accesses() {
        let base = base();
        let mut request = SandboxScopeRequest {
            readable_roots: vec![],
            writable_roots: vec![],
        };
        let file = base.execution_cwd.join("denied.txt");
        let out = base.execution_cwd.join("denied-out");
        push_denial_scope(&mut request, &base, &file, Access::Read);
        push_denial_scope(&mut request, &base, &file, Access::Read);
        push_denial_scope(&mut request, &base, &out, Access::Write);
        assert_eq!(request.readable_roots, vec![file.clone()]);
        assert_eq!(request.writable_roots, vec![out.clone()]);
    }
}
