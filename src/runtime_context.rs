// Runtime context request construction and runtime history reset support.
use crate::{
    context::Context,
    context_budget::{RequestBudgetLimits, approx_token_count},
    daat_locus_paths::daat_locus_paths,
    memory::{
        RuntimeHistoryResetOutcome, RuntimeHistoryResetPhase, RuntimeHistoryResetPlan,
        RuntimeHistoryResetReason, RuntimeHistoryResetRecord,
        RuntimeHistoryResetReinjectionStrategy, RuntimeRequestEnvelope, RuntimeStepConversation,
        RuntimeStepResetPolicy,
    },
    persistence::append_bytes_durable,
    preturn_state::PreTurnState,
    reasoning::{
        prompt_assembler::AfterClaimContextAssembler,
        prompt_parts::AfterClaimContextInput,
        prompt_renderer::LlmPromptRenderer,
        runtime::{AgentMessage, AgentToolSpec, HistoryMessage},
    },
};
use chrono::Utc;
use miette::{Result, miette};
use serde::Serialize;
use std::sync::OnceLock;
use tracing::error;

pub const MID_TURN_RESET_MAX_RECOVERIES: usize = 3;
/// Message injected as the only history item after the runtime history is reset.
/// The model is expected to recover task state from the shared session history with
/// the `read_history` tool before continuing work.
pub const RUNTIME_HISTORY_RESET_PROMPT_MESSAGE: &str = "上下文已超出，更早的消息历史可使用 read_history 工具了解。请先调用它恢复任务状态，再继续当前工作。";
const RUNTIME_HISTORY_RESET_EVENT_FILE_NAME: &str = "runtime_history_reset_events.jsonl";
static RUNTIME_HISTORY_RESET_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Serialize)]
struct RuntimeHistoryResetTelemetryEvent {
    timestamp_ms: i64,
    phase: RuntimeHistoryResetPhase,
    reason: RuntimeHistoryResetReason,
    reinjection_strategy: RuntimeHistoryResetReinjectionStrategy,
    status: &'static str,
    source_item_count: usize,
    source_message_count: usize,
    trimmed_item_count: usize,
    retained_user_message_count: usize,
    before_tokens: usize,
    after_tokens: usize,
    recovery_prompt_tokens: usize,
    error: Option<String>,
}

pub fn build_runtime_request_envelope(context: &Context) -> RuntimeRequestEnvelope {
    RuntimeRequestEnvelope::from_system_messages(vec![context.runtime_system_prompt_text()])
}

pub fn build_preturn_context_text(context: &mut Context, state: &PreTurnState) -> String {
    LlmPromptRenderer::render_document_with_root(
        &context.preturn_context_doc(state),
        Some("preturn_context"),
    )
}

pub fn build_afterclaim_context_text(context: &Context, input: &AfterClaimContextInput) -> String {
    LlmPromptRenderer::render_document_with_root(
        &AfterClaimContextAssembler::default_runtime().assemble(context, input),
        Some("afterclaim_context"),
    )
}

pub fn runtime_request_budget_limits(context: &Context) -> RequestBudgetLimits {
    context.model_provider.request_budget_limits()
}

pub async fn execute_pre_turn_history_reset(
    plan: &RuntimeHistoryResetPlan,
) -> Result<RuntimeHistoryResetOutcome> {
    execute_runtime_history_reset(RuntimeHistoryResetRequest {
        source_messages: plan.source_messages(),
        retained_user_message_count: 0,
        phase: RuntimeHistoryResetPhase::PreTurn,
        reason: RuntimeHistoryResetReason::BudgetThreshold,
        reinjection_strategy: RuntimeHistoryResetReinjectionStrategy::RebuildRuntimeEnvelope,
    })
    .await
}

pub async fn maybe_reset_runtime_history(
    context: &Context,
    runtime_step: &mut RuntimeStepConversation,
    tools: &[AgentToolSpec],
    reset_for_overflow: bool,
) -> Result<bool> {
    maybe_reset_agent_history(
        context.model_provider.as_ref(),
        runtime_step,
        tools,
        &context.token_estimate_baseline,
        reset_for_overflow,
    )
    .await
}

pub async fn maybe_reset_agent_history(
    provider: &(dyn crate::core::ModelProvider + Send + Sync),
    conversation: &mut RuntimeStepConversation,
    tools: &[AgentToolSpec],
    baseline: &crate::context_budget::TokenEstimateBaseline,
    reset_for_overflow: bool,
) -> Result<bool> {
    let reset_result = conversation
        .maybe_reset(
            tools,
            provider.request_budget_limits(),
            baseline,
            reset_for_overflow,
            runtime_step_reset_policy(),
            |messages| async move {
                match build_mid_turn_reset_outcome(&messages, reset_for_overflow).await {
                    Ok(outcome) => Ok(outcome),
                    Err(err) => Err(err.to_string()),
                }
            },
        )
        .await;
    match reset_result {
        Ok(reset) => Ok(reset),
        Err(err) => Err(miette!("runtime history reset failed: {err}")),
    }
}

