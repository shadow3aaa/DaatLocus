//! 本模块包含context，它是spinova自旋循环中承载状态的结构体

use crate::{
    config::Config,
    memory::Memory,
    tasks::{Task, Tasks},
};

pub struct Context {
    pub config: Config,
    pub memory: Memory,
    pub tasks: Tasks,
    /// 无聊程度，范围0-1
    pub boredom: f32,
}

impl Context {
    pub async fn shutdown(self) {
        self.memory.shutdown().await;
        self.tasks.shutdown().await;
    }
}
