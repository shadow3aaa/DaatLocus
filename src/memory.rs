//! Runtime conversation state.
use std::{collections::VecDeque, future::Future};

use crate::{
    context_budget::{
        RequestBudgetBreakdown, RequestBudgetLimits, TokenEstimateBaseline,
        estimate_agent_turn_request, estimate_runtime_request_envelope,
        truncate_text_to_token_budget_with_notice,
    },
    dashboard::SessionActivityEvent,
    persistence::PersistenceStore,
    reasoning::{
        prompts::HISTORY_COMPACTION_SUMMARY_PREFIX,
        runtime::{AgentMessage, AgentToolSpec, HistoryMessage},
    },
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

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
    pub summary: String,
}

#[derive(Clone, Copy)]
pub struct PlanCompactionInput<'a> {
    pub envelope: &'a RuntimeRequestEnvelope,
    pub injected_messages: &'a [HistoryMessage],
    pub tools: &'a [AgentToolSpec],
    pub limits: RequestBudgetLimits,
    pub baseline: &'a TokenEstimateBaseline,
    pub min_messages: usize,
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

    pub async fn with_session(session_id: &str) -> Self {
        let runtime_conversation =
            RuntimeConversation::with_session(None, Vec::new(), session_id).await;
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

    pub fn checkpoint_runtime_turn(&mut self, draft: RuntimeTurnDraft) {
        let (current_doing, messages, compaction_records) = draft.into_parts();
        self.runtime_conversation_mut()
            .append_turn(current_doing, messages, compaction_records);
    }

    pub async fn sync_runtime_conversation(&self) {
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
        RuntimeStepConversation::with_turn_draft(self.begin_runtime_turn(), agent_messages)
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

    pub fn plan_runtime_conversation_compaction_for_request(
        &self,
        input: PlanCompactionInput<'_>,
    ) -> Option<RuntimeConversationCompactionPlan> {
        self.runtime_conversation.plan_compaction_for_request(input)
    }

    pub async fn apply_runtime_conversation_compaction(
        &mut self,
        plan: RuntimeConversationCompactionPlan,
        outcome: RuntimeCompactionOutcome,
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

    pub const fn runtime_conversation(&self) -> &RuntimeConversation {
        &self.runtime_conversation
    }

    pub const fn runtime_conversation_mut(&mut self) -> &mut RuntimeConversation {
        &mut self.runtime_conversation
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
    #[serde(default, skip)]
    session_id: Option<String>,
    last_focus: Option<String>,
    messages: Vec<HistoryMessage>,
    #[serde(default)]
    compaction_records: VecDeque<RuntimeCompactionRecord>,
}

impl RuntimeTurnDraft {
    const fn new(current_doing: String) -> Self {
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

    pub fn record_compaction(&mut self, record: RuntimeCompactionRecord) {
        self.compaction_records.push(record);
    }

    fn into_parts(self) -> (String, Vec<HistoryMessage>, Vec<RuntimeCompactionRecord>) {
        (self.current_doing, self.messages, self.compaction_records)
    }

    fn take_checkpoint(&mut self) -> Self {
        Self {
            current_doing: self.current_doing.clone(),
            messages: std::mem::take(&mut self.messages),
            compaction_records: std::mem::take(&mut self.compaction_records),
        }
    }
}

impl RuntimeRequestEnvelope {
    pub const fn from_system_messages(system_messages: Vec<String>) -> Self {
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

impl RuntimeStepConversation {
    pub const fn new(agent_messages: Vec<AgentMessage>) -> Self {
        Self::with_turn_draft(RuntimeTurnDraft::new(String::new()), agent_messages)
    }

    const fn with_turn_draft(
        turn_draft: RuntimeTurnDraft,
        agent_messages: Vec<AgentMessage>,
    ) -> Self {
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

    pub fn into_turn_draft(self) -> RuntimeTurnDraft {
        self.turn_draft
    }

    pub fn take_turn_checkpoint(&mut self) -> RuntimeTurnDraft {
        self.turn_draft.take_checkpoint()
    }

    pub async fn maybe_compact<F, Fut>(
        &mut self,
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
        baseline: &TokenEstimateBaseline,
        compact_for_overflow: bool,
        policy: RuntimeStepCompactionPolicy,
        mut build_summary: F,
    ) -> Result<bool, String>
    where
        F: FnMut(Vec<AgentMessage>, usize) -> Fut,
        Fut: Future<Output = Result<RuntimeCompactionOutcome, String>>,
    {
        if compact_for_overflow {
            self.compact_once(policy, &mut build_summary).await?;
            return Ok(true);
        }

        let mut compacted_any = false;
        for _ in 0..policy.max_recoveries {
            let breakdown = estimate_agent_turn_request(self.agent_messages(), tools, limits)
                .with_conservative_calibrated_input_tokens(baseline);
            if !breakdown.above_auto_compact_threshold() {
                break;
            }
            self.compact_once(policy, &mut build_summary).await?;
            compacted_any = true;
        }
        Ok(compacted_any)
    }

    async fn compact_once<F, Fut>(
        &mut self,
        policy: RuntimeStepCompactionPolicy,
        build_summary: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(Vec<AgentMessage>, usize) -> Fut,
        Fut: Future<Output = Result<RuntimeCompactionOutcome, String>>,
    {
        let source_messages = self.agent_messages.clone();
        if source_messages.is_empty() {
            return Err("runtime compaction has no messages to summarize".to_string());
        }
        let has_non_system = source_messages
            .iter()
            .any(|message| !matches!(message, AgentMessage::System { .. }));
        if !has_non_system {
            return Err("runtime compaction has no non-system messages to summarize".to_string());
        }

        let outcome = build_summary(source_messages.clone(), policy.summary_max_tokens).await?;
        self.agent_messages =
            rebuild_compacted_agent_messages(&source_messages, outcome.summary.clone());
        self.turn_draft.record_compaction(outcome.record);
        Ok(())
    }
}

impl RuntimeConversationCompactionPlan {
    pub fn source_messages(&self) -> &[HistoryMessage] {
        &self.source_messages
    }
    #[cfg(test)]
    pub(crate) const fn for_test(source_messages: Vec<HistoryMessage>) -> Self {
        Self { source_messages }
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
    rebuilt.push(AgentMessage::user(summary));
    rebuilt
}

impl RuntimeConversation {
    async fn new(bootstrap_focus: Option<String>, bootstrap_messages: Vec<HistoryMessage>) -> Self {
        Self::open_with_session(bootstrap_focus, bootstrap_messages, None).await
    }

    async fn with_session(
        bootstrap_focus: Option<String>,
        bootstrap_messages: Vec<HistoryMessage>,
        session_id: &str,
    ) -> Self {
        Self::open_with_session(
            bootstrap_focus,
            bootstrap_messages,
            Some(session_id.to_string()),
        )
        .await
    }

    async fn open_with_session(
        bootstrap_focus: Option<String>,
        bootstrap_messages: Vec<HistoryMessage>,
        session_id: Option<String>,
    ) -> Self {
        let persistence = PersistenceStore::for_session(session_id.as_deref()).await;
        if let Some(conversation) = persistence
            .read_json_memory::<Self>(RUNTIME_CONVERSATION_FILE_NAME, "runtime conversation")
            .await
        {
            return conversation.with_runtime_session(session_id);
        }
        if let Some(conversation) = persistence
            .read_postcard_memory::<Self>(
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
            return conversation.with_runtime_session(session_id);
        }
        Self {
            session_id,
            last_focus: bootstrap_focus,
            messages: bootstrap_messages,
            compaction_records: VecDeque::new(),
        }
    }

    fn with_runtime_session(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
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
        _max_tokens: usize,
        _min_messages: usize,
        _summary_max_tokens: usize,
    ) -> Vec<HistoryMessage> {
        self.messages()
    }

    fn plan_compaction_for_request(
        &self,
        input: PlanCompactionInput<'_>,
    ) -> Option<RuntimeConversationCompactionPlan> {
        let _ = input.min_messages;
        let all_messages = self.messages();
        let mut request_messages = all_messages.clone();
        request_messages.extend(input.injected_messages.iter().cloned());
        let agent_messages = input
            .envelope
            .agent_messages_with_history(&request_messages);
        let breakdown = estimate_agent_turn_request(&agent_messages, input.tools, input.limits)
            .with_conservative_calibrated_input_tokens(input.baseline);
        if !breakdown.above_auto_compact_threshold() {
            return None;
        }
        Self::compaction_plan_from_messages(all_messages)
    }

    fn compaction_plan_from_messages(
        source_messages: Vec<HistoryMessage>,
    ) -> Option<RuntimeConversationCompactionPlan> {
        if source_messages.is_empty() {
            return None;
        }
        Some(RuntimeConversationCompactionPlan { source_messages })
    }

    fn apply_compaction(
        &mut self,
        _plan: RuntimeConversationCompactionPlan,
        outcome: RuntimeCompactionOutcome,
    ) -> bool {
        self.messages.clear();
        self.messages.push(HistoryMessage::user(outcome.summary));
        self.messages = normalize_runtime_prompt_messages(std::mem::take(&mut self.messages));
        self.push_compaction_record(outcome.record);
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
        let persistence = PersistenceStore::for_session(self.session_id.as_deref()).await;
        if let Err(err) = persistence
            .write_json_memory(RUNTIME_CONVERSATION_FILE_NAME, self)
            .await
        {
            tracing::error!("persist runtime conversation failed: {err}");
        }
    }
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
    if let AgentMessage::AssistantToolCallProtocol { content, calls, .. } = &mut message.message
        && !calls.is_empty()
    {
        *content = content
            .take()
            .map(|content| content.trim().to_string())
            .filter(|content| !content.is_empty());
        return Some(message);
    }

    let visible_content = history_message_content(&message).trim().to_string();
    if visible_content.is_empty() {
        if !message.tool_call_activity_events.is_empty() {
            message.message = AgentMessage::assistant(summarize_tool_call_activity_events(
                &message.tool_call_activity_events,
            ));
        } else if let Some(activity_event) = &message.activity_event {
            message.message = AgentMessage::assistant(summarize_activity_event(activity_event));
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

fn summarize_tool_call_activity_events(events: &[SessionActivityEvent]) -> String {
    let titles = events
        .iter()
        .map(activity_event_title)
        .filter(|title| !title.trim().is_empty())
        .take(4)
        .map(|title| summarize_runtime_inline_text(&title))
        .collect::<Vec<_>>();
    if titles.is_empty() {
        "assistant tool-call protocol".to_string()
    } else {
        format!("assistant tool-call protocol: {}", titles.join(" | "))
    }
}

fn summarize_activity_event(event: &SessionActivityEvent) -> String {
    match event {
        SessionActivityEvent::Assistant(data) => summarize_runtime_inline_text(&data.content),
        SessionActivityEvent::GenericApp(data) => summarize_runtime_inline_text(&data.title),
        SessionActivityEvent::ExecResult(data) => summarize_runtime_inline_text(&data.title),
        SessionActivityEvent::LiveExec(data) => summarize_runtime_inline_text(&data.title),
        SessionActivityEvent::TerminalWait(data) => summarize_runtime_inline_text(&data.title),
        SessionActivityEvent::Warning(data) | SessionActivityEvent::Error(data) => {
            summarize_runtime_inline_text(&data.title)
        }
        SessionActivityEvent::User(data) => summarize_runtime_inline_text(&data.content),
        SessionActivityEvent::CodingOpenProject(data) => {
            format!(
                "opened coding project {}",
                summarize_runtime_inline_text(&data.project_root)
            )
        }
        SessionActivityEvent::Explored(data) => format!(
            "{} with {} call(s)",
            summarize_runtime_inline_text(&data.title),
            data.calls.len()
        ),
        SessionActivityEvent::CodingEdit(data) => {
            let title = if data.title.trim().is_empty() {
                "edited files"
            } else {
                data.title.trim()
            };
            if data.propagation_count > 0 {
                format!(
                    "{} {} (+{} -{}, {} propagation review(s))",
                    summarize_runtime_inline_text(title),
                    summarize_runtime_inline_text(&data.selector),
                    data.added_lines,
                    data.removed_lines,
                    data.propagation_count
                )
            } else {
                format!(
                    "{} {} (+{} -{})",
                    summarize_runtime_inline_text(title),
                    summarize_runtime_inline_text(&data.selector),
                    data.added_lines,
                    data.removed_lines
                )
            }
        }
        SessionActivityEvent::CodingReview(data) => summarize_runtime_inline_text(&data.title),
        SessionActivityEvent::Browser(data) => summarize_runtime_inline_text(&data.title),
        SessionActivityEvent::LiveBrowser(data) => summarize_runtime_inline_text(&data.title),
        SessionActivityEvent::WebSearch(data) => {
            format!("web search {}", summarize_runtime_inline_text(&data.query))
        }
        SessionActivityEvent::PlanResult(data) => format!("plan with {} step(s)", data.steps.len()),
        SessionActivityEvent::Patch(data) => summarize_runtime_inline_text(&data.summary_line),
        SessionActivityEvent::Telegram(data) => summarize_runtime_inline_text(&data.title),
        SessionActivityEvent::Reply(data) => data
            .message_lines
            .iter()
            .find(|line| !line.trim().is_empty())
            .map_or_else(
                || "reply submitted".to_string(),
                |line| summarize_runtime_inline_text(line),
            ),
        SessionActivityEvent::Thinking(data) => summarize_runtime_inline_text(&data.content),
        SessionActivityEvent::RuntimeStatus(data) => summarize_runtime_inline_text(&data.label),
        SessionActivityEvent::Workflow(data) => format!(
            "workflow {}: {:?}",
            summarize_runtime_inline_text(&data.workflow_id),
            data.status
        ),
    }
}

fn activity_event_title(event: &SessionActivityEvent) -> String {
    summarize_activity_event(event)
}

fn is_runtime_summary_message(message: &HistoryMessage) -> bool {
    message.is_assistant()
        && message
            .text_content()
            .unwrap_or_default()
            .starts_with(HISTORY_COMPACTION_SUMMARY_PREFIX)
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
            session_id: None,
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

        assert!(
            conversation
                .plan_compaction_for_request(PlanCompactionInput {
                    envelope: &envelope,
                    injected_messages: &injected_messages,
                    tools: &tools,
                    limits,
                    baseline: &TokenEstimateBaseline::default(),
                    min_messages: 0,
                })
                .is_some()
        );
    }

    #[test]
    fn low_observed_baseline_does_not_hide_pre_turn_compaction() {
        let conversation = RuntimeConversation {
            last_focus: Some("test".to_string()),
            messages: vec![HistoryMessage::assistant("runtime history ".repeat(1_000))],
            compaction_records: VecDeque::new(),
            session_id: None,
        };
        let envelope = RuntimeRequestEnvelope::from_system_messages(vec!["system".to_string()]);
        let tools = Vec::<AgentToolSpec>::new();
        let limits = RequestBudgetLimits {
            context_window_tokens: 2_000,
            auto_compact_threshold_tokens: 1_000,
            reserved_output_tokens: 100,
        };
        let baseline = TokenEstimateBaseline {
            estimated_input_tokens: 10_000,
            observed_input_tokens: Some(1),
        };

        assert!(
            conversation
                .plan_compaction_for_request(PlanCompactionInput {
                    envelope: &envelope,
                    injected_messages: &[],
                    tools: &tools,
                    limits,
                    baseline: &baseline,
                    min_messages: 0,
                })
                .is_some()
        );
    }

    #[tokio::test]
    async fn low_observed_baseline_does_not_hide_overflow_compaction() {
        let mut runtime_step = RuntimeStepConversation::new(vec![
            AgentMessage::system("system"),
            AgentMessage::user("x".repeat(10_000)),
        ]);
        let limits = RequestBudgetLimits {
            context_window_tokens: 1_000,
            auto_compact_threshold_tokens: 900,
            reserved_output_tokens: 100,
        };
        let baseline = TokenEstimateBaseline {
            estimated_input_tokens: 10_000,
            observed_input_tokens: Some(1),
        };

        let compacted = runtime_step
            .maybe_compact(
                &[],
                limits,
                &baseline,
                true,
                RuntimeStepCompactionPolicy {
                    summary_max_tokens: 80,
                    max_recoveries: 1,
                },
                |_messages, _max_tokens| async {
                    Ok(RuntimeCompactionOutcome {
                        summary: "summary".to_string(),
                        record: RuntimeCompactionRecord {
                            timestamp_ms: 0,
                            phase: RuntimeCompactionPhase::MidTurn,
                            reason: RuntimeCompactionReason::OverflowRecovery,
                            reinjection_strategy:
                                RuntimeCompactionReinjectionStrategy::PreserveSystemOnly,
                            source_item_count: 1,
                            source_message_count: 2,
                            trimmed_item_count: 0,
                            retained_user_message_count: 0,
                            summary: "summary".to_string(),
                        },
                    })
                },
            )
            .await
            .expect("overflow compaction should succeed");

        assert!(compacted);
    }

    #[test]
    fn runtime_conversation_compaction_rebuilds_history_as_summary_only() {
        let mut conversation = RuntimeConversation {
            last_focus: Some("test".to_string()),
            messages: vec![
                HistoryMessage::user("user one"),
                HistoryMessage::assistant("assistant one"),
                HistoryMessage::tool("call-1", "tool-one", "tool output one", None),
                HistoryMessage::user("user two"),
                HistoryMessage::assistant("assistant two"),
                HistoryMessage::tool("call-2", "tool-two", "tool output two", None),
            ],
            compaction_records: VecDeque::new(),
            session_id: None,
        };

        let all_messages = conversation.messages();
        let plan = RuntimeConversation::compaction_plan_from_messages(all_messages)
            .expect("expected compaction plan");

        let applied = conversation.apply_compaction(
            plan,
            RuntimeCompactionOutcome {
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
                    summary: "summary".to_string(),
                },
            },
        );
        assert!(applied);
        assert_eq!(conversation.messages.len(), 1);
        assert!(
            conversation
                .messages
                .last()
                .is_some_and(HistoryMessage::is_user)
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
        assert!(matches!(rebuilt[1], AgentMessage::User { .. }));
        assert!(rebuilt.iter().all(|message| {
            !matches!(
                message,
                AgentMessage::Tool { .. }
                    | AgentMessage::Assistant { .. }
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
            activity_event: None,
            tool_call_activity_events: Vec::new(),
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
    fn normalizing_tool_call_history_preserves_protocol_without_visible_text() {
        let message = HistoryMessage {
            message: AgentMessage::assistant_tool_call_protocol_with_reasoning(
                None,
                Some("provider reasoning".to_string()),
                vec![crate::reasoning::runtime::AgentToolCall {
                    id: "call_1".to_string(),
                    name: "terminal_exec".to_string(),
                    arguments: serde_json::json!({ "cmd": "pwd" }),
                }],
            ),
            activity_event: None,
            tool_call_activity_events: Vec::new(),
        };

        let normalized = normalize_runtime_prompt_message(message).expect("message should remain");
        assert!(matches!(
            normalized.message,
            AgentMessage::AssistantToolCallProtocol { content: None, calls, .. }
                if calls.len() == 1 && calls[0].id == "call_1"
        ));
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
                activity_event: None,
                tool_call_activity_events: Vec::new(),
            }],
            compaction_records: VecDeque::new(),
            session_id: None,
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
