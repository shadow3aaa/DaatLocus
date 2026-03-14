//! 本模块包含context，它是spinova自旋循环中承载状态的结构体

use crate::{
    config::Config, core::LLM, device::DeviceManager, emotion::Emotion, memory::Memory,
    obligations::Obligations, projects::Projects, tasks::Tasks,
    telegram_device::TelegramDeviceHandle,
};

pub struct Context {
    pub llm: Box<dyn LLM + Send + Sync>,
    pub config: Config,
    pub memory: Memory,
    pub obligations: Obligations,
    pub projects: Projects,
    pub tasks: Tasks,
    pub emotion: Emotion,
    pub devices: DeviceManager,
    pub telegram: TelegramDeviceHandle,
}

impl Context {
    pub async fn shutdown(self) {
        self.memory.shutdown().await;
        self.obligations.shutdown().await;
        self.projects.shutdown().await;
        self.tasks.shutdown().await;
        self.emotion.shutdown().await;
    }
}
