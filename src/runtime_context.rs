// Runtime context request construction and compaction support.
use crate::{
    context::Context,
    context_budget::{RequestBudgetLimits, approx_token_count},
    daat_locus_paths::daat_locus_paths,
    dashboard::HISTORY_ARCHIVE_BATCH_KEEP_LIMIT,
    memory::{
        RuntimeCompactionOutcome, RuntimeCompactionPhase, RuntimeCompactionReason,
        RuntimeCompactionRecord, RuntimeCompactionReinjectionStrategy,
        RuntimeConversationCompactionPlan, RuntimeRequestEnvelope, RuntimeStepCompactionPolicy,
        RuntimeStepConversation,
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

const MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS: usize = 900;
pub const MID_TURN_COMPACTION_MAX_RECOVERIES: usize = 3;
/// Message injected as the only history item after a clear-and-archive
/// compaction. The model is expected to recover the task state by reading the
/// archived history with the `read_history` tool before continuing work.
pub const HISTORY_ARCHIVE_PROMPT_MESSAGE: &str =
    "上下文已超出，更早的消息历史可使用 read_history 工具了解。请先调用它恢复任务状态，再继续当前工作。";
const RUNTIME_COMPACTION_EVENT_FILE_NAME: &str = "runtime_compaction_events.jsonl";
static RUNTIME_COMPACTION_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Serialize)]
struct RuntimeCompactionTelemetryEvent {
    timestamp_ms: i64,
    phase: RuntimeCompactionPhase,
    reason: RuntimeCompactionReason,
    reinjection_strategy: RuntimeCompactionReinjectionStrategy,
    status: &'static str,
    source_item_count: usize,
    source_message_count: usize,
    trimmed_item_count: usize,
    retained_user_message_count: usize,
    before_tokens: usize,
    after_tokens: usize,
    summary_tokens: usize,
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

pub async fn execute_pre_turn_runtime_compaction(
    context: &Context,
    plan: &RuntimeConversationCompactionPlan,
) -> Result<RuntimeCompactionOutcome> {
    execute_runtime_compaction(
        context,
        RuntimeCompactionRequest {
            source_messages: plan.source_messages(),
            retained_user_message_count: 0,
            phase: RuntimeCompactionPhase::PreTurn,
            reason: RuntimeCompactionReason::BudgetThreshold,
            reinjection_strategy: RuntimeCompactionReinjectionStrategy::RebuildRuntimeEnvelope,
        },
    )
    .await
}

pub async fn maybe_compact_runtime_messages(
    context: &Context,
    runtime_step: &mut RuntimeStepConversation,
    tools: &[AgentToolSpec],
    compact_for_overflow: bool,
) -> Result<bool> {
    maybe_compact_agent_messages(
        context,
        context.model_provider.as_ref(),
        runtime_step,
        tools,
        &context.token_estimate_baseline,
        compact_for_overflow,
    )
    .await
}

pub async fn maybe_compact_agent_messages(
    context: &Context,
    provider: &(dyn crate::core::ModelProvider + Send + Sync),
    conversation: &mut RuntimeStepConversation,
    tools: &[AgentToolSpec],
    baseline: &crate::context_budget::TokenEstimateBaseline,
    compact_for_overflow: bool,
) -> Result<bool> {
    let compaction_result = conversation
        .maybe_compact(
            tools,
            provider.request_budget_limits(),
            baseline,
            compact_for_overflow,
            runtime_step_compaction_policy(),
            |messages, max_tokens| async move {
                match build_mid_turn_compaction_outcome(
                    context,
                    &messages,
                    max_tokens,
                    compact_for_overflow,
                )
                .await
                {
                    Ok(outcome) => Ok(outcome),
                    Err(err) => Err(err.to_string()),
                }
            },
        )
        .await;
    match compaction_result {
        Ok(compacted) => Ok(compacted),
        Err(err) => Err(miette!(
            "main model failed to generate runtime compaction summary: {err}"
        )),
    }
}

const fn runtime_step_compaction_policy() -> RuntimeStepCompactionPolicy {
    RuntimeStepCompactionPolicy {
        summary_max_tokens: MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS,
        max_recoveries: MID_TURN_COMPACTION_MAX_RECOVERIES,
    }
}


fn history_message_token_cost(message: &HistoryMessage) -> usize {
    let role = message.role_name();
    approx_token_count(role) + approx_token_count(message.text_content().unwrap_or_default()) + 4
}

fn history_messages_total_token_cost(messages: &[HistoryMessage]) -> usize {
    messages.iter().map(history_message_token_cost).sum()
}


struct RuntimeCompactionRequest<'a> {
    source_messages: &'a [HistoryMessage],
    retained_user_message_count: usize,
    phase: RuntimeCompactionPhase,
    reason: RuntimeCompactionReason,
    reinjection_strategy: RuntimeCompactionReinjectionStrategy,
}

