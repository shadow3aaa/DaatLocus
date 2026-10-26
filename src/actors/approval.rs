//! Independent automatic review of explicit device sandbox applications.

use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    ActorControl, ActorRecord, ActorSystem, ApprovalDecision, ApprovalRecord, MessageRecord,
    MessageStatus,
};
use crate::{
    app::AppToolExecutionContext,
    core::ModelRequestOptions,
    model_protocol::{AgentToolCall, PromptRequest},
    schema_utils::{model_schema_for, validate_value_against_schema},
};

const REVIEW_ARGUMENT_BYTES: usize = 16 * 1024;
const CANCEL_INTERVAL: Duration = Duration::from_millis(25);

pub(crate) fn scopes_digest(record: &ApprovalRecord) -> Result<String> {
    if let Some(invocation) = &record.sudo {
        return Ok(crate::stage::digest(
            &serde_json::to_vec(&json!({
                "scopes":record.scopes,"sudo":invocation
            }))
            .map_err(|error| miette!("{error}"))?,
        ));
    }
    Ok(crate::stage::digest(
        &serde_json::to_vec(&record.scopes).map_err(|error| miette!("{error}"))?,
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReviewFailure {
    Arguments,
    Configuration,
    Budget,
    Request,
    Timeout,
    InvalidResponse,
}
impl ReviewFailure {
    fn reason(&self) -> &'static str {
        match self {
            Self::Arguments => {
                "Arguments exceed the automatic review limit; narrow the proposed operation"
            }
            Self::Configuration => {
                "Automatic review model could not be initialized; this operation was denied"
            }
            Self::Budget => {
                "Automatic review input exceeds the model budget; this operation was denied"
            }
            Self::Request => "Automatic review model request failed; this operation was denied",
            Self::Timeout => "Automatic review model request timed out; this operation was denied",
            Self::InvalidResponse => {
                "Automatic review model returned an invalid decision; this operation was denied"
            }
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Arguments => "arguments",
            Self::Configuration => "configuration",
            Self::Budget => "budget",
            Self::Request => "request",
            Self::Timeout => "timeout",
            Self::InvalidResponse => "invalid_response",
        }
    }

    /// The stored denial reason appends the stable category for later queries.
    pub(crate) fn denial_reason(&self) -> String {
        format!("{} [review_failure={}]", self.reason(), self.code())
    }
}

/// Review log identifiers; absent when the reviewer runs without the caller's
/// approval record, for example across the Core review RPC.
#[derive(Clone, Copy, Default)]
pub(crate) struct ReviewLogContext<'a> {
    pub approval_id: Option<&'a str>,
    pub actor_id: Option<&'a str>,
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("Automatic review denied the App call: {0}")]
pub struct AutomaticReviewDenied(pub ApprovalRejection);

/// A permission decision, distinct from an App execution failure. References
/// stay structured across devices rather than being recovered from error prose.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRejection {
    pub approval_id: String,
    pub stage_id: String,
    pub reason: String,
}

impl std::fmt::Display for ApprovalRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

pub(crate) fn rejection(error: &miette::Report) -> Option<ApprovalRejection> {
    if let Some(error) = error.downcast_ref::<AutomaticReviewDenied>() {
        return Some(error.0.clone());
    }
    match error.downcast_ref::<crate::stage::StageError>() {
        Some(crate::stage::StageError::ApprovalDenied(rejection)) => Some(rejection.clone()),
        _ => None,
    }
}

impl ApprovalRejection {
    pub(crate) fn payload(&self) -> serde_json::Value {
        json!({"approval": {
            "decision": "denied", "approval_id": self.approval_id,
            "stage_id": self.stage_id, "reason": self.reason
        }})
    }

    pub(crate) fn model_content(&self) -> String {
        let guidance = "Do not submit this effect for automatic review again. If needed, use actors__appeal_approval with this approval_id and stage_id, null tool_result_id/readable_roots/writable_roots and an explanation to ask the Owner through Core without blocking.";
        format!(
            "{}\nThe operation was not executed. Do not retry the denied effect before authorization. {guidance}",
            self.payload()
        )
    }
}

#[model_schema]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ReviewDecision {
    Approve,
    Deny,
}

#[model_schema]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewVerdict {
    decision: ReviewDecision,
    reason: String,
}

const REVIEW_INSTRUCTIONS: &str = include_str!("../prompts/approval_review.md");

