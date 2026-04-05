//! 此模块定义运行时会话状态与 hindsight retain 队列。
use std::{collections::VecDeque, fmt::Display, future::Future};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    context_budget::{
        RequestBudgetLimits, approx_token_count, estimate_agent_turn_request,
        estimate_runtime_request_envelope, truncate_text_to_token_budget,
    },
    hindsight::{HindsightRetainItem, HindsightRetainJob},
    reasoning::runtime::{AgentMessage, AgentToolSpec, PromptMessage, PromptRole},
    spinova_paths::spinova_paths,
    tool_ui::{
        PatchUiData, TelegramUiData, TerminalUiData, ToolCallUiEvent, ToolUiData, ToolUiEvent,
    },
};

const RUNTIME_HISTORY_SUMMARY_PREFIX: &str = "Earlier runtime history summary:";
const MID_TURN_SUMMARY_PREFIX: &str = "Earlier tool/context progress summary:";
const RUNTIME_CONVERSATION_FILE_NAME: &str = "runtime_conversation";
const HINDSIGHT_QUEUE_FILE_NAME: &str = "hindsight_queue";

pub struct Memory {
    runtime_conversation: RuntimeConversation,
    hindsight_queue: HindsightQueue,
}

pub struct MemoryRetainPlan {
    pub jobs: Vec<HindsightRetainJob>,
    pub must_flush_before_continue: bool,
}

pub struct RuntimeTurnDraft {
    current_doing: String,
    messages: Vec<PromptMessage>,
}

pub struct RuntimeRequestEnvelope {
    system_messages: Vec<String>,
    user_message: String,
}

pub struct RuntimeStepConversation {
    agent_messages: Vec<AgentMessage>,
    turn_draft: RuntimeTurnDraft,
}

pub struct RuntimeConversationCompactionPlan {
    omitted_prefix: Vec<PromptMessage>,
    selected_tail: Vec<PromptMessage>,
    summary_max_tokens: usize,
}

#[derive(Clone, Copy)]
pub struct RuntimeStepCompactionPolicy {
    pub keep_tool_cycles: usize,
    pub keep_messages_without_tool_cycles: usize,
    pub summary_max_tokens: usize,
    pub max_recoveries: usize,
}

const HINDSIGHT_RETAIN_BACKLOG_LIMIT: usize = 3;

impl Memory {
    pub async fn new() -> Self {
        let mut hindsight_queue = HindsightQueue::new().await;
        hindsight_queue.reset_inflight_retain_state();
        let runtime_conversation = RuntimeConversation::new(
            hindsight_queue.current_focus(),
            hindsight_queue.bootstrap_messages(),
        )
        .await;
        Self {
            runtime_conversation,
            hindsight_queue,
        }
    }

    pub async fn record_agent_turn(
        &mut self,
        current_doing: String,
        messages: Vec<PromptMessage>,
    ) -> MemoryRetainPlan {
        self.runtime_conversation_mut()
            .append_turn(current_doing.clone(), messages.clone());
        self.hindsight_queue.push_turn(current_doing, messages);
        let jobs = self.collect_pending_retain_jobs();
        let must_flush_before_continue =
            self.hindsight_queue.retain_backlog_count() >= HINDSIGHT_RETAIN_BACKLOG_LIMIT;
        MemoryRetainPlan {
            jobs,
            must_flush_before_continue,
        }
    }

    pub fn current_thread_focus(&self) -> Option<String> {
        self.runtime_conversation()
            .current_focus()
            .or_else(|| self.hindsight_queue.current_focus())
    }

    pub fn trail(&self) -> Vec<String> {
        self.hindsight_queue
            .trail
            .clone()
            .into_iter()
            .flat_map(|item| item.render_messages())
            .collect()
    }

    pub fn runtime_conversation_messages(&self) -> Vec<PromptMessage> {
        self.runtime_conversation().messages()
    }

