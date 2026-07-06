//! Runtime context state carried by the Daat Locus main loop.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use parking_lot::Mutex;

use crate::{
    app::AppManager,
    config::Config,
    context_budget::TokenEstimateBaseline,
    core::Llm,
    daemon::DaemonControlCommand,
    dashboard::{DashboardActivityHistoryStore, DashboardState},
    events::EventStore,
    live_progress::{LiveProgressEvent, TelegramLiveStatus},
    memory::Memory,
    openskills::OpenSkillsCatalog,
    pending_work::PendingWorkQueue,
    plan::Plan,
    preturn_state::PreTurnState,
    reasoning::{
        compiled::CompiledPromptStore,
        prompt_assembler::{PreTurnContextAssembler, runtime_system_prompt_text},
        prompt_doc::PromptDocument,
    },
    sandbox::RuntimeSandboxPolicy,
    telegram_acl::TelegramAclHandle,
    telegram_transport::state::TelegramTransportStateHandle,
    workspace_app::WorkspaceAppRegistry,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTurnPhase {
    PreflightPreTurnContext,
    PreflightCompaction,
    ModelRequest,
    ToolExecution,
}

impl RuntimeTurnPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::PreflightPreTurnContext => "preflight: preturn context",
            Self::PreflightCompaction => "preflight: compaction",
            Self::ModelRequest => "model request",
            Self::ToolExecution => "tool execution",
        }
    }
}

pub struct Context {
    pub session_id: Option<String>,
    pub llm: Box<dyn Llm + Send + Sync>,
    pub judge_llm: Box<dyn Llm + Send + Sync>,
    pub efficient_llm: Box<dyn Llm + Send + Sync>,
    pub config: Config,
    pub memory: Memory,
    pub plan: Plan,
    pub events: EventStore,
    pub pending_work: PendingWorkQueue,
    pub openskills: OpenSkillsCatalog,
    pub active_skill_run: Option<ActiveSkillRunSession>,
    pub pending_skill_run_flushes: Vec<PendingSkillRunFlush>,
    pub current_work_origin: Option<String>,
    pub apps: AppManager,
    pub workspace_apps: WorkspaceAppRegistry,
    pub telegram: TelegramTransportStateHandle,
    pub telegram_acl: TelegramAclHandle,
    pub compiled_prompts: CompiledPromptStore,
    pub execution_cwd: PathBuf,
    pub coding_project_dir: Option<PathBuf>,
    pub sandbox_policy: RuntimeSandboxPolicy,
    pub dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
    pub dashboard_history: Option<DashboardActivityHistoryStore>,
    pub daemon_control_tx: tokio::sync::mpsc::UnboundedSender<DaemonControlCommand>,
    pub latest_context_composition: Option<crate::dashboard::DashboardContextCompositionSnapshot>,
    pub active_runtime_turn: bool,
    pub active_runtime_phase: Option<RuntimeTurnPhase>,
    pub runtime_turn_started_at: Option<Instant>,
    pub runtime_turn_started_at_ms: Option<i64>,
    pub runtime_turn_epoch: u64,
    pub runtime_overflow_failures: Arc<Mutex<HashMap<String, usize>>>,
    pub runtime_model_request_failures: Arc<Mutex<HashMap<String, usize>>>,
    pub live_progress_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<LiveProgressEvent>>>>,
    pub telegram_live_drafts: TelegramLiveDraftRegistry,
    pub claimed_event_ids: Vec<String>,
    pub afterclaim_context_fingerprint: Option<String>,
    pub delivered_root_instruction_fingerprint: Option<String>,
    pub visible_source_lines:
        HashSet<crate::runtime::runtime_loop::coding_source_elision::CodingSourceLineKey>,
    pub idle_since: Option<Instant>,
    pub last_idle_sleep_at: Option<Instant>,
    pub session_title: crate::runtime::session_title::SessionTitleState,
    pub token_estimate_baseline: TokenEstimateBaseline,
}

#[derive(Debug, Clone)]
pub struct TelegramLiveDraftRecord {
    pub draft_id: i64,
    pub last_sent_text: Option<String>,
}

pub type TelegramLiveDraftRegistry = Arc<Mutex<HashMap<String, TelegramLiveDraftRecord>>>;

