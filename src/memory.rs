//! 此模块定义运行时会话状态与 hindsight retain 队列。
use std::{collections::VecDeque, fmt::Display, future::Future};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    context_budget::{
        RequestBudgetLimits, approx_token_count, estimate_agent_turn_request,
        estimate_runtime_request_envelope, truncate_text_to_token_budget,
        truncate_text_to_token_budget_with_notice,
    },
    daat_locus_paths::daat_locus_paths,
    hindsight::{HindsightRetainItem, HindsightRetainJob},
    reasoning::runtime::{AgentMessage, AgentToolSpec, PromptMessage, PromptRole},
    tool_ui::{
        PatchUiData, TelegramUiData, TerminalUiData, ToolCallUiEvent, ToolUiData, ToolUiEvent,
    },
};

const RUNTIME_HISTORY_SUMMARY_PREFIX: &str = "Earlier runtime history summary:";
const MID_TURN_SUMMARY_PREFIX: &str = "Earlier tool/context progress summary:";
const RUNTIME_CONVERSATION_FILE_NAME: &str = "runtime_conversation";
const HINDSIGHT_QUEUE_FILE_NAME: &str = "hindsight_queue";
const RUNTIME_HISTORY_TOOL_MESSAGE_MAX_TOKENS: usize = 600;
const MID_TURN_COMPACTION_RETAINED_USER_MAX_TOKENS: usize = 20_000;
const RUNTIME_COMPACTION_RECORD_LIMIT: usize = 32;

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
    compaction_records: Vec<RuntimeCompactionRecord>,
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
    source_messages: Vec<PromptMessage>,
    retained_user_messages: Vec<PromptMessage>,
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
        compaction_records: Vec<RuntimeCompactionRecord>,
    ) -> MemoryRetainPlan {
        self.runtime_conversation_mut().append_turn(
            current_doing.clone(),
            messages.clone(),
            compaction_records,
        );
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
        let (current_doing, messages, compaction_records) = draft.into_parts();
        self.record_agent_turn(current_doing, messages, compaction_records)
            .await
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
        outcome: Option<RuntimeCompactionOutcome>,
    ) -> bool {
        self.runtime_conversation.apply_compaction(plan, outcome)
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

    pub fn mark_retained_by_document_ids(&mut self, document_ids: &[String]) {
        self.hindsight_queue
            .mark_retained_by_document_ids(document_ids);
    }

    pub async fn clear_runtime_conversation(&mut self) -> MemoryRetainPlan {
        let retain_plan = self.runtime_conversation.take_for_hindsight().map_or(
            MemoryRetainPlan {
                jobs: Vec::new(),
                must_flush_before_continue: false,
            },
            |(current_doing, messages)| {
                self.hindsight_queue.push_turn(current_doing, messages);
                let jobs = self.collect_pending_retain_jobs();
                let must_flush_before_continue =
                    self.hindsight_queue.retain_backlog_count() >= HINDSIGHT_RETAIN_BACKLOG_LIMIT;
                MemoryRetainPlan {
                    jobs,
                    must_flush_before_continue,
                }
            },
        );
        self.runtime_conversation.sync_to_disk().await;
        self.hindsight_queue.sync_to_disk().await;
        retain_plan
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
    #[serde(default)]
    compaction_records: VecDeque<RuntimeCompactionRecord>,
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
            compaction_records: Vec::new(),
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

    pub fn record_compaction(&mut self, record: RuntimeCompactionRecord) {
        self.compaction_records.push(record);
    }

    fn into_parts(self) -> (String, Vec<PromptMessage>, Vec<RuntimeCompactionRecord>) {
        (self.current_doing, self.messages, self.compaction_records)
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
        Fut: Future<Output = Option<RuntimeCompactionOutcome>>,
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
    pub fn source_messages(&self) -> &[PromptMessage] {
        &self.source_messages
    }

    pub fn retained_user_messages(&self) -> &[PromptMessage] {
        &self.retained_user_messages
    }

    pub fn summary_max_tokens(&self) -> usize {
        self.summary_max_tokens
    }
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

fn rebuild_compacted_agent_messages(
    source_messages: &[AgentMessage],
    summary: String,
) -> Vec<AgentMessage> {
    let mut rebuilt = source_messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect::<Vec<_>>();
    rebuilt.extend(select_recent_user_agent_messages_for_compaction(
        source_messages,
    ));
    rebuilt.push(AgentMessage::assistant(summary));
    rebuilt
}

fn select_recent_user_agent_messages_for_compaction(
    messages: &[AgentMessage],
) -> Vec<AgentMessage> {
    let prompt_messages = messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::User { content } => Some(PromptMessage::user(content.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    select_recent_user_messages_for_compaction(
        &prompt_messages,
        MID_TURN_COMPACTION_RETAINED_USER_MAX_TOKENS,
    )
    .into_iter()
    .map(|message| AgentMessage::user(message.content))
    .collect()
}

impl RuntimeConversation {
    async fn new(bootstrap_focus: Option<String>, bootstrap_messages: Vec<PromptMessage>) -> Self {
        let persistence_path = daat_locus_paths()
            .await
            .memory_file(RUNTIME_CONVERSATION_FILE_NAME);
        tokio::fs::read(persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_else(|| Self {
                last_focus: bootstrap_focus,
                messages: bootstrap_messages,
                compaction_records: VecDeque::new(),
            })
    }

    pub fn append_turn(
        &mut self,
        current_doing: String,
        messages: Vec<PromptMessage>,
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

    pub fn take_for_hindsight(&mut self) -> Option<(String, Vec<PromptMessage>)> {
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

    pub fn messages(&self) -> Vec<PromptMessage> {
        normalize_runtime_prompt_messages(self.messages.clone())
    }

    pub fn select_messages_for_runtime(
        &self,
        max_tokens: usize,
        _min_messages: usize,
        summary_max_tokens: usize,
    ) -> Vec<PromptMessage> {
        if max_tokens == 0 {
            return Vec::new();
        }
        let all_messages = self.messages();
        if prompt_messages_total_token_cost(&all_messages) <= max_tokens {
            return all_messages;
        }

        let summary_max_tokens = summary_max_tokens.min(max_tokens);
        if summary_max_tokens == 0 {
            return Vec::new();
        }
        let retained_user_messages = select_recent_user_messages_for_compaction(
            &all_messages,
            max_tokens.saturating_sub(summary_max_tokens),
        );
        let mut messages = Vec::new();
        messages.extend(retained_user_messages);
        if let Some(summary) =
            build_runtime_prompt_history_summary(&all_messages, summary_max_tokens)
        {
            messages.push(summary);
        }
        messages
    }

    fn plan_compaction(
        &self,
        max_tokens: usize,
        _min_messages: usize,
        summary_max_tokens: usize,
    ) -> Option<RuntimeConversationCompactionPlan> {
        if max_tokens == 0 {
            return None;
        }
        let all_messages = self.messages();
        if prompt_messages_total_token_cost(&all_messages) <= max_tokens {
            return None;
        }

        let summary_max_tokens = summary_max_tokens.min(max_tokens);
        if summary_max_tokens == 0 {
            return None;
        }
        let retained_user_messages = select_recent_user_messages_for_compaction(
            &all_messages,
            max_tokens.saturating_sub(summary_max_tokens),
        );
        Some(RuntimeConversationCompactionPlan {
            source_messages: all_messages,
            retained_user_messages,
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
                PromptMessage::assistant(outcome.summary),
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
        self.messages.extend(plan.retained_user_messages);
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
        let persistence_path = daat_locus_paths()
            .await
            .memory_file(RUNTIME_CONVERSATION_FILE_NAME);
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
        let tags = self.classification_tags();
        let mut metadata = std::collections::HashMap::from([
            ("current_doing".to_string(), self.current_doing.clone()),
            ("entry_id".to_string(), self.id.to_string()),
            ("origin".to_string(), "runtime_step".to_string()),
        ]);
        if let Some(primary_scope) = tags
            .iter()
            .find_map(|tag| tag.strip_prefix("scope:").map(str::to_string))
        {
            metadata.insert("primary_scope".to_string(), primary_scope);
        }
        if let Some(primary_kind) = tags
            .iter()
            .find_map(|tag| tag.strip_prefix("kind:").map(str::to_string))
        {
            metadata.insert("primary_kind".to_string(), primary_kind);
        }
        HindsightRetainItem {
            content: self.render_for_retain(),
            timestamp: None,
            context: Some("runtime hindsight step".to_string()),
            metadata: Some(metadata),
            document_id: Some(format!("hindsight-step:{}", self.id)),
            tags: Some(tags),
        }
    }

    fn classification_tags(&self) -> Vec<String> {
        let mut tags = vec![
            "daat-locus".to_string(),
            "hindsight-step".to_string(),
            "origin:runtime_step".to_string(),
            "source:runtime_step".to_string(),
            "scope:runtime".to_string(),
        ];

        if self.has_telegram_activity() {
            tags.push("scope:telegram".to_string());
        }
        if self.has_workspace_activity() {
            tags.push("scope:workspace".to_string());
            tags.push("kind:project_fact".to_string());
        }
        if self.has_failure_signal() {
            tags.push("kind:failure_pattern".to_string());
        }
        if self.has_user_preference_signal() {
            tags.push("kind:user_preference".to_string());
        }
        if !tags.iter().any(|tag| tag.starts_with("kind:")) {
            tags.push("kind:strategy_lesson".to_string());
        }

        tags.sort();
        tags.dedup();
        tags
    }

    fn has_workspace_activity(&self) -> bool {
        self.messages.iter().any(message_has_workspace_signal)
    }

    fn has_telegram_activity(&self) -> bool {
        self.messages.iter().any(message_has_telegram_signal)
    }

    fn has_failure_signal(&self) -> bool {
        self.messages.iter().any(message_has_failure_signal)
    }

    fn has_user_preference_signal(&self) -> bool {
        self.messages.iter().any(message_has_preference_signal)
    }
}

impl HindsightQueue {
    async fn new() -> Self {
        let persistence_path = daat_locus_paths()
            .await
            .memory_file(HINDSIGHT_QUEUE_FILE_NAME);
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
        let persistence_path = daat_locus_paths()
            .await
            .memory_file(HINDSIGHT_QUEUE_FILE_NAME);
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

    fn mark_retained_by_document_ids(&mut self, document_ids: &[String]) {
        if document_ids.is_empty() {
            return;
        }
        for item in &mut self.trail {
            let document_id = format!("hindsight-step:{}", item.id);
            if document_ids
                .iter()
                .any(|candidate| candidate == &document_id)
            {
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

fn select_recent_user_messages_for_compaction(
    messages: &[PromptMessage],
    max_tokens: usize,
) -> Vec<PromptMessage> {
    if max_tokens == 0 {
        return Vec::new();
    }

    let user_messages = messages
        .iter()
        .filter(|message| {
            matches!(message.role, PromptRole::User) && !is_runtime_summary_message(message)
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut selected = Vec::new();
    let mut remaining = max_tokens;
    for message in user_messages.into_iter().rev() {
        if remaining == 0 {
            break;
        }
        let cost = prompt_message_token_cost(&message);
        if cost <= remaining {
            remaining = remaining.saturating_sub(cost);
            selected.push(message);
            continue;
        }

        let truncated = truncate_text_to_token_budget_with_notice(
            message.content.trim(),
            remaining,
            "... [user message too long; runtime history truncated]",
        );
        if !truncated.trim().is_empty() {
            selected.push(PromptMessage::user(truncated));
        }
        break;
    }
    selected.reverse();
    selected
}

fn prompt_messages_total_token_cost(messages: &[PromptMessage]) -> usize {
    messages.iter().map(prompt_message_token_cost).sum()
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

    if matches!(message.role, PromptRole::Tool) {
        message.content = truncate_text_to_token_budget_with_notice(
            message.content.trim(),
            RUNTIME_HISTORY_TOOL_MESSAGE_MAX_TOKENS,
            "... [tool output too long; runtime history truncated]",
        );
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
        | ToolUiEvent::App(data)
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
        | ToolCallUiEvent::App(ToolUiData { title, .. })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_conversation_compaction_rebuilds_history_without_assistant_or_tool_tail() {
        let mut conversation = RuntimeConversation {
            last_focus: Some("test".to_string()),
            messages: vec![
                PromptMessage::user("user one"),
                PromptMessage::assistant("assistant one"),
                PromptMessage::tool_with_ui(
                    "tool output one",
                    ToolUiEvent::Exec(ToolUiData {
                        title: "tool output".to_string(),
                        body_lines: Vec::new(),
                    }),
                ),
                PromptMessage::user("user two"),
                PromptMessage::assistant("assistant two"),
                PromptMessage::tool_with_ui(
                    "tool output two",
                    ToolUiEvent::Exec(ToolUiData {
                        title: "tool output".to_string(),
                        body_lines: Vec::new(),
                    }),
                ),
            ],
            compaction_records: VecDeque::new(),
        };

        let plan = conversation
            .plan_compaction(
                /*max_tokens*/ 20, /*min_messages*/ 0, /*summary_max_tokens*/ 8,
            )
            .expect("expected compaction plan");
        assert!(!plan.retained_user_messages.is_empty());
        assert!(
            plan.retained_user_messages
                .iter()
                .all(|message| matches!(message.role, PromptRole::User))
        );

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
                    retained_user_message_count: 2,
                    used_fallback_summary: false,
                    summary: "summary".to_string(),
                },
            }),
        );
        assert!(applied);
        assert!(!conversation.messages.is_empty());
        assert!(
            conversation.messages[..conversation.messages.len() - 1]
                .iter()
                .all(|message| matches!(message.role, PromptRole::User))
        );
        assert!(matches!(
            conversation.messages.last().map(|message| &message.role),
            Some(PromptRole::Assistant)
        ));
        assert!(
            conversation
                .messages
                .iter()
                .all(|message| !matches!(message.role, PromptRole::Tool))
        );
    }

    #[test]
    fn select_recent_user_messages_for_compaction_truncates_overlong_user_message() {
        let messages = vec![PromptMessage::user("word ".repeat(200))];
        let selected = select_recent_user_messages_for_compaction(&messages, 16);
        assert_eq!(selected.len(), 1);
        assert!(selected[0].content.contains("runtime history truncated"));
    }

    #[test]
    fn rebuild_compacted_agent_messages_drops_assistant_and_tool_history() {
        let messages = vec![
            AgentMessage::system("system"),
            AgentMessage::user("claimed input"),
            AgentMessage::user("<world_snapshot>snapshot</world_snapshot>"),
            AgentMessage::assistant("assistant detail"),
            AgentMessage::tool("call-1", "shell", "tool output"),
        ];

        let rebuilt = rebuild_compacted_agent_messages(&messages, "summary".to_string());
        assert_eq!(rebuilt.len(), 4);
        assert!(matches!(rebuilt[0], AgentMessage::System { .. }));
        assert!(matches!(rebuilt[1], AgentMessage::User { .. }));
        assert!(matches!(rebuilt[2], AgentMessage::User { .. }));
        assert!(matches!(rebuilt[3], AgentMessage::Assistant { .. }));
        assert!(rebuilt.iter().all(|message| {
            !matches!(
                message,
                AgentMessage::Tool { .. } | AgentMessage::AssistantToolCallProtocol { .. }
            )
        }));
    }
}

impl HindsightQueueItem {
    fn render_for_retain(&self) -> String {
        let mut lines = vec![
            "runtime step narrative".to_string(),
            format!("focus: {}", compact_inline_text(&self.current_doing)),
            "goal: preserve durable facts, decisions, boundaries, preferences, and reusable lessons from this step.".to_string(),
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
                    "assistant reasoning: {}",
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
                    "tool outcome: {}",
                    compact_inline_text(&message.content)
                ));
            }
        }
        PromptRole::User => {
            if !message.content.trim().is_empty() {
                lines.push(format!(
                    "user/runtime context: {}",
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
        | ToolCallUiEvent::App(data)
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
        | ToolUiEvent::App(data)
        | ToolUiEvent::Error(data) => render_tool_data_for_retain("tool result", data),
        ToolUiEvent::Terminal(data) => render_terminal_data_for_retain("tool result", data),
        ToolUiEvent::Patch(data) => render_patch_data_for_retain("tool result", data),
        ToolUiEvent::Telegram(data) => render_telegram_data_for_retain("tool result", data),
    }
}

fn render_tool_data_for_retain(prefix: &str, data: &ToolUiData) -> Vec<String> {
    let mut lines = vec![format!(
        "{prefix} action: {}",
        compact_inline_text(&data.title)
    )];
    if !data.body_lines.is_empty() {
        lines.push(format!(
            "{prefix} result: {}",
            compact_inline_text(&data.body_lines.join(" || "))
        ));
    }
    lines
}

fn render_terminal_data_for_retain(prefix: &str, data: &TerminalUiData) -> Vec<String> {
    let mut lines = vec![format!(
        "{prefix} terminal action: {}",
        compact_inline_text(&data.title)
    )];
    if !data.body_lines.is_empty() {
        lines.push(format!(
            "{prefix} terminal output: {}",
            compact_inline_text(&data.body_lines.join(" || "))
        ));
    }
    lines
}

fn render_patch_data_for_retain(prefix: &str, data: &PatchUiData) -> Vec<String> {
    let mut lines = vec![format!(
        "{prefix} patch action: {}",
        compact_inline_text(&data.title)
    )];
    lines.push(format!(
        "{prefix} patch summary: {}",
        compact_inline_text(&data.summary_line)
    ));
    for file in data.files.iter().take(6) {
        let marker = match file.operation.as_str() {
            "add" => "+",
            "delete" => "-",
            _ => "~",
        };
        lines.push(format!(
            "{prefix} changed file: {marker} {} (+{} -{})",
            file.path, file.added_lines, file.removed_lines
        ));
    }
    lines
}

fn render_telegram_data_for_retain(prefix: &str, data: &TelegramUiData) -> Vec<String> {
    let mut lines = vec![format!(
        "{prefix} telegram action: {}",
        compact_inline_text(&data.title)
    )];
    if !data.detail_lines.is_empty() {
        lines.push(format!(
            "{prefix} telegram details: {}",
            compact_inline_text(&data.detail_lines.join(" || "))
        ));
    }
    if !data.message_lines.is_empty() {
        lines.push(format!(
            "{prefix} telegram messages: {}",
            compact_inline_text(&data.message_lines.join(" || "))
        ));
    }
    lines
}

fn message_has_workspace_signal(message: &PromptMessage) -> bool {
    if message
        .tool_call_ui_events
        .iter()
        .any(tool_call_event_is_workspace_signal)
    {
        return true;
    }
    match &message.tool_ui_event {
        Some(event) => tool_event_is_workspace_signal(event),
        None => false,
    }
}

fn message_has_telegram_signal(message: &PromptMessage) -> bool {
    if message
        .tool_call_ui_events
        .iter()
        .any(|event| matches!(event, ToolCallUiEvent::Telegram(_)))
    {
        return true;
    }
    matches!(message.tool_ui_event, Some(ToolUiEvent::Telegram(_)))
}

fn message_has_failure_signal(message: &PromptMessage) -> bool {
    if message.content.to_ascii_lowercase().contains("failed") {
        return true;
    }
    if message
        .tool_call_ui_events
        .iter()
        .any(|event| matches!(event, ToolCallUiEvent::Error(_)))
    {
        return true;
    }
    matches!(message.tool_ui_event, Some(ToolUiEvent::Error(_)))
}

fn message_has_preference_signal(message: &PromptMessage) -> bool {
    let content = message.content.to_ascii_lowercase();
    content.contains("prefer")
        || content.contains("偏好")
        || content.contains("喜欢")
        || matches!(message.tool_ui_event, Some(ToolUiEvent::Telegram(_)))
}

fn tool_call_event_is_workspace_signal(event: &ToolCallUiEvent) -> bool {
    matches!(
        event,
        ToolCallUiEvent::Exec(_)
            | ToolCallUiEvent::Terminal(_)
            | ToolCallUiEvent::Patch(_)
            | ToolCallUiEvent::App(_)
    )
}

fn tool_event_is_workspace_signal(event: &ToolUiEvent) -> bool {
    matches!(
        event,
        ToolUiEvent::Exec(_)
            | ToolUiEvent::Terminal(_)
            | ToolUiEvent::Patch(_)
            | ToolUiEvent::App(_)
    )
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
        | crate::tool_ui::ToolCallUiEvent::App(data)
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
