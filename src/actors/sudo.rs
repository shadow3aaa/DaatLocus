//! Review is separate from execution, so incoming messages can settle while it waits.
use super::*;
use crate::{
    app::{AppToolExecutionContext, AppToolExecutionResult},
    model_protocol::AgentToolCall,
    sudo_app::{ExecuteArgs, Invocation},
};
use miette::{Result, miette};

pub(crate) enum ReviewOutcome {
    Execute {
        approval_id: String,
        invocation: Invocation,
    },
    Submitted(AppToolExecutionResult),
}

pub(crate) fn unrestricted_context(
    base: &AppToolExecutionContext,
) -> Result<AppToolExecutionContext> {
    if base
        .actor_call
        .as_ref()
        .is_none_or(|caller| caller.remote_read_only)
        || base.sandbox_policy.filesystem.deny_all_writes
    {
        return Err(miette!(
            "Sudo requires an active request with full authenticated authority"
        ));
    }
    let mut context = base.clone();
    context.sandbox_policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    Ok(context)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn review(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    base: &AppToolExecutionContext,
    call: &AgentToolCall,
    args: ExecuteArgs,
    invocation: Invocation,
) -> Result<ReviewOutcome> {
    unrestricted_context(base)?;
    if message.remote_authority.is_some() {
        return Err(miette!(
            "This remote request cannot bypass the executing device's sandbox"
        ));
    }
    approval::ensure_current(system, actor, message, control)?;
    let existing = match args.approval_id {
        Some(id) => Some(system.store().approval(&id)?),
        None => system
            .store()
            .sudo_application_for_call(&message.id, &call.id)?,
    };
    let record = if let Some(record) = existing {
        if record.sudo.as_ref() != Some(&invocation)
            || record.source_actor(system.store())? != actor.id
            || record.execution_claim.is_some()
            || !system.store().native_approval(&record.id)?
        {
            return Err(miette!(
                "Sudo application does not match this target or has already been consumed"
            ));
        }
        record
    } else {
        system.store().create_approval(ApprovalRecord {
            seq: 0,
            id: uuid::Uuid::new_v4().to_string(),
            message_id: message.id.clone(),
            call: call.clone(),
            requested: crate::sandbox::SandboxScopeRequest::default(),
            scopes: vec![],
            sudo: Some(invocation.clone()),
            execution_claim: None,
            previous_denials: vec![],
            decision: ApprovalDecision::ApplicationRequired,
            reason: "Explicit Sudo review required".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            resolved_at_ms: None,
            appeal: None,
            application: None,
        })?
    };
    if record
        .appeal
        .as_ref()
        .map_or(record.decision, |appeal| appeal.decision)
        == ApprovalDecision::Approved
    {
        return Ok(ReviewOutcome::Execute {
            approval_id: record.id,
            invocation,
        });
    }
    let application = record.application.clone().unwrap_or(ReviewApplication {
        task: message.content.chars().take(1000).collect(),
        reason: args.reason,
    });
    let approval_id = record.id.clone();
    let record = match runtime_permissions::submit(
        system,
        actor,
        message,
        control,
        base,
        runtime_permissions::ApplicationSource::Approval {
            approval_id: record.id,
        },
        application,
    )
    .await
    {
        Ok(record) => record,
        Err(error) => {
            // Keep the immutable incident available even if Core delivery fails.
            // Retrying with this ID must not start a second automatic review.
            let error = system.safe_error(&error.to_string());
            let payload = serde_json::json!({"status":"submission_failed", "approval_id":approval_id,
                "executed":false, "error":error});
            let mut result = AppToolExecutionResult::new(
                "Sudo application could not be submitted",
                payload.clone(),
                Some(format!(
                    "{payload}\nNo operation was executed. Resolve the submission failure before continuing."
                )),
            );
            result.failure = Some(error);
            return Ok(ReviewOutcome::Submitted(result));
        }
    };
    if record
        .appeal
        .as_ref()
        .map_or(record.decision, |appeal| appeal.decision)
        == ApprovalDecision::Approved
    {
        Ok(ReviewOutcome::Execute {
            approval_id: record.id,
            invocation,
        })
    } else {
        let payload = serde_json::json!({"status":"submitted","approval_id":record.id,
            "stage_id":system.store().public_stage_id()?,"executed":false});
        Ok(ReviewOutcome::Submitted(AppToolExecutionResult::new(
            "Sudo application submitted to Core",
            payload.clone(),
            Some(format!(
                "{payload}\nNo operation was executed. Await the permission decision before continuing this operation."
            )),
        )))
    }
}