#[derive(Debug, Clone)]
pub struct ActiveSkillRunSession {
    pub run_id: String,
    pub skill_name: String,
    pub started_at_ms: i64,
    pub origin: String,
    pub turn_count: usize,
    pub tool_action_count: usize,
    pub manual_fix_detected: bool,
    pub rollback_detected: bool,
    pub failure_types: BTreeSet<String>,
    pub final_summary: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SkillRunOutcome {
    Completed,
    Blocked,
    Abandoned,
    Superseded,
    NoProgress,
}

#[derive(Debug, Clone)]
pub struct PendingSkillRunFlush {
    pub session: ActiveSkillRunSession,
    pub outcome: SkillRunOutcome,
}

impl Context {
    pub fn runtime_system_prompt_text(&self) -> String {
        runtime_system_prompt_text(self)
    }

    pub fn preturn_context_doc(&mut self, state: &PreTurnState) -> PromptDocument {
        PreTurnContextAssembler::default_runtime().assemble(self, state)
    }

    pub fn begin_skill_run_session(
        &mut self,
        skill_name: impl Into<String>,
        origin: impl Into<String>,
    ) {
        let skill_name = skill_name.into();
        if self
            .active_skill_run
            .as_ref()
            .is_some_and(|session| session.skill_name == skill_name)
        {
            return;
        }
        self.active_skill_run = Some(ActiveSkillRunSession {
            run_id: format!("skill-run:{}", uuid::Uuid::new_v4()),
            skill_name,
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            origin: origin.into(),
            turn_count: 0,
            tool_action_count: 0,
            manual_fix_detected: false,
            rollback_detected: false,
            failure_types: BTreeSet::new(),
            final_summary: String::new(),
        });
    }

    pub fn queue_active_skill_run_for_flush(&mut self, outcome: SkillRunOutcome) {
        if let Some(session) = self.active_skill_run.take() {
            self.pending_skill_run_flushes
                .push(PendingSkillRunFlush { session, outcome });
        }
    }

    pub fn install_live_progress(
        &mut self,
        tx: Option<tokio::sync::mpsc::UnboundedSender<LiveProgressEvent>>,
    ) {
        *self.live_progress_tx.lock() = tx;
    }

    pub fn get_or_create_telegram_live_draft(
        &self,
        event_id: impl Into<String>,
        draft_id: i64,
    ) -> (i64, Option<String>) {
        let event_id = event_id.into();
        let mut drafts = self.telegram_live_drafts.lock();
        let record = drafts
            .entry(event_id)
            .or_insert_with(|| TelegramLiveDraftRecord {
                draft_id,
                last_sent_text: None,
            });
        (record.draft_id, record.last_sent_text.clone())
    }

    pub fn clear_telegram_live_draft(&self, event_id: &str) {
        self.telegram_live_drafts.lock().remove(event_id);
    }

    pub fn emit_live_generation_started(&self) {
        self.emit_live_progress(LiveProgressEvent::GenerationStarted);
    }

    pub fn emit_live_assistant_progress(&self, content: &str) {
        self.emit_live_progress(LiveProgressEvent::AssistantContent {
            content: content.to_string(),
        });
    }

    pub fn emit_live_reasoning_progress(&self, content: &str) {
        self.emit_live_progress(LiveProgressEvent::ReasoningContent {
            content: content.to_string(),
        });
    }

    pub fn emit_live_telegram_status(&self, status: TelegramLiveStatus) {
        self.emit_live_progress(LiveProgressEvent::TelegramStatus(status));
    }

    fn emit_live_progress(&self, event: LiveProgressEvent) {
        if let Some(tx) = self.live_progress_tx.lock().as_ref() {
            let _ = tx.send(event);
        }
    }

    pub fn set_runtime_phase(&mut self, phase: Option<RuntimeTurnPhase>) {
        self.active_runtime_phase = phase;
    }

    pub fn record_runtime_overflow_failure(&self, key: &str) -> usize {
        let mut failures = self.runtime_overflow_failures.lock();
        let entry = failures.entry(key.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    pub fn clear_runtime_overflow_failure(&self, key: &str) {
        self.runtime_overflow_failures.lock().remove(key);
    }

    pub fn record_model_request_failure(&self, key: &str) -> usize {
        let mut failures = self.runtime_model_request_failures.lock();
        let entry = failures.entry(key.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    pub fn clear_model_request_failure(&self, key: &str) {
        self.runtime_model_request_failures.lock().remove(key);
    }

    pub async fn shutdown(self) {
        self.memory.shutdown().await;
        self.plan.shutdown().await;
        self.events.shutdown().await;
        self.pending_work.shutdown().await;
        let _ = self.apps.shutdown().await;
    }
}