/// Bound model explanations before binding them to an immutable application.
pub(crate) fn validate_application(application: &super::ReviewApplication) -> Result<()> {
    if application.task.trim().is_empty()
        || application.reason.trim().is_empty()
        || application.task.len() + application.reason.len() > 8192
    {
        return Err(super::ActorError::Invalid(
            "State the current task and a bounded reason for this permission".into(),
        )
        .into());
    }
    Ok(())
}

/// Submit only a reference and explanation. Stored arguments/scopes are never replaced or executed.
pub(crate) async fn submit_application(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    base: &AppToolExecutionContext,
    id: &str,
    application: super::ReviewApplication,
) -> Result<ApprovalRecord> {
    validate_application(&application)?;
    ensure_current(system, actor, message, control)?;
    let record = system.store().approval(id)?;
    if record.source_actor(system.store())? != actor.id
        || record.message_id != message.id
        || !system.store().native_approval(id)?
    {
        return Err(super::ActorError::Forbidden(
            "Approval reference is outside the active Actor authority".into(),
        )
        .into());
    }
    if record.requested.clone().canonicalized()? != record.requested {
        return Err(super::ActorError::Forbidden(
            "Approval targets changed; propose a fresh operation".into(),
        )
        .into());
    }
    let effective = system.requested_tool_context(base, &record.requested)?;
    let (record, claimed) =
        system
            .store()
            .submit_review_application(&actor.id, &message.id, id, &application)?;
    if record.decision != ApprovalDecision::Pending {
        apply_decision(system, &record)?;
        return Ok(record);
    }
    if !claimed {
        let changed = system.store().changed();
        loop {
            let notified = changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            ensure_current(system, actor, message, control)?;
            let current = system.store().approval(id)?;
            if current.decision != ApprovalDecision::Pending {
                effective.sandbox_policy.filesystem.ensure_root_bindings()?;
                apply_decision(system, &current)?;
                return Ok(current);
            }
            tokio::select! { _ = &mut notified => {}, _ = tokio::time::sleep(CANCEL_INTERVAL) => {} }
        }
    }
    let result = authorize_record(system, actor, message, &record.call, control, &record).await;
    if result.is_err() && ensure_current(system, actor, message, control).is_err() {
        let _ = system.store().resolve_approval(
            id,
            ApprovalDecision::Cancelled,
            "Request interrupted or Actor configuration changed",
        );
    }
    result?;
    ensure_current(system, actor, message, control)?;
    effective.sandbox_policy.filesystem.ensure_root_bindings()?;
    system.store().approval(id)
}

async fn authorize_record(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    control: &ActorControl,
    record: &ApprovalRecord,
) -> Result<()> {
    let review = if call.arguments.to_string().len() > REVIEW_ARGUMENT_BYTES {
        Err(ReviewFailure::Arguments)
    } else {
        automatic_review(system, actor, message, call, control, record).await?
    };
    // Cancellation can resolve a pending review while the model request is in flight.
    let current = system.store().approval(&record.id)?;
    if current.decision != ApprovalDecision::Pending {
        ensure_current(system, actor, message, control)?;
        return apply_decision(system, &current);
    }
    let (decision, reason) = match review {
        Ok(ReviewVerdict {
            decision: ReviewDecision::Approve,
            reason,
        }) => (ApprovalDecision::Approved, reason),
        Ok(ReviewVerdict {
            decision: ReviewDecision::Deny,
            reason,
        }) => (ApprovalDecision::Denied, reason),
        Err(failure) => (ApprovalDecision::Denied, failure.denial_reason()),
    };
    ensure_current(system, actor, message, control)?;
    if decision == ApprovalDecision::Approved
        && record.requested.clone().canonicalized()? != record.requested
    {
        let _ = system.store().resolve_approval(
            &record.id,
            ApprovalDecision::Cancelled,
            "Sandbox target changed while review was pending",
        );
        return Err(miette!("Sandbox target changed while review was pending"));
    }
    let reason = bounded_reason(system, &reason);
    let resolved = match system
        .store()
        .resolve_approval(&record.id, decision, &reason)
    {
        Ok(record) => record,
        Err(_) => system.store().approval(&record.id)?,
    };
    ensure_current(system, actor, message, control)?;
    apply_decision(system, &resolved)
}

fn bounded_reason(system: &ActorSystem, text: &str) -> String {
    let text = system.safe_error(text);
    let reason: String = text.chars().take(1000).collect();
    if reason.trim().is_empty() {
        "Approval decision requires a clear reason".into()
    } else {
        reason
    }
}

