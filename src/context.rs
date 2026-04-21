//! 本模块包含 context，它是 Daat Locus 主循环中承载状态的结构体。

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::{
    app::{AppId, AppManager},
    config::Config,
    core::LLM,
    dashboard::DashboardState,
    events::EventStore,
    hindsight::{HindsightClient, HindsightRetainHandle},
    memory::Memory,
    pending_work::PendingWorkQueue,
    plan::Plan,
    reasoning::runtime::PromptMemoryContext,
    reasoning::{
        compiled::CompiledPromptStore,
        prompt_assembler::{SnapshotAssembler, SystemPromptAssembler},
        prompt_doc::PromptDocument,
    },
    sandbox::RuntimeSandboxPolicy,
    snapshot::Snapshot,
    telegram_acl::TelegramAclHandle,
    telegram_transport::state::TelegramTransportStateHandle,
    workflow::{WorkflowRunOutcome, WorkflowSpec, WorkflowStore},
    workspace_app::WorkspaceAppRegistry,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTurnPhase {
    PreflightMemory,
    PreflightSnapshot,
    PreflightCompaction,
    ModelRequest,
    ToolExecution,
}

impl RuntimeTurnPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::PreflightMemory => "preflight: hindsight memory",
            Self::PreflightSnapshot => "preflight: snapshot",
            Self::PreflightCompaction => "preflight: compaction",
            Self::ModelRequest => "model request",
            Self::ToolExecution => "tool execution",
        }
    }
}

pub struct Context {
    pub llm: Box<dyn LLM + Send + Sync>,
    pub judge_llm: Box<dyn LLM + Send + Sync>,
    pub config: Config,
    pub hindsight: HindsightClient,
    pub hindsight_retain: HindsightRetainHandle,
    pub memory: Memory,
    pub prompt_memory: PromptMemoryContext,
    pub plan: Plan,
    pub events: EventStore,
    pub pending_work: PendingWorkQueue,
    pub workflows: WorkflowStore,
    pub bound_workflow_id: Option<String>,
    pub active_workflow_run: Option<ActiveWorkflowRunSession>,
    pub pending_workflow_run_flushes: Vec<PendingWorkflowRunFlush>,
    pub current_work_origin: Option<String>,
    pub workflow_step_started_bound_id: Option<String>,
    pub apps: AppManager,
    pub workspace_apps: WorkspaceAppRegistry,
    pub telegram: TelegramTransportStateHandle,
    pub telegram_acl: TelegramAclHandle,
    pub compiled_prompts: CompiledPromptStore,
    pub execution_cwd: PathBuf,
    pub sandbox_policy: RuntimeSandboxPolicy,
    pub dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
    pub active_runtime_turn: bool,
    pub active_runtime_phase: Option<RuntimeTurnPhase>,
    pub runtime_turn_started_at: Option<Instant>,
    pub active_app_notices: HashSet<AppId>,
    pub runtime_overflow_failures: Arc<Mutex<HashMap<String, usize>>>,
    pub suppressed_app_notices: Arc<Mutex<HashMap<AppId, SuppressedAppNotice>>>,
    pub live_assistant_progress_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>,
    pub claimed_event_ids: Vec<String>,
    pub idle_since: Option<Instant>,
    pub last_idle_sleep_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct ActiveWorkflowRunSession {
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
pub struct PendingWorkflowRunFlush {
    pub session: ActiveWorkflowRunSession,
    pub outcome: WorkflowRunOutcome,
}

#[derive(Debug, Clone)]
pub struct SuppressedAppNotice {
    pub reason: String,
    pub until: Instant,
}

impl Context {
    pub fn runtime_system_prompt_doc(&self) -> PromptDocument {
        SystemPromptAssembler::default_runtime().assemble(self)
    }

    pub fn runtime_snapshot_doc(&self, snapshot: &Snapshot) -> PromptDocument {
        SnapshotAssembler::default_runtime().assemble(self, snapshot)
    }

    pub fn resolve_tool_path(&self, path: &Path, base: Option<&Path>) -> PathBuf {
        self.sandbox_policy
            .resolve_path(path, base.or(Some(&self.execution_cwd)))
    }

    pub fn bound_workflow(&self) -> Option<&WorkflowSpec> {
        self.bound_workflow_id
            .as_deref()
            .and_then(|workflow_id| self.workflows.get(workflow_id))
    }

    pub fn begin_workflow_run_session(&mut self, workflow_id: impl Into<String>) {
        let workflow_id = workflow_id.into();
        if self
            .active_workflow_run
            .as_ref()
            .is_some_and(|session| session.workflow_id == workflow_id)
        {
            return;
        }
        self.active_workflow_run = Some(ActiveWorkflowRunSession {
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

    pub fn queue_active_workflow_run_for_flush(&mut self, outcome: WorkflowRunOutcome) {
        if let Some(session) = self.active_workflow_run.take() {
            self.pending_workflow_run_flushes
                .push(PendingWorkflowRunFlush { session, outcome });
        }
    }

    pub fn install_live_assistant_progress(
        &mut self,
        tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    ) {
        *self.live_assistant_progress_tx.lock() = tx;
    }

    pub fn emit_live_assistant_progress(&self, content: &str) {
        if let Some(tx) = self.live_assistant_progress_tx.lock().as_ref() {
            let _ = tx.send(content.to_string());
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

    pub fn suppress_app_notice(&self, app: &AppId, reason: impl Into<String>, duration: Duration) {
        self.suppressed_app_notices.lock().insert(
            app.clone(),
            SuppressedAppNotice {
                reason: reason.into(),
                until: Instant::now() + duration,
            },
        );
    }

    pub fn clear_app_notice_suppression(&self, app: &AppId) {
        self.suppressed_app_notices.lock().remove(app);
    }

    pub fn is_app_notice_suppressed(&self, app: &AppId, reason: &str) -> bool {
        let mut suppressed = self.suppressed_app_notices.lock();
        let Some(entry) = suppressed.get(app) else {
            return false;
        };
        if entry.reason != reason || Instant::now() >= entry.until {
            suppressed.remove(app);
            return false;
        }
        true
    }

    pub async fn shutdown(mut self) {
        if self
            .hindsight_retain
            .flush_with_timeout(Duration::from_secs(30))
            .await
            .is_ok()
        {
            self.memory.mark_queued_retained();
        }
        self.hindsight_retain.shutdown().await;
        self.workflows.shutdown().await;
        self.memory.shutdown().await;
        self.plan.shutdown().await;
        self.events.shutdown().await;
        self.pending_work.shutdown().await;
        let _ = self.apps.shutdown().await;
    }
}
