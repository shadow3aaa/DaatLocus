//! Runtime conversation state.
use std::{collections::VecDeque, future::Future};

use crate::{
    context_budget::{
        RequestBudgetBreakdown, RequestBudgetLimits, TokenEstimateBaseline, approx_token_count,
        estimate_agent_message_tokens, estimate_agent_turn_request,
        estimate_runtime_request_envelope, truncate_text_to_token_budget,
        truncate_text_to_token_budget_with_notice,
    },
    persistence::PersistenceStore,
    reasoning::runtime::{AgentMessage, AgentToolSpec, HistoryMessage},
    tool_ui::{
        ActivatePrimitiveUiData, CreatePrimitiveSpecUiData, PatchUiData, PlanUiData,
        TelegramUiData, TerminalUiData, ToolCallUiEvent, ToolUiData, ToolUiEvent,
    },
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

const RUNTIME_HISTORY_SUMMARY_PREFIX: &str = "Earlier runtime history summary:";
const MID_TURN_SUMMARY_PREFIX: &str = "Earlier tool/context progress summary:";
const RUNTIME_CONVERSATION_FILE_NAME: &str = "runtime_conversation.json";
const RUNTIME_CONVERSATION_LEGACY_FILE_NAME: &str = "runtime_conversation";
const RUNTIME_HISTORY_TOOL_MESSAGE_MAX_TOKENS: usize = 600;
const RUNTIME_COMPACTION_RECORD_LIMIT: usize = 32;

pub struct Memory {
    runtime_conversation: RuntimeConversation,
}

pub struct RuntimeTurnDraft {
    current_doing: String,
    messages: Vec<HistoryMessage>,
    compaction_records: Vec<RuntimeCompactionRecord>,
}

pub struct RuntimeRequestEnvelope {
    system_messages: Vec<String>,
    user_message: Option<String>,
}

pub struct RuntimeStepConversation {
    agent_messages: Vec<AgentMessage>,
    turn_draft: RuntimeTurnDraft,
}

pub struct RuntimeConversationCompactionPlan {
    source_messages: Vec<HistoryMessage>,
    summary_max_tokens: usize,
}

#[derive(Clone, Debug)]
pub struct RuntimeCompactionOutcome {
    pub summary: String,
    pub record: RuntimeCompactionRecord,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCompactionPhase {
    PreTurn,
    MidTurn,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCompactionReason {
    BudgetThreshold,
    OverflowRecovery,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCompactionReinjectionStrategy {
    RebuildRuntimeEnvelope,
    PreserveSystemOnly,
    PreserveSystemAndRecentUsers,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeCompactionRecord {
    pub timestamp_ms: i64,
    pub phase: RuntimeCompactionPhase,
    pub reason: RuntimeCompactionReason,
    pub reinjection_strategy: RuntimeCompactionReinjectionStrategy,
    pub source_item_count: usize,
    pub source_message_count: usize,
    pub trimmed_item_count: usize,
    pub retained_user_message_count: usize,
    pub used_fallback_summary: bool,
    pub summary: String,
}

#[derive(Clone, Copy)]
pub struct RuntimeStepCompactionPolicy {
    pub summary_max_tokens: usize,
    pub max_recoveries: usize,
}

impl Memory {
    pub async fn new() -> Self {
        let runtime_conversation = RuntimeConversation::new(None, Vec::new()).await;
        Self {
            runtime_conversation,
        }
    }

    pub async fn record_agent_turn(
        &mut self,
        current_doing: String,
        messages: Vec<HistoryMessage>,
        compaction_records: Vec<RuntimeCompactionRecord>,
    ) {
        self.runtime_conversation_mut()
            .append_turn(current_doing, messages, compaction_records);
        self.sync_to_disk().await;
    }

    pub fn current_thread_focus(&self) -> Option<String> {
        self.runtime_conversation().current_focus()
    }

    pub fn runtime_conversation_messages(&self) -> Vec<HistoryMessage> {
        self.runtime_conversation().messages()
    }

    pub fn begin_runtime_turn(&self) -> RuntimeTurnDraft {
        RuntimeTurnDraft::new(
            self.current_thread_focus()
                .unwrap_or_else(|| "waiting for next tool decision".to_string()),
        )
    }

    pub fn begin_runtime_step(&self, agent_messages: Vec<AgentMessage>) -> RuntimeStepConversation {
        RuntimeStepConversation::new(self.begin_runtime_turn(), agent_messages)
    }

    pub fn begin_runtime_step_from_parts(
        &self,
        envelope: RuntimeRequestEnvelope,
        conversation_messages: Vec<HistoryMessage>,
    ) -> RuntimeStepConversation {
        self.begin_runtime_step(envelope.into_agent_messages(conversation_messages))
    }

    pub async fn commit_runtime_turn(&mut self, draft: RuntimeTurnDraft) {
        let (current_doing, messages, compaction_records) = draft.into_parts();
        self.record_agent_turn(current_doing, messages, compaction_records)
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn plan_runtime_conversation_compaction_for_request(
        &self,
        envelope: &RuntimeRequestEnvelope,
        injected_messages: &[HistoryMessage],
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
        baseline: &TokenEstimateBaseline,
        min_messages: usize,
        summary_max_tokens: usize,
    ) -> Option<RuntimeConversationCompactionPlan> {
        self.runtime_conversation.plan_compaction_for_request(
            envelope,
            injected_messages,
            tools,
            limits,
            baseline,
            min_messages,
            summary_max_tokens,
        )
    }

    pub async fn apply_runtime_conversation_compaction(
        &mut self,
        plan: RuntimeConversationCompactionPlan,
        outcome: Option<RuntimeCompactionOutcome>,
    ) -> bool {
        let changed = self.runtime_conversation.apply_compaction(plan, outcome);
        if changed {
            self.runtime_conversation.sync_to_disk().await;
        }
        changed
    }

    pub fn runtime_conversation_slice(
        &self,
        max_tokens: usize,
        min_messages: usize,
        summary_max_tokens: usize,
    ) -> Vec<HistoryMessage> {
        self.runtime_conversation.select_messages_for_runtime(
            max_tokens,
            min_messages,
            summary_max_tokens,
        )
    }

    pub fn runtime_conversation(&self) -> &RuntimeConversation {
        &self.runtime_conversation
    }

    pub fn runtime_conversation_mut(&mut self) -> &mut RuntimeConversation {
        &mut self.runtime_conversation
    }

    pub async fn force_trim_runtime_conversation_to_fit_budget(
        &mut self,
        envelope: &RuntimeRequestEnvelope,
        injected_messages: &[HistoryMessage],
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
        baseline: &TokenEstimateBaseline,
    ) -> bool {
        let trimmed = self.runtime_conversation.force_trim_messages_to_fit_budget(
            envelope,
            injected_messages,
            tools,
            limits,
            baseline,
        );
        if trimmed {
            self.runtime_conversation.sync_to_disk().await;
        }
        trimmed
    }

    pub async fn clear_runtime_conversation(&mut self) {
        let _ = self.runtime_conversation.take_for_memory();
        self.runtime_conversation.sync_to_disk().await;
    }

    pub async fn shutdown(self) {
        self.sync_to_disk().await;
    }

    async fn sync_to_disk(&self) {
        self.runtime_conversation.sync_to_disk().await;
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct RuntimeConversation {
    last_focus: Option<String>,
    messages: Vec<HistoryMessage>,
    #[serde(default)]
    compaction_records: VecDeque<RuntimeCompactionRecord>,
}

impl RuntimeTurnDraft {
    fn new(current_doing: String) -> Self {
        Self {
            current_doing,
            messages: Vec::new(),
            compaction_records: Vec::new(),
        }
    }

    pub fn set_current_doing(&mut self, current_doing: impl Into<String>) {
        let current_doing = current_doing.into();
        if !current_doing.trim().is_empty() {
            self.current_doing = current_doing;
        }
    }

    pub fn push(&mut self, message: HistoryMessage) {
        self.messages.push(message);
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn messages(&self) -> &[HistoryMessage] {
        &self.messages
    }

    pub fn record_compaction(&mut self, record: RuntimeCompactionRecord) {
        self.compaction_records.push(record);
    }

    fn into_parts(self) -> (String, Vec<HistoryMessage>, Vec<RuntimeCompactionRecord>) {
        (self.current_doing, self.messages, self.compaction_records)
    }
}

impl RuntimeRequestEnvelope {
    pub fn from_system_messages(system_messages: Vec<String>) -> Self {
        Self {
            system_messages,
            user_message: None,
        }
    }

    pub fn conversation_budget_tokens(
        &self,
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
    ) -> usize {
        let envelope_breakdown = self.request_envelope_budget_breakdown(tools, limits);
        envelope_breakdown
            .input_budget_tokens()
            .saturating_sub(envelope_breakdown.total_input_tokens)
    }

    fn request_envelope_budget_breakdown(
        &self,
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
    ) -> RequestBudgetBreakdown {
        estimate_runtime_request_envelope(
            &self.system_messages,
            self.user_message.as_deref().unwrap_or_default(),
            tools,
            limits,
        )
    }

    fn agent_messages_with_history(
        &self,
        conversation_messages: &[HistoryMessage],
    ) -> Vec<AgentMessage> {
        let mut messages = self
            .system_messages
            .iter()
            .cloned()
            .map(AgentMessage::system)
            .collect::<Vec<_>>();
        messages.extend(
            conversation_messages
                .iter()
                .cloned()
                .map(|message| message.message),
        );
        if let Some(user_message) = self.user_message.clone() {
            messages.push(AgentMessage::user(user_message));
        }
        messages
    }

    fn into_agent_messages(self, conversation_messages: Vec<HistoryMessage>) -> Vec<AgentMessage> {
        let mut messages = self
            .system_messages
            .into_iter()
            .map(AgentMessage::system)
            .collect::<Vec<_>>();
        messages.extend(
            conversation_messages
                .into_iter()
                .map(|message| message.message),
        );
        if let Some(user_message) = self.user_message {
            messages.push(AgentMessage::user(user_message));
        }
        messages
    }
}

/// Forced trimming: compute how many tokens to free, then drop the oldest
/// non-system messages in bulk until the projected savings cover the excess.
impl RuntimeStepConversation {
    pub fn force_trim_to_fit_budget(
        &mut self,
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
        baseline: &TokenEstimateBaseline,
    ) -> bool {
        let breakdown = estimate_agent_turn_request(self.agent_messages(), tools, limits)
            .with_calibrated_input_tokens(baseline);
        if breakdown.within_context_window() {
            return true;
        }
        let excess = breakdown
            .total_with_reserve_tokens
            .saturating_sub(limits.context_window_tokens);
        if excess == 0 {
            return true;
        }
        let first_non_system = self
            .agent_messages
            .iter()
            .position(|message| !matches!(message, AgentMessage::System { .. }));
        let Some(start) = first_non_system else {
            return false;
        };
        let mut saved = 0usize;
        let mut cut = 0usize;
        for message in &self.agent_messages[start..] {
            if saved >= excess {
                break;
            }
            saved += estimate_agent_message_tokens(message);
            cut += 1;
        }
        if cut == 0 {
            return false;
        }
        self.agent_messages.drain(start..start + cut);
        let check = estimate_agent_turn_request(self.agent_messages(), tools, limits)
            .with_calibrated_input_tokens(baseline);
        check.within_context_window()
    }
}

impl RuntimeStepConversation {
    fn new(turn_draft: RuntimeTurnDraft, agent_messages: Vec<AgentMessage>) -> Self {
        Self {
            agent_messages,
            turn_draft,
        }
    }

    pub fn clone_agent_messages(&self) -> Vec<AgentMessage> {
        self.agent_messages.clone()
    }

    pub fn agent_messages(&self) -> &[AgentMessage] {
        &self.agent_messages
    }

    pub fn push_agent_message(&mut self, message: AgentMessage) {
        self.agent_messages.push(message);
    }

    pub fn push_history_message(&mut self, message: HistoryMessage) {
        self.turn_draft.push(message);
    }

    pub fn set_current_doing(&mut self, current_doing: impl Into<String>) {
        self.turn_draft.set_current_doing(current_doing);
    }

    pub fn history_messages(&self) -> &[HistoryMessage] {
        self.turn_draft.messages()
    }

    pub fn is_history_empty(&self) -> bool {
        self.turn_draft.is_empty()
    }

    pub fn into_turn_draft(self) -> RuntimeTurnDraft {
        self.turn_draft
    }

    pub async fn maybe_compact<F, Fut>(
        &mut self,
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
        baseline: &TokenEstimateBaseline,
        compact_for_overflow: bool,
        policy: RuntimeStepCompactionPolicy,
        mut build_summary: F,
    ) -> bool
    where
        F: FnMut(Vec<AgentMessage>, usize) -> Fut,
        Fut: Future<Output = Option<RuntimeCompactionOutcome>>,
    {
        let mut compacted_any = false;
        for _ in 0..policy.max_recoveries {
            let breakdown = estimate_agent_turn_request(self.agent_messages(), tools, limits)
                .with_calibrated_input_tokens(baseline);
            let needs_compaction = if compact_for_overflow {
                !breakdown.within_context_window()
            } else {
                breakdown.above_auto_compact_threshold()
            };
            if !needs_compaction {
                break;
            }
            if !self.compact_once(policy, &mut build_summary).await {
                break;
            }
            compacted_any = true;
        }
        compacted_any
    }

    async fn compact_once<F, Fut>(
        &mut self,
        policy: RuntimeStepCompactionPolicy,
        build_summary: &mut F,
    ) -> bool
    where
        F: FnMut(Vec<AgentMessage>, usize) -> Fut,
        Fut: Future<Output = Option<RuntimeCompactionOutcome>>,
    {
        let source_messages = self.agent_messages.clone();
        if source_messages.is_empty() {
            return false;
        }
        let has_non_system = source_messages
            .iter()
            .any(|message| !matches!(message, AgentMessage::System { .. }));
        if !has_non_system {
            return false;
        }

        let Some(outcome) = build_summary(source_messages.clone(), policy.summary_max_tokens).await
        else {
            return false;
        };

        self.agent_messages =
            rebuild_compacted_agent_messages(&source_messages, outcome.summary.clone());
        self.turn_draft.record_compaction(outcome.record);
        true
    }
}

impl RuntimeConversationCompactionPlan {
    pub fn source_messages(&self) -> &[HistoryMessage] {
        &self.source_messages
    }

    pub fn summary_max_tokens(&self) -> usize {
        self.summary_max_tokens
    }
}

fn rebuild_compacted_agent_messages(
    source_messages: &[AgentMessage],
    summary: String,
) -> Vec<AgentMessage> {
    let mut rebuilt = source_messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect::<Vec<_>>();
    rebuilt.push(AgentMessage::assistant(summary));
    rebuilt
}

impl RuntimeConversation {
    async fn new(bootstrap_focus: Option<String>, bootstrap_messages: Vec<HistoryMessage>) -> Self {
        let persistence = PersistenceStore::runtime().await;
        if let Some(conversation) = persistence
            .read_json_memory(RUNTIME_CONVERSATION_FILE_NAME, "runtime conversation")
            .await
        {
            return conversation;
        }
        if let Some(conversation) = persistence
            .read_postcard_memory(
                RUNTIME_CONVERSATION_LEGACY_FILE_NAME,
                "legacy runtime conversation",
            )
            .await
        {
            if let Err(err) = persistence
                .write_json_memory(RUNTIME_CONVERSATION_FILE_NAME, &conversation)
                .await
            {
                tracing::error!("migrate legacy runtime conversation to json failed: {err}");
            }
            return conversation;
        }
        Self {
            last_focus: bootstrap_focus,
            messages: bootstrap_messages,
            compaction_records: VecDeque::new(),
        }
    }

    pub fn append_turn(
        &mut self,
        current_doing: String,
        messages: Vec<HistoryMessage>,
        compaction_records: Vec<RuntimeCompactionRecord>,
    ) {
        if !current_doing.trim().is_empty() {
            self.last_focus = Some(current_doing);
        }
        self.messages.extend(messages);
        self.messages = normalize_runtime_prompt_messages(std::mem::take(&mut self.messages));
        for record in compaction_records {
            self.push_compaction_record(record);
        }
    }

    pub fn current_focus(&self) -> Option<String> {
        self.last_focus.clone()
    }

    pub fn clear(&mut self) {
        self.last_focus = None;
        self.messages.clear();
        self.compaction_records.clear();
    }

    pub fn take_for_memory(&mut self) -> Option<(String, Vec<HistoryMessage>)> {
        let messages = self.messages();
        if messages.is_empty() {
            self.clear();
            return None;
        }
        let current_doing = self
            .current_focus()
            .unwrap_or_else(|| "manual runtime conversation clear".to_string());
        self.clear();
        Some((current_doing, messages))
    }

    pub fn messages(&self) -> Vec<HistoryMessage> {
        normalize_runtime_prompt_messages(self.messages.clone())
    }

    pub fn select_messages_for_runtime(
        &self,
        max_tokens: usize,
        _min_messages: usize,
        summary_max_tokens: usize,
    ) -> Vec<HistoryMessage> {
        if max_tokens == 0 {
            return Vec::new();
        }
        let all_messages = self.messages();
        if history_messages_total_token_cost(&all_messages) <= max_tokens {
            return all_messages;
        }

        let summary_max_tokens = summary_max_tokens.min(max_tokens);
        if summary_max_tokens == 0 {
            return Vec::new();
        }
        let mut messages = Vec::new();
        if let Some(summary) =
            build_runtime_prompt_history_summary(&all_messages, summary_max_tokens)
        {
            messages.push(summary);
        }
        messages
    }

    fn force_trim_messages_to_fit_budget(
        &mut self,
        envelope: &RuntimeRequestEnvelope,
        injected_messages: &[HistoryMessage],
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
        baseline: &TokenEstimateBaseline,
    ) -> bool {
        let all_messages = self.messages();
        let mut request_messages = all_messages.clone();
        request_messages.extend(injected_messages.iter().cloned());
        let agent_messages = envelope.agent_messages_with_history(&request_messages);
        let breakdown = estimate_agent_turn_request(&agent_messages, tools, limits)
            .with_calibrated_input_tokens(baseline);
        if breakdown.within_context_window() {
            return true;
        }
        let excess = breakdown
            .total_with_reserve_tokens
            .saturating_sub(limits.context_window_tokens);
        if excess == 0 {
            return true;
        }
        let first_non_system = self
            .messages
            .iter()
            .position(|msg| !matches!(msg.message, AgentMessage::System { .. }));
        let Some(start) = first_non_system else {
            return false;
        };
        let mut saved = 0usize;
        let mut cut = 0usize;
        for message in &self.messages[start..] {
            if saved >= excess {
                break;
            }
            saved += history_message_token_cost(message);
            cut += 1;
        }
        if cut == 0 {
            return false;
        }
        self.messages.drain(start..start + cut);
        let check_all = self.messages();
        let mut check_request = check_all.clone();
        check_request.extend(injected_messages.iter().cloned());
        let check_agent = envelope.agent_messages_with_history(&check_request);
        let check = estimate_agent_turn_request(&check_agent, tools, limits)
            .with_calibrated_input_tokens(baseline);
        check.within_context_window()
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_compaction_for_request(
        &self,
        envelope: &RuntimeRequestEnvelope,
        injected_messages: &[HistoryMessage],
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
        baseline: &TokenEstimateBaseline,
        _min_messages: usize,
        summary_max_tokens: usize,
    ) -> Option<RuntimeConversationCompactionPlan> {
        let all_messages = self.messages();
        let mut request_messages = all_messages.clone();
        request_messages.extend(injected_messages.iter().cloned());
        let agent_messages = envelope.agent_messages_with_history(&request_messages);
        let breakdown = estimate_agent_turn_request(&agent_messages, tools, limits)
            .with_calibrated_input_tokens(baseline);
        if !breakdown.above_auto_compact_threshold() {
            return None;
        }
        let summary_max_tokens = summary_max_tokens
            .min(breakdown.input_budget_tokens())
            .min(breakdown.auto_compact_input_threshold_tokens());
        Self::compaction_plan_from_messages(all_messages, summary_max_tokens)
    }

    fn compaction_plan_from_messages(
        source_messages: Vec<HistoryMessage>,
        summary_max_tokens: usize,
    ) -> Option<RuntimeConversationCompactionPlan> {
        if summary_max_tokens == 0 {
            return None;
        }
        Some(RuntimeConversationCompactionPlan {
            source_messages,
            summary_max_tokens,
        })
    }

    fn apply_compaction(
        &mut self,
        plan: RuntimeConversationCompactionPlan,
        outcome: Option<RuntimeCompactionOutcome>,
    ) -> bool {
        let (summary, record) = match outcome {
            Some(outcome) => (
                HistoryMessage::assistant(outcome.summary),
                Some(outcome.record),
            ),
            None => {
                let Some(summary) = build_runtime_prompt_history_summary(
                    &plan.source_messages,
                    plan.summary_max_tokens,
                ) else {
                    return false;
                };
                (summary, None)
            }
        };

        self.messages.clear();
        self.messages.push(summary);
        self.messages = normalize_runtime_prompt_messages(std::mem::take(&mut self.messages));
        if let Some(record) = record {
            self.push_compaction_record(record);
        }
        true
    }

    fn push_compaction_record(&mut self, mut record: RuntimeCompactionRecord) {
        record.timestamp_ms = Utc::now().timestamp_millis();
        self.compaction_records.push_back(record);
        while self.compaction_records.len() > RUNTIME_COMPACTION_RECORD_LIMIT {
            self.compaction_records.pop_front();
        }
    }

    async fn sync_to_disk(&self) {
        let persistence = PersistenceStore::runtime().await;
        if let Err(err) = persistence
            .write_json_memory(RUNTIME_CONVERSATION_FILE_NAME, self)
            .await
        {
            tracing::error!("persist runtime conversation failed: {err}");
        }
    }
}

fn history_message_token_cost(message: &HistoryMessage) -> usize {
    match &message.message {
        AgentMessage::System { content } => {
            approx_token_count("system") + approx_token_count(content) + 4
        }
        AgentMessage::User { content } => {
            approx_token_count("user")
                + approx_token_count(content.as_text())
                + content.parts().len() * 1024
                + 4
        }
        AgentMessage::Assistant { content } => {
            approx_token_count("assistant") + approx_token_count(content) + 4
        }
        AgentMessage::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
        } => {
            approx_token_count("assistant")
                + approx_token_count(content.as_deref().unwrap_or_default())
                + approx_token_count(reasoning_content.as_deref().unwrap_or_default())
                + calls
                    .iter()
                    .map(|call| {
                        approx_token_count(&call.id)
                            + approx_token_count(&call.name)
                            + approx_token_count(&call.arguments.to_string())
                            + 8
                    })
                    .sum::<usize>()
                + 4
        }
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => {
            approx_token_count("tool")
                + approx_token_count(tool_call_id)
                + approx_token_count(name)
                + approx_token_count(content)
                + 8
        }
    }
}

fn history_messages_total_token_cost(messages: &[HistoryMessage]) -> usize {
    messages.iter().map(history_message_token_cost).sum()
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

fn history_message_content(message: &HistoryMessage) -> &str {
    message.text_content().unwrap_or_default()
}

fn trim_history_message_content(mut message: HistoryMessage) -> HistoryMessage {
    let trimmed = history_message_content(&message).trim().to_string();
    message.message = match message.message {
        AgentMessage::System { .. } => AgentMessage::system(trimmed),
        AgentMessage::User { content } => AgentMessage::user_content(content.with_text(trimmed)),
        AgentMessage::Assistant { .. } => AgentMessage::assistant(trimmed),
        AgentMessage::AssistantToolCallProtocol {
            reasoning_content,
            calls,
            ..
        } => AgentMessage::assistant_tool_call_protocol_with_reasoning(
            Some(trimmed),
            reasoning_content,
            calls,
        ),
        AgentMessage::Tool {
            tool_call_id, name, ..
        } => AgentMessage::tool(tool_call_id, name, trimmed),
    };
    message
}

fn normalize_runtime_prompt_messages(messages: Vec<HistoryMessage>) -> Vec<HistoryMessage> {
    let mut normalized: Vec<HistoryMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        let Some(message) = normalize_runtime_prompt_message(message) else {
            continue;
        };

        if let Some(previous) = normalized.last_mut() {
            if previous.message == message.message {
                continue;
            }

            if is_runtime_summary_message(previous) && is_runtime_summary_message(&message) {
                *previous = message;
                continue;
            }
        }

        normalized.push(message);
    }
    normalized
}

fn normalize_runtime_prompt_message(mut message: HistoryMessage) -> Option<HistoryMessage> {
    let visible_content = history_message_content(&message).trim().to_string();
    if visible_content.is_empty() {
        if !message.tool_call_ui_events.is_empty() {
            message.message = AgentMessage::assistant(summarize_tool_call_ui_events(
                &message.tool_call_ui_events,
            ));
        } else if let Some(tool_ui_event) = &message.tool_ui_event {
            message.message = AgentMessage::assistant(summarize_tool_ui_event(tool_ui_event));
        }
    }

    if message.is_tool() {
        let truncated = truncate_text_to_token_budget_with_notice(
            history_message_content(&message).trim(),
            RUNTIME_HISTORY_TOOL_MESSAGE_MAX_TOKENS,
            "... [tool output too long; runtime history truncated]",
        );
        if let AgentMessage::Tool {
            tool_call_id, name, ..
        } = &message.message
        {
            message.message = AgentMessage::tool(tool_call_id.clone(), name.clone(), truncated);
        }
    }

    if history_message_content(&message).trim().is_empty() {
        return None;
    }

    Some(trim_history_message_content(message))
}

fn summarize_tool_call_ui_events(events: &[ToolCallUiEvent]) -> String {
    let titles = events
        .iter()
        .map(tool_call_ui_event_title)
        .filter(|title| !title.trim().is_empty())
        .take(4)
        .map(summarize_runtime_inline_text)
        .collect::<Vec<_>>();
    if titles.is_empty() {
        "assistant tool-call protocol".to_string()
    } else {
        format!("assistant tool-call protocol: {}", titles.join(" | "))
    }
}

fn summarize_tool_ui_event(event: &ToolUiEvent) -> String {
    match event {
        ToolUiEvent::Exec(data) | ToolUiEvent::App(data) | ToolUiEvent::Error(data) => {
            summarize_runtime_inline_text(&data.title)
        }
        ToolUiEvent::CodingOpenProject(data) => {
            format!(
                "opened coding project {}",
                summarize_runtime_inline_text(&data.project_root)
            )
        }
        ToolUiEvent::CodingToolGroup(data) => format!(
            "{} with {} coding call(s)",
            summarize_runtime_inline_text(&data.title),
            data.calls.len()
        ),
        ToolUiEvent::CodingEdit(data) => format!(
            "edited code {} (+{} -{}, {} propagation review(s))",
            summarize_runtime_inline_text(&data.selector),
            data.added_lines,
            data.removed_lines,
            data.propagation_count
        ),
        ToolUiEvent::CodingReview(data) => summarize_runtime_inline_text(&data.title),
        ToolUiEvent::Browser(crate::tool_ui::BrowserUiData { title, .. }) => {
            summarize_runtime_inline_text(title)
        }
        ToolUiEvent::AppAttention(data) => match data.action {
            crate::tool_ui::AppAttentionUiAction::Focus => data
                .app
                .as_deref()
                .map(|app| format!("focused app {app}"))
                .unwrap_or_else(|| "focused app".to_string()),
            crate::tool_ui::AppAttentionUiAction::PutAway => "put away focused app".to_string(),
        },
        ToolUiEvent::Plan(PlanUiData { steps }) => format!("plan with {} step(s)", steps.len()),
        ToolUiEvent::CreatePrimitiveSpec(CreatePrimitiveSpecUiData { primitive_id }) => {
            format!("created primitive spec {primitive_id}")
        }
        ToolUiEvent::ActivatePrimitive(ActivatePrimitiveUiData { primitive_id }) => {
            format!("activated primitive {primitive_id}")
        }
        ToolUiEvent::Terminal(data) => summarize_runtime_inline_text(&data.title),
        ToolUiEvent::Patch(data) => summarize_runtime_inline_text(&data.summary_line),
        ToolUiEvent::Telegram(data) => summarize_runtime_inline_text(&data.title),
        ToolUiEvent::Reply(data) => data
            .message_lines
            .iter()
            .find(|line| !line.trim().is_empty())
            .map(|line| summarize_runtime_inline_text(line))
            .unwrap_or_else(|| "reply submitted".to_string()),
    }
}

fn tool_call_ui_event_title(event: &ToolCallUiEvent) -> &str {
    match event {
        ToolCallUiEvent::Exec(ToolUiData { title, .. })
        | ToolCallUiEvent::Plan(ToolUiData { title, .. })
        | ToolCallUiEvent::CreatePrimitiveSpec(ToolUiData { title, .. })
        | ToolCallUiEvent::ActivatePrimitive(ToolUiData { title, .. })
        | ToolCallUiEvent::App(ToolUiData { title, .. })
        | ToolCallUiEvent::Error(ToolUiData { title, .. }) => title,
        ToolCallUiEvent::Terminal(TerminalUiData { title, .. }) => title,
        ToolCallUiEvent::Browser(crate::tool_ui::BrowserUiData { title, .. }) => title,
        ToolCallUiEvent::Patch(PatchUiData { summary_line, .. }) => summary_line,
        ToolCallUiEvent::Telegram(TelegramUiData { title, .. }) => title,
    }
}

fn is_runtime_summary_message(message: &HistoryMessage) -> bool {
    let content = history_message_content(message);
    message.is_assistant()
        && (content.starts_with(RUNTIME_HISTORY_SUMMARY_PREFIX)
            || content.starts_with(MID_TURN_SUMMARY_PREFIX))
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

fn summarize_prompt_message_for_history_compaction(message: &HistoryMessage) -> Option<String> {
    match &message.message {
        AgentMessage::System { .. } => Some(format!(
            "system: {}",
            summarize_runtime_inline_text(history_message_content(message))
        )),
        AgentMessage::User { .. } => Some(format!(
            "user: {}",
            summarize_runtime_inline_text(history_message_content(message))
        )),
        AgentMessage::Assistant { .. } | AgentMessage::AssistantToolCallProtocol { .. } => {
            Some(format!(
                "assistant: {}",
                summarize_runtime_inline_text(history_message_content(message))
            ))
        }
        AgentMessage::Tool { .. } => Some(format!(
            "tool: {}",
            summarize_tool_message_content(history_message_content(message))
        )),
    }
}

fn build_runtime_prompt_history_summary(
    messages: &[HistoryMessage],
    max_tokens: usize,
) -> Option<HistoryMessage> {
    let rendered = messages
        .iter()
        .filter_map(summarize_prompt_message_for_history_compaction)
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
    Some(HistoryMessage::assistant(truncate_text_to_token_budget(
        &lines.join("\n"),
        max_tokens.max(1),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_level_pre_turn_compaction_accounts_for_injected_context() {
        let conversation = RuntimeConversation {
            last_focus: Some("test".to_string()),
            messages: vec![HistoryMessage::assistant("runtime history".repeat(12))],
            compaction_records: VecDeque::new(),
        };
        let envelope = RuntimeRequestEnvelope::from_system_messages(vec!["system".repeat(8)]);
        let injected_messages = vec![HistoryMessage::user(
            "<preturn_context>".to_string() + &"x".repeat(180),
        )];
        let tools = Vec::<AgentToolSpec>::new();
        let limits = RequestBudgetLimits {
            context_window_tokens: 1_000,
            auto_compact_threshold_tokens: 100,
            reserved_output_tokens: 100,
        };

        let history_only_messages = conversation.messages();
        let history_only_budget = envelope.conversation_budget_tokens(&tools, limits);
        assert!(history_messages_total_token_cost(&history_only_messages) <= history_only_budget);
        assert!(
            conversation
                .plan_compaction_for_request(
                    &envelope,
                    &injected_messages,
                    &tools,
                    limits,
                    &TokenEstimateBaseline::default(),
                    0,
                    80,
                )
                .is_some()
        );
    }

    #[test]
    fn runtime_conversation_compaction_rebuilds_history_as_summary_only() {
        let mut conversation = RuntimeConversation {
            last_focus: Some("test".to_string()),
            messages: vec![
                HistoryMessage::user("user one"),
                HistoryMessage::assistant("assistant one"),
                HistoryMessage::tool(
                    "call-1",
                    "tool-one",
                    "tool output one",
                    ToolUiEvent::Exec(ToolUiData {
                        title: "tool output".to_string(),
                        body_lines: Vec::new(),
                    }),
                ),
                HistoryMessage::user("user two"),
                HistoryMessage::assistant("assistant two"),
                HistoryMessage::tool(
                    "call-2",
                    "tool-two",
                    "tool output two",
                    ToolUiEvent::Exec(ToolUiData {
                        title: "tool output".to_string(),
                        body_lines: Vec::new(),
                    }),
                ),
            ],
            compaction_records: VecDeque::new(),
        };

        let all_messages = conversation.messages();
        assert!(history_messages_total_token_cost(&all_messages) > 20);
        let plan = RuntimeConversation::compaction_plan_from_messages(all_messages, 8)
            .expect("expected compaction plan");

        let applied = conversation.apply_compaction(
            plan,
            Some(RuntimeCompactionOutcome {
                summary: "summary".to_string(),
                record: RuntimeCompactionRecord {
                    timestamp_ms: 0,
                    phase: RuntimeCompactionPhase::PreTurn,
                    reason: RuntimeCompactionReason::BudgetThreshold,
                    reinjection_strategy:
                        RuntimeCompactionReinjectionStrategy::RebuildRuntimeEnvelope,
                    source_item_count: 2,
                    source_message_count: 6,
                    trimmed_item_count: 0,
                    retained_user_message_count: 0,
                    used_fallback_summary: false,
                    summary: "summary".to_string(),
                },
            }),
        );
        assert!(applied);
        assert_eq!(conversation.messages.len(), 1);
        assert!(
            conversation
                .messages
                .last()
                .map(HistoryMessage::is_assistant)
                .unwrap_or(false)
        );
        assert!(
            conversation
                .messages
                .iter()
                .all(|message| !message.is_tool())
        );
    }

    #[test]
    fn rebuild_compacted_agent_messages_drops_runtime_user_context_and_tool_history() {
        let messages = vec![
            AgentMessage::system("system"),
            AgentMessage::user("claimed input"),
            AgentMessage::user("<preturn_context>context</preturn_context>"),
            AgentMessage::assistant("assistant detail"),
            AgentMessage::tool("call-1", "shell", "tool output"),
        ];

        let rebuilt = rebuild_compacted_agent_messages(&messages, "summary".to_string());
        assert_eq!(rebuilt.len(), 2);
        assert!(matches!(rebuilt[0], AgentMessage::System { .. }));
        assert!(matches!(rebuilt[1], AgentMessage::Assistant { .. }));
        assert!(rebuilt.iter().all(|message| {
            !matches!(
                message,
                AgentMessage::User { .. }
                    | AgentMessage::Tool { .. }
                    | AgentMessage::AssistantToolCallProtocol { .. }
            )
        }));
    }

    #[test]
    fn normalizing_tool_call_history_preserves_reasoning_content() {
        let message = HistoryMessage {
            message: AgentMessage::assistant_tool_call_protocol_with_reasoning(
                Some("  checking state  ".to_string()),
                Some("provider reasoning".to_string()),
                vec![crate::reasoning::runtime::AgentToolCall {
                    id: "call_1".to_string(),
                    name: "terminal_exec".to_string(),
                    arguments: serde_json::json!({ "cmd": "pwd" }),
                }],
            ),
            tool_ui_event: None,
            tool_call_ui_events: Vec::new(),
        };

        let normalized = normalize_runtime_prompt_message(message).expect("message should remain");
        match normalized.message {
            AgentMessage::AssistantToolCallProtocol {
                content,
                reasoning_content,
                ..
            } => {
                assert_eq!(content.as_deref(), Some("checking state"));
                assert_eq!(reasoning_content.as_deref(), Some("provider reasoning"));
            }
            _ => panic!("expected assistant tool-call protocol"),
        }
    }

    #[test]
    fn memory_json_round_trips_tool_call_arguments() {
        let tool_call = crate::reasoning::runtime::AgentToolCall {
            id: "call_1".to_string(),
            name: "terminal_exec".to_string(),
            arguments: serde_json::json!({
                "cmd": "printf hi",
                "env": { "A": "B" },
                "timeout_ms": 1000
            }),
        };
        let conversation = RuntimeConversation {
            last_focus: Some("json persistence".to_string()),
            messages: vec![HistoryMessage {
                message: AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    Some("checking state".to_string()),
                    Some("reasoning".to_string()),
                    vec![tool_call.clone()],
                ),
                tool_ui_event: None,
                tool_call_ui_events: Vec::new(),
            }],
            compaction_records: VecDeque::new(),
        };
        let bytes = serde_json::to_vec_pretty(&conversation).expect("serialize conversation");
        let restored: RuntimeConversation =
            serde_json::from_slice(&bytes).expect("deserialize conversation");

        match &restored.messages[0].message {
            AgentMessage::AssistantToolCallProtocol { calls, .. } => {
                assert_eq!(calls[0].arguments, tool_call.arguments);
            }
            _ => panic!("expected assistant tool-call protocol"),
        }
    }
}
