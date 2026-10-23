//! App-declared and sandbox review with durable Actor-local fingerprints.

use std::{sync::atomic::Ordering, time::Duration};

use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::{
    ActorControl, ActorRecord, ActorSystem, AppReview, ApprovalDecision, ApprovalFingerprint,
    ApprovalMode, ApprovalRecord, ApprovalScope, MessageRecord, MessageStatus,
    ToolPermissionRequest,
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
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("App call approval was denied: {0}")]
pub struct ApprovalDenied(pub ApprovalRejection);

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
    pub mode: ApprovalMode,
}

impl std::fmt::Display for ApprovalRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

pub(crate) fn rejection(error: &miette::Report) -> Option<ApprovalRejection> {
    if let Some(error) = error.downcast_ref::<ApprovalDenied>() {
        return Some(error.0.clone());
    }
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
            "stage_id": self.stage_id, "mode": self.mode, "reason": self.reason
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

const REVIEW_INSTRUCTIONS: &str = include_str!("../app_instructions/approval_review.md");

/// The caller must first validate the App registry, arguments, and capabilities.
/// A denial prevents this operation and permits authorized alternatives.
#[allow(clippy::too_many_arguments)]
pub async fn authorize(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
    control: &ActorControl,
    base: &AppToolExecutionContext,
    requested: &ToolPermissionRequest,
    review: Option<&AppReview>,
) -> Result<AppToolExecutionContext> {
    ensure_current(system, actor, message, control)?;
    let requested = requested.clone().canonicalized()?;
    let effective = system.requested_tool_context(base, &requested)?;
    let mode = actor.config.permissions.approval_mode;
    if mode == ApprovalMode::None {
        return Ok(effective);
    }
    let mut scopes = Vec::new();
    if let Some(review) = review {
        scopes.push(review.approval_fingerprint()?);
    }
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
    if scopes.is_empty() {
        return Ok(effective);
    }
    let scopes = system.store().unapproved_scopes(&actor.id, scopes)?;
    if scopes.is_empty() {
        return Ok(effective);
    }
    // A changed call id or retry is not a new review of an already denied action.
    if let Some(record) = system.store().denied_operation(
        &actor.id,
        &message.id,
        call,
        &requested,
        review,
        mode,
        &scopes,
    )? {
        apply_decision(system, &record)?;
    }
    // Pre-execution review suspends this invocation; no model round trip or
    // second execution is needed. Post-execution references remain separate.
    let task: String = message.content.chars().take(1000).collect();
    let record = system.store().create_approval(ApprovalRecord {
        seq: 0,
        id: Uuid::new_v4().to_string(),
        actor_id: actor.id.clone(),
        message_id: message.id.clone(),
        call: call.clone(),
        requested: requested.clone(),
        review: review.cloned(),
        scopes,
        mode,
        decision: ApprovalDecision::Pending,
        reason: "Permission review requested".into(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        resolved_at_ms: None,
        appeal: None,
        application: Some(super::ReviewApplication {
            task: if task.trim().is_empty() {
                "Complete the active request".into()
            } else {
                task
            },
            reason: format!(
                "Review the requested {} operation before execution",
                call.name
            ),
        }),
    })?;
    let result = authorize_record(system, actor, message, call, control, &record).await;
    if result.is_err() && ensure_current(system, actor, message, control).is_err() {
        let _ = system.store().resolve_approval(
            &record.id,
            ApprovalDecision::Cancelled,
            "Request interrupted or Actor configuration changed",
        );
    }
    result?;
    ensure_current(system, actor, message, control)?;
    effective.sandbox_policy.filesystem.ensure_root_bindings()?;
    Ok(effective)
}

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
    if record.actor_id != actor.id
        || record.message_id != message.id
        || !system.store().native_approval(id)?
        || record.mode != actor.config.permissions.approval_mode
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
    let record =
        system
            .store()
            .submit_review_application(&actor.id, &message.id, id, &application)?;
    if record.decision != ApprovalDecision::Pending {
        apply_decision(system, &record)?;
        return Ok(record);
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
    if record.mode == ApprovalMode::None {
        ensure_current(system, actor, message, control)?;
        let resolved = system.store().resolve_approval(
            &record.id,
            ApprovalDecision::Approved,
            "Permission allowed by the account Full access policy",
        )?;
        return apply_decision(system, &resolved);
    }
    if record.mode == ApprovalMode::Auto {
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
            Err(failure) => (ApprovalDecision::Denied, failure.reason().into()),
        };
        ensure_current(system, actor, message, control)?;
        let reason = bounded_reason(system, &reason);
        let resolved = match system
            .store()
            .resolve_approval(&record.id, decision, &reason)
        {
            Ok(record) => record,
            Err(_) => system.store().approval(&record.id)?,
        };
        ensure_current(system, actor, message, control)?;
        return apply_decision(system, &resolved);
    }
    wait_for_owner(system, actor, message, control, &record.id).await
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
        ApprovalDecision::Denied => {
            let rejection = ApprovalRejection {
                approval_id: record.id.clone(),
                stage_id: system.store().public_stage_id()?.to_string(),
                reason: system.safe_error(&record.reason),
                mode: record.mode,
            };
            if record.mode == ApprovalMode::Ask {
                Err(ApprovalDenied(rejection).into())
            } else {
                Err(AutomaticReviewDenied(rejection).into())
            }
        }
        ApprovalDecision::ApplicationRequired => {
            Err(miette!("Submit the approval application first"))
        }
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
    if current_actor.config.permissions.approval_mode != actor.config.permissions.approval_mode
        || (current_actor.config != actor.config
            && !(actor.id == crate::stage::shared::ACTOR_ID
                && super::permissions::check_recipient(&current_actor.config, &actor.config)
                    .is_ok()))
    {
        return Err(miette!(
            "Actor configuration changed while awaiting approval"
        ));
    }
    Ok(())
}

async fn wait_for_owner(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    control: &ActorControl,
    approval_id: &str,
) -> Result<()> {
    let changed = system.store().changed();
    loop {
        let notified = changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        ensure_current(system, actor, message, control)?;
        let record = system.store().approval(approval_id)?;
        if record.decision != ApprovalDecision::Pending {
            return apply_decision(system, &record);
        }
        let heartbeat = tokio::time::sleep(Duration::from_secs(1));
        tokio::pin!(heartbeat);
        loop {
            tokio::select! {
                () = &mut notified => break,
                () = &mut heartbeat => break,
                () = tokio::time::sleep(CANCEL_INTERVAL) => {
                    if cancelled(control, message) { return Err(miette!("Execution interrupted while awaiting approval")); }
                }
            }
        }
    }
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
    let facts = match system.approval_facts(actor, message, call) {
        Ok(facts) => facts,
        Err(_) => return Ok(Err(ReviewFailure::Arguments)),
    };
    let application = record
        .application
        .as_ref()
        .ok_or_else(|| miette!("Approval application is missing"))?;
    let proposed_call = call;
    let input = json!({"actor":{"id":actor.id,"grants":actor.config.permissions,
            "installed_apps":actor.config.apps,"workspace":actor.config.workspace,"remote_read_only":actor.remote_read_only},
            "request":{"content":message.content,"source":message.source,
                "kind":message.kind,"in_reply_to":message.in_reply_to},
            "application":application,
            "remembered_permissions":system.store().approval_scopes(&actor.id)?,
            "operation_facts":facts,"proposed_call":proposed_call,
            "requested_additional_filesystem_access":record.requested,
            "app_review":record.review,"unapproved_scopes":record.scopes})
        .to_string();
    // Device execution never claims or moves Core. Its independent review RPC
    // goes to the already selected account worker; local accounts use this host.
    let future = async {
        if let Some(core) = system.shared_core() {
            core.review_approval(input)
                .await
                .map_err(|_| ReviewFailure::Request)?
        } else if system.local_core() {
            review_on_core(system, input).await
        } else {
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
) -> std::result::Result<ReviewVerdict, ReviewFailure> {
    if facts.len() > 256 * 1024 {
        return Err(ReviewFailure::Arguments);
    }
    let provider = system
        .approval_provider()
        .map_err(|_| ReviewFailure::Configuration)?;
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
    let options = ModelRequestOptions::for_prompt(provider.as_ref(), &request, None)
        .map_err(|_| ReviewFailure::Budget)?;
    let value = provider
        .complete_json(request, options)
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<crate::providers::ModelRequestFailure>()
                .is_some_and(|failure| failure.kind == crate::providers::ModelFailureKind::Timeout)
            {
                ReviewFailure::Timeout
            } else {
                ReviewFailure::Request
            }
        })?;
    validate_value_against_schema(&value, &schema, "approval verdict")
        .map_err(|_| ReviewFailure::InvalidResponse)?;
    serde_json::from_value(value).map_err(|_| ReviewFailure::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Config, ModelConfig, ProviderConfig},
        context_budget::RequestBudgetLimits,
        core::{ModelProvider, TokenUsageInfo},
        model_protocol::{AgentTurnRequest, AgentTurnStreamResult},
    };
    use async_trait::async_trait;
    use std::sync::{Arc, atomic::AtomicBool};

    enum ReviewReply {
        Json(serde_json::Value),
        Delayed(Duration, serde_json::Value),
        Fail,
        Timeout,
        Pending,
    }
    struct Reviewer {
        reply: parking_lot::Mutex<Option<ReviewReply>>,
        requests: parking_lot::Mutex<Vec<PromptRequest>>,
        started: tokio::sync::Notify,
        dropped: Arc<AtomicBool>,
    }
    impl Reviewer {
        fn new(reply: ReviewReply) -> Self {
            Self {
                reply: parking_lot::Mutex::new(Some(reply)),
                requests: parking_lot::Mutex::new(vec![]),
                started: tokio::sync::Notify::new(),
                dropped: Arc::new(AtomicBool::new(false)),
            }
        }
    }
    struct DropMarker(Arc<AtomicBool>);
    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    #[async_trait]
    impl ModelProvider for Reviewer {
        async fn complete_json(
            &self,
            request: PromptRequest,
            options: ModelRequestOptions,
        ) -> Result<serde_json::Value> {
            assert!(
                options.conversation_id.is_none(),
                "reviewer must not reuse the executing conversation"
            );
            self.requests.lock().push(request);
            self.started.notify_one();
            let reply = self.reply.lock().take().expect("one reviewer request");
            match reply {
                ReviewReply::Json(value) => Ok(value),
                ReviewReply::Delayed(delay, value) => {
                    tokio::time::sleep(delay).await;
                    Ok(value)
                }
                ReviewReply::Fail => Err(miette!("mock review unavailable")),
                ReviewReply::Timeout => Err(crate::providers::ModelRequestFailure::report(
                    crate::providers::ModelFailureKind::Timeout,
                    "fixture provider reached its configured timeout".into(),
                )),
                ReviewReply::Pending => {
                    let _drop = DropMarker(self.dropped.clone());
                    std::future::pending().await
                }
            }
        }
        async fn complete_agent_turn(
            &self,
            _: AgentTurnRequest,
            _: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            panic!("permission reviewer cannot execute Actor turns");
        }
        fn request_budget_limits(&self) -> RequestBudgetLimits {
            RequestBudgetLimits {
                context_window_tokens: 100_000,
                auto_compact_threshold_tokens: 100_000,
                reserved_output_tokens: 2_000,
            }
        }
        fn token_usage_info(&self) -> TokenUsageInfo {
            TokenUsageInfo::default()
        }
        fn model_name(&self) -> String {
            "mock-reviewer".into()
        }
    }
    struct Factory(Arc<Reviewer>);
    impl super::super::ModelFactory for Factory {
        fn build(&self, name: &str, _: &Config) -> Result<Arc<dyn ModelProvider>> {
            assert_eq!(
                name, "reviewer",
                "use the independently selected approval model"
            );
            Ok(self.0.clone())
        }
    }
    async fn fixture(
        mode: ApprovalMode,
        reviewer: Arc<Reviewer>,
    ) -> (tempfile::TempDir, ActorSystem, ActorRecord, MessageRecord) {
        fixture_with_config(mode, reviewer, |_, _| {}).await
    }
    async fn fixture_with_config(
        mode: ApprovalMode,
        reviewer: Arc<Reviewer>,
        configure: impl FnOnce(&mut Config, &std::path::Path),
    ) -> (tempfile::TempDir, ActorSystem, ActorRecord, MessageRecord) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let mut config = Config {
            main_model: "main".into(),
            approval_model: Some("reviewer".into()),
            ..Default::default()
        };
        config.models.insert(
            "main".into(),
            ModelConfig {
                provider: "mock".into(),
                model_id: "mock".into(),
                ..Default::default()
            },
        );
        config
            .models
            .insert("reviewer".into(), config.models["main"].clone());
        config.providers.insert(
            "mock".into(),
            ProviderConfig::OpenaiCompatible {
                base_url: "https://example.invalid".into(),
                api_key: "unused-test-key".into(),
            },
        );
        configure(&mut config, dir.path());
        let system = ActorSystem::open_paused_with_factory(
            dir.path().join("data"),
            workspace,
            config,
            Arc::new(Factory(reviewer)),
        )
        .await
        .unwrap();
        system.install_local_core().unwrap();
        system
            .patch_owned_preferences("core", None, Some(mode))
            .await
            .unwrap();
        system.seed_test_actor("root");
        let actor = system.store().actor("root").unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let queued = system
            .store()
            .enqueue(
                MessageRecord {
                    seq: 0,
                    id: Uuid::new_v4().to_string(),
                    actor_id: actor.id.clone(),
                    source: super::super::MessageSource::User,
                    reply_to: super::super::ReplyTarget::User,
                    kind: super::super::MessageKind::Request,
                    in_reply_to: None,
                    delegation_grant: None,
                    remote_authority: None,
                    content: "Read source.txt in this workspace".into(),
                    attachments: vec![],
                    status: MessageStatus::Queued,
                    cancel_requested: false,
                    final_generation: None,
                    started_at_ms: None,
                    finished_at_ms: None,
                    reply: None,
                    error: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
                None,
            )
            .unwrap();
        let message = system.store().claim_next(&actor.id).unwrap().unwrap();
        assert_eq!(message.actor_id, actor.id);
        assert_eq!(message.id, queued.id);
        (dir, system, actor, message)
    }
    async fn authorize_reviewed(
        system: &ActorSystem,
        actor: &ActorRecord,
        message: &MessageRecord,
        call: &AgentToolCall,
        control: &ActorControl,
        base: &AppToolExecutionContext,
        requested: &ToolPermissionRequest,
    ) -> Result<AppToolExecutionContext> {
        let call = call.clone();
        let review = AppReview::new(
            "test/review/v1",
            "Review this test operation against the explicit user request.",
            json!({"tool":call.name,"arguments":call.arguments}),
        );
        super::authorize(
            system,
            actor,
            message,
            &call,
            control,
            base,
            requested,
            Some(&review),
        )
        .await
    }
    fn call() -> AgentToolCall {
        AgentToolCall {
            id: "proposed-call".into(),
            name: "files__read_file".into(),
            arguments: json!({"path":"source.txt","start_line":null,"line_count":null}),
        }
    }
    async fn execute_control(
        system: &ActorSystem,
        actor: &ActorRecord,
        message: &MessageRecord,
        apps: &mut crate::app::AppManager,
        call: &AgentToolCall,
    ) -> Result<crate::app::AppToolExecutionResult> {
        let base = system.execution_context(actor, message)?;
        let requested = apps.permission_request(call, &base)?;
        let review = apps.review_requirement(call, &base)?;
        let effective = super::authorize(
            system,
            actor,
            message,
            call,
            &ActorControl::default(),
            &base,
            &requested,
            review.as_ref(),
        )
        .await?;
        apps.execute_tool(call, &effective).await
    }

    #[tokio::test]
    async fn unavailable_core_never_falls_back_to_the_app_devices_review_model() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"must not be requested"}),
        )));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        system.disable_local_core();
        let result =
            authorize_simple(&system, &actor, &message, &call(), &ActorControl::default()).await;
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<AutomaticReviewDenied>()
                .is_some()
        );
        assert!(reviewer.requests.lock().is_empty());
        assert!(
            system
                .store()
                .approval_scopes(&actor.id)
                .unwrap()
                .is_empty()
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn preflight_review_keeps_declared_schema_and_needs_no_application() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"requested command"}),
        )));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let apps =
            crate::app::AppManager::new(vec![Box::new(crate::terminal_app::TerminalApp::new())])
                .unwrap();
        let mut call = AgentToolCall {
            id: "new-command".into(),
            name: "terminal__terminal_exec".into(),
            arguments: json!({
            "command":"echo fixture","session_id":null,"workdir":null,"additional_read_roots":null,
            "additional_write_roots":null,"yield_time_ms":null,"max_chars":null}),
        };
        let base = system.execution_context(&actor, &message).unwrap();
        apps.validate_tool_call_for_approval(&call, &base).unwrap();
        let declared = apps
            .all_tool_specs()
            .into_iter()
            .find(|(id, _)| id.as_str() == "terminal")
            .unwrap()
            .1;
        let declared = declared
            .into_iter()
            .find(|tool| tool.name == "terminal_exec")
            .unwrap();
        let model = apps
            .model_tool_specs()
            .unwrap()
            .into_iter()
            .find(|tool| tool.name == call.name)
            .unwrap();
        let crate::model_protocol::AgentToolInputSpec::JsonSchema { schema } = model.input_spec
        else {
            panic!("expected portable schema")
        };
        assert_eq!(
            schema, declared.input_schema,
            "ordinary tools must retain exactly their declared schema"
        );
        let requested = apps.permission_request(&call, &base).unwrap();
        let review = apps.review_requirement(&call, &base).unwrap();
        super::authorize(
            &system,
            &actor,
            &message,
            &call,
            &ActorControl::default(),
            &base,
            &requested,
            review.as_ref(),
        )
        .await
        .unwrap();
        let record = approval(&system, &message.id).await;
        assert_eq!(record.decision, ApprovalDecision::Approved);
        assert_eq!(record.call.arguments, call.arguments);
        call.id = Uuid::new_v4().to_string();
        super::authorize(
            &system,
            &actor,
            &message,
            &call,
            &ActorControl::default(),
            &base,
            &requested,
            review.as_ref(),
        )
        .await
        .unwrap();
        assert_eq!(reviewer.requests.lock().len(), 1);
        assert_eq!(
            system
                .store()
                .approvals(Some(&actor.id), None, 10)
                .unwrap()
                .items
                .len(),
            1
        );
        let facts: serde_json::Value =
            serde_json::from_str(&reviewer.requests.lock()[0].current_user_message).unwrap();
        assert_eq!(facts["request"]["content"], message.content);
        assert_eq!(facts["proposed_call"]["arguments"], call.arguments);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn an_unsubmitted_reference_cannot_outlive_its_originating_input() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let base = system.execution_context(&actor, &message).unwrap();
        let target = base
            .execution_cwd
            .parent()
            .unwrap()
            .join("outside-input-fixture");
        let error = base
            .sandbox_policy
            .ensure_path_readable(&target, "fixture input")
            .unwrap_err();
        let mut result = crate::app::AppToolExecutionResult::new("Fixture denial", json!({}), None);
        super::super::runtime_permissions::attach_reference(
            &system,
            &actor,
            &message,
            &call(),
            &base,
            "fixture-result",
            &mut result,
            Some(&error),
        )
        .unwrap();
        let reference = result.permission_reference.unwrap();
        let approval_id = reference.approval_id.unwrap();
        let application = super::super::ReviewApplication {
            task: "Read the requested evidence".into(),
            reason: "Inspect the requested source".into(),
        };
        system.store().request_cancel(&message.id).unwrap();
        assert!(
            super::submit_application(
                &system,
                &actor,
                &message,
                &ActorControl::default(),
                &base,
                &approval_id,
                application.clone()
            )
            .await
            .is_err()
        );
        system
            .store()
            .finish_message(&message.id, MessageStatus::Cancelled, None, None)
            .unwrap();
        assert_eq!(
            system.store().approval(&approval_id).unwrap().decision,
            ApprovalDecision::Cancelled
        );
        let queued = system
            .send_user(&actor.id, "A different request".into(), vec![], None)
            .unwrap();
        let next = system.store().claim_next(&actor.id).unwrap().unwrap();
        assert_eq!(next.id, queued.id);
        let context = system.execution_context(&actor, &next).unwrap();
        assert!(
            super::submit_application(
                &system,
                &actor,
                &next,
                &ActorControl::default(),
                &context,
                &approval_id,
                application
            )
            .await
            .is_err()
        );
        assert!(reviewer.requests.lock().is_empty());
        assert!(
            system
                .store()
                .approval_scopes(&actor.id)
                .unwrap()
                .is_empty()
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn owner_appeal_is_nonblocking_origin_bound_and_remembers_without_replaying_effects() {
        for mode in [ApprovalMode::Auto, ApprovalMode::Ask] {
            for decision in ["approve", "deny"] {
                let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
                    json!({"decision":"deny","reason":"Need the Owner's confirmation for the external output folder"}),
                )));
                let (dir, system, actor, message) = fixture(mode, reviewer.clone()).await;
                let outside = dir.path().join("requested-output");
                let worker_caller = system
                    .execution_context(&actor, &message)
                    .unwrap()
                    .actor_call
                    .unwrap();
                std::fs::create_dir(&outside).unwrap();
                let target = outside.join("result.txt");
                let write = AgentToolCall {
                    id: "write-output".into(),
                    name: "files__write_file".into(),
                    arguments: json!({"path":target,"content":"verified output","expected_sha256":null}),
                };
                let mut apps =
                    crate::builtin_apps::create_apps(&actor, system.clone(), &system.config())
                        .unwrap();
                let attempt = execute_control(&system, &actor, &message, &mut apps, &write);
                let owner = async {
                    if mode == ApprovalMode::Ask {
                        let pending = approval(&system, &message.id).await;
                        system
                            .store()
                            .resolve_approval(
                                &pending.id,
                                ApprovalDecision::Denied,
                                "Need the Owner's confirmation",
                            )
                            .unwrap();
                    }
                };
                let (attempt, ()) = tokio::join!(attempt, owner);
                let error = attempt.unwrap_err();
                let denied = system
                    .store()
                    .approvals(Some(&actor.id), None, 10)
                    .unwrap()
                    .items
                    .pop()
                    .unwrap();
                assert_eq!(rejection(&error).unwrap().approval_id, denied.id);
                assert!(!target.exists());
                let appeal_call = AgentToolCall {
                    id: "appeal-once".into(),
                    name: "actors__appeal_approval".into(),
                    arguments: json!({"stage_id":null,"approval_id":denied.id,"tool_result_id":null,"readable_roots":null,"writable_roots":null,"explanation":"The requested deliverable belongs in this output folder; may I write it there?"}),
                };
                let result = execute_control(&system, &actor, &message, &mut apps, &appeal_call)
                    .await
                    .unwrap();
                let repeated = execute_control(&system, &actor, &message, &mut apps, &appeal_call)
                    .await
                    .unwrap();
                assert_eq!(
                    result.payload["core_message_id"],
                    repeated.payload["core_message_id"]
                );
                assert_eq!(
                    system.store().message(&message.id).unwrap().status,
                    MessageStatus::Running
                );
                assert!(
                    system
                        .store()
                        .approval_scopes(&actor.id)
                        .unwrap()
                        .is_empty()
                );
                // Another ordinary operation can finish while the appeal waits.
                let local = actor.config.workspace.join("local.txt");
                std::fs::write(&local, "independent work").unwrap();
                let local_call = AgentToolCall {
                    id: "continue-work".into(),
                    name: "files__read_file".into(),
                    arguments: json!({"path":local,"start_line":null,"line_count":null}),
                };
                assert_eq!(
                    execute_control(&system, &actor, &message, &mut apps, &local_call)
                        .await
                        .unwrap()
                        .payload["content"],
                    "independent work"
                );
                // Yield the original inbox input, rather than keeping a tool call
                // pending. The later runtime notification must itself resume work.
                system
                    .store()
                    .finish_message(&message.id, MessageStatus::Completed, None, None)
                    .unwrap();
                let core = system.actor("core").unwrap();
                let mut core_apps =
                    crate::builtin_apps::create_apps(&core, system.clone(), &system.config())
                        .unwrap();
                let appeal_input = system.store().claim_next("core").unwrap().unwrap();
                assert_eq!(appeal_input.id, result.payload["core_message_id"]);
                let resolve = AgentToolCall {
                    id: "owner-decision".into(),
                    name: "actors__resolve_approval_appeal".into(),
                    arguments: json!({"stage_id":null,"approval_id":denied.id,"decision":decision,"reason":"The Owner explicitly answered this request"}),
                };
                // Core may decide on any active account input, while restricted
                // caller authority must remain unable to grant permissions.
                let core_caller = system
                    .execution_context(&core, &appeal_input)
                    .unwrap()
                    .actor_call
                    .unwrap();
                let mut read_only = core_caller.clone();
                read_only.remote_read_only = true;
                assert!(
                    system
                        .resolve_appeal_from_core(
                            &read_only,
                            &denied.id,
                            ApprovalDecision::Approved,
                            "restricted Core decision"
                        )
                        .await
                        .is_err()
                );
                // This fixture keeps inference paused. Exercise the actual durable
                // delivery/publication boundary used by Core's live deliver tool.
                let question = system
                    .store()
                    .deliver(
                        "core",
                        &appeal_input.id,
                        "ask-owner",
                        "The worker requests write access to the output folder. Do you approve?",
                    )
                    .unwrap();
                system.store().publish_delivery(&question.id).unwrap();
                system
                    .store()
                    .finish_message(&appeal_input.id, MessageStatus::Completed, None, None)
                    .unwrap();
                assert!(
                    system
                        .resolve_appeal_from_core(
                            &core_caller,
                            &denied.id,
                            ApprovalDecision::Approved,
                            "stale Core decision"
                        )
                        .await
                        .is_err(),
                    "a finished Core input must not retain tool authority"
                );
                system
                    .send_user(
                        "core",
                        format!("I {decision} this output-folder request"),
                        vec![],
                        None,
                    )
                    .unwrap();
                let owner = system.store().claim_next("core").unwrap().unwrap();
                // A normal worker cannot resolve a request even with the right ID.
                assert!(
                    system
                        .resolve_appeal_from_core(
                            &worker_caller,
                            &denied.id,
                            ApprovalDecision::Approved,
                            "forged worker permission"
                        )
                        .await
                        .is_err()
                );
                let result = execute_control(&system, &core, &owner, &mut core_apps, &resolve)
                    .await
                    .unwrap();
                assert_eq!(result.payload["effect_executed"], false);
                assert!(
                    !target.exists(),
                    "Owner resolution must not execute the denied write"
                );
                let record = system.store().approval(&denied.id).unwrap();
                assert_eq!(
                    record.decision,
                    ApprovalDecision::Denied,
                    "preserve the original automatic decision"
                );
                let appeal = record.appeal.unwrap();
                assert_eq!(appeal.owner_message_id.as_deref(), Some(owner.id.as_str()));
                assert_eq!(
                    appeal.decision_message_id.as_deref(),
                    Some(owner.id.as_str())
                );
                let notice = system
                    .store()
                    .message(appeal.notification_id.as_deref().unwrap())
                    .unwrap();
                assert_eq!(notice.actor_id, actor.id);
                assert_eq!(notice.in_reply_to.as_deref(), Some(message.id.as_str()));
                assert_eq!(notice.status, MessageStatus::Queued);
                let resumed = system.store().claim_next(&actor.id).unwrap().unwrap();
                assert_eq!(resumed.id, notice.id);
                let repeated = execute_control(&system, &core, &owner, &mut core_apps, &resolve)
                    .await
                    .unwrap();
                assert_eq!(
                    repeated.payload["appeal"]["notification_id"],
                    json!(notice.id)
                );
                if decision == "approve" {
                    let mut retry = write.clone();
                    retry.id = "retry-after-owner-approval".into();
                    execute_control(&system, &actor, &resumed, &mut apps, &retry)
                        .await
                        .unwrap();
                    assert_eq!(std::fs::read_to_string(&target).unwrap(), "verified output");
                    let base = system.execution_context(&actor, &resumed).unwrap();
                    assert!(
                        base.sandbox_policy
                            .is_path_writable(&outside.join("child.txt"))
                    );
                    assert!(
                        !base
                            .sandbox_policy
                            .is_path_writable(&dir.path().join("sibling/result.txt"))
                    );
                } else {
                    assert!(
                        system
                            .store()
                            .approval_scopes(&actor.id)
                            .unwrap()
                            .is_empty()
                    );
                }
                assert_eq!(
                    reviewer.requests.lock().len(),
                    usize::from(mode == ApprovalMode::Auto),
                    "appeals, control tools and remembered calls must never re-enter automatic review"
                );
                system.shutdown().await;
            }
        }
    }

    #[tokio::test]
    async fn owner_reply_after_core_final_question_can_resolve_appeal() {
        assert_owner_appeal_resolution(true, false).await;
    }

    #[tokio::test]
    async fn owner_reply_can_resolve_appeal_during_a_later_worker_input() {
        assert_owner_appeal_resolution(false, true).await;
    }

    async fn assert_owner_appeal_resolution(final_question: bool, deferred_decision: bool) {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"deny","reason":"Confirm the requested output directory"}),
        )));
        let (dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let output = dir.path().join("project-output");
        std::fs::create_dir(&output).unwrap();
        let target = output.join("result.txt");
        let mut apps =
            crate::builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
        let write = AgentToolCall {
            id: "write-project-output".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":target,"content":"Owner-approved output","expected_sha256":null}),
        };
        let denied = execute_control(&system, &actor, &message, &mut apps, &write)
            .await
            .unwrap_err();
        let approval_id = rejection(&denied).unwrap().approval_id;
        let appeal = system
            .appeal_from_actor(
                &system
                    .execution_context(&actor, &message)
                    .unwrap()
                    .actor_call
                    .unwrap(),
                &approval_id,
                "The requested build output needs write access to this directory",
            )
            .unwrap();
        system
            .store()
            .finish_message(&message.id, MessageStatus::Completed, None, None)
            .unwrap();
        let core_input = system.store().claim_next("core").unwrap().unwrap();
        assert_eq!(core_input.id, appeal.appeal.unwrap().core_message_id);
        let question = "May the worker read and write the requested project output directory?";
        // Isolate final-answer publication from deferred resolution: the latter
        // uses the delivery path already covered by the existing happy-path test.
        let published = if final_question {
            system
                .store()
                .finish_message(
                    &core_input.id,
                    MessageStatus::Completed,
                    Some(question),
                    None,
                )
                .unwrap();
            system.store().message(&core_input.id).unwrap()
        } else {
            let delivery = system
                .store()
                .deliver("core", &core_input.id, "ask-owner", question)
                .unwrap();
            system.store().publish_delivery(&delivery.id).unwrap();
            system
                .store()
                .finish_message(&core_input.id, MessageStatus::Completed, None, None)
                .unwrap();
            system.store().message(&delivery.id).unwrap()
        };
        assert_eq!(published.reply.as_deref(), Some(question));
        assert_eq!(published.status, MessageStatus::Completed);
        system
            .send_user(
                "core",
                "Allow it to read and write that directory".into(),
                vec![],
                None,
            )
            .unwrap();
        let owner = system.store().claim_next("core").unwrap().unwrap();
        assert!(owner.seq > published.seq);
        let decision_input = if deferred_decision {
            system
                .store()
                .finish_message(
                    &owner.id,
                    MessageStatus::Completed,
                    Some("I will record the permission while checking the worker's update."),
                    None,
                )
                .unwrap();
            system
                .send_user(
                    &actor.id,
                    "Report the independent work completed while awaiting permission".into(),
                    vec![],
                    None,
                )
                .unwrap();
            let worker_input = system.store().claim_next(&actor.id).unwrap().unwrap();
            let update = system.send_from_actor(&actor.id, &worker_input.id, "core", "Independent verification finished; the output directory still needs the Owner-approved grant.".into()).unwrap();
            // Leave the worker idle so the eventual approval notification can
            // become its next input; no cancelled or replayed effect is involved.
            system
                .store()
                .finish_message(&worker_input.id, MessageStatus::Completed, None, None)
                .unwrap();
            let current = system.store().claim_next("core").unwrap().unwrap();
            assert_eq!(current.id, update.id);
            assert!(
                matches!(&current.source, super::super::MessageSource::Actor { actor_id } if actor_id == &actor.id)
            );
            current
        } else {
            owner.clone()
        };
        let core = system.actor("core").unwrap();
        let mut core_apps =
            crate::builtin_apps::create_apps(&core, system.clone(), &system.config()).unwrap();
        let result = execute_control(&system, &core, &decision_input, &mut core_apps, &AgentToolCall {
            id:"record-owner-approval".into(), name:"actors__resolve_approval_appeal".into(),
            arguments:json!({"stage_id":null,"approval_id":approval_id,"decision":"approve","reason":"The Owner explicitly approved after Core's question"}),
        }).await;
        if let Err(error) = &result {
            system.shutdown().await;
            panic!(
                "The Owner approved after Core's visible question; resolution must also work with final_question={final_question}, deferred_decision={deferred_decision}: {error}"
            );
        }
        assert_eq!(result.unwrap().payload["effect_executed"], false);
        assert!(
            !target.exists(),
            "recording approval must not replay the denied write"
        );
        let record = system.store().approval(&approval_id).unwrap();
        let appeal = record.appeal.unwrap();
        assert_eq!(appeal.decision, ApprovalDecision::Approved);
        assert_eq!(
            appeal.decision_message_id.as_deref(),
            Some(decision_input.id.as_str())
        );
        assert_eq!(
            appeal.owner_message_id.as_deref(),
            (!deferred_decision).then_some(owner.id.as_str()),
            "a worker-input decision must not be mislabeled as a direct Owner input"
        );
        system
            .store()
            .finish_message(&decision_input.id, MessageStatus::Completed, None, None)
            .unwrap();
        system
            .send_user(
                "core",
                "Continue coordinating the work".into(),
                vec![],
                None,
            )
            .unwrap();
        let later = system.store().claim_next("core").unwrap().unwrap();
        let repeated = execute_control(&system, &core, &later, &mut core_apps, &AgentToolCall {
            id:"confirm-recorded-approval".into(), name:"actors__resolve_approval_appeal".into(),
            arguments:json!({"stage_id":null,"approval_id":approval_id,"decision":"approve","reason":"Confirm the previously recorded permission"}),
        }).await.unwrap();
        assert_eq!(repeated.payload["effect_executed"], false);
        let unchanged = system
            .store()
            .approval(&approval_id)
            .unwrap()
            .appeal
            .unwrap();
        assert_eq!(unchanged.decision, appeal.decision);
        assert_eq!(unchanged.decision_message_id, appeal.decision_message_id);
        assert_eq!(unchanged.owner_message_id, appeal.owner_message_id);
        assert_eq!(unchanged.notification_id, appeal.notification_id);
        assert_eq!(
            system
                .store()
                .queued_messages(&actor.id, None, 10)
                .unwrap()
                .items
                .iter()
                .filter(|notice| notice.in_reply_to.as_deref() == Some(message.id.as_str()))
                .count(),
            1,
            "repeating a decision must not notify the worker twice"
        );
        let notice = system.store().claim_next(&actor.id).unwrap().unwrap();
        assert_eq!(Some(notice.id.as_str()), appeal.notification_id.as_deref());
        let retry = AgentToolCall {
            id: "retry-approved-write".into(),
            ..write
        };
        execute_control(&system, &actor, &notice, &mut apps, &retry)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "Owner-approved output"
        );
        assert_eq!(
            reviewer.requests.lock().len(),
            1,
            "Owner resolution and a granted retry must not run Auto again"
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn reviewer_request_failure_can_be_appealed_without_another_automatic_review() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let outside = dir.path().join("project");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("Cargo.toml"), "[workspace]\n").unwrap();
        let mut apps =
            crate::builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
        let list = AgentToolCall {
            id: "list-outside-project".into(),
            name: "files__list_directory".into(),
            arguments: json!({"path":outside,"cursor":null,"limit":null}),
        };
        let error = execute_control(&system, &actor, &message, &mut apps, &list)
            .await
            .unwrap_err();
        let rejection = rejection(&error).unwrap();
        assert!(
            system
                .store()
                .approval_scopes(&actor.id)
                .unwrap()
                .is_empty()
        );
        assert!(
            system
                .store()
                .pending_approvals(Some(&actor.id), None, 10)
                .unwrap()
                .items
                .is_empty(),
            "a failed Auto request must not silently start Ask"
        );
        let appeal = AgentToolCall {
            id: "appeal-reviewer-outage".into(),
            name: "actors__appeal_approval".into(),
            arguments: json!({"stage_id":null,"approval_id":rejection.approval_id,"tool_result_id":null,"readable_roots":null,"writable_roots":null,"explanation":"The review service could not be reached. Ask the Owner to authorize the project directory."}),
        };
        let submitted = execute_control(&system, &actor, &message, &mut apps, &appeal)
            .await
            .unwrap();
        let core_input = system.store().claim_next("core").unwrap().unwrap();
        assert_eq!(submitted.payload["core_message_id"], core_input.id);
        let facts: serde_json::Value = serde_json::from_str(&core_input.content).unwrap();
        assert_eq!(
            facts["denial_reason"], rejection.reason,
            "Core must receive the original failure reason"
        );
        assert_eq!(facts["proposed_call"]["arguments"], list.arguments);
        assert_eq!(
            reviewer.requests.lock().len(),
            1,
            "a previously reviewed denial goes to Owner, not Auto again"
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn file_read_grant_requires_separate_directory_review_and_then_covers_children() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"Read the requested manifest"}),
        )));
        let (dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let project = dir.path().join("outside-project");
        std::fs::create_dir(&project).unwrap();
        let manifest = project.join("Cargo.toml");
        std::fs::write(&manifest, "[workspace]\n").unwrap();
        std::fs::write(project.join("Cargo.lock"), "version = 4\n").unwrap();
        let mut apps =
            crate::builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
        let read = AgentToolCall {
            id: "read-only-manifest".into(),
            name: "files__read_file".into(),
            arguments: json!({"path":manifest,"start_line":null,"line_count":null}),
        };
        let result = execute_control(&system, &actor, &message, &mut apps, &read)
            .await
            .unwrap();
        assert_eq!(result.payload["content"], "[workspace]\n");
        let context = system.execution_context(&actor, &message).unwrap();
        assert!(context.sandbox_policy.is_path_readable(&manifest));
        assert!(
            !context.sandbox_policy.is_path_readable(&project),
            "a file grant must not broaden to its parent"
        );
        *reviewer.reply.lock() = Some(ReviewReply::Json(
            json!({"decision":"approve","reason":"List and inspect the requested project"}),
        ));
        let listing = execute_control(
            &system,
            &actor,
            &message,
            &mut apps,
            &AgentToolCall {
                id: "review-project-directory".into(),
                name: "files__list_directory".into(),
                arguments: json!({"path":project,"cursor":null,"limit":null}),
            },
        )
        .await
        .unwrap();
        let names: Vec<_> = listing.payload["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Cargo.lock", "Cargo.toml"]);
        assert_eq!(
            reviewer.requests.lock().len(),
            2,
            "listing the unapproved parent needs its own review"
        );
        let nested = project.join("new/child.txt");
        std::fs::create_dir(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "new child evidence").unwrap();
        let result = execute_control(
            &system,
            &actor,
            &message,
            &mut apps,
            &AgentToolCall {
                id: "read-new-child-with-remembered-grant".into(),
                name: "files__read_file".into(),
                arguments: json!({"path":nested,"start_line":null,"line_count":null}),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.payload["content"], "new child evidence");
        assert_eq!(
            reviewer.requests.lock().len(),
            2,
            "the directory grant recursively covers subsequent reads"
        );
        assert!(
            !system
                .execution_context(&actor, &message)
                .unwrap()
                .sandbox_policy
                .is_path_writable(&nested)
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn core_decision_reaches_source_without_moving_core_or_replicating_its_execution_context()
    {
        use crate::stage::{StageId, blob::ContentStore, shared_store};
        fn replicate(
            source: &super::super::store::ActorStore,
            target: &super::super::store::ActorStore,
            leader: StageId,
            follower: StageId,
        ) {
            let batch = shared_store::capture(source, "appeal-account", leader, 1)
                .unwrap()
                .unwrap();
            for change in &batch.changes {
                let bytes = ContentStore::new(source)
                    .verified_bytes(&change.content)
                    .unwrap();
                ContentStore::new(target)
                    .store_source("core", &bytes)
                    .unwrap();
            }
            shared_store::prepare(target, &batch, "appeal-account").unwrap();
            shared_store::commit(source, &batch, leader).unwrap();
            shared_store::commit(target, &batch, follower).unwrap();
        }
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"deny","reason":"Confirm the output location"}),
        )));
        let (dir, source, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let (_core_dir, coordinator, _, _) = fixture(
            ApprovalMode::Auto,
            Arc::new(Reviewer::new(ReviewReply::Fail)),
        )
        .await;
        let a = source.store().public_stage_id().unwrap();
        let b = coordinator.store().public_stage_id().unwrap();
        let outside = dir.path().join("external");
        std::fs::create_dir(&outside).unwrap();
        let base = source.execution_context(&actor, &message).unwrap();
        assert!(
            super::authorize(
                &source,
                &actor,
                &message,
                &call(),
                &ActorControl::default(),
                &base,
                &ToolPermissionRequest {
                    writable_roots: vec![outside.clone()],
                    ..Default::default()
                },
                None
            )
            .await
            .is_err()
        );
        let denied = source
            .store()
            .approvals(Some(&actor.id), None, 10)
            .unwrap()
            .items
            .pop()
            .unwrap();
        shared_store::initialize(source.store(), "appeal-account").unwrap();
        shared_store::initialize(coordinator.store(), "appeal-account").unwrap();
        let appeal = source
            .appeal_from_actor(
                base.actor_call.as_ref().unwrap(),
                &denied.id,
                "Ask the Owner about the output location",
            )
            .unwrap()
            .appeal
            .unwrap();
        replicate(source.store(), coordinator.store(), a, b);
        let input = coordinator.store().claim_next("core").unwrap().unwrap();
        assert_eq!(input.id, appeal.core_message_id);
        let request: serde_json::Value = serde_json::from_str(&input.content).unwrap();
        assert_eq!(request["stage_id"], json!(a));
        let question = coordinator
            .store()
            .deliver(
                "core",
                &input.id,
                "ask-owner",
                "May the worker write to the external output folder?",
            )
            .unwrap();
        coordinator
            .store()
            .finish_message(&input.id, MessageStatus::Completed, None, None)
            .unwrap();
        let batch = shared_store::capture(coordinator.store(), "appeal-account", b, 1)
            .unwrap()
            .unwrap();
        shared_store::commit(coordinator.store(), &batch, b).unwrap();
        coordinator.store().publish_delivery(&question.id).unwrap();
        coordinator
            .send_user("core", "Yes, allow that output folder".into(), vec![], None)
            .unwrap();
        let owner = coordinator.store().claim_next("core").unwrap().unwrap();
        let batch = shared_store::capture(coordinator.store(), "appeal-account", b, 1)
            .unwrap()
            .unwrap();
        shared_store::commit(coordinator.store(), &batch, b).unwrap();
        assert!(
            coordinator.store().approval(&denied.id).is_err(),
            "hosted approvals remain owned by their source device"
        );
        assert!(
            source.store().message(&owner.id).is_err(),
            "granting device must not need Core's Owner input"
        );
        assert_eq!(
            source.actor("core").unwrap().status,
            super::super::ActorStatus::Idle
        );
        let authorization = coordinator
            .prepare_appeal_authorization(
                coordinator
                    .execution_context(&coordinator.actor("core").unwrap(), &owner)
                    .unwrap()
                    .actor_call
                    .as_ref()
                    .unwrap(),
                &source.store().approval(&denied.id).unwrap(),
                ApprovalDecision::Approved,
                "Owner approved the output folder",
            )
            .unwrap();
        for changed_scope in [true, false] {
            let mut changed = authorization.clone();
            if changed_scope {
                changed.scopes_digest = "different-scope".into();
            } else {
                changed.core_message_id = "different-appeal".into();
            }
            assert!(source.apply_appeal_authorization(&changed).await.is_err());
        }
        assert!(
            source
                .store()
                .approval_scopes(&actor.id)
                .unwrap()
                .is_empty()
        );
        let record = source
            .apply_appeal_authorization(&authorization)
            .await
            .unwrap();
        let mut reversed = authorization.clone();
        reversed.decision = ApprovalDecision::Denied;
        assert!(source.apply_appeal_authorization(&reversed).await.is_err());
        let mut changed = authorization.clone();
        changed.scopes_digest = "different-scope".into();
        assert!(
            source.apply_appeal_authorization(&changed).await.is_err(),
            "idempotent decisions must still validate their original scope"
        );
        assert_eq!(
            source.actor("core").unwrap().status,
            super::super::ActorStatus::Idle
        );
        assert_eq!(
            coordinator.actor("core").unwrap().status,
            super::super::ActorStatus::Running
        );
        assert!(
            source
                .execution_context(&actor, &message)
                .unwrap()
                .sandbox_policy
                .is_path_writable(&outside.join("later.txt"))
        );
        assert!(
            coordinator
                .store()
                .approval_scopes(&actor.id)
                .unwrap()
                .is_empty()
        );
        let notice = source
            .store()
            .message(record.appeal.unwrap().notification_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(notice.actor_id, actor.id);
        assert_eq!(notice.status, MessageStatus::Queued);
        assert!(!outside.join("later.txt").exists());
        assert_eq!(reviewer.requests.lock().len(), 1);
        source.shutdown().await;
        coordinator.shutdown().await;
    }

    #[tokio::test]
    #[cfg(not(target_os = "android"))]
    async fn computer_desktop_grant_is_remembered_without_expanding_project_filesystem() {
        use crate::{app::App, computer_use_app::ComputerUseApp};
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(json!({
            "decision":"approve", "reason":"Requested desktop operation"
        }))));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let app = ComputerUseApp::new(actor.id.clone(), system.clone(), true);
        let base = system.execution_context(&actor, &message).unwrap();
        assert!(!base.sandbox_policy.filesystem.full_disk_read);
        let mut original_policy = base.sandbox_policy.clone();
        original_policy.filesystem.readable_roots.sort();
        original_policy.filesystem.readable_roots.dedup();
        let control = ActorControl::default();
        for (name, arguments) in [
            ("open_window", json!({"window_id":"discovered-window"})),
            (
                "type_text",
                json!({"window_id":"discovered-window","state_id":"image","text":"中文"}),
            ),
            (
                "give_window",
                json!({"window_id":"discovered-window","state_id":"new-image","placement":"center"}),
            ),
        ] {
            let call = AgentToolCall {
                id: Uuid::new_v4().to_string(),
                name: name.into(),
                arguments,
            };
            let requested = app.permission_request(&call, &base).unwrap();
            let review = app.review_requirement(&call, &base).unwrap();
            let effective = super::authorize(
                &system,
                &actor,
                &message,
                &call.clone(),
                &control,
                &base,
                &requested,
                review.as_ref(),
            )
            .await
            .unwrap();
            app.before_tool_call(&call, &effective).unwrap();
            assert_eq!(effective.sandbox_policy, original_policy);
        }
        assert_eq!(
            reviewer.requests.lock().len(),
            1,
            "coordinate/text changes must reuse the desktop grant"
        );
        system.shutdown().await;
    }
    async fn approval(system: &ActorSystem, message_id: &str) -> ApprovalRecord {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let actor = system.store().message(message_id).unwrap().actor_id;
        loop {
            for event in system.store().events(0, Some(&actor), false, 100).unwrap() {
                if let super::super::ActorEvent::ApprovalRequested {
                    approval_id,
                    message_id: id,
                    ..
                } = event.event
                    && id == message_id
                {
                    return system.store().approval(&approval_id).unwrap();
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "approval was not persisted"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    fn gate(
        system: &ActorSystem,
        actor: &ActorRecord,
        message: &MessageRecord,
        control: Arc<ActorControl>,
        call: AgentToolCall,
    ) -> tokio::task::JoinHandle<Result<()>> {
        let (system, actor, message) = (system.clone(), actor.clone(), message.clone());
        tokio::spawn(
            async move { authorize_simple(&system, &actor, &message, &call, &control).await },
        )
    }
    async fn authorize_simple(
        system: &ActorSystem,
        actor: &ActorRecord,
        message: &MessageRecord,
        call: &AgentToolCall,
        control: &ActorControl,
    ) -> Result<()> {
        let base = system.execution_context(actor, message)?;
        authorize_reviewed(
            system,
            actor,
            message,
            call,
            control,
            &base,
            &ToolPermissionRequest {
                operation_details: serde_json::Value::Null,
                readable_roots: vec![],
                writable_roots: vec![],
            },
        )
        .await
        .map(|_| ())
    }
    #[test]
    fn review_contract_is_strict_and_has_no_execution_tool_schemas() {
        let schema = model_schema_for::<ReviewVerdict>();
        crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert!(
            validate_value_against_schema(
                &json!({"decision":"approve","reason":"authorized"}),
                &schema,
                "verdict"
            )
            .is_ok()
        );
        assert!(
            validate_value_against_schema(
                &json!({"decision":"approve","reason":"authorized","tool":"exec"}),
                &schema,
                "verdict"
            )
            .is_err()
        );
        assert!(
            validate_value_against_schema(
                &json!({"decision":"override_grants","reason":"x"}),
                &schema,
                "verdict"
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn auto_uses_independent_structured_review_with_empty_history() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"User requested this workspace read"}),
        )));
        let (_dir, system, actor, original) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let workspace = actor.config.workspace.join("review-target");
        let target = system
            .create_owned(
                "root",
                super::super::ActorConfig {
                    name: "Review target".into(),
                    workspace: workspace.clone(),
                    apps: vec!["files".into()],
                    model: None,
                    instructions: String::new(),
                    permissions: super::super::ActorPermissions::workspace(workspace),
                },
            )
            .unwrap();
        system
            .store()
            .finish_message(
                &original.id,
                MessageStatus::Completed,
                Some("Delegated"),
                None,
            )
            .unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let response = system
            .store()
            .enqueue(
                MessageRecord {
                    seq: 0,
                    id: Uuid::new_v4().to_string(),
                    actor_id: actor.id.clone(),
                    source: super::super::MessageSource::Actor {
                        actor_id: target.id.clone(),
                    },
                    reply_to: super::super::ReplyTarget::User,
                    kind: super::super::MessageKind::Result,
                    in_reply_to: Some(original.id.clone()),
                    delegation_grant: None,
                    remote_authority: None,
                    content: "Untrusted worker result".into(),
                    attachments: vec![],
                    status: MessageStatus::Queued,
                    cancel_requested: false,
                    final_generation: None,
                    started_at_ms: None,
                    finished_at_ms: None,
                    reply: None,
                    error: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
                None,
            )
            .unwrap();
        let message = system.store().claim_next(&actor.id).unwrap().unwrap();
        assert_eq!(message.id, response.id);
        let proposed = AgentToolCall {
            id: "followup-call".into(),
            name: "actors__send".into(),
            arguments: json!({"actor_id":target.id,"content":"Continue the requested read"}),
        };
        authorize_simple(
            &system,
            &actor,
            &message,
            &proposed,
            &ActorControl::default(),
        )
        .await
        .unwrap();
        let record = approval(&system, &message.id).await;
        assert_eq!(record.decision, ApprovalDecision::Approved);
        {
            let requests = reviewer.requests.lock();
            assert_eq!(requests.len(), 1);
            assert!(
                requests[0].history_messages.is_empty() && requests[0].retry_messages.is_empty()
            );
            assert!(requests[0].current_user_message.contains("source.txt"));
            let facts: serde_json::Value =
                serde_json::from_str(&requests[0].current_user_message).unwrap();
            assert_eq!(facts["request"]["kind"], "result");
            assert_eq!(facts["request"]["in_reply_to"], original.id);
            assert_eq!(
                facts["operation_facts"]["originating_request"]["content"],
                original.content
            );
            assert_eq!(facts["operation_facts"]["recipient"]["id"], target.id);
            assert_eq!(
                facts["operation_facts"]["recipient"]["grants"]["writable_roots"],
                json!(target.config.permissions.writable_roots)
            );
            assert_eq!(facts["app_review"]["namespace"], "test/review/v1");
            assert_eq!(
                facts["app_review"],
                serde_json::to_value(record.review.as_ref().unwrap()).unwrap()
            );
        }
        system.shutdown().await;
    }

    #[tokio::test]
    async fn healthy_auto_review_can_finish_after_thirty_seconds() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Delayed(
            Duration::from_secs(31),
            json!({"decision":"approve","reason":"User requested this read"}),
        )));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let pending = gate(
            &system,
            &actor,
            &message,
            Arc::new(ActorControl::default()),
            call(),
        );
        reviewer.started.notified().await;
        pending.await.unwrap().unwrap();
        assert_eq!(
            approval(&system, &message.id).await.decision,
            ApprovalDecision::Approved
        );
        assert_eq!(reviewer.requests.lock().len(), 1);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn provider_timeout_still_denies_without_owner_fallback() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Timeout));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let error = authorize_simple(&system, &actor, &message, &call(), &ActorControl::default())
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<AutomaticReviewDenied>().is_some());
        let record = approval(&system, &message.id).await;
        assert_eq!(record.decision, ApprovalDecision::Denied);
        assert_eq!(record.reason, ReviewFailure::Timeout.reason());
        assert_eq!(reviewer.requests.lock().len(), 1);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn auto_denial_preserves_the_decision_and_approval_reference() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"deny","reason":"Operation does not match intent"}),
        )));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer).await;
        let error = authorize_simple(&system, &actor, &message, &call(), &ActorControl::default())
            .await
            .unwrap_err();
        let rejected = rejection(&error).unwrap();
        assert_eq!(rejected.reason, "Operation does not match intent");
        assert_eq!(
            rejected.approval_id,
            approval(&system, &message.id).await.id
        );
        assert_eq!(
            rejected.stage_id,
            system.store().public_stage_id().unwrap().to_string()
        );
        assert_eq!(
            approval(&system, &message.id).await.decision,
            ApprovalDecision::Denied
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn ask_requires_owner_resolution_without_calling_reviewer() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Ask, reviewer.clone()).await;
        let pending = gate(
            &system,
            &actor,
            &message,
            Arc::new(ActorControl::default()),
            call(),
        );
        let record = approval(&system, &message.id).await;
        assert!(!pending.is_finished());
        assert!(reviewer.requests.lock().is_empty());
        system
            .store()
            .resolve_approval(
                &record.id,
                ApprovalDecision::Approved,
                "Owner authorized this call",
            )
            .unwrap();
        pending.await.unwrap().unwrap();
        system.shutdown().await;
    }

    #[tokio::test]
    async fn failed_or_malformed_auto_review_denies_without_waiting_for_owner() {
        for reply in [
            ReviewReply::Fail,
            ReviewReply::Json(json!({"decision":"approve"})),
            ReviewReply::Json(json!({"decision":"ask","reason":"Old reviewer response"})),
        ] {
            let reviewer = Arc::new(Reviewer::new(reply));
            let (_dir, system, actor, message) =
                fixture(ApprovalMode::Auto, reviewer.clone()).await;
            let pending = gate(
                &system,
                &actor,
                &message,
                Arc::new(ActorControl::default()),
                call(),
            );
            tokio::time::timeout(Duration::from_secs(3), reviewer.started.notified())
                .await
                .unwrap();
            let error = tokio::time::timeout(Duration::from_secs(1), pending)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(error.downcast_ref::<AutomaticReviewDenied>().is_some());
            assert_eq!(
                approval(&system, &message.id).await.decision,
                ApprovalDecision::Denied
            );
            system.shutdown().await;
        }
    }

    #[tokio::test]
    async fn oversized_arguments_are_denied_without_partial_model_review() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let mut proposed = call();
        proposed.arguments = json!({"content":"x".repeat(REVIEW_ARGUMENT_BYTES + 1)});
        let control = Arc::new(ActorControl::default());
        let pending = gate(&system, &actor, &message, control.clone(), proposed);
        let error = tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.downcast_ref::<AutomaticReviewDenied>().is_some());
        assert_eq!(
            approval(&system, &message.id).await.decision,
            ApprovalDecision::Denied
        );
        assert!(reviewer.requests.lock().is_empty());
        system.shutdown().await;
    }

    #[tokio::test]
    async fn cancelling_review_drops_model_future_and_cancels_pending_record() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Pending));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let control = Arc::new(ActorControl::default());
        let pending = gate(&system, &actor, &message, control.clone(), call());
        tokio::time::timeout(Duration::from_secs(3), reviewer.started.notified())
            .await
            .unwrap();
        let record = approval(&system, &message.id).await;
        control.cancelled.lock().insert(message.id.clone());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), pending)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(reviewer.dropped.load(Ordering::Acquire));
        assert_eq!(
            system.store().approval(&record.id).unwrap().decision,
            ApprovalDecision::Cancelled
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn actor_configuration_changes_cancel_pending_owner_approval() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Ask, reviewer).await;
        let pending = gate(
            &system,
            &actor,
            &message,
            Arc::new(ActorControl::default()),
            call(),
        );
        let record = approval(&system, &message.id).await;
        let mut changed = actor.clone();
        changed.config.instructions = "New configuration".into();
        system.store().save_actor(&changed).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), pending)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(
            system.store().approval(&record.id).unwrap().decision,
            ApprovalDecision::Cancelled
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn account_full_access_applies_to_reused_and_new_executors_without_review() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (_dir, system, original, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        system
            .patch_owned_preferences("core", None, Some(ApprovalMode::None))
            .await
            .unwrap();
        let actor = system.execution_actor(&message).unwrap();
        assert_eq!(actor.config.permissions.approval_mode, ApprovalMode::None);
        let mut config = actor.config.clone();
        config.name = "New executor".into();
        let child = system.create_owned(&actor.id, config).unwrap();
        assert_eq!(child.config.permissions.approval_mode, ApprovalMode::None);
        let base = system.execution_context(&actor, &message).unwrap();
        authorize_reviewed(
            &system,
            &actor,
            &message,
            &call(),
            &ActorControl::default(),
            &base,
            &ToolPermissionRequest::default(),
        )
        .await
        .unwrap();
        assert!(reviewer.requests.lock().is_empty());
        assert!(
            system
                .store()
                .approvals(Some(&original.id), None, 10)
                .unwrap()
                .items
                .is_empty()
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn account_mode_changes_cancel_existing_executor_reviews() {
        for mode in [ApprovalMode::Auto, ApprovalMode::Ask] {
            for target in ["root", "core"] {
                let reviewer = Arc::new(Reviewer::new(ReviewReply::Pending));
                let (_dir, system, mut actor, mut message) = fixture(mode, reviewer.clone()).await;
                if target == "core" {
                    let queued = system
                        .send_user("core", "Review this operation".into(), vec![], None)
                        .unwrap();
                    message = system.store().claim_next("core").unwrap().unwrap();
                    assert_eq!(message.id, queued.id);
                    actor = system.core_device_actor().unwrap();
                }
                let pending = gate(
                    &system,
                    &actor,
                    &message,
                    Arc::new(ActorControl::default()),
                    call(),
                );
                let record = approval(&system, &message.id).await;
                if mode == ApprovalMode::Auto {
                    tokio::time::timeout(Duration::from_secs(2), reviewer.started.notified())
                        .await
                        .unwrap();
                }
                system
                    .patch_owned_preferences("core", None, Some(ApprovalMode::None))
                    .await
                    .unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_secs(2), pending)
                        .await
                        .expect("account mode change left the old review waiting")
                        .unwrap()
                        .is_err()
                );
                assert_eq!(
                    system.store().approval(&record.id).unwrap().decision,
                    ApprovalDecision::Cancelled
                );
                if mode == ApprovalMode::Auto {
                    assert!(reviewer.dropped.load(Ordering::Acquire));
                }
                system.shutdown().await;
            }
        }
    }

    #[tokio::test]
    async fn account_tightening_reviews_an_existing_full_access_executor() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (_dir, system, _, message) = fixture(ApprovalMode::None, reviewer.clone()).await;
        system
            .patch_owned_preferences("core", None, Some(ApprovalMode::Auto))
            .await
            .unwrap();
        let actor = system.execution_actor(&message).unwrap();
        let error = gate(
            &system,
            &actor,
            &message,
            Arc::new(ActorControl::default()),
            call(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.downcast_ref::<AutomaticReviewDenied>().is_some());
        assert_eq!(reviewer.requests.lock().len(), 1);
        assert_eq!(
            approval(&system, &message.id).await.decision,
            ApprovalDecision::Denied
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn auto_and_none_grant_only_requested_scope_without_rewriting_actor_configuration() {
        for mode in [ApprovalMode::Auto, ApprovalMode::None] {
            let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
                json!({"decision":"approve","reason":"User requested this specific path"}),
            )));
            let (dir, system, actor, message) = fixture(mode, reviewer.clone()).await;
            let outside = dir.path().join("outside.txt");
            std::fs::write(&outside, "requested data").unwrap();
            let base = system.execution_context(&actor, &message).unwrap();
            assert!(!base.sandbox_policy.is_path_readable(&outside));
            let requested = ToolPermissionRequest {
                operation_details: serde_json::Value::Null,
                readable_roots: vec![outside.clone()],
                writable_roots: vec![],
            };
            let effective = authorize_reviewed(
                &system,
                &actor,
                &message,
                &call(),
                &ActorControl::default(),
                &base,
                &requested,
            )
            .await
            .unwrap();
            assert!(effective.sandbox_policy.is_path_readable(&outside));
            assert!(
                !effective
                    .sandbox_policy
                    .is_path_readable(&dir.path().join("unrequested.txt"))
            );
            assert!(!base.sandbox_policy.is_path_readable(&outside));
            assert_eq!(
                system.store().actor("root").unwrap().config.permissions,
                actor.config.permissions
            );
            assert_eq!(
                reviewer.requests.lock().len(),
                usize::from(mode == ApprovalMode::Auto)
            );
            system.shutdown().await;
        }
    }

    #[tokio::test]
    async fn terminal_read_admission_remembers_effect_approval_without_granting_files_access() {
        use crate::app::App;
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"Read the requested signing metadata"}),
        )));
        let (dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let signing = system
            .account_credentials_dir()
            .parent()
            .unwrap()
            .to_path_buf();
        let properties = signing.join("release.properties");
        std::fs::write(&properties, "keyAlias=fixture\n").unwrap();
        let app = crate::terminal_app::TerminalApp::new();
        let call = AgentToolCall {
            id: "read-signing-metadata".into(),
            name: "terminal_exec".into(),
            arguments: json!({
                "command":format!("cat '{}'", properties.display()),
                "session_id":null,"workdir":null,
                "additional_read_roots":[signing],"additional_write_roots":null,
                "yield_time_ms":null,"max_chars":null
            }),
        };
        let base = system.execution_context(&actor, &message).unwrap();
        let read = crate::file_io::ReadFileArgs {
            path: properties.display().to_string(),
            start_line: None,
            line_count: None,
        };
        assert!(
            crate::file_io::read_file(&base.execution_cwd, &base.sandbox_policy, &read).is_err()
        );
        for _ in 0..2 {
            let base = system.execution_context(&actor, &message).unwrap();
            let requested = app.permission_request(&call, &base).unwrap();
            assert!(requested.readable_roots.is_empty());
            let review = app.review_requirement(&call, &base).unwrap();
            let effective = super::authorize(
                &system,
                &actor,
                &message,
                &call.clone(),
                &ActorControl::default(),
                &base,
                &requested,
                review.as_ref(),
            )
            .await
            .unwrap();
            // Validate Terminal's actual admission path without requiring a
            // platform-native process sandbox in this isolated approval test.
            app.before_tool_call(&call, &effective).unwrap();
            assert!(
                crate::file_io::read_file(
                    &effective.execution_cwd,
                    &effective.sandbox_policy,
                    &read,
                )
                .is_err()
            );
            assert!(!effective.sandbox_policy.is_path_writable(&properties));
            assert!(
                !effective
                    .sandbox_policy
                    .is_path_readable(&dir.path().join("unrequested.txt"))
            );
        }
        assert_eq!(reviewer.requests.lock().len(), 1);
        assert_eq!(
            approval(&system, &message.id).await.decision,
            ApprovalDecision::Approved
        );
        system.shutdown().await;
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    fn scoped_terminal_call(
        read_root: &std::path::Path,
        write_root: &std::path::Path,
        unrequested: &std::path::Path,
    ) -> AgentToolCall {
        let input = read_root.join("source.txt");
        let output = write_root.join("result.txt");
        let quote = |path: &std::path::Path| {
            let value = path.to_str().unwrap();
            if cfg!(windows) {
                format!("'{}'", value.replace('\'', "''"))
            } else {
                format!("'{}'", value.replace('\'', "'\"'\"'"))
            }
        };
        let command = if cfg!(windows) {
            format!(
                "$ErrorActionPreference='Stop'; [IO.File]::WriteAllText({}, [IO.File]::ReadAllText({})); $allowed=$false; try {{ [IO.File]::WriteAllText({}, 'unexpected'); $allowed=$true }} catch {{}}; if ($allowed) {{ throw 'unrequested write succeeded' }}; Write-Output 'scope-ok'",
                quote(&output),
                quote(&input),
                quote(unrequested)
            )
        } else {
            format!(
                "set -eu; cat {} > {}; if printf unexpected > {}; then exit 17; fi; printf 'scope-ok\\n'",
                quote(&input),
                quote(&output),
                quote(unrequested)
            )
        };
        AgentToolCall {
            id: "scoped-terminal".into(),
            name: "terminal_exec".into(),
            arguments: json!({
                "command":command,"session_id":null,"workdir":write_root,
                "additional_read_roots":[read_root],"additional_write_roots":[write_root],
                "yield_time_ms":1000,"max_chars":2000
            }),
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn approved_terminal_scope_runs_natively_only_after_explicit_retry() {
        if !crate::sandbox::test_support::native_sandbox_available() {
            return;
        }
        use crate::app::App;
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"Use the requested fixture caches"}),
        )));
        let (dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let reads = dir.path().join("read-cache");
        let writes = dir.path().join("write-cache");
        let unrelated = dir.path().join("unrequested");
        for path in [&reads, &writes, &unrelated] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(reads.join("source.txt"), "approved cache data").unwrap();
        let outside = unrelated.join("forbidden.txt");
        let call = scoped_terminal_call(&reads, &writes, &outside);
        let mut app = crate::terminal_app::TerminalApp::new();
        let base = system.execution_context(&actor, &message).unwrap();
        assert!(!base.sandbox_policy.is_path_readable(&reads));
        assert!(!base.sandbox_policy.is_path_writable(&writes));
        assert!(app.before_tool_call(&call, &base).is_err());
        let requested = app.permission_request(&call, &base).unwrap();
        let review = app.review_requirement(&call, &base).unwrap();
        let effective = super::authorize(
            &system,
            &actor,
            &message,
            &call,
            &ActorControl::default(),
            &base,
            &requested,
            review.as_ref(),
        )
        .await
        .unwrap();
        assert!(
            !writes.join("result.txt").exists(),
            "approval must not execute the proposed command"
        );
        assert!(!effective.sandbox_policy.is_path_writable(&outside));
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let mut result = app.execute_tool(&call, &effective).await.unwrap();
            while result.payload["running"].as_bool() == Some(true) {
                let poll = AgentToolCall {
                    id: "poll-scoped-terminal".into(),
                    name: "terminal_write_stdin".into(),
                    arguments: json!({
                        "session_id":result.payload["session"]["session_id"],
                        "text":"","wait_mode":"timeout","yield_time_ms":1000,"max_chars":2000
                    }),
                };
                result = app.execute_tool(&poll, &effective).await.unwrap();
            }
            result
        })
        .await
        .expect("approved native command did not finish");
        assert_eq!(
            result.payload["session"]["exit_code"],
            json!(0),
            "{}",
            result.payload
        );
        assert_eq!(
            std::fs::read_to_string(writes.join("result.txt")).unwrap(),
            "approved cache data"
        );
        assert!(
            !outside.exists(),
            "native process escaped the approved scope"
        );
        assert_eq!(reviewer.requests.lock().len(), 1);
        assert_eq!(
            system.store().actor("root").unwrap().config.permissions,
            actor.config.permissions
        );
        app.shutdown().await.unwrap();
        system.shutdown().await;
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn rejected_or_unavailable_scope_review_keeps_terminal_admission_closed() {
        use crate::app::App;
        for reply in [
            ReviewReply::Json(json!({"decision":"deny","reason":"Not authorized"})),
            ReviewReply::Fail,
        ] {
            let reviewer = Arc::new(Reviewer::new(reply));
            let (dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
            let outside = dir.path().join("cache");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("source.txt"), "scope fixture").unwrap();
            let call = scoped_terminal_call(&outside, &outside, &dir.path().join("unrequested"));
            let mut app = crate::terminal_app::TerminalApp::new();
            let base = system.execution_context(&actor, &message).unwrap();
            let requested = app.permission_request(&call, &base).unwrap();
            let review = app.review_requirement(&call, &base).unwrap();
            let error = super::authorize(
                &system,
                &actor,
                &message,
                &call,
                &ActorControl::default(),
                &base,
                &requested,
                review.as_ref(),
            )
            .await
            .err()
            .expect("review must deny the requested scope");
            assert!(rejection(&error).is_some());
            let fresh = system.execution_context(&actor, &message).unwrap();
            assert!(!fresh.sandbox_policy.is_path_readable(&outside));
            assert!(!fresh.sandbox_policy.is_path_writable(&outside));
            assert!(app.execute_tool(&call, &fresh).await.is_err());
            assert!(!outside.join("result.txt").exists());
            assert_eq!(reviewer.requests.lock().len(), 1);
            system.shutdown().await;
        }
    }

    #[tokio::test]
    async fn configured_oauth_fixture_accepts_normal_scoped_approval() {
        use crate::app::App;
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"Read the synthetic fixture explicitly requested"}),
        )));
        let (dir, system, actor, message) =
            fixture_with_config(ApprovalMode::Auto, reviewer.clone(), |config, root| {
                config.providers.insert(
                    "fixture-oauth".into(),
                    ProviderConfig::OpenaiCodexOauth {
                        base_url: None,
                        auth_file: root.join(".daat-locus/oauth.json").display().to_string(),
                    },
                );
            })
            .await;
        // This is inert fixture content, never a credential or provider request.
        let path = dir.path().join(".daat-locus/oauth.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "synthetic fixture, not credentials").unwrap();
        let mut app = crate::files_app::FilesApp::new(dir.path().join("images"), false);
        let call = AgentToolCall {
            id: "read-configured-path-fixture".into(),
            name: "read_file".into(),
            arguments: json!({"path":path,"start_line":null,"line_count":null}),
        };
        let base = system.execution_context(&actor, &message).unwrap();
        assert!(app.execute_tool(&call, &base).await.is_err());
        for _ in 0..2 {
            let base = system.execution_context(&actor, &message).unwrap();
            let requested = app.permission_request(&call, &base).unwrap();
            let effective = super::authorize(
                &system,
                &actor,
                &message,
                &call,
                &ActorControl::default(),
                &base,
                &requested,
                None,
            )
            .await
            .unwrap();
            let result = app.execute_tool(&call, &effective).await.unwrap();
            assert_eq!(
                result.payload["content"],
                "synthetic fixture, not credentials"
            );
            assert!(!effective.sandbox_policy.is_path_writable(&path));
            assert!(
                !effective
                    .sandbox_policy
                    .is_path_readable(&path.with_file_name("unrequested"))
            );
            assert!(!effective.sandbox_policy.filesystem.full_disk_read);
            assert!(!effective.sandbox_policy.filesystem.full_disk_write);
        }
        assert_eq!(reviewer.requests.lock().len(), 1);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn changed_builtin_can_request_and_receive_an_ordinary_read_grant() {
        use crate::app::App;
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"Read the changed fixture as an ordinary file"}),
        )));
        let (dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let path = dir.path().join("SKILL.md");
        std::fs::write(&path, "modified fixture").unwrap();
        let mut base = system.execution_context(&actor, &message).unwrap();
        base.sandbox_policy.builtin_resources = vec![crate::builtin_resources::BuiltinResource {
            path: path.clone(),
            content: b"original compiled fixture",
        }];
        let mut app = crate::files_app::FilesApp::new(dir.path().join("images"), false);
        let call = AgentToolCall {
            id: "read-modified-builtin-fixture".into(),
            name: "read_file".into(),
            arguments: json!({"path":path,"start_line":null,"line_count":null}),
        };
        assert!(app.execute_tool(&call, &base).await.is_err());
        let requested = app.permission_request(&call, &base).unwrap();
        assert_eq!(requested.readable_roots, vec![path.clone()]);
        let effective = super::authorize(
            &system,
            &actor,
            &message,
            &call,
            &ActorControl::default(),
            &base,
            &requested,
            None,
        )
        .await
        .unwrap();
        let result = app.execute_tool(&call, &effective).await.unwrap();
        assert_eq!(result.payload["content"], "modified fixture");
        assert!(
            crate::file_io::read_builtin_bytes(
                &effective.sandbox_policy.builtin_resources[0],
                1024
            )
            .is_err()
        );
        assert!(!effective.sandbox_policy.is_path_writable(&path));
        assert_eq!(reviewer.requests.lock().len(), 1);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn ordinary_project_reads_need_no_review_in_ask_or_auto_mode() {
        for mode in [ApprovalMode::Ask, ApprovalMode::Auto] {
            let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
            let (_dir, system, actor, message) = fixture(mode, reviewer.clone()).await;
            let nested = actor.config.workspace.join("deep/project/source.txt");
            std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
            std::fs::write(&nested, "nested evidence").unwrap();
            let mut proposed = call();
            proposed.arguments["path"] = json!(nested);
            let mut apps =
                crate::builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
            let base = system.execution_context(&actor, &message).unwrap();
            let requested = apps.permission_request(&proposed, &base).unwrap();
            let review = apps.review_requirement(&proposed, &base).unwrap();
            let context = super::authorize(
                &system,
                &actor,
                &message,
                &proposed,
                &ActorControl::default(),
                &base,
                &requested,
                review.as_ref(),
            )
            .await
            .unwrap();
            let result = apps.execute_tool(&proposed, &context).await.unwrap();
            assert_eq!(result.payload["content"], "nested evidence");
            assert!(
                system
                    .store()
                    .approvals(Some(&actor.id), None, 10)
                    .unwrap()
                    .items
                    .is_empty()
            );
            assert!(reviewer.requests.lock().is_empty());
            system.shutdown().await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_directory_retargeted_during_review_cannot_rebind_the_decision_or_its_memory() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Delayed(
            Duration::from_millis(100),
            json!({"decision":"approve","reason":"requested directory"}),
        )));
        let (dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let outside = dir.path().join("approved");
        let secret = dir.path().join("unapproved");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join("data.txt"), "unapproved data").unwrap();
        let base = system.execution_context(&actor, &message).unwrap();
        let requested = ToolPermissionRequest {
            readable_roots: vec![outside.clone()],
            ..Default::default()
        };
        let pending = {
            let (system, actor, message) = (system.clone(), actor.clone(), message.clone());
            tokio::spawn(async move {
                super::authorize(
                    &system,
                    &actor,
                    &message,
                    &call(),
                    &ActorControl::default(),
                    &base,
                    &requested,
                    None,
                )
                .await
            })
        };
        tokio::time::timeout(Duration::from_secs(3), reviewer.started.notified())
            .await
            .unwrap();
        std::fs::rename(&outside, dir.path().join("original-directory")).unwrap();
        std::os::unix::fs::symlink(&secret, &outside).unwrap();
        assert!(pending.await.unwrap().is_err());
        let context = system.execution_context(&actor, &message).unwrap();
        assert!(
            !context
                .sandbox_policy
                .is_path_readable(&outside.join("data.txt"))
        );
        assert!(
            !context
                .sandbox_policy
                .is_path_readable(&secret.join("data.txt"))
        );
        let mut proposed = call();
        proposed.arguments["path"] = json!(outside.join("data.txt"));
        let mut apps =
            crate::builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
        assert!(apps.execute_tool(&proposed, &context).await.is_err());
        assert_eq!(reviewer.requests.lock().len(), 1);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn owner_directory_approval_restores_recursive_access_after_reopen_without_another_review()
     {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (dir, system, actor, message) = fixture(ApprovalMode::Ask, reviewer.clone()).await;
        let outside = dir.path().join("approved");
        let nested = outside.join("new/deep/source.txt");
        std::fs::create_dir(&outside).unwrap();
        let requested = ToolPermissionRequest {
            readable_roots: vec![outside.clone()],
            ..Default::default()
        };
        let base = system.execution_context(&actor, &message).unwrap();
        let pending = {
            let (system, actor, message) = (system.clone(), actor.clone(), message.clone());
            tokio::spawn(async move {
                super::authorize(
                    &system,
                    &actor,
                    &message,
                    &call(),
                    &ActorControl::default(),
                    &base,
                    &requested,
                    None,
                )
                .await
            })
        };
        let record = approval(&system, &message.id).await;
        assert!(
            record.review.is_none(),
            "sandbox review must not depend on an App policy"
        );
        system
            .store()
            .resolve_approval(
                &record.id,
                ApprovalDecision::Approved,
                "Read the requested folder",
            )
            .unwrap();
        assert!(
            pending
                .await
                .unwrap()
                .unwrap()
                .sandbox_policy
                .is_path_readable(&nested)
        );
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "durable directory evidence").unwrap();
        let config = system.config();
        system.shutdown().await;
        let reopened = ActorSystem::open_paused_with_factory(
            dir.path().join("data"),
            dir.path().join("workspace"),
            config,
            Arc::new(Factory(reviewer.clone())),
        )
        .await
        .unwrap();
        let input = reopened
            .send_user(
                &actor.id,
                "Read the newly created child file".into(),
                vec![],
                None,
            )
            .unwrap();
        let active = reopened.store().claim_next(&actor.id).unwrap().unwrap();
        assert_eq!(active.id, input.id);
        let actor = reopened.actor(&actor.id).unwrap();
        let base = reopened.execution_context(&actor, &active).unwrap();
        assert!(base.sandbox_policy.is_path_readable(&nested));
        assert!(!base.sandbox_policy.is_path_writable(&nested));
        assert!(
            !base
                .sandbox_policy
                .is_path_readable(&dir.path().join("approved-sibling/private.txt"))
        );
        let mut apps =
            crate::builtin_apps::create_apps(&actor, reopened.clone(), &reopened.config()).unwrap();
        let mut proposed = call();
        proposed.arguments["path"] = json!(nested);
        let requested = apps.permission_request(&proposed, &base).unwrap();
        let context = super::authorize(
            &reopened,
            &actor,
            &active,
            &proposed,
            &ActorControl::default(),
            &base,
            &requested,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            apps.execute_tool(&proposed, &context)
                .await
                .unwrap()
                .payload["content"],
            "durable directory evidence"
        );
        assert_eq!(
            reopened
                .store()
                .approvals(Some(&actor.id), None, 10)
                .unwrap()
                .items
                .len(),
            1
        );
        assert!(reviewer.requests.lock().is_empty());
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn remembered_app_actions_skip_review_until_arguments_or_policy_change() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"requested action"}),
        )));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Auto, reviewer.clone()).await;
        authorize_simple(&system, &actor, &message, &call(), &ActorControl::default())
            .await
            .unwrap();
        let mut retry = call();
        retry.id = "another-protocol-call".into();
        authorize_simple(&system, &actor, &message, &retry, &ActorControl::default())
            .await
            .unwrap();
        assert_eq!(reviewer.requests.lock().len(), 1);
        *reviewer.reply.lock() = Some(ReviewReply::Json(
            json!({"decision":"deny","reason":"different target"}),
        ));
        retry.arguments["path"] = json!("other.txt");
        let error = authorize_simple(&system, &actor, &message, &retry, &ActorControl::default())
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<AutomaticReviewDenied>().is_some());
        assert_eq!(reviewer.requests.lock().len(), 2);
        *reviewer.reply.lock() = Some(ReviewReply::Json(
            json!({"decision":"deny","reason":"changed policy"}),
        ));
        let review = AppReview::new(
            "test/review/v1",
            "A revised review policy.",
            json!({"tool":call().name,"arguments":call().arguments}),
        );
        let base = system.execution_context(&actor, &message).unwrap();
        assert!(
            super::authorize(
                &system,
                &actor,
                &message,
                &call(),
                &ActorControl::default(),
                &base,
                &ToolPermissionRequest::default(),
                Some(&review)
            )
            .await
            .is_err()
        );
        assert_eq!(reviewer.requests.lock().len(), 3);
        assert_eq!(system.store().approval_scopes(&actor.id).unwrap().len(), 1);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn ask_owner_denial_has_a_distinct_permission_decision() {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Fail));
        let (_dir, system, actor, message) = fixture(ApprovalMode::Ask, reviewer).await;
        let pending = gate(
            &system,
            &actor,
            &message,
            Arc::new(ActorControl::default()),
            call(),
        );
        let record = approval(&system, &message.id).await;
        system
            .store()
            .resolve_approval(
                &record.id,
                ApprovalDecision::Denied,
                "Do not run this operation",
            )
            .unwrap();
        assert!(
            pending
                .await
                .unwrap()
                .unwrap_err()
                .downcast_ref::<ApprovalDenied>()
                .is_some()
        );
        system.shutdown().await;
    }
    include!("runtime_permission_tests.rs");
}