async fn execute_runtime_compaction(
    context: &Context,
    request: RuntimeCompactionRequest<'_>,
) -> Result<RuntimeCompactionOutcome> {
    let RuntimeCompactionRequest {
        source_messages,
        retained_user_message_count,
        phase,
        reason,
        reinjection_strategy,
    } = request;
    if source_messages.is_empty() {
        return Err(miette!("runtime compaction has no messages to archive"));
    }
    let before_tokens = history_messages_total_token_cost(source_messages);
    let batch_id = format!(
        "{}-{}",
        context.session_id.as_deref().unwrap_or("unknown-session"),
        Utc::now().timestamp_millis()
    );
    let archived = archive_runtime_conversation_history(context, &batch_id, source_messages).await?;
    if archived == 0 {
        return Err(miette!("runtime compaction archived no messages"));
    }
    let summary = HISTORY_ARCHIVE_PROMPT_MESSAGE.to_string();
    let record = RuntimeCompactionRecord {
        timestamp_ms: Utc::now().timestamp_millis(),
        phase,
        reason,
        reinjection_strategy,
        source_item_count: source_messages.len(),
        source_message_count: source_messages.len(),
        trimmed_item_count: 0,
        retained_user_message_count,
        summary: summary.clone(),
    };
    let after_tokens = retained_user_message_count
        .saturating_add(1)
        .saturating_mul(4)
        .saturating_add(approx_token_count(&summary));
    append_runtime_compaction_event(RuntimeCompactionTelemetryEvent {
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
        summary_tokens: approx_token_count(&summary),
        error: None,
    })
    .await;
    Ok(RuntimeCompactionOutcome { summary, record })
}

async fn archive_runtime_conversation_history(
    context: &Context,
    batch_id: &str,
    messages: &[HistoryMessage],
) -> Result<usize> {
    let store = context.dashboard_history.as_ref().ok_or_else(|| {
        miette!("runtime compaction archive requires an active session history store")
    })?;
    let archived = store.archive_history_messages(batch_id, messages)?;
    let pruned = store.prune_history_archive(HISTORY_ARCHIVE_BATCH_KEEP_LIMIT)?;
    if pruned > 0 {
        tracing::debug!(pruned, "pruned old runtime history archive batches");
    }
    Ok(archived)
}

fn agent_message_to_history_message_for_archival(message: &AgentMessage) -> HistoryMessage {
    HistoryMessage {
        message: message.clone(),
        activity_event: None,
        tool_call_activity_events: Vec::new(),
    }
}

async fn build_mid_turn_compaction_outcome(
    context: &Context,
    messages: &[AgentMessage],
    _max_tokens: usize,
    compact_for_overflow: bool,
) -> Result<RuntimeCompactionOutcome> {
    let compacted_messages = messages
        .iter()
        .map(agent_message_to_history_message_for_archival)
        .collect::<Vec<_>>();
    if compacted_messages.is_empty() {
        return Err(miette!("runtime compaction has no mid-turn messages to archive"));
    }
    let reason = if compact_for_overflow {
        RuntimeCompactionReason::OverflowRecovery
    } else {
        RuntimeCompactionReason::BudgetThreshold
    };
    execute_runtime_compaction(
        context,
        RuntimeCompactionRequest {
            source_messages: &compacted_messages,
            retained_user_message_count: 0,
            phase: RuntimeCompactionPhase::MidTurn,
            reason,
            reinjection_strategy: RuntimeCompactionReinjectionStrategy::PreserveSystemOnly,
        },
    )
    .await
}

async fn append_runtime_compaction_event(event: RuntimeCompactionTelemetryEvent) {
    let guard = runtime_compaction_io_lock().lock().await;
    let path = daat_locus_paths()
        .await
        .journal_file(RUNTIME_COMPACTION_EVENT_FILE_NAME);
    let mut line = match serde_json::to_vec(&event) {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("failed to serialize runtime compaction telemetry event: {err}");
            drop(guard);
            return;
        }
    };
    line.push(b'\n');
    if let Err(err) = append_bytes_durable(path, line).await {
        error!("failed to append runtime compaction telemetry event: {err}");
    }
    drop(guard);
}

fn runtime_compaction_io_lock() -> &'static tokio::sync::Mutex<()> {
    RUNTIME_COMPACTION_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

