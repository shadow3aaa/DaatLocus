//! 本模块包含context，它是spinova自旋循环中承载状态的结构体

use crate::{
    config::Config,
    core::LLM,
    device::DeviceManager,
    emotion::Emotion,
    memory::Memory,
    tasks::Tasks,
};

pub struct Context {
    pub llm: Box<dyn LLM + Send + Sync>,
    pub config: Config,
    pub memory: Memory,
    pub tasks: Tasks,
    pub emotion: Emotion,
    pub devices: DeviceManager,
}

impl Context {
    pub async fn shutdown(self) {
        self.memory.shutdown().await;
        self.tasks.shutdown().await;
        self.emotion.shutdown().await;
    }
}