fn apply_decision(system: &ActorSystem, record: &ApprovalRecord) -> Result<()> {
    match record.decision {
        ApprovalDecision::Approved => Ok(()),
        ApprovalDecision::Denied | ApprovalDecision::ApplicationRequired
            if record.requires_core_review() =>
        {
            let rejection = ApprovalRejection {
                approval_id: record.id.clone(),
                stage_id: system.store().public_stage_id()?.to_string(),
                reason: system.safe_error(&record.reason),
            };
            Err(AutomaticReviewDenied(rejection).into())
        }
        ApprovalDecision::ApplicationRequired => {
            Err(miette!("Submit the approval application first"))
        }
        ApprovalDecision::Denied => unreachable!("denied records require Core review"),
        ApprovalDecision::Cancelled => Err(miette!("App call approval was cancelled")),
        ApprovalDecision::Pending => Err(miette!("App call approval is still pending")),
    }
}

fn cancelled(control: &ActorControl, message: &MessageRecord) -> bool {
    control.stop.load(Ordering::Acquire)
        || message.cancel_requested
        || control.cancelled.lock().contains(&message.id)
}

pub(crate) fn ensure_current(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
) -> Result<()> {
    if cancelled(control, message) {
        return Err(miette!("Execution interrupted while awaiting approval"));
    }
    let current_message = system.store().message(&message.id)?;
    if current_message.actor_id != actor.id
        || current_message.cancel_requested
        || current_message.status != MessageStatus::Running
    {
        return Err(miette!("App call request is no longer active"));
    }
    let current_actor = if actor.id == crate::stage::shared::ACTOR_ID {
        system.core_device_actor()?
    } else {
        system.execution_actor(&current_message)?
    };
    if current_actor.config != actor.config {
        return Err(miette!(
            "Actor configuration changed while awaiting approval"
        ));
    }
    Ok(())
}

/// Empty model history and structured-only output keep the reviewer separate
/// from the executing Actor. Any provider, schema, timeout, or budget failure
/// denies the operation rather than granting execution or waiting for a person.
async fn automatic_review(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    control: &ActorControl,
    record: &ApprovalRecord,
) -> Result<std::result::Result<ReviewVerdict, ReviewFailure>> {
    let started = Instant::now();
    let application = record
        .application
        .as_ref()
        .ok_or_else(|| miette!("Approval application is missing"))?;
    let proposed_call = call;
    let runtime_reads = match crate::sandbox::runtime_read_scopes() {
        Ok(roots) => roots,
        Err(_) => return Ok(Err(ReviewFailure::Configuration)),
    };
    let input = json!({"actor":{"id":actor.id,
            "installed_apps":actor.config.apps,"workspace":actor.config.workspace,"remote_read_only":actor.remote_read_only},
            "request":{"content":message.content,"source":message.source,
                "kind":message.kind,"in_reply_to":message.in_reply_to},
            "application":application,
            "remembered_permissions":system.store().sandbox_scopes()?,
            "authorized_baseline":{"system_temporary_paths":crate::sandbox::system_temporary_paths()
                .iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<String>>(),
                "runtime_read_scopes":runtime_reads},
            "source_call":proposed_call,
            "sudo_invocation":record.sudo,
            "requested_additional_filesystem_access":record.requested,
            "requested_scopes":record.scopes})
        .to_string();
    // Device execution never claims or moves Core. Its independent review RPC
    // goes to the already selected account worker; local accounts use this host.
    let future = async {
        if let Some(core) = system.shared_core() {
            core.review_approval(input).await.map_err(|error| {
                tracing::warn!(
                    approval_id = %record.id,
                    actor_id = %actor.id,
                    failure = ReviewFailure::Request.code(),
                    elapsed_ms = started.elapsed().as_millis(),
                    error = %system.safe_error(&error.to_string()),
                    "Automatic review RPC failed"
                );
                ReviewFailure::Request
            })?
        } else if system.local_core() {
            review_on_core(
                system,
                input,
                ReviewLogContext {
                    approval_id: Some(&record.id),
                    actor_id: Some(&actor.id),
                },
            )
            .await
        } else {
            tracing::warn!(
                approval_id = %record.id,
                actor_id = %actor.id,
                failure = ReviewFailure::Request.code(),
                elapsed_ms = started.elapsed().as_millis(),
                "Automatic review is unavailable without a Core worker"
            );
            Err(ReviewFailure::Request)
        }
    };
    tokio::pin!(future);
    let changed = system.store().changed();
    let verdict = 'review: loop {
        let notified = changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        ensure_current(system, actor, message, control)?;
        if system.store().approval(&record.id)?.decision != ApprovalDecision::Pending {
            tracing::debug!(
                approval_id = %record.id,
                "Automatic review was superseded by a resolved approval"
            );
            return Ok(Err(ReviewFailure::Request));
        }
        loop {
            tokio::select! {
                result = &mut future => break 'review result,
                () = &mut notified => break,
                () = tokio::time::sleep(CANCEL_INTERVAL) => {
                    if cancelled(control, message) { return Err(miette!("Execution interrupted during automatic review")); }
                },
            }
        }
    };
    Ok(verdict)
}

