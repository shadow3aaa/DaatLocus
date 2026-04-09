//! 本模块包含 context，它是 Daat Locus 主循环中承载状态的结构体。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
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
    skill::{GlobalSkillRegistry, SkillContent},
    snapshot::Snapshot,
    telegram_transport_state::TelegramTransportStateHandle,
    workspace_app::WorkspaceAppRegistry,
};

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
    pub apps: AppManager,
    pub global_skills: GlobalSkillRegistry,
    pub workspace_apps: WorkspaceAppRegistry,
    pub telegram: TelegramTransportStateHandle,
    pub compiled_prompts: CompiledPromptStore,
    pub execution_cwd: PathBuf,
    pub sandbox_policy: RuntimeSandboxPolicy,
    pub dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
    pub active_runtime_turn: bool,
    pub active_app_notices: HashSet<AppId>,
    pub live_assistant_progress_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>,
    pub idle_since: Option<Instant>,
    pub last_idle_sleep_at: Option<Instant>,
    pub record_runtime_reviews: bool,
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

    pub fn read_skill(&self, id: &str) -> Result<SkillContent, miette::Report> {
        if let Some(skill) = self.apps.read_focused_skill(id)? {
            return Ok(skill);
        }
        if let Some(skill) = self.global_skills.read_skill(id) {
            return Ok(skill);
        }
        if let Some(focused) = self.apps.focused() {
            Err(miette::miette!(
                "skill `{id}` is not available as a global skill or on focused app {focused}"
            ))
        } else {
            Err(miette::miette!(
                "skill `{id}` is not available as a global skill; if you meant an app skill, focus the matching app first"
            ))
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

    pub async fn shutdown(mut self) {
        if self.hindsight_retain.flush().await.is_ok() {
            self.memory.mark_queued_retained();
        }
        self.hindsight_retain.shutdown().await;
        self.memory.shutdown().await;
        self.plan.shutdown().await;
        self.events.shutdown().await;
        self.pending_work.shutdown().await;
        let _ = self.apps.shutdown().await;
    }
}
