//! 本模块包含context，它是spinova自旋循环中承载状态的结构体

use std::{path::PathBuf, time::Instant};

use crate::{
    config::Config,
    core::LLM,
    dashboard::DashboardState,
    device::DeviceManager,
    emotion::Emotion,
    hindsight::{HindsightClient, HindsightRetainHandle},
    memory::Memory,
    obligations::Obligations,
    projects::Projects,
    reasoning::compiled::CompiledPromptStore,
    reasoning::runtime::PromptMemoryContext,
    telegram_device::TelegramDeviceHandle,
    work_state::WorkState,
};

pub struct Context {
    pub llm: Box<dyn LLM + Send + Sync>,
    pub judge_llm: Box<dyn LLM + Send + Sync>,
    pub config: Config,
    pub hindsight: HindsightClient,
    pub hindsight_retain: HindsightRetainHandle,
    pub memory: Memory,
    pub prompt_memory: PromptMemoryContext,
    pub obligations: Obligations,
    pub projects: Projects,
    pub work_state: WorkState,
    pub emotion: Emotion,
    pub devices: DeviceManager,
    pub telegram: TelegramDeviceHandle,
    pub compiled_prompts: CompiledPromptStore,
    pub execution_cwd: PathBuf,
    pub dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
    pub idle_since: Option<Instant>,
    pub last_idle_sleep_at: Option<Instant>,
    pub record_runtime_reviews: bool,
}

impl Context {
    pub async fn shutdown(self) {
        let _ = self.hindsight_retain.flush().await;
        self.hindsight_retain.shutdown().await;
        self.memory.shutdown().await;
        self.obligations.shutdown().await;
        self.projects.shutdown().await;
        self.work_state.shutdown().await;
        self.emotion.shutdown().await;
        let _ = self.devices.shutdown().await;
    }
}
