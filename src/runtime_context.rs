// src/runtime_context.rs 文件定义了许多与运行时上下文管理相关的功能和结构，包括依赖模块（如 context::Context、context_budget、memory、reasoning）、运行时压缩相关常量（如 MID_TURN_COMPACTION_KEEP_TOOL_CYCLES、MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS）、以及结构体（如 HistoryCompactionOutput）。
// src/runtime_context.rs 文件定义了许多与运行时上下文管理相关的功能和结构，包括依赖模块（如 context::Context、context_budget、memory、reasoning）、运行时压缩相关常量（如 MID_TURN_COMPACTION_KEEP_TOOL_CYCLES、MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS）、以及结构体（如 HistoryCompactionOutput）。
use crate::{
    context::Context,
    context_budget::{
        RequestBudgetLimits, approx_token_count, estimate_prompt_request,
        truncate_text_to_token_budget, truncate_text_to_token_budget_with_notice,
    },
    daat_locus_paths::daat_locus_paths,
    memory::{
        RuntimeCompactionOutcome, RuntimeCompactionPhase, RuntimeCompactionReason,
        RuntimeCompactionRecord, RuntimeCompactionReinjectionStrategy,
        RuntimeConversationCompactionPlan, RuntimeRequestEnvelope, RuntimeStepCompactionPolicy,
        RuntimeStepConversation,
    },
    reasoning::{
        prompt_renderer::LlmPromptRenderer,
        prompts::{HISTORY_COMPACTION_PROMPT, HISTORY_COMPACTION_SUMMARY_PREFIX},
        runtime::{
            AgentMessage, AgentToolSpec, HistoryMessage, PromptRequest,
            summarize_assistant_tool_call_protocol,
        },
    },
    schema_utils::normalize_openai_json_schema,
};
use chrono::Utc;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};
use tracing::{error, warn};

const MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS: usize = 900;
pub const MID_TURN_COMPACTION_MAX_RECOVERIES: usize = 3;
const RUNTIME_COMPACTION_EVENT_FILE_NAME: &str = "runtime_compaction_events.jsonl";
static RUNTIME_COMPACTION_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct HistoryCompactionOutput {
    summary: String,
}

#[derive(Clone)]
struct HistoryCompactionSourceItem {
    messages: Vec<HistoryMessage>,
}

struct TrimmedHistoryCompactionInput {
    messages: Vec<HistoryMessage>,
    source_item_count: usize,
    trimmed_item_count: usize,
}

struct RuntimeCompactionExecution {
    outcome: RuntimeCompactionOutcome,
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
    used_fallback_summary: bool,
    before_tokens: usize,
    after_tokens: usize,
    summary_tokens: usize,
    error: Option<String>,
}

pub fn build_runtime_request_envelope(
    context: &Context,
    snapshot_text: &str,
) -> RuntimeRequestEnvelope {
    let system_messages =
        LlmPromptRenderer::render_system_messages(&context.runtime_system_prompt_doc());
    RuntimeRequestEnvelope::from_world_snapshot(system_messages, snapshot_text)
}

pub fn build_runtime_snapshot_text(
    context: &Context,
    snapshot: &crate::snapshot::Snapshot,
) -> String {
    LlmPromptRenderer::render_document_with_root(
        &context.runtime_snapshot_doc(snapshot),
        Some("runtime_snapshot"),
    )
}

pub fn runtime_request_budget_limits(context: &Context) -> RequestBudgetLimits {
    RequestBudgetLimits {
        context_window_tokens: context
            .config
            .main_model_config()
            .effective_context_window_tokens(),
        auto_compact_threshold_tokens: context
            .config
            .main_model_config()
            .auto_compact_token_limit(),
        reserved_output_tokens: context.config.main_model_config().max_completion_tokens(),
    }
}

pub async fn execute_pre_turn_runtime_compaction(
    context: &Context,
    plan: &RuntimeConversationCompactionPlan,
) -> Option<RuntimeCompactionOutcome> {
    let fallback_summary = build_runtime_prompt_history_summary_fallback(
        plan.source_messages(),
        plan.summary_max_tokens(),
    )?;
    let retained_user_message_count = plan.retained_user_messages().len();
    execute_runtime_compaction(
        context,
        plan.source_messages(),
        retained_user_message_count,
        plan.summary_max_tokens(),
        RuntimeCompactionPhase::PreTurn,
        RuntimeCompactionReason::BudgetThreshold,
        RuntimeCompactionReinjectionStrategy::RebuildRuntimeEnvelope,
        fallback_summary,
    )
    .await
    .map(|execution| execution.outcome)
}

