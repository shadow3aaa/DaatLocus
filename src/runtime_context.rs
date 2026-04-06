// src/runtime_context.rs 文件定义了许多与运行时上下文管理相关的功能和结构，包括依赖模块（如 context::Context、context_budget、memory、reasoning）、运行时压缩相关常量（如 MID_TURN_COMPACTION_KEEP_TOOL_CYCLES、MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS）、以及结构体（如 HistoryCompactionOutput）。
// src/runtime_context.rs 文件定义了许多与运行时上下文管理相关的功能和结构，包括依赖模块（如 context::Context、context_budget、memory、reasoning）、运行时压缩相关常量（如 MID_TURN_COMPACTION_KEEP_TOOL_CYCLES、MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS）、以及结构体（如 HistoryCompactionOutput）。
use crate::{
    context::Context,
    context_budget::{RequestBudgetLimits, truncate_text_to_token_budget},
    memory::{
        RuntimeConversationCompactionPlan, RuntimeRequestEnvelope, RuntimeStepCompactionPolicy,
        RuntimeStepConversation,
    },
    reasoning::{
        prompts::{
            HISTORY_COMPACTION_PROMPT, HISTORY_COMPACTION_SUMMARY_PREFIX, SYSTEM_PROMPT_KERNEL,
            TOOL_ACTION_PROMPT, build_app_context_prompt,
        },
        runtime::{
            AgentMessage, AgentToolSpec, PromptMessage, PromptRequest, PromptRole,
            summarize_assistant_tool_call_protocol,
        },
    },
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

const MID_TURN_COMPACTION_KEEP_TOOL_CYCLES: usize = 1;
const MID_TURN_COMPACTION_KEEP_MESSAGES_WITHOUT_TOOL_CYCLES: usize = 4;
const MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS: usize = 900;
pub const MID_TURN_COMPACTION_MAX_RECOVERIES: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct HistoryCompactionOutput {
    summary: String,
}

pub fn build_runtime_request_envelope(
    context: &Context,
    snapshot_text: &str,
) -> RuntimeRequestEnvelope {
    let mut system_messages = vec![
        SYSTEM_PROMPT_KERNEL.to_string(),
        TOOL_ACTION_PROMPT.to_string(),
    ];
    system_messages.extend(
        context
            .compiled_prompts
            .runtime_system_additions()
            .iter()
            .filter(|line| !line.trim().is_empty())
            .cloned(),
    );
    system_messages.push(build_app_context_prompt(context));
    if !context.prompt_memory.recalled_memories.is_empty() {
        system_messages.push(format!(
            "相关长期记忆：\n{}",
            context.prompt_memory.recalled_memories.join("\n")
        ));
    }
    RuntimeRequestEnvelope::from_world_snapshot(system_messages, snapshot_text)
}

pub fn runtime_request_budget_limits(context: &Context) -> RequestBudgetLimits {
    RequestBudgetLimits {
        context_window_tokens: context.config.main_model.effective_context_window_tokens(),
        auto_compact_threshold_tokens: context.config.main_model.auto_compact_token_limit(),
        reserved_output_tokens: context.config.main_model.max_completion_tokens(),
    }
}

pub async fn build_runtime_conversation_summary(
    context: &Context,
    plan: &RuntimeConversationCompactionPlan,
) -> Option<PromptMessage> {
    let summary =
        build_handoff_summary_with_llm(context, plan.omitted_prefix(), plan.summary_max_tokens())
            .await?;
    Some(PromptMessage::assistant(summary))
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
                build_mid_turn_compaction_summary(context, &messages, max_tokens).await
            },
        )
        .await
}

fn runtime_step_compaction_policy() -> RuntimeStepCompactionPolicy {
    RuntimeStepCompactionPolicy {
        keep_tool_cycles: MID_TURN_COMPACTION_KEEP_TOOL_CYCLES,
        keep_messages_without_tool_cycles: MID_TURN_COMPACTION_KEEP_MESSAGES_WITHOUT_TOOL_CYCLES,
        summary_max_tokens: MID_TURN_COMPACTION_SUMMARY_MAX_TOKENS,
        max_recoveries: MID_TURN_COMPACTION_MAX_RECOVERIES,
    }
}

async fn build_handoff_summary_with_llm(
    context: &Context,
    messages: &[PromptMessage],
    max_tokens: usize,
) -> Option<String> {
    let request = PromptRequest {
        tool_name: "history_compaction_summary".to_string(),
        tool_description: "Generate a concise handoff summary for compacted runtime context"
            .to_string(),
        output_schema: serde_json::to_value(schema_for!(HistoryCompactionOutput)).ok()?,
        system_messages: vec![HISTORY_COMPACTION_PROMPT.to_string()],
        long_term_memory_messages: Vec::new(),
        history_messages: messages.to_vec(),
        current_user_message: "请基于以上将被压缩移出的运行时上下文，生成一段 handoff summary。只输出 `summary` 字段。"
            .to_string(),
        retry_messages: Vec::new(),
    };
    let value = context.judge_llm.run_json(context, request).await.ok()?;
    let output = serde_json::from_value::<HistoryCompactionOutput>(value).ok()?;
    let summary = output.summary.trim();
    if summary.is_empty() {
        return None;
    }

    Some(truncate_text_to_token_budget(
        &format!("{}\n{}", HISTORY_COMPACTION_SUMMARY_PREFIX, summary),
        max_tokens.max(1),
    ))
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
        AgentMessage::AssistantToolCallProtocol { content, calls } => Some(
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

fn prompt_message_for_compaction(role: PromptRole, content: impl Into<String>) -> PromptMessage {
    PromptMessage {
        role,
        content: content.into(),
        tool_ui_event: None,
        tool_call_ui_events: Vec::new(),
    }
}

fn agent_message_to_prompt_message_for_compaction(message: &AgentMessage) -> Option<PromptMessage> {
    match message {
        AgentMessage::System { content } => Some(prompt_message_for_compaction(
            PromptRole::System,
            summarize_runtime_inline_text(content),
        )),
        AgentMessage::User { content } => Some(prompt_message_for_compaction(
            PromptRole::User,
            summarize_runtime_inline_text(content),
        )),
        AgentMessage::Assistant { content } => Some(prompt_message_for_compaction(
            PromptRole::Assistant,
            summarize_runtime_inline_text(content),
        )),
        AgentMessage::AssistantToolCallProtocol { content, calls } => {
            Some(prompt_message_for_compaction(
                PromptRole::Assistant,
                summarize_assistant_tool_call_protocol(content.as_deref(), calls),
            ))
        }
        AgentMessage::Tool { name, content, .. } => Some(prompt_message_for_compaction(
            PromptRole::Tool,
            format!("{name}: {}", summarize_tool_message_content(content)),
        )),
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

async fn build_mid_turn_compaction_summary(
    context: &Context,
    messages: &[AgentMessage],
    max_tokens: usize,
) -> Option<String> {
    let compacted_messages = messages
        .iter()
        .filter_map(agent_message_to_prompt_message_for_compaction)
        .collect::<Vec<_>>();
    if compacted_messages.is_empty() {
        return None;
    }

    if let Some(summary) =
        build_handoff_summary_with_llm(context, &compacted_messages, max_tokens).await
    {
        return Some(summary);
    }

    build_mid_turn_compaction_summary_fallback(messages, max_tokens)
}
