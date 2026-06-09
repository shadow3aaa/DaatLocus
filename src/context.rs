//! Runtime context state carried by the Daat Locus main loop.

use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::{
    app::{AppId, AppManager},
    config::Config,
    context_budget::TokenEstimateBaseline,
    core::Llm,
    daemon::DaemonControlCommand,
    dashboard::{DashboardActivityHistoryStore, DashboardState},
    events::EventStore,
    live_progress::{LiveProgressEvent, TelegramLiveStatus},
    memory::Memory,
    pending_work::PendingWorkQueue,
    plan::Plan,
    preturn_state::PreTurnState,
    reasoning::{
        compiled::CompiledPromptStore,
        prompt_assembler::{PreTurnContextAssembler, SystemPromptAssembler},
        prompt_doc::PromptDocument,
    },
    sandbox::RuntimeSandboxPolicy,
    telegram_acl::TelegramAclHandle,
    telegram_transport::state::TelegramTransportStateHandle,
    workflow::{PrimitiveComposition, PrimitiveRunOutcome, PrimitiveSpec, PrimitiveStore},
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
    pub llm: Box<dyn Llm + Send + Sync>,
    pub judge_llm: Box<dyn Llm + Send + Sync>,
    pub efficient_llm: Box<dyn Llm + Send + Sync>,
    pub config: Config,
    pub memory: Memory,
    pub plan: Plan,
    pub events: EventStore,
    pub pending_work: PendingWorkQueue,
    pub workflows: PrimitiveStore,
    pub bound_primitive_id: Option<String>,
    pub bound_primitive_composition: Option<PrimitiveComposition>,
    pub active_primitive_run: Option<ActivePrimitiveRunSession>,
    pub pending_primitive_run_flushes: Vec<PendingPrimitiveRunFlush>,
    pub current_work_origin: Option<String>,
    pub workflow_step_started_bound_id: Option<String>,
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
    pub runtime_turn_epoch: u64,
    pub active_app_notices: HashMap<AppNoticeKey, ActiveAppNotice>,
    pub runtime_overflow_failures: Arc<Mutex<HashMap<String, usize>>>,
    pub runtime_model_request_failures: Arc<Mutex<HashMap<String, usize>>>,
    pub suppressed_app_notices: Arc<Mutex<HashMap<AppNoticeKey, SuppressedAppNotice>>>,
    pub live_progress_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<LiveProgressEvent>>>>,
    pub telegram_live_drafts: TelegramLiveDraftRegistry,
    pub claimed_event_ids: Vec<String>,
    pub claimed_app_notices: Vec<AppNoticeKey>,
    pub afterclaim_context_fingerprint: Option<String>,
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
pub struct ActivePrimitiveRunSession {
    pub run_id: String,
    pub workflow_id: String,
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
pub struct PendingPrimitiveRunFlush {
    pub session: ActivePrimitiveRunSession,
    pub outcome: PrimitiveRunOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppNoticeKey {
    pub app: AppId,
    pub reason: String,
}

impl AppNoticeKey {
    pub fn new(app: AppId, reason: impl Into<String>) -> Self {
        Self {
            app,
            reason: normalize_app_notice_reason_lossy(reason.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActiveAppNotice {
    pub resolved: bool,
    pub unresolved_turns: usize,
}

#[derive(Debug, Clone)]
pub struct SuppressedAppNotice {
    pub until: Instant,
}

pub fn normalize_app_notice_reason(reason: &str) -> Option<String> {
    let normalized = normalize_app_notice_reason_lossy(reason);
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_app_notice_reason_lossy(reason: impl AsRef<str>) -> String {
    reason.as_ref().trim().to_string()
}

impl Context {
    pub fn runtime_system_prompt_doc(&self) -> PromptDocument {
        SystemPromptAssembler::default_runtime().assemble(self)
    }

    pub fn preturn_context_doc(&self, state: &PreTurnState) -> PromptDocument {
        PreTurnContextAssembler::default_runtime().assemble(self, state)
    }

    pub fn bound_primitive(&self) -> Option<&PrimitiveSpec> {
        self.bound_primitive_id
            .as_deref()
            .and_then(|workflow_id| self.workflows.get(workflow_id))
    }

    pub fn bound_primitive_composition_specs(&self) -> Vec<&PrimitiveSpec> {
        self.bound_primitive_composition
            .as_ref()
            .map(|composition| {
                composition
                    .primitive_ids
                    .iter()
                    .filter_map(|primitive_id| self.workflows.get(primitive_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn begin_composed_primitive_run_session(&mut self, workflow_id: impl Into<String>) {
        let workflow_id = workflow_id.into();
        if self
            .active_primitive_run
            .as_ref()
            .is_some_and(|session| session.workflow_id == workflow_id)
        {
            return;
        }
        self.active_primitive_run = Some(ActivePrimitiveRunSession {
            run_id: format!("workflow-run:{}", uuid::Uuid::new_v4()),
            workflow_id,
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            origin: self
                .current_work_origin
                .clone()
                .unwrap_or_else(|| "runtime_work".to_string()),
            turn_count: 0,
            tool_action_count: 0,
            manual_fix_detected: false,
            rollback_detected: false,
            failure_types: BTreeSet::new(),
            final_summary: String::new(),
        });
    }

    pub fn queue_active_primitive_run_for_flush(&mut self, outcome: PrimitiveRunOutcome) {
        if let Some(session) = self.active_primitive_run.take() {
            self.pending_primitive_run_flushes
                .push(PendingPrimitiveRunFlush { session, outcome });
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

    pub fn suppress_app_notice(&self, app: &AppId, reason: impl Into<String>, duration: Duration) {
        self.suppressed_app_notices.lock().insert(
            AppNoticeKey::new(app.clone(), reason),
            SuppressedAppNotice {
                until: Instant::now() + duration,
            },
        );
    }

    pub fn clear_app_notice_suppression(&self, app: &AppId) {
        self.suppressed_app_notices
            .lock()
            .retain(|key, _| &key.app != app);
    }

    pub fn is_app_notice_suppressed(&self, app: &AppId, reason: &str) -> bool {
        let mut suppressed = self.suppressed_app_notices.lock();
        let key = AppNoticeKey::new(app.clone(), reason);
        let Some(entry) = suppressed.get(&key) else {
            return false;
        };
        if Instant::now() >= entry.until {
            suppressed.remove(&key);
            return false;
        }
        true
    }

    pub fn activate_app_notice(&mut self, app: AppId, reason: impl Into<String>) {
        self.clear_active_app_notice(&app);
        self.active_app_notices.insert(
            AppNoticeKey::new(app, reason),
            ActiveAppNotice {
                resolved: false,
                unresolved_turns: 0,
            },
        );
    }

    pub fn clear_active_app_notice(&mut self, app: &AppId) {
        self.active_app_notices.retain(|key, _| &key.app != app);
    }

    pub fn app_notice_is_resolved(&self, key: &AppNoticeKey) -> bool {
        self.active_app_notices
            .get(key)
            .is_some_and(|notice| notice.resolved)
    }

    pub fn resolve_claimed_app_notice(&mut self, key: &AppNoticeKey) -> bool {
        if !self
            .claimed_app_notices
            .iter()
            .any(|claimed| claimed == key)
        {
            return false;
        }
        self.clear_active_app_notice(&key.app);
        self.active_app_notices.insert(
            key.clone(),
            ActiveAppNotice {
                resolved: true,
                unresolved_turns: 0,
            },
        );
        true
    }

    pub fn record_unresolved_app_notice_turn(&mut self, key: &AppNoticeKey) -> usize {
        let entry = self
            .active_app_notices
            .entry(key.clone())
            .or_insert_with(|| ActiveAppNotice {
                resolved: false,
                unresolved_turns: 0,
            });
        entry.unresolved_turns += 1;
        entry.unresolved_turns
    }

    pub fn claimed_app_notices_are_resolved(&self) -> bool {
        !self.claimed_app_notices.is_empty()
            && self
                .claimed_app_notices
                .iter()
                .all(|notice| self.app_notice_is_resolved(notice))
    }

    pub async fn shutdown(self) {
        self.workflows.shutdown().await;
        self.memory.shutdown().await;
        self.plan.shutdown().await;
        self.events.shutdown().await;
        self.pending_work.shutdown().await;
        let _ = self.apps.shutdown().await;
    }
}