    pub fn begin_runtime_turn(&self) -> RuntimeTurnDraft {
        RuntimeTurnDraft::new(
            self.current_thread_focus()
                .unwrap_or_else(|| "等待下一轮工具决策".to_string()),
        )
    }

    pub fn begin_runtime_step(&self, agent_messages: Vec<AgentMessage>) -> RuntimeStepConversation {
        RuntimeStepConversation::new(self.begin_runtime_turn(), agent_messages)
    }

    pub fn begin_runtime_step_from_parts(
        &self,
        envelope: RuntimeRequestEnvelope,
        conversation_messages: Vec<PromptMessage>,
    ) -> RuntimeStepConversation {
        self.begin_runtime_step(envelope.into_agent_messages(conversation_messages))
    }

    pub async fn commit_runtime_turn(&mut self, draft: RuntimeTurnDraft) -> MemoryRetainPlan {
        let (current_doing, messages) = draft.into_parts();
        self.record_agent_turn(current_doing, messages).await
    }

    pub fn plan_runtime_conversation_compaction(
        &self,
        max_tokens: usize,
        min_messages: usize,
        summary_max_tokens: usize,
    ) -> Option<RuntimeConversationCompactionPlan> {
        self.runtime_conversation
            .plan_compaction(max_tokens, min_messages, summary_max_tokens)
    }

    pub fn apply_runtime_conversation_compaction(
        &mut self,
        plan: RuntimeConversationCompactionPlan,
        summary: Option<PromptMessage>,
    ) -> bool {
        self.runtime_conversation.apply_compaction(plan, summary)
    }