pub async fn maybe_compact_runtime_messages(
    context: &Context,
    runtime_step: &mut RuntimeStepConversation,
    tools: &[AgentToolSpec],
    compact_for_overflow: bool,
) -> bool {
    runtime_step
        .maybe_compact(
            tools,
            runtime_request_budget_limits(context),
            compact_for_overflow,
            runtime_step_compaction_policy(),
            |messages, max_tokens| async move {
                build_mid_turn_compaction_outcome(
                    context,
                    &messages,
                    max_tokens,
                    compact_for_overflow,
                )
                .await
            },
        )
        .await
}

fn runtime_step_compaction_policy() -> RuntimeStepCompactionPolicy {
    RuntimeStepCompactionPolicy {
        summary_max_tokens: MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS,
        max_recoveries: MID_TURN_COMPACTION_MAX_RECOVERIES,
    }
}

fn judge_request_budget_limits(context: &Context) -> RequestBudgetLimits {
    let model = context.config.judge_model_config();
    RequestBudgetLimits {
        context_window_tokens: model.effective_context_window_tokens(),
        auto_compact_threshold_tokens: model.auto_compact_token_limit(),
        reserved_output_tokens: model.max_completion_tokens(),
    }
}

fn build_history_compaction_request(messages: Vec<HistoryMessage>) -> Option<PromptRequest> {
    Some(PromptRequest {
        tool_name: "history_compaction_summary".to_string(),
        tool_description: "Generate a concise handoff summary for compacted runtime context"
            .to_string(),
        output_schema: normalize_openai_json_schema(
            serde_json::to_value(schema_for!(HistoryCompactionOutput)).ok()?,
        ),
        system_messages: vec![HISTORY_COMPACTION_PROMPT.to_string()],
        long_term_memory_messages: Vec::new(),
        history_messages: messages,
        current_user_message: "请基于以上将被压缩移出的运行时上下文，生成一段 handoff summary。只输出 `summary` 字段。"
            .to_string(),
        retry_messages: Vec::new(),
    })
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
            items.push(HistoryCompactionSourceItem { messages: current });
            current = Vec::new();
        }
        current.push(message.clone());
    }
    if !current.is_empty() {
        items.push(HistoryCompactionSourceItem { messages: current });
    }

    items
}

fn flatten_history_compaction_source_items(
    items: &[HistoryCompactionSourceItem],
) -> Vec<HistoryMessage> {
    items
        .iter()
        .flat_map(|item| item.messages.clone())
        .collect()
}

