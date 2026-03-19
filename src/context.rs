//! 本模块包含context，它是spinova自旋循环中承载状态的结构体

use crate::{
    config::Config, core::LLM, device::DeviceManager, emotion::Emotion,
    hindsight::{HindsightClient, HindsightRetainHandle}, memory::Memory,
    obligations::Obligations, projects::Projects, reasoning::compiled::CompiledPromptStore,
    reasoning::runtime::PromptMemoryContext, tasks::Tasks, telegram_device::TelegramDeviceHandle,
};

pub struct Context {
    pub llm: Box<dyn LLM + Send + Sync>,
    pub judge_llm: Box<dyn LLM + Send + Sync>,
    pub config: Config,
    pub hindsight: Option<HindsightClient>,
    pub hindsight_retain: Option<HindsightRetainHandle>,
    pub memory: Memory,
    pub prompt_memory: PromptMemoryContext,
    pub obligations: Obligations,
    pub projects: Projects,
    pub tasks: Tasks,
    pub emotion: Emotion,
    pub devices: DeviceManager,
    pub telegram: TelegramDeviceHandle,
    pub compiled_prompts: CompiledPromptStore,
}

impl Context {
    pub async fn shutdown(self) {
        if let Some(handle) = &self.hindsight_retain {
            let _ = handle.flush().await;
            handle.shutdown().await;
        }
        self.memory.shutdown().await;
        self.obligations.shutdown().await;
        self.projects.shutdown().await;
        self.tasks.shutdown().await;
        self.emotion.shutdown().await;
        let _ = self.devices.shutdown().await;
    }
}