    pub fn runtime_conversation_slice(
        &self,
        max_tokens: usize,
        min_messages: usize,
        summary_max_tokens: usize,
    ) -> Vec<PromptMessage> {
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

    pub fn retain_backlog_count(&self) -> usize {
        self.hindsight_queue.retain_backlog_count()
    }

    pub fn should_block_new_turns_on_retain_backlog(&self) -> bool {
        self.retain_backlog_count() >= HINDSIGHT_RETAIN_BACKLOG_LIMIT
    }

    pub fn mark_queued_retained(&mut self) {
        self.hindsight_queue.mark_queued_retained();
    }

    pub async fn shutdown(self) {
        self.runtime_conversation.sync_to_disk().await;
        self.hindsight_queue.sync_to_disk().await;
    }

    fn collect_pending_retain_jobs(&mut self) -> Vec<HindsightRetainJob> {
        self.hindsight_queue.collect_pending_retain_jobs()
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct RuntimeConversation {
    last_focus: Option<String>,
    messages: Vec<PromptMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
struct HindsightQueueItem {
    id: Uuid,
    current_doing: String,
    messages: Vec<PromptMessage>,
    #[serde(default)]
    queued: bool,
    #[serde(default)]
    retained: bool,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct HindsightQueue {
    trail: VecDeque<HindsightQueueItem>,
}

impl RuntimeTurnDraft {
    fn new(current_doing: String) -> Self {
        Self {
            current_doing,
            messages: Vec::new(),
        }
    }

    pub fn set_current_doing(&mut self, current_doing: impl Into<String>) {
        let current_doing = current_doing.into();
        if !current_doing.trim().is_empty() {
            self.current_doing = current_doing;
        }
    }

    pub fn push(&mut self, message: PromptMessage) {
        self.messages.push(message);
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn messages(&self) -> &[PromptMessage] {
        &self.messages
    }

    fn into_parts(self) -> (String, Vec<PromptMessage>) {
        (self.current_doing, self.messages)
    }
}

impl RuntimeRequestEnvelope {
    pub fn from_world_snapshot(system_messages: Vec<String>, snapshot_text: &str) -> Self {
        Self {
            system_messages,
            user_message: format!("<world_snapshot>\n{snapshot_text}\n</world_snapshot>"),
        }
    }

    pub fn conversation_budget_tokens(
        &self,
        tools: &[AgentToolSpec],
        limits: RequestBudgetLimits,
    ) -> usize {
        let envelope_breakdown = estimate_runtime_request_envelope(
            &self.system_messages,
            &self.user_message,
            tools,
            limits,
        );
        envelope_breakdown
            .input_budget_tokens()
            .saturating_sub(envelope_breakdown.total_input_tokens)
    }

    fn into_agent_messages(self, conversation_messages: Vec<PromptMessage>) -> Vec<AgentMessage> {
        let mut messages = self
            .system_messages
            .into_iter()
            .map(AgentMessage::system)
            .collect::<Vec<_>>();
        messages.extend(
            conversation_messages
                .into_iter()
                .map(prompt_message_to_agent_message),
        );
        messages.push(AgentMessage::user(self.user_message));
        messages
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

    pub fn push_history_message(&mut self, message: PromptMessage) {
        self.turn_draft.push(message);
    }

    pub fn set_current_doing(&mut self, current_doing: impl Into<String>) {
        self.turn_draft.set_current_doing(current_doing);
    }

    pub fn history_messages(&self) -> &[PromptMessage] {
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
        compact_for_overflow: bool,
        policy: RuntimeStepCompactionPolicy,
        mut build_summary: F,
    ) -> bool
    where
        F: FnMut(Vec<AgentMessage>, usize) -> Fut,
        Fut: Future<Output = Option<String>>,
    {
        let mut compacted_any = false;
        for _ in 0..policy.max_recoveries {
            let breakdown = estimate_agent_turn_request(self.agent_messages(), tools, limits);
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
        Fut: Future<Output = Option<String>>,
    {
        let Some(last_user_index) = self
            .agent_messages
            .iter()
            .rposition(|message| matches!(message, AgentMessage::User { .. }))
        else {
            return false;
        };
        let tail = &self.agent_messages[last_user_index + 1..];
        if tail.is_empty() {
            return false;
        }

        let keep_start = keep_start_for_mid_turn_messages(tail, policy);
        if keep_start == 0 || keep_start >= tail.len() {
            return false;
        }

        let compacted_slice = tail[..keep_start].to_vec();
        let Some(summary) = build_summary(compacted_slice, policy.summary_max_tokens).await else {
            return false;
        };

        self.agent_messages.splice(
            last_user_index + 1..last_user_index + 1 + keep_start,
            [AgentMessage::assistant(summary)],
        );
        true
    }
}

impl RuntimeConversationCompactionPlan {
    pub fn omitted_prefix(&self) -> &[PromptMessage] {
        &self.omitted_prefix
    }

    pub fn summary_max_tokens(&self) -> usize {
        self.summary_max_tokens
    }
}

fn keep_start_for_mid_turn_messages(
    messages: &[AgentMessage],
    policy: RuntimeStepCompactionPolicy,
) -> usize {
    let mut cycles_kept = 0usize;
    for index in (0..messages.len()).rev() {
        if is_tool_cycle_boundary(&messages[index]) {
            cycles_kept += 1;
            if cycles_kept >= policy.keep_tool_cycles {
                return index;
            }
        }
    }
    messages
        .len()
        .saturating_sub(policy.keep_messages_without_tool_cycles)
}

fn is_tool_cycle_boundary(message: &AgentMessage) -> bool {
    matches!(
        message,
        AgentMessage::AssistantToolCallProtocol { .. } | AgentMessage::Tool { .. }
    )
}

fn prompt_message_to_agent_message(message: PromptMessage) -> AgentMessage {
    match message.role {
        PromptRole::System => AgentMessage::system(message.content),
        PromptRole::User => AgentMessage::user(message.content),
        PromptRole::Assistant => AgentMessage::assistant(message.content),
        PromptRole::Tool => {
            AgentMessage::tool("historical-tool", "historical_tool", message.content)
        }
    }
}

impl RuntimeConversation {
    async fn new(bootstrap_focus: Option<String>, bootstrap_messages: Vec<PromptMessage>) -> Self {
        let persistence_path = spinova_paths()
            .await
            .state_file(RUNTIME_CONVERSATION_FILE_NAME);
        tokio::fs::read(persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_else(|| Self {
                last_focus: bootstrap_focus,
                messages: bootstrap_messages,
            })
    }

    pub fn append_turn(&mut self, current_doing: String, messages: Vec<PromptMessage>) {
        if !current_doing.trim().is_empty() {
            self.last_focus = Some(current_doing);
        }
        self.messages.extend(messages);
        self.messages = normalize_runtime_prompt_messages(std::mem::take(&mut self.messages));
    }

    pub fn current_focus(&self) -> Option<String> {
        self.last_focus.clone()
    }

    pub fn messages(&self) -> Vec<PromptMessage> {
        normalize_runtime_prompt_messages(self.messages.clone())
    }

    pub fn select_messages_for_runtime(
        &self,
        max_tokens: usize,
        min_messages: usize,
        summary_max_tokens: usize,
    ) -> Vec<PromptMessage> {
        if max_tokens == 0 {
            return Vec::new();
        }
        let all_messages = self.messages();
        let selected = select_recent_items_by_token_budget(
            all_messages.clone(),
            max_tokens,
            min_messages,
            prompt_message_token_cost,
        );
        let omitted_count = all_messages.len().saturating_sub(selected.len());
        if omitted_count == 0 {
            return selected;
        }

        let summary_max_tokens = summary_max_tokens.min(max_tokens);
        if summary_max_tokens == 0 {
            return selected;
        }
        let reserved_tail_budget = max_tokens
            .saturating_sub(summary_max_tokens)
            .max(min_messages);
        let selected_tail = select_recent_items_by_token_budget(
            all_messages.clone(),
            reserved_tail_budget,
            min_messages,
            prompt_message_token_cost,
        );
        let omitted_prefix_len = all_messages.len().saturating_sub(selected_tail.len());
        let omitted_prefix = &all_messages[..omitted_prefix_len];
        let mut messages = Vec::new();
        if let Some(summary) =
            build_runtime_prompt_history_summary(omitted_prefix, summary_max_tokens)
        {
            messages.push(summary);
        }
        messages.extend(selected_tail);
        messages
    }

    fn plan_compaction(
        &self,
        max_tokens: usize,
        min_messages: usize,
        summary_max_tokens: usize,
    ) -> Option<RuntimeConversationCompactionPlan> {
        if max_tokens == 0 {
            return None;
        }
        let all_messages = self.messages();
        let selected = select_recent_items_by_token_budget(
            all_messages.clone(),
            max_tokens,
            min_messages,
            prompt_message_token_cost,
        );
        let omitted_count = all_messages.len().saturating_sub(selected.len());
        if omitted_count == 0 {
            return None;
        }

        let summary_max_tokens = summary_max_tokens.min(max_tokens);
        if summary_max_tokens == 0 {
            return None;
        }
        let reserved_tail_budget = max_tokens
            .saturating_sub(summary_max_tokens)
            .max(min_messages);
        let selected_tail = select_recent_items_by_token_budget(
            all_messages.clone(),
            reserved_tail_budget,
            min_messages,
            prompt_message_token_cost,
        );
        let omitted_prefix_len = all_messages.len().saturating_sub(selected_tail.len());
        Some(RuntimeConversationCompactionPlan {
            omitted_prefix: all_messages[..omitted_prefix_len].to_vec(),
            selected_tail,
            summary_max_tokens,
        })
    }

    fn apply_compaction(
        &mut self,
        plan: RuntimeConversationCompactionPlan,
        summary: Option<PromptMessage>,
    ) -> bool {
        let summary = match summary {
            Some(summary) => summary,
            None => {
                let Some(summary) = build_runtime_prompt_history_summary(
                    &plan.omitted_prefix,
                    plan.summary_max_tokens,
                ) else {
                    return false;
                };
                summary
            }
        };

        self.messages.clear();
        self.messages.push(summary);
        self.messages.extend(plan.selected_tail);
        self.messages = normalize_runtime_prompt_messages(std::mem::take(&mut self.messages));
        true
    }

    async fn sync_to_disk(&self) {
        let persistence_path = spinova_paths()
            .await
            .state_file(RUNTIME_CONVERSATION_FILE_NAME);
        let data = match postcard::to_allocvec(self) {
            Ok(data) => data,
            Err(err) => {
                tracing::error!("serialize runtime conversation failed: {err}");
                return;
            }
        };
        if let Err(err) = tokio::fs::write(persistence_path, data).await {
            tracing::error!("persist runtime conversation failed: {err}");
        }
    }
}

impl Display for HindsightQueueItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.render_for_memory())
    }
}

impl HindsightQueueItem {
    fn render_for_memory(&self) -> String {
        self.messages
            .iter()
            .map(format_message_for_memory)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_messages(&self) -> Vec<String> {
        self.messages
            .iter()
            .map(format_message_for_memory)
            .collect()
    }

    fn bootstrap_messages(&self) -> Vec<PromptMessage> {
        self.messages.clone()
    }

    fn to_hindsight_item(&self) -> HindsightRetainItem {
        HindsightRetainItem {
            content: self.render_for_retain(),
            timestamp: None,
            context: Some("runtime hindsight step".to_string()),
            metadata: Some(std::collections::HashMap::from([
                ("current_doing".to_string(), self.current_doing.clone()),
                ("entry_id".to_string(), self.id.to_string()),
            ])),
            document_id: Some(format!("hindsight-step:{}", self.id)),
            tags: Some(vec!["spinova".to_string(), "hindsight-step".to_string()]),
        }
    }
}

impl HindsightQueue {
    async fn new() -> Self {
        let persistence_path = spinova_paths()
            .await
            .state_file(HINDSIGHT_QUEUE_FILE_NAME);
        tokio::fs::read(persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_default()
    }

    fn reset_inflight_retain_state(&mut self) {
        for item in &mut self.trail {
            if !item.retained {
                item.queued = false;
            }
        }
    }

    fn current_focus(&self) -> Option<String> {
        self.trail.back().map(|item| item.current_doing.clone())
    }

    fn bootstrap_messages(&self) -> Vec<PromptMessage> {
        self.trail
            .iter()
            .flat_map(|item| item.bootstrap_messages())
            .collect()
    }

    fn push_turn(&mut self, current_doing: String, messages: Vec<PromptMessage>) {
        self.trail.push_back(HindsightQueueItem {
            id: Uuid::new_v4(),
            current_doing,
            messages,
            queued: false,
            retained: false,
        });
    }

    async fn sync_to_disk(&self) {
        let persistence_path = spinova_paths()
            .await
            .state_file(HINDSIGHT_QUEUE_FILE_NAME);
        let data = match postcard::to_allocvec(self) {
            Ok(data) => data,
            Err(err) => {
                tracing::error!("serialize hindsight queue failed: {err}");
                return;
            }
        };
        if let Err(err) = tokio::fs::write(persistence_path, data).await {
            tracing::error!("persist hindsight queue failed: {err}");
        }
    }

    fn collect_pending_retain_jobs(&mut self) -> Vec<HindsightRetainJob> {
        let mut jobs = Vec::new();
        for item in &mut self.trail {
            if item.retained || item.queued {
                continue;
            }
            item.queued = true;
            jobs.push(HindsightRetainJob {
                items: vec![item.to_hindsight_item()],
                document_id: Some(format!("hindsight-step:{}", item.id)),
            });
        }
        jobs
    }

    fn retain_backlog_count(&self) -> usize {
        self.trail.iter().filter(|item| !item.retained).count()
    }

    fn mark_queued_retained(&mut self) {
        for item in &mut self.trail {
            if item.queued && !item.retained {
                item.queued = false;
                item.retained = true;
            }
        }
        while self
            .trail
            .front()
            .map(|item| item.retained)
            .unwrap_or(false)
        {
            self.trail.pop_front();
        }
    }
}

fn prompt_message_token_cost(message: &PromptMessage) -> usize {
    let role = match message.role {
        PromptRole::System => "system",
        PromptRole::User => "user",
        PromptRole::Assistant => "assistant",
        PromptRole::Tool => "tool",
    };
    approx_token_count(role) + approx_token_count(&message.content) + 4
}

fn select_recent_items_by_token_budget<T, F>(
    items: Vec<T>,
    max_tokens: usize,
    min_items: usize,
    mut token_cost: F,
) -> Vec<T>
where
    F: FnMut(&T) -> usize,
{
    let mut selected = Vec::new();
    let mut total_tokens = 0usize;
    for item in items.into_iter().rev() {
        let cost = token_cost(&item);
        let can_fit = total_tokens.saturating_add(cost) <= max_tokens;
        if selected.len() < min_items || can_fit {
            total_tokens = total_tokens.saturating_add(cost);
            selected.push(item);
        } else {
            break;
        }
    }
    selected.reverse();
    selected
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

fn normalize_runtime_prompt_messages(messages: Vec<PromptMessage>) -> Vec<PromptMessage> {
    let mut normalized: Vec<PromptMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        let Some(message) = normalize_runtime_prompt_message(message) else {
            continue;
        };

        if let Some(previous) = normalized.last_mut() {
            if previous.role == message.role && previous.content == message.content {
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

fn normalize_runtime_prompt_message(mut message: PromptMessage) -> Option<PromptMessage> {
    let visible_content = message.content.trim();
    if visible_content.is_empty() {
        if !message.tool_call_ui_events.is_empty() {
            message.content = summarize_tool_call_ui_events(&message.tool_call_ui_events);
        } else if let Some(tool_ui_event) = &message.tool_ui_event {
            message.content = summarize_tool_ui_event(tool_ui_event);
        }
    }

    if message.content.trim().is_empty() {
        return None;
    }

    Some(PromptMessage {
        role: message.role,
        content: message.content.trim().to_string(),
        tool_ui_event: message.tool_ui_event,
        tool_call_ui_events: message.tool_call_ui_events,
    })
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
        ToolUiEvent::Exec(data)
        | ToolUiEvent::Work(data)
        | ToolUiEvent::Device(data)
        | ToolUiEvent::Error(data) => summarize_runtime_inline_text(&data.title),
        ToolUiEvent::Terminal(data) => summarize_runtime_inline_text(&data.title),
        ToolUiEvent::Patch(data) => summarize_runtime_inline_text(&data.summary_line),
        ToolUiEvent::Telegram(data) => summarize_runtime_inline_text(&data.title),
    }
}

fn tool_call_ui_event_title(event: &ToolCallUiEvent) -> &str {
    match event {
        ToolCallUiEvent::Exec(ToolUiData { title, .. })
        | ToolCallUiEvent::Work(ToolUiData { title, .. })
        | ToolCallUiEvent::Device(ToolUiData { title, .. })
        | ToolCallUiEvent::Error(ToolUiData { title, .. }) => title,
        ToolCallUiEvent::Terminal(TerminalUiData { title, .. }) => title,
        ToolCallUiEvent::Patch(PatchUiData { summary_line, .. }) => summary_line,
        ToolCallUiEvent::Telegram(TelegramUiData { title, .. }) => title,
    }
}

fn is_runtime_summary_message(message: &PromptMessage) -> bool {
    matches!(message.role, PromptRole::Assistant)
        && (message.content.starts_with(RUNTIME_HISTORY_SUMMARY_PREFIX)
            || message.content.starts_with(MID_TURN_SUMMARY_PREFIX))
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

fn summarize_prompt_message_for_history_compaction(message: &PromptMessage) -> Option<String> {
    match message.role {
        PromptRole::System => Some(format!(
            "system: {}",
            summarize_runtime_inline_text(&message.content)
        )),
        PromptRole::User => Some(format!(
            "user: {}",
            summarize_runtime_inline_text(&message.content)
        )),
        PromptRole::Assistant => Some(format!(
            "assistant: {}",
            summarize_runtime_inline_text(&message.content)
        )),
        PromptRole::Tool => Some(format!(
            "tool: {}",
            summarize_tool_message_content(&message.content)
        )),
    }
}

fn build_runtime_prompt_history_summary(
    messages: &[PromptMessage],
    max_tokens: usize,
) -> Option<PromptMessage> {
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
    Some(PromptMessage::assistant(truncate_text_to_token_budget(
        &lines.join("\n"),
        max_tokens.max(1),
    )))
}

fn format_message_for_memory(message: &PromptMessage) -> String {
    let role = match message.role {
        PromptRole::System => "system",
        PromptRole::User => "user",
        PromptRole::Assistant => "assistant",
        PromptRole::Tool => "tool",
    };
    let mut parts = Vec::new();
    if !message.content.trim().is_empty() {
        parts.push(message.content.clone());
    }
    if !message.tool_call_ui_events.is_empty() {
        let rendered = message
            .tool_call_ui_events
            .iter()
            .map(format_tool_call_ui_event_for_memory)
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(rendered);
    }
    format!("{role}:\n{}", parts.join("\n"))
}

impl HindsightQueueItem {
    fn render_for_retain(&self) -> String {
        let mut lines = vec![
            "runtime step".to_string(),
            format!("focus: {}", self.current_doing),
        ];
        for message in &self.messages {
            lines.extend(render_prompt_message_for_retain(message));
        }
        lines.join("\n")
    }
}

fn render_prompt_message_for_retain(message: &PromptMessage) -> Vec<String> {
    let mut lines = Vec::new();
    match message.role {
        PromptRole::Assistant => {
            if !message.content.trim().is_empty() {
                lines.push(format!(
                    "assistant action: {}",
                    compact_inline_text(&message.content)
                ));
            }
            for event in &message.tool_call_ui_events {
                lines.extend(render_tool_call_event_for_retain(event));
            }
        }
        PromptRole::Tool => {
            if let Some(event) = &message.tool_ui_event {
                lines.extend(render_tool_result_event_for_retain(event));
            } else if !message.content.trim().is_empty() {
                lines.push(format!(
                    "tool result: {}",
                    compact_inline_text(&message.content)
                ));
            }
        }
        PromptRole::User => {
            if !message.content.trim().is_empty() {
                lines.push(format!(
                    "user context: {}",
                    compact_inline_text(&message.content)
                ));
            }
        }
        PromptRole::System => {}
    }
    lines
}

fn render_tool_call_event_for_retain(event: &ToolCallUiEvent) -> Vec<String> {
    match event {
        ToolCallUiEvent::Error(data) if data.title == "apply_patch" => Vec::new(),
        ToolCallUiEvent::Exec(data)
        | ToolCallUiEvent::Work(data)
        | ToolCallUiEvent::Device(data)
        | ToolCallUiEvent::Error(data) => render_tool_data_for_retain("tool call", data),
        ToolCallUiEvent::Terminal(data) => render_terminal_data_for_retain("tool call", data),
        ToolCallUiEvent::Patch(data) => render_patch_data_for_retain("tool call", data),
        ToolCallUiEvent::Telegram(data) => render_telegram_data_for_retain("tool call", data),
    }
}

fn render_tool_result_event_for_retain(event: &ToolUiEvent) -> Vec<String> {
    match event {
        ToolUiEvent::Error(data) if data.title == "apply_patch failed" => Vec::new(),
        ToolUiEvent::Exec(data)
        | ToolUiEvent::Work(data)
        | ToolUiEvent::Device(data)
        | ToolUiEvent::Error(data) => render_tool_data_for_retain("tool result", data),
        ToolUiEvent::Terminal(data) => render_terminal_data_for_retain("tool result", data),
        ToolUiEvent::Patch(data) => render_patch_data_for_retain("tool result", data),
        ToolUiEvent::Telegram(data) => render_telegram_data_for_retain("tool result", data),
    }
}

fn render_tool_data_for_retain(prefix: &str, data: &ToolUiData) -> Vec<String> {
    let mut lines = vec![format!("{prefix}: {}", compact_inline_text(&data.title))];
    if !data.body_lines.is_empty() {
        lines.push(format!(
            "{prefix} details: {}",
            compact_inline_text(&data.body_lines.join(" | "))
        ));
    }
    lines
}

fn render_terminal_data_for_retain(prefix: &str, data: &TerminalUiData) -> Vec<String> {
    let mut lines = vec![format!("{prefix}: {}", compact_inline_text(&data.title))];
    if !data.body_lines.is_empty() {
        lines.push(format!(
            "{prefix} output: {}",
            compact_inline_text(&data.body_lines.join(" | "))
        ));
    }
    lines
}

fn render_patch_data_for_retain(prefix: &str, data: &PatchUiData) -> Vec<String> {
    let mut lines = vec![format!("{prefix}: {}", compact_inline_text(&data.title))];
    lines.push(format!(
        "{prefix} summary: {}",
        compact_inline_text(&data.summary_line)
    ));
    for file in data.files.iter().take(6) {
        let marker = match file.operation.as_str() {
            "add" => "+",
            "delete" => "-",
            _ => "~",
        };
        lines.push(format!(
            "{prefix} file: {marker} {} (+{} -{})",
            file.path, file.added_lines, file.removed_lines
        ));
    }
    lines
}

fn render_telegram_data_for_retain(prefix: &str, data: &TelegramUiData) -> Vec<String> {
    let mut lines = vec![format!("{prefix}: {}", compact_inline_text(&data.title))];
    if !data.detail_lines.is_empty() {
        lines.push(format!(
            "{prefix} details: {}",
            compact_inline_text(&data.detail_lines.join(" | "))
        ));
    }
    if !data.message_lines.is_empty() {
        lines.push(format!(
            "{prefix} messages: {}",
            compact_inline_text(&data.message_lines.join(" | "))
        ));
    }
    lines
}

fn compact_inline_text(text: &str) -> String {
    const MAX_CHARS: usize = 280;
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}…")
}

fn format_tool_call_ui_event_for_memory(event: &crate::tool_ui::ToolCallUiEvent) -> String {
    match event {
        crate::tool_ui::ToolCallUiEvent::Exec(data)
        | crate::tool_ui::ToolCallUiEvent::Work(data)
        | crate::tool_ui::ToolCallUiEvent::Device(data)
        | crate::tool_ui::ToolCallUiEvent::Error(data) => {
            let mut lines = vec![format!("tool_call: {}", data.title)];
            lines.extend(data.body_lines.iter().map(|line| format!("  {line}")));
            lines.join("\n")
        }
        crate::tool_ui::ToolCallUiEvent::Telegram(data) => {
            let mut lines = vec![format!("tool_call: {}", data.title)];
            lines.extend(data.detail_lines.iter().map(|line| format!("  {line}")));
            lines.extend(data.message_lines.iter().map(|line| format!("  {line}")));
            lines.join("\n")
        }
        crate::tool_ui::ToolCallUiEvent::Terminal(data) => {
            let mut lines = vec![format!("tool_call: {}", data.title)];
            lines.extend(data.body_lines.iter().map(|line| format!("  {line}")));
            lines.join("\n")
        }
        crate::tool_ui::ToolCallUiEvent::Patch(data) => {
            let mut lines = vec![
                format!("tool_call: {}", data.title),
                format!("  {}", data.summary_line),
            ];
            lines.extend(data.files.iter().map(|file| {
                let marker = match file.operation.as_str() {
                    "add" => "+",
                    "delete" => "-",
                    _ => "~",
                };
                format!(
                    "  {marker} {} (+{} -{})",
                    file.path, file.added_lines, file.removed_lines
                )
            }));
            lines.join("\n")
        }
    }
}