fn collapse_history_compaction_source_item(
    item: &HistoryCompactionSourceItem,
    available_history_tokens: usize,
) -> Option<HistoryMessage> {
    if available_history_tokens == 0 {
        return None;
    }
    let rendered = item
        .messages
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
    let truncated_content = truncate_text_to_token_budget_with_notice(
        rendered.trim(),
        available_history_tokens,
        "... [compaction input truncated to fit judge model context]",
    );
    (!truncated_content.trim().is_empty()).then(|| HistoryMessage::assistant(truncated_content))
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
        let Some(request) = build_history_compaction_request(flattened.clone()) else {
            return TrimmedHistoryCompactionInput {
                messages: Vec::new(),
                source_item_count,
                trimmed_item_count,
            };
        };
        let budget = estimate_prompt_request(&request, limits);
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

        let history_tokens = budget
            .sections
            .iter()
            .find_map(|section| (section.name == "history_messages").then_some(section.tokens))
            .unwrap_or(0);
        let non_history_tokens = budget.total_input_tokens.saturating_sub(history_tokens);
        let available_history_tokens = budget
            .input_budget_tokens()
            .saturating_sub(non_history_tokens);
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

async fn execute_runtime_compaction(
    context: &Context,
    source_messages: &[HistoryMessage],
    retained_user_message_count: usize,
    max_tokens: usize,
    phase: RuntimeCompactionPhase,
    reason: RuntimeCompactionReason,
    reinjection_strategy: RuntimeCompactionReinjectionStrategy,
    fallback_summary: String,
) -> Option<RuntimeCompactionExecution> {
    let source_items = build_history_compaction_source_items(source_messages);
    let before_tokens = history_messages_total_token_cost(source_messages);
    let trimmed = trim_compaction_source_items_to_fit_budget(
        &source_items,
        judge_request_budget_limits(context),
    );
    let mut status = "completed";
    let mut error_message = None;
    let mut used_fallback_summary = false;

    let summary = if trimmed.messages.is_empty() {
        used_fallback_summary = true;
        status = "fallback";
        fallback_summary
    } else {
        if trimmed.trimmed_item_count > 0 {
            warn!(
                trimmed_item_count = trimmed.trimmed_item_count,
                source_item_count = trimmed.source_item_count,
                "trimmed oldest compaction source items before issuing history compaction summary request"
            );
        }
        let request = build_history_compaction_request(trimmed.messages.clone())?;
        match context.judge_llm.run_json(context, request).await {
            Ok(value) => match serde_json::from_value::<HistoryCompactionOutput>(value) {
                Ok(output) if !output.summary.trim().is_empty() => truncate_text_to_token_budget(
                    &format!(
                        "{}\n{}",
                        HISTORY_COMPACTION_SUMMARY_PREFIX,
                        output.summary.trim()
                    ),
                    max_tokens.max(1),
                ),
                Ok(_) => {
                    used_fallback_summary = true;
                    status = "fallback";
                    error_message = Some("history compaction summary was empty".to_string());
                    fallback_summary
                }
                Err(err) => {
                    used_fallback_summary = true;
                    status = "fallback";
                    error_message = Some(format!(
                        "failed to decode history compaction summary: {err}"
                    ));
                    fallback_summary
                }
            },
            Err(err) => {
                used_fallback_summary = true;
                status = "fallback";
                error_message = Some(format!(
                    "history compaction summary request failed: {err:?}"
                ));
                fallback_summary
            }
        }
    };

    let record = RuntimeCompactionRecord {
        timestamp_ms: Utc::now().timestamp_millis(),
        phase,
        reason,
        reinjection_strategy,
        source_item_count: trimmed.source_item_count,
        source_message_count: source_messages.len(),
        trimmed_item_count: trimmed.trimmed_item_count,
        retained_user_message_count,
        used_fallback_summary,
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
        status,
        source_item_count: trimmed.source_item_count,
        source_message_count: source_messages.len(),
        trimmed_item_count: trimmed.trimmed_item_count,
        retained_user_message_count,
        used_fallback_summary,
        before_tokens,
        after_tokens,
        summary_tokens: approx_token_count(&summary),
        error: error_message,
    })
    .await;
    Some(RuntimeCompactionExecution {
        outcome: RuntimeCompactionOutcome { summary, record },
    })
}