/// Fixed independent reviewer, run by the current Core host without its inbox/history.
pub(crate) async fn review_on_core(
    system: &ActorSystem,
    facts: String,
    log: ReviewLogContext<'_>,
) -> std::result::Result<ReviewVerdict, ReviewFailure> {
    let started = Instant::now();
    let approval_id = log.approval_id.unwrap_or("unknown");
    let actor_id = log.actor_id.unwrap_or("unknown");
    if facts.len() > 256 * 1024 {
        tracing::warn!(
            approval_id,
            actor_id,
            failure = ReviewFailure::Arguments.code(),
            facts_bytes = facts.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "Automatic review input exceeds the automatic review limit"
        );
        return Err(ReviewFailure::Arguments);
    }
    let provider = system.approval_provider().map_err(|_| {
        tracing::warn!(
            approval_id,
            actor_id,
            failure = ReviewFailure::Configuration.code(),
            elapsed_ms = started.elapsed().as_millis(),
            "Automatic review model could not be initialized"
        );
        ReviewFailure::Configuration
    })?;
    let schema = model_schema_for::<ReviewVerdict>();
    let request = PromptRequest {
        tool_name: "review_app_call".into(),
        tool_description: "Review one proposed App operation".into(),
        output_schema: schema.clone(),
        system_messages: vec![REVIEW_INSTRUCTIONS.into()],
        history_messages: vec![],
        retry_messages: vec![],
        current_user_message: facts,
    };
    let options =
        ModelRequestOptions::for_prompt(provider.as_ref(), &request, None).map_err(|_| {
            tracing::warn!(
                approval_id,
                actor_id,
                failure = ReviewFailure::Budget.code(),
                elapsed_ms = started.elapsed().as_millis(),
                "Automatic review input exceeds the model budget"
            );
            ReviewFailure::Budget
        })?;
    let value = provider
        .complete_json(request, options)
        .await
        .map_err(|error| {
            let provider_failure = error.downcast_ref::<crate::providers::ModelRequestFailure>();
            let failure = if provider_failure
                .is_some_and(|failure| failure.kind == crate::providers::ModelFailureKind::Timeout)
            {
                ReviewFailure::Timeout
            } else {
                ReviewFailure::Request
            };
            tracing::warn!(
                approval_id,
                actor_id,
                failure = failure.code(),
                provider_code = provider_failure.map_or("none", |failure| failure.code()),
                elapsed_ms = started.elapsed().as_millis(),
                error = %system.safe_error(&crate::core::model_request_error_detail(&error)),
                "Automatic review model request failed"
            );
            failure
        })?;
    validate_value_against_schema(&value, &schema, "approval verdict").map_err(|error| {
        tracing::warn!(
            approval_id,
            actor_id,
            failure = ReviewFailure::InvalidResponse.code(),
            elapsed_ms = started.elapsed().as_millis(),
            error = %error,
            "Automatic review model decision was rejected"
        );
        ReviewFailure::InvalidResponse
    })?;
    let verdict: ReviewVerdict = serde_json::from_value(value).map_err(|error| {
        tracing::warn!(
            approval_id,
            actor_id,
            failure = ReviewFailure::InvalidResponse.code(),
            elapsed_ms = started.elapsed().as_millis(),
            error = %error,
            "Automatic review decision could not be parsed"
        );
        ReviewFailure::InvalidResponse
    })?;
    tracing::debug!(
        approval_id,
        actor_id,
        decision = ?verdict.decision,
        elapsed_ms = started.elapsed().as_millis(),
        "Automatic review decision received"
    );
    Ok(verdict)
}

#[cfg(test)]
#[path = "sandbox_review_tests.rs"]
mod tests;
