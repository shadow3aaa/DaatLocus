// Runtime context request construction and compaction support.
use crate::{
    context::Context,
    context_budget::{
        RequestBudgetLimits, approx_token_count, estimate_agent_turn_request,
        truncate_text_to_token_budget, truncate_text_to_token_budget_with_notice,
    },
    daat_locus_paths::daat_locus_paths,
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
        prompts::{
            HISTORY_COMPACTION_PROMPT, HISTORY_COMPACTION_SUMMARY_PREFIX,
            HISTORY_COMPACTION_USER_MESSAGE,
        },
        runtime::{
            AgentMessage, AgentToolSpec, AgentTurnRequest, HistoryMessage,
            summarize_assistant_tool_call_protocol,
        },
    },
};
use chrono::Utc;
use miette::{Result, miette};
use serde::Serialize;
use std::sync::OnceLock;
use tracing::{error, warn};

const MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS: usize = 900;
pub const MID_TURN_COMPACTION_MAX_RECOVERIES: usize = 3;
const RUNTIME_COMPACTION_EVENT_FILE_NAME: &str = "runtime_compaction_events.jsonl";
static RUNTIME_COMPACTION_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

type HistoryCompactionSourceItem = Vec<HistoryMessage>;

struct TrimmedHistoryCompactionInput {
    messages: Vec<HistoryMessage>,
    source_item_count: usize,
    trimmed_item_count: usize,
}

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
            max_tokens: plan.summary_max_tokens(),
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

fn build_history_compaction_request(messages: Vec<HistoryMessage>) -> AgentTurnRequest {
    let mut request_messages = Vec::with_capacity(messages.len().saturating_add(2));
    request_messages.push(AgentMessage::system(HISTORY_COMPACTION_PROMPT));
    request_messages.extend(messages.into_iter().map(|message| message.message));
    request_messages.push(AgentMessage::user(HISTORY_COMPACTION_USER_MESSAGE));
    AgentTurnRequest {
        messages: request_messages,
        tools: Vec::new(),
    }
}

fn history_message_token_cost(message: &HistoryMessage) -> usize {
    let role = message.role_name();
    approx_token_count(role) + approx_token_count(message.text_content().unwrap_or_default()) + 4
}

fn history_messages_total_token_cost(messages: &[HistoryMessage]) -> usize {
    messages.iter().map(history_message_token_cost).sum()
}

fn build_history_compaction_source_items(
    messages: &[HistoryMessage],
) -> Vec<HistoryCompactionSourceItem> {
    let mut items = Vec::new();
    let mut current = Vec::new();

    for message in messages {
        if message.is_user() && !current.is_empty() {
            items.push(current);
            current = Vec::new();
        }
        current.push(message.clone());
    }
    if !current.is_empty() {
        items.push(current);
    }

    items
}

fn flatten_history_compaction_source_items(
    items: &[HistoryCompactionSourceItem],
) -> Vec<HistoryMessage> {
    items.iter().flat_map(std::clone::Clone::clone).collect()
}

