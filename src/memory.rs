//! Runtime conversation state and the hindsight handoff queue.
use std::{
    collections::{HashSet, VecDeque},
    fmt::Display,
    future::Future,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    context_budget::{
        RequestBudgetLimits, approx_token_count, estimate_agent_turn_request,
        estimate_runtime_request_envelope, truncate_text_to_token_budget,
        truncate_text_to_token_budget_with_notice,
    },
    hindsight::{HINDSIGHT_RUNTIME_DOCUMENT_ID, HindsightRetainItem, HindsightRetainJob},
    persistence::PersistenceStore,
    reasoning::runtime::{AgentMessage, AgentToolSpec, HistoryMessage},
    tool_ui::{
        ActivateWorkflowUiData, CreateWorkflowUiData, DeepRecallUiData, PatchUiData, PlanUiData,
        TelegramUiData, TerminalUiData, ToolCallUiEvent, ToolUiData, ToolUiEvent,
    },
};

const RUNTIME_HISTORY_SUMMARY_PREFIX: &str = "Earlier runtime history summary:";
const MID_TURN_SUMMARY_PREFIX: &str = "Earlier tool/context progress summary:";
const RUNTIME_CONVERSATION_FILE_NAME: &str = "runtime_conversation.json";
const RUNTIME_CONVERSATION_LEGACY_FILE_NAME: &str = "runtime_conversation";
const HINDSIGHT_QUEUE_FILE_NAME: &str = "hindsight_queue.json";
const HINDSIGHT_QUEUE_LEGACY_FILE_NAME: &str = "hindsight_queue";
const RUNTIME_HISTORY_TOOL_MESSAGE_MAX_TOKENS: usize = 600;
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

const HINDSIGHT_HANDOFF_BACKLOG_LIMIT: usize = 3;

impl Memory {
    pub async fn new() -> Self {
        let mut hindsight_queue = HindsightQueue::new().await;
        let reset_inflight = hindsight_queue.reset_inflight_retain_state();
        let runtime_conversation = RuntimeConversation::new(
            hindsight_queue.current_focus(),
            hindsight_queue.bootstrap_messages(),
        )
        .await;
        let memory = Self {
            runtime_conversation,
            hindsight_queue,
        };
        if reset_inflight {
            memory.hindsight_queue.sync_to_disk().await;
        }
        memory
    }