const fn runtime_step_reset_policy() -> RuntimeStepResetPolicy {
    RuntimeStepResetPolicy {
        max_recoveries: MID_TURN_RESET_MAX_RECOVERIES,
    }
}

fn history_message_token_cost(message: &HistoryMessage) -> usize {
    let role = message.role_name();
    approx_token_count(role) + approx_token_count(message.text_content().unwrap_or_default()) + 4
}

fn history_messages_total_token_cost(messages: &[HistoryMessage]) -> usize {
    messages.iter().map(history_message_token_cost).sum()
}

struct RuntimeHistoryResetRequest<'a> {
    source_messages: &'a [HistoryMessage],
    retained_user_message_count: usize,
    phase: RuntimeHistoryResetPhase,
    reason: RuntimeHistoryResetReason,
    reinjection_strategy: RuntimeHistoryResetReinjectionStrategy,
}

async fn execute_runtime_history_reset(
    request: RuntimeHistoryResetRequest<'_>,
) -> Result<RuntimeHistoryResetOutcome> {
    let RuntimeHistoryResetRequest {
        source_messages,
        retained_user_message_count,
        phase,
        reason,
        reinjection_strategy,
    } = request;
    if source_messages.is_empty() {
        return Err(miette!("runtime history reset has no messages to clear"));
    }
    let before_tokens = history_messages_total_token_cost(source_messages);
    let recovery_prompt = RUNTIME_HISTORY_RESET_PROMPT_MESSAGE.to_string();
    let record = RuntimeHistoryResetRecord {
        timestamp_ms: Utc::now().timestamp_millis(),
        phase,
        reason,
        reinjection_strategy,
        source_item_count: source_messages.len(),
        source_message_count: source_messages.len(),
        trimmed_item_count: 0,
        retained_user_message_count,
        recovery_prompt: recovery_prompt.clone(),
    };
    let after_tokens = retained_user_message_count
        .saturating_add(1)
        .saturating_mul(4)
        .saturating_add(approx_token_count(&recovery_prompt));
    append_runtime_history_reset_event(RuntimeHistoryResetTelemetryEvent {
        timestamp_ms: Utc::now().timestamp_millis(),
        phase,
        reason,
        reinjection_strategy,
        status: "completed",
        source_item_count: source_messages.len(),
        source_message_count: source_messages.len(),
        trimmed_item_count: 0,
        retained_user_message_count,
        before_tokens,
        after_tokens,
        recovery_prompt_tokens: approx_token_count(&recovery_prompt),
        error: None,
    })
    .await;
    Ok(RuntimeHistoryResetOutcome {
        recovery_prompt,
        record,
    })
}

fn agent_message_to_history_message(message: &AgentMessage) -> HistoryMessage {
    HistoryMessage {
        message: message.clone(),
        activity_event: None,
        tool_call_activity_events: Vec::new(),
    }
}

async fn build_mid_turn_reset_outcome(
    messages: &[AgentMessage],
    reset_for_overflow: bool,
) -> Result<RuntimeHistoryResetOutcome> {
    let reset_messages = messages
        .iter()
        .map(agent_message_to_history_message)
        .collect::<Vec<_>>();
    if reset_messages.is_empty() {
        return Err(miette!(
            "runtime history reset has no mid-turn messages to clear"
        ));
    }
    let reason = if reset_for_overflow {
        RuntimeHistoryResetReason::OverflowRecovery
    } else {
        RuntimeHistoryResetReason::BudgetThreshold
    };
    execute_runtime_history_reset(RuntimeHistoryResetRequest {
        source_messages: &reset_messages,
        retained_user_message_count: 0,
        phase: RuntimeHistoryResetPhase::MidTurn,
        reason,
        reinjection_strategy: RuntimeHistoryResetReinjectionStrategy::PreserveSystemOnly,
    })
    .await
}

async fn append_runtime_history_reset_event(event: RuntimeHistoryResetTelemetryEvent) {
    let guard = runtime_history_reset_io_lock().lock().await;
    let path = daat_locus_paths()
        .await
        .journal_file(RUNTIME_HISTORY_RESET_EVENT_FILE_NAME);
    let mut line = match serde_json::to_vec(&event) {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("failed to serialize runtime history reset telemetry event: {err}");
            drop(guard);
            return;
        }
    };
    line.push(b'\n');
    if let Err(err) = append_bytes_durable(path, line).await {
        error!("failed to append runtime history reset telemetry event: {err}");
    }
    drop(guard);
}

fn runtime_history_reset_io_lock() -> &'static tokio::sync::Mutex<()> {
    RUNTIME_HISTORY_RESET_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