fn summarize_compacted_agent_message(message: &AgentMessage) -> Option<String> {
    match message {
        AgentMessage::System { content } => Some(format!(
            "system hint: {}",
            summarize_runtime_inline_text(content)
        )),
        AgentMessage::Assistant { content } => Some(format!(
            "assistant: {}",
            summarize_runtime_inline_text(content)
        )),
        AgentMessage::AssistantToolCallProtocol { content, calls, .. } => Some(
            summarize_assistant_tool_call_protocol(content.as_deref(), calls),
        ),
        AgentMessage::Tool { name, content, .. } => Some(format!(
            "{name}: {}",
            summarize_tool_message_content(content)
        )),
        AgentMessage::User { .. } => None,
    }
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
        .map(summarize_runtime_inline_text)
        .unwrap_or_else(|| "<no content>".to_string())
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

fn history_message_for_compaction(message: AgentMessage) -> HistoryMessage {
    HistoryMessage {
        message,
        tool_ui_event: None,
        tool_call_ui_events: Vec::new(),
    }
}

fn agent_message_to_history_message_for_compaction(
    message: &AgentMessage,
) -> Option<HistoryMessage> {
    match message {
        AgentMessage::System { content } => Some(history_message_for_compaction(
            AgentMessage::system(summarize_runtime_inline_text(content)),
        )),
        AgentMessage::User { content } => Some(history_message_for_compaction(AgentMessage::user(
            summarize_runtime_inline_text(content),
        ))),
        AgentMessage::Assistant { content } => Some(history_message_for_compaction(
            AgentMessage::assistant(summarize_runtime_inline_text(content)),
        )),
        AgentMessage::AssistantToolCallProtocol { content, calls, .. } => {
            Some(history_message_for_compaction(AgentMessage::assistant(
                summarize_assistant_tool_call_protocol(content.as_deref(), calls),
            )))
        }
        AgentMessage::Tool { name, content, .. } => {
            Some(history_message_for_compaction(AgentMessage::assistant(
                format!("{name}: {}", summarize_tool_message_content(content)),
            )))
        }
    }
}

fn build_mid_turn_compaction_summary_fallback(
    messages: &[AgentMessage],
    max_tokens: usize,
) -> Option<String> {
    let rendered_lines = messages
        .iter()
        .filter_map(summarize_compacted_agent_message)
        .collect::<Vec<_>>();
    if rendered_lines.is_empty() {
        return None;
    }

    let omitted = rendered_lines.len().saturating_sub(12);
    let mut lines = vec!["Earlier tool/context progress summary:".to_string()];
    lines.extend(
        rendered_lines
            .into_iter()
            .take(12)
            .map(|line| format!("- {line}")),
    );
    if omitted > 0 {
        lines.push(format!("- ... {omitted} older interaction(s) compacted"));
    }
    Some(truncate_text_to_token_budget(
        &lines.join("\n"),
        max_tokens.max(1),
    ))
}

fn build_runtime_prompt_history_summary_fallback(
    messages: &[HistoryMessage],
    max_tokens: usize,
) -> Option<String> {
    let rendered = messages
        .iter()
        .filter_map(|message| {
            let content = message.text_content().unwrap_or_default();
            match &message.message {
                AgentMessage::System { .. } => Some(format!(
                    "system: {}",
                    summarize_runtime_inline_text(content)
                )),
                AgentMessage::User { .. } => {
                    Some(format!("user: {}", summarize_runtime_inline_text(content)))
                }
                AgentMessage::Assistant { .. } | AgentMessage::AssistantToolCallProtocol { .. } => {
                    Some(format!(
                        "assistant: {}",
                        summarize_runtime_inline_text(content)
                    ))
                }
                AgentMessage::Tool { .. } => {
                    Some(format!("tool: {}", summarize_tool_message_content(content)))
                }
            }
        })
        .collect::<Vec<_>>();
    if rendered.is_empty() {
        return None;
    }

    let omitted = rendered.len().saturating_sub(12);
    let mut lines = vec!["Earlier runtime history summary:".to_string()];
    lines.extend(
        rendered
            .into_iter()
            .take(12)
            .map(|line| format!("- {line}")),
    );
    if omitted > 0 {
        lines.push(format!(
            "- ... {omitted} earlier history message(s) compacted"
        ));
    }
    Some(truncate_text_to_token_budget(
        &lines.join("\n"),
        max_tokens.max(1),
    ))
}

async fn build_mid_turn_compaction_outcome(
    context: &Context,
    messages: &[AgentMessage],
    max_tokens: usize,
    compact_for_overflow: bool,
) -> Option<RuntimeCompactionOutcome> {
    let compacted_messages = messages
        .iter()
        .filter_map(agent_message_to_history_message_for_compaction)
        .collect::<Vec<_>>();
    if compacted_messages.is_empty() {
        return None;
    }

    let fallback_summary = build_mid_turn_compaction_summary_fallback(messages, max_tokens)?;
    let reason = if compact_for_overflow {
        RuntimeCompactionReason::OverflowRecovery
    } else {
        RuntimeCompactionReason::BudgetThreshold
    };
    let retained_user_message_count = messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::User { .. }))
        .count();
    execute_runtime_compaction(
        context,
        &compacted_messages,
        retained_user_message_count,
        max_tokens,
        RuntimeCompactionPhase::MidTurn,
        reason,
        RuntimeCompactionReinjectionStrategy::PreserveSystemAndRecentUsers,
        fallback_summary,
    )
    .await
    .map(|execution| execution.outcome)
}

async fn append_runtime_compaction_event(event: RuntimeCompactionTelemetryEvent) {
    let guard = runtime_compaction_io_lock().lock().await;
    let path = daat_locus_paths()
        .await
        .journal_file(RUNTIME_COMPACTION_EVENT_FILE_NAME);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    else {
        drop(guard);
        return;
    };
    let mut line = match serde_json::to_vec(&event) {
        Ok(bytes) => bytes,
        Err(err) => {
            error!("failed to serialize runtime compaction telemetry event: {err}");
            drop(guard);
            return;
        }
    };
    line.push(b'\n');
    let _ = file.write_all(&line).await;
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
            HistoryMessage::assistant("a".repeat(800)),
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

        let request = build_history_compaction_request(trimmed.messages).expect("request");
        let budget = estimate_prompt_request(&request, limits);
        assert!(budget.within_context_window());
    }
}