fn collapse_history_compaction_source_item(
    item: &HistoryCompactionSourceItem,
    available_history_tokens: usize,
) -> Option<HistoryMessage> {
    if available_history_tokens == 0 {
        return None;
    }
    let rendered = item
        .iter()
        .map(|message| {
            format!(
                "{}: {}",
                message.role_name(),
                message.text_content().unwrap_or_default().trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut content_budget = available_history_tokens;
    loop {
        let truncated_content = truncate_text_to_token_budget_with_notice(
            rendered.trim(),
            content_budget,
            "... [compaction input truncated to fit main model context]",
        );
        if truncated_content.trim().is_empty() {
            return None;
        }

        let message = HistoryMessage::assistant(truncated_content);
        let message_tokens = history_message_token_cost(&message);
        if message_tokens <= available_history_tokens {
            return Some(message);
        }
        if content_budget == 0 {
            return None;
        }
        content_budget = content_budget
            .saturating_mul(available_history_tokens)
            .checked_div(message_tokens)
            .unwrap_or_default()
            .min(content_budget.saturating_sub(1));
    }
}

fn trim_compaction_source_items_to_fit_budget(
    items: &[HistoryCompactionSourceItem],
    limits: RequestBudgetLimits,
) -> TrimmedHistoryCompactionInput {
    let source_item_count = items.len();
    let mut trimmed_items = items.to_vec();
    let mut trimmed_item_count = 0usize;
    loop {
        let flattened = flatten_history_compaction_source_items(&trimmed_items);
        let request = build_history_compaction_request(flattened.clone());
        let budget = estimate_agent_turn_request(&request.messages, &request.tools, limits);
        if budget.within_context_window() {
            return TrimmedHistoryCompactionInput {
                messages: flattened,
                source_item_count,
                trimmed_item_count,
            };
        }
        if trimmed_items.len() > 1 {
            trimmed_items.remove(0);
            trimmed_item_count += 1;
            continue;
        }

        let fixed_request = build_history_compaction_request(Vec::new());
        let fixed_tokens =
            estimate_agent_turn_request(&fixed_request.messages, &fixed_request.tools, limits)
                .total_input_tokens;
        let available_history_tokens = budget
            .input_budget_tokens()
            .saturating_sub(fixed_tokens)
            .saturating_sub(32);
        let messages = trimmed_items
            .first()
            .and_then(|item| {
                collapse_history_compaction_source_item(item, available_history_tokens)
            })
            .into_iter()
            .collect::<Vec<_>>();
        return TrimmedHistoryCompactionInput {
            messages,
            source_item_count,
            trimmed_item_count,
        };
    }
}

struct RuntimeCompactionRequest<'a> {
    source_messages: &'a [HistoryMessage],
    retained_user_message_count: usize,
    max_tokens: usize,
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
        max_tokens,
        phase,
        reason,
        reinjection_strategy,
    } = request;

    let source_items = build_history_compaction_source_items(source_messages);
    let before_tokens = history_messages_total_token_cost(source_messages);
    let trimmed = trim_compaction_source_items_to_fit_budget(
        &source_items,
        runtime_request_budget_limits(context),
    );
    if trimmed.messages.is_empty() {
        return Err(miette!("runtime compaction has no messages to summarize"));
    }
    if trimmed.trimmed_item_count > 0 {
        warn!(
            trimmed_item_count = trimmed.trimmed_item_count,
            source_item_count = trimmed.source_item_count,
            "trimmed oldest compaction source items before issuing history compaction summary request"
        );
    }

    let request = build_history_compaction_request(trimmed.messages.clone());
    let options = crate::core::ModelRequestOptions::for_agent_turn(
        context.model_provider.as_ref(),
        &request,
        context.session_id.clone(),
    )?;
    let response = context
        .model_provider
        .complete_agent_turn(request, options)
        .await
        .map_err(|err| {
            miette!("main model failed to generate runtime compaction summary: {err}")
        })?;
    let output = response
        .protocol()
        .final_assistant_message
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| miette!("main model returned an empty runtime compaction summary"))?;
    let summary = truncate_text_to_token_budget(
        &format!("{}\n{}", HISTORY_COMPACTION_SUMMARY_PREFIX, output.trim()),
        max_tokens.max(1),
    );

    let record = RuntimeCompactionRecord {
        timestamp_ms: Utc::now().timestamp_millis(),
        phase,
        reason,
        reinjection_strategy,
        source_item_count: trimmed.source_item_count,
        source_message_count: source_messages.len(),
        trimmed_item_count: trimmed.trimmed_item_count,
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
        source_item_count: trimmed.source_item_count,
        source_message_count: source_messages.len(),
        trimmed_item_count: trimmed.trimmed_item_count,
        retained_user_message_count,
        before_tokens,
        after_tokens,
        summary_tokens: approx_token_count(&summary),
        error: None,
    })
    .await;
    Ok(RuntimeCompactionOutcome { summary, record })
}

fn summarize_tool_message_content(content: &str) -> String {
    if let Some(summary_line) = content
        .lines()
        .find_map(|line| line.strip_prefix("summary="))
        .map(str::trim)
        && !summary_line.is_empty()
    {
        return summarize_runtime_inline_text(summary_line);
    }

    content
        .lines()
        .find(|line| !line.trim().is_empty())
        .map_or_else(|| "<no content>".to_string(), summarize_runtime_inline_text)
}