    pub async fn record_agent_turn(
        &mut self,
        current_doing: String,
        messages: Vec<HistoryMessage>,
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
            self.hindsight_queue.handoff_backlog_count() >= HINDSIGHT_HANDOFF_BACKLOG_LIMIT;
        self.sync_to_disk().await;
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

    pub fn handoff_backlog_count(&self) -> usize {
        self.hindsight_queue.handoff_backlog_count()
    }

    pub fn should_block_new_turns_on_handoff_backlog(&self) -> bool {
        self.handoff_backlog_count() >= HINDSIGHT_HANDOFF_BACKLOG_LIMIT
    }

    pub async fn mark_handoffs_submitted(&mut self, handoff_ids: &[Uuid]) {
        if self.hindsight_queue.mark_handoffs_submitted(handoff_ids) {
            self.hindsight_queue.sync_to_disk().await;
        }
    }

    pub async fn discard_hindsight_handoff_backlog(&mut self) -> usize {
        let discarded = self.hindsight_queue.discard_unsubmitted();
        if discarded > 0 {
            self.hindsight_queue.sync_to_disk().await;
        }
        discarded
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
                    self.hindsight_queue.handoff_backlog_count() >= HINDSIGHT_HANDOFF_BACKLOG_LIMIT;
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
        self.sync_to_disk().await;
    }

    fn collect_pending_retain_jobs(&mut self) -> Vec<HindsightRetainJob> {
        self.hindsight_queue.collect_pending_retain_jobs()
    }

    async fn sync_to_disk(&self) {
        self.runtime_conversation.sync_to_disk().await;
        self.hindsight_queue.sync_to_disk().await;
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct RuntimeConversation {
    last_focus: Option<String>,
    messages: Vec<HistoryMessage>,
    #[serde(default)]
    compaction_records: VecDeque<RuntimeCompactionRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
struct HindsightQueueItem {
    id: Uuid,
    current_doing: String,
    messages: Vec<HistoryMessage>,
    #[serde(default)]
    inflight: bool,
    #[serde(default)]
    submitted: bool,
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
        let envelope_breakdown = estimate_runtime_request_envelope(
            &self.system_messages,
            self.user_message.as_deref().unwrap_or_default(),
            tools,
            limits,
        );
        envelope_breakdown
            .input_budget_tokens()
            .saturating_sub(envelope_breakdown.total_input_tokens)
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

    pub fn take_for_hindsight(&mut self) -> Option<(String, Vec<HistoryMessage>)> {
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
        if history_messages_total_token_cost(&all_messages) <= max_tokens {
            return None;
        }

        let summary_max_tokens = summary_max_tokens.min(max_tokens);
        if summary_max_tokens == 0 {
            return None;
        }
        Some(RuntimeConversationCompactionPlan {
            source_messages: all_messages,
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

    fn bootstrap_messages(&self) -> Vec<HistoryMessage> {
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
            document_id: Some(HINDSIGHT_RUNTIME_DOCUMENT_ID.to_string()),
            tags: Some(tags),
            update_mode: Some("append".to_string()),
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
        let persistence = PersistenceStore::runtime().await;
        if let Some(queue) = persistence
            .read_json_memory(HINDSIGHT_QUEUE_FILE_NAME, "hindsight queue")
            .await
        {
            return queue;
        }
        if let Some(queue) = persistence
            .read_postcard_memory(HINDSIGHT_QUEUE_LEGACY_FILE_NAME, "legacy hindsight queue")
            .await
        {
            if let Err(err) = persistence
                .write_json_memory(HINDSIGHT_QUEUE_FILE_NAME, &queue)
                .await
            {
                tracing::error!("migrate legacy hindsight queue to json failed: {err}");
            }
            return queue;
        }
        Self::default()
    }

    fn reset_inflight_retain_state(&mut self) -> bool {
        let mut changed = false;
        for item in &mut self.trail {
            if !item.submitted {
                changed |= item.inflight;
                item.inflight = false;
            }
        }
        changed
    }

    fn current_focus(&self) -> Option<String> {
        self.trail.back().map(|item| item.current_doing.clone())
    }

    fn bootstrap_messages(&self) -> Vec<HistoryMessage> {
        self.trail
            .iter()
            .flat_map(|item| item.bootstrap_messages())
            .collect()
    }

    fn push_turn(&mut self, current_doing: String, messages: Vec<HistoryMessage>) {
        self.trail.push_back(HindsightQueueItem {
            id: Uuid::new_v4(),
            current_doing,
            messages,
            inflight: false,
            submitted: false,
        });
    }

    async fn sync_to_disk(&self) {
        let persistence = PersistenceStore::runtime().await;
        if let Err(err) = persistence
            .write_json_memory(HINDSIGHT_QUEUE_FILE_NAME, self)
            .await
        {
            tracing::error!("persist hindsight queue failed: {err}");
        }
    }

    fn collect_pending_retain_jobs(&mut self) -> Vec<HindsightRetainJob> {
        let mut jobs = Vec::new();
        for item in &mut self.trail {
            if item.submitted || item.inflight {
                continue;
            }
            item.inflight = true;
            jobs.push(HindsightRetainJob {
                handoff_id: item.id,
                items: vec![item.to_hindsight_item()],
                document_id: Some(HINDSIGHT_RUNTIME_DOCUMENT_ID.to_string()),
            });
        }
        jobs
    }

    fn handoff_backlog_count(&self) -> usize {
        self.trail.iter().filter(|item| !item.submitted).count()
    }

    fn mark_handoffs_submitted(&mut self, handoff_ids: &[Uuid]) -> bool {
        let handoff_ids = handoff_ids.iter().copied().collect::<HashSet<_>>();
        let mut changed = false;
        for item in &mut self.trail {
            if handoff_ids.contains(&item.id) && !item.submitted {
                item.inflight = false;
                item.submitted = true;
                changed = true;
            }
        }
        while self
            .trail
            .front()
            .map(|item| item.submitted)
            .unwrap_or(false)
        {
            self.trail.pop_front();
            changed = true;
        }
        changed
    }

    fn discard_unsubmitted(&mut self) -> usize {
        let before = self.trail.len();
        self.trail.retain(|item| item.submitted);
        before.saturating_sub(self.trail.len())
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
        ToolUiEvent::CreateWorkflow(CreateWorkflowUiData { workflow_id }) => {
            format!("created workflow {workflow_id}")
        }
        ToolUiEvent::ActivateWorkflow(ActivateWorkflowUiData { workflow_id }) => {
            format!("activated workflow {workflow_id}")
        }
        ToolUiEvent::DeepRecall(DeepRecallUiData { memory_count }) => {
            format!("recalled {memory_count} memories")
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
        | ToolCallUiEvent::CreateWorkflow(ToolUiData { title, .. })
        | ToolCallUiEvent::ActivateWorkflow(ToolUiData { title, .. })
        | ToolCallUiEvent::DeepRecall(ToolUiData { title, .. })
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

fn format_message_for_memory(message: &HistoryMessage) -> String {
    let role = message.role_name();
    let mut parts = Vec::new();
    if !history_message_content(message).trim().is_empty() {
        parts.push(history_message_content(message).to_string());
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
        let transcript = build_hindsight_retain_transcript(&self.messages);
        serde_json::to_string(&transcript).unwrap_or_else(|_| "[]".to_string())
    }
}

fn build_hindsight_retain_transcript(messages: &[HistoryMessage]) -> Vec<serde_json::Value> {
    let mut transcript = Vec::new();
    let mut skipped_tool_call_ids = HashSet::new();

    for message in messages {
        match &message.message {
            AgentMessage::User { .. } => {
                let text = history_message_content(message).trim();
                if !text.is_empty() {
                    transcript.push(json!({
                        "role": "user",
                        "content": [{ "type": "text", "text": text }],
                    }));
                }
            }
            AgentMessage::Assistant { .. } => {
                let text = history_message_content(message).trim();
                if !text.is_empty() {
                    transcript.push(json!({
                        "role": "assistant",
                        "content": [{ "type": "text", "text": text }],
                    }));
                }
            }
            AgentMessage::AssistantToolCallProtocol { content, calls, .. } => {
                let mut blocks = Vec::new();
                if let Some(text) = content.as_deref().map(str::trim)
                    && !text.is_empty()
                {
                    blocks.push(json!({
                        "type": "text",
                        "text": text,
                    }));
                }
                blocks.extend(calls.iter().filter_map(|call| {
                    if is_hindsight_operational_tool(&call.name) {
                        skipped_tool_call_ids.insert(call.id.clone());
                        return None;
                    }
                    Some(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }))
                }));
                if !blocks.is_empty() {
                    transcript.push(json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
            }
            AgentMessage::Tool { tool_call_id, .. } => {
                if skipped_tool_call_ids.contains(tool_call_id) {
                    continue;
                }
                let result = strip_tool_history_envelope(history_message_content(message));
                if result.trim().is_empty() {
                    continue;
                }
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": result,
                });
                if let Some(last) = transcript.last_mut()
                    && last.get("role").and_then(serde_json::Value::as_str) == Some("user")
                    && last
                        .get("content")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|items| {
                            items.iter().all(|item| {
                                item.get("type").and_then(serde_json::Value::as_str)
                                    == Some("tool_result")
                            })
                        })
                    && let Some(items) = last
                        .get_mut("content")
                        .and_then(serde_json::Value::as_array_mut)
                {
                    items.push(block);
                    continue;
                }
                transcript.push(json!({
                    "role": "user",
                    "content": [block],
                }));
            }
            AgentMessage::System { .. } => {}
        }
    }

    transcript
}

fn is_hindsight_operational_tool(name: &str) -> bool {
    matches!(name.trim(), "deep_recall")
}

fn strip_tool_history_envelope(content: &str) -> String {
    let mut lines = content.lines();
    let first = lines.next().unwrap_or_default();
    let second = lines.next().unwrap_or_default();
    if first.starts_with("tool_call_id=") && second.starts_with("name=") {
        return lines.collect::<Vec<_>>().join("\n").trim().to_string();
    }
    content.trim().to_string()
}

fn message_has_workspace_signal(message: &HistoryMessage) -> bool {
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

fn message_has_telegram_signal(message: &HistoryMessage) -> bool {
    if message
        .tool_call_ui_events
        .iter()
        .any(|event| matches!(event, ToolCallUiEvent::Telegram(_)))
    {
        return true;
    }
    matches!(message.tool_ui_event, Some(ToolUiEvent::Telegram(_)))
}

fn message_has_failure_signal(message: &HistoryMessage) -> bool {
    if history_message_content(message)
        .to_ascii_lowercase()
        .contains("failed")
    {
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

fn message_has_preference_signal(message: &HistoryMessage) -> bool {
    let content = history_message_content(message).to_ascii_lowercase();
    content.contains("prefer") || matches!(message.tool_ui_event, Some(ToolUiEvent::Telegram(_)))
}

fn tool_call_event_is_workspace_signal(event: &ToolCallUiEvent) -> bool {
    matches!(
        event,
        ToolCallUiEvent::Exec(_)
            | ToolCallUiEvent::Terminal(_)
            | ToolCallUiEvent::Browser(_)
            | ToolCallUiEvent::Patch(_)
            | ToolCallUiEvent::App(_)
    )
}

fn tool_event_is_workspace_signal(event: &ToolUiEvent) -> bool {
    matches!(
        event,
        ToolUiEvent::Exec(_)
            | ToolUiEvent::Terminal(_)
            | ToolUiEvent::Browser(_)
            | ToolUiEvent::Patch(_)
            | ToolUiEvent::App(_)
    )
}

fn format_tool_call_ui_event_for_memory(event: &crate::tool_ui::ToolCallUiEvent) -> String {
    match event {
        crate::tool_ui::ToolCallUiEvent::Exec(data)
        | crate::tool_ui::ToolCallUiEvent::Plan(data)
        | crate::tool_ui::ToolCallUiEvent::CreateWorkflow(data)
        | crate::tool_ui::ToolCallUiEvent::ActivateWorkflow(data)
        | crate::tool_ui::ToolCallUiEvent::DeepRecall(data)
        | crate::tool_ui::ToolCallUiEvent::App(data)
        | crate::tool_ui::ToolCallUiEvent::Error(data) => {
            let mut lines = vec![format!("tool_call: {}", data.title)];
            lines.extend(data.body_lines.iter().map(|line| format!("  {line}")));
            lines.join("\n")
        }
        crate::tool_ui::ToolCallUiEvent::Browser(data) => {
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
                "tool_call: apply_patch".to_string(),
                format!("  {}", data.summary_line),
            ];
            lines.extend(data.files.iter().map(|file| {
                let marker = match file.operation {
                    crate::tool_ui::PatchFileOperation::Add => "+",
                    crate::tool_ui::PatchFileOperation::Delete => "-",
                    crate::tool_ui::PatchFileOperation::Update => "~",
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

#[cfg(test)]
mod tests {
    use super::*;

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

        let plan = conversation
            .plan_compaction(
                /*max_tokens*/ 20, /*min_messages*/ 0, /*summary_max_tokens*/ 8,
            )
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

        let mut queue = HindsightQueue::default();
        queue.push_turn("json persistence".to_string(), restored.messages.clone());
        let bytes = serde_json::to_vec_pretty(&queue).expect("serialize hindsight queue");
        let restored_queue: HindsightQueue =
            serde_json::from_slice(&bytes).expect("deserialize hindsight queue");
        match &restored_queue.trail[0].messages[0].message {
            AgentMessage::AssistantToolCallProtocol { calls, .. } => {
                assert_eq!(calls[0].arguments, tool_call.arguments);
            }
            _ => panic!("expected assistant tool-call protocol"),
        }
    }

    #[test]
    fn hindsight_queue_tracks_handoff_inflight_and_submitted_ids() {
        let mut queue = HindsightQueue::default();
        queue.push_turn(
            "testing handoff".to_string(),
            vec![HistoryMessage::user("remember this")],
        );

        let jobs = queue.collect_pending_retain_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(queue.handoff_backlog_count(), 1);
        assert_eq!(queue.trail.len(), 1);
        assert!(queue.trail[0].inflight);
        assert!(!queue.trail[0].submitted);

        let handoff_id = jobs[0].handoff_id;
        assert!(!queue.mark_handoffs_submitted(&[]));
        assert!(queue.mark_handoffs_submitted(&[handoff_id]));
        assert_eq!(queue.handoff_backlog_count(), 0);
        assert!(queue.trail.is_empty());
    }

    #[test]
    fn hindsight_queue_discards_only_unsubmitted_handoffs() {
        let mut queue = HindsightQueue::default();
        queue.push_turn(
            "pending handoff".to_string(),
            vec![HistoryMessage::user("not yet submitted")],
        );
        queue.push_turn(
            "submitted handoff".to_string(),
            vec![HistoryMessage::user("already submitted")],
        );

        let jobs = queue.collect_pending_retain_jobs();
        assert_eq!(jobs.len(), 2);
        assert!(queue.mark_handoffs_submitted(&[jobs[1].handoff_id]));

        assert_eq!(queue.discard_unsubmitted(), 1);
        assert_eq!(queue.trail.len(), 1);
        assert_eq!(queue.trail[0].current_doing, "submitted handoff");
        assert!(queue.trail[0].submitted);
    }

    #[test]
    fn hindsight_queue_resets_inflight_handoff_state_on_startup() {
        let mut queue = HindsightQueue::default();
        queue.push_turn(
            "testing retry".to_string(),
            vec![HistoryMessage::user("retry this")],
        );

        let jobs = queue.collect_pending_retain_jobs();
        assert_eq!(jobs.len(), 1);
        assert!(queue.trail[0].inflight);

        queue.reset_inflight_retain_state();
        assert!(!queue.trail[0].inflight);
        assert!(!queue.trail[0].submitted);

        let retry_jobs = queue.collect_pending_retain_jobs();
        assert_eq!(retry_jobs.len(), 1);
        assert_eq!(retry_jobs[0].handoff_id, queue.trail[0].id);
    }

    #[test]
    fn hindsight_retain_transcript_keeps_structured_tool_blocks() {
        let transcript = build_hindsight_retain_transcript(&[
            HistoryMessage::user("user input"),
            HistoryMessage {
                message: AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    Some("checking state".to_string()),
                    None,
                    vec![crate::reasoning::runtime::AgentToolCall {
                        id: "call_1".to_string(),
                        name: "terminal_exec".to_string(),
                        arguments: serde_json::json!({ "cmd": "pwd" }),
                    }],
                ),
                tool_ui_event: None,
                tool_call_ui_events: Vec::new(),
            },
            HistoryMessage::tool(
                "call_1",
                "terminal_exec",
                "tool_call_id=call_1\nname=terminal_exec\nsummary=ok\npayload=\n{\"cwd\":\"/tmp\"}",
                ToolUiEvent::Exec(ToolUiData {
                    title: "terminal_exec".to_string(),
                    body_lines: vec!["pwd".to_string()],
                }),
            ),
        ]);

        assert_eq!(transcript.len(), 3);
        assert_eq!(transcript[0]["role"], "user");
        assert_eq!(transcript[1]["role"], "assistant");
        assert_eq!(transcript[1]["content"][1]["type"], "tool_use");
        assert_eq!(transcript[1]["content"][1]["name"], "terminal_exec");
        assert_eq!(transcript[2]["role"], "user");
        assert_eq!(transcript[2]["content"][0]["type"], "tool_result");
        assert_eq!(transcript[2]["content"][0]["tool_use_id"], "call_1");
        assert_eq!(
            transcript[2]["content"][0]["content"],
            "summary=ok\npayload=\n{\"cwd\":\"/tmp\"}"
        );
    }

    #[test]
    fn hindsight_retain_transcript_skips_deep_recall_blocks() {
        let transcript = build_hindsight_retain_transcript(&[
            HistoryMessage::user("what changed?"),
            HistoryMessage {
                message: AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    Some("checking memory".to_string()),
                    None,
                    vec![crate::reasoning::runtime::AgentToolCall {
                        id: "call_h1".to_string(),
                        name: "deep_recall".to_string(),
                        arguments: serde_json::json!({ "query": "recent changes" }),
                    }],
                ),
                tool_ui_event: None,
                tool_call_ui_events: Vec::new(),
            },
            HistoryMessage::tool(
                "call_h1",
                "deep_recall",
                "summary=found prior notes",
                ToolUiEvent::DeepRecall(DeepRecallUiData { memory_count: 1 }),
            ),
        ]);

        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0]["role"], "user");
        assert_eq!(transcript[1]["role"], "assistant");
        assert_eq!(transcript[1]["content"].as_array().map(Vec::len), Some(1));
        assert_eq!(transcript[1]["content"][0]["type"], "text");
        assert!(transcript.iter().all(|message| {
            message["content"].as_array().is_none_or(|blocks| {
                blocks.iter().all(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) != Some("tool_result")
                })
            })
        }));
    }
}