fn summarize_runtime_inline_text(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let compact = text.replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

const fn history_message_for_compaction(message: AgentMessage) -> HistoryMessage {
    HistoryMessage {
        message,
        activity_event: None,
        tool_call_activity_events: Vec::new(),
    }
}

fn agent_message_to_history_message_for_compaction(message: &AgentMessage) -> HistoryMessage {
    match message {
        AgentMessage::System { content } => history_message_for_compaction(AgentMessage::system(
            summarize_runtime_inline_text(content),
        )),
        AgentMessage::User { content } => history_message_for_compaction(AgentMessage::user(
            summarize_runtime_inline_text(content.as_text()),
        )),
        AgentMessage::Assistant { content } => history_message_for_compaction(
            AgentMessage::assistant(summarize_runtime_inline_text(content)),
        ),
        AgentMessage::AssistantToolCallProtocol { content, calls, .. } => {
            history_message_for_compaction(AgentMessage::assistant(
                summarize_assistant_tool_call_protocol(content.as_deref(), calls),
            ))
        }
        AgentMessage::Tool { name, content, .. } => {
            history_message_for_compaction(AgentMessage::assistant(format!(
                "{name}: {}",
                summarize_tool_message_content(content)
            )))
        }
    }
}

async fn build_mid_turn_compaction_outcome(
    context: &Context,
    messages: &[AgentMessage],
    max_tokens: usize,
    compact_for_overflow: bool,
) -> Result<RuntimeCompactionOutcome> {
    let compacted_messages = messages
        .iter()
        .map(agent_message_to_history_message_for_compaction)
        .collect::<Vec<_>>();
    if compacted_messages.is_empty() {
        return Err(miette!(
            "runtime compaction has no mid-turn messages to summarize"
        ));
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
            max_tokens,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_compaction_messages_drops_oldest_until_budget_fits() {
        let limits = RequestBudgetLimits {
            context_window_tokens: 512,
            auto_compact_threshold_tokens: 448,
            reserved_output_tokens: 16,
        };
        let messages = vec![
            HistoryMessage::assistant("a".repeat(8000)),
            HistoryMessage::user("user one"),
            HistoryMessage::assistant("b".repeat(24)),
            HistoryMessage::user("user two"),
            HistoryMessage::assistant("c".repeat(24)),
        ];

        let items = build_history_compaction_source_items(&messages);
        let trimmed = trim_compaction_source_items_to_fit_budget(&items, limits);
        assert!(!trimmed.messages.is_empty());
        assert!(
            trimmed.trimmed_item_count > 0
                || history_messages_total_token_cost(&trimmed.messages)
                    < history_messages_total_token_cost(&messages)
        );

        let request = build_history_compaction_request(trimmed.messages);
        let budget = estimate_agent_turn_request(&request.messages, &request.tools, limits);
        assert!(budget.within_context_window());
    }

    #[test]
    fn history_compaction_request_is_a_tool_free_text_turn() {
        let request = build_history_compaction_request(vec![
            HistoryMessage::user("original request"),
            HistoryMessage::assistant("work completed"),
        ]);

        assert!(request.tools.is_empty());
        assert!(matches!(
            request.messages.first(),
            Some(AgentMessage::System { .. })
        ));
        assert!(matches!(
            request.messages.last(),
            Some(AgentMessage::User { .. })
        ));
        assert!(request.messages.iter().any(|message| {
            matches!(message, AgentMessage::Assistant { content } if content == "work completed")
        }));
    }

    #[test]
    fn trim_compaction_messages_keeps_multibyte_text_within_budget() {
        let limits = RequestBudgetLimits {
            context_window_tokens: 512,
            auto_compact_threshold_tokens: 448,
            reserved_output_tokens: 16,
        };
        let messages = vec![HistoryMessage::assistant("中".repeat(8_000))];

        let items = build_history_compaction_source_items(&messages);
        let trimmed = trim_compaction_source_items_to_fit_budget(&items, limits);
        assert!(!trimmed.messages.is_empty());

        let request = build_history_compaction_request(trimmed.messages);
        let budget = estimate_agent_turn_request(&request.messages, &request.tools, limits);
        assert!(budget.within_context_window());
    }
}
