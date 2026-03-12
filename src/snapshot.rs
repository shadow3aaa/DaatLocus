//! 本模块包括快照定义，以及感官输入。记忆和短期任务在别处定义，因其较为复杂。

use std::fmt::Display;

use crate::system_info::SystemInfo;

/// 快照保存着当前agent的大脑状态
///
/// 这包括记忆、短期任务、感官输入。
pub struct Snapshot {
    sensory: Sensory,
}

impl Snapshot {
    pub fn new() -> Self {
        Self {
            sensory: Sensory::new(),
        }
    }
}

impl Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.sensory)
    }
}

struct Sensory {
    time: String,
    machine_status: SystemInfo,
}

impl Sensory {
    fn new() -> Self {
        let local = chrono::Local::now();
        let time = local.format("%Y-%m-%d %H:%M:%S %z").to_string();
        let machine_status = SystemInfo::sample();
        Self {
            time,
            machine_status,
        }
    }
}

impl Display for Sensory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "当前时间：{}", self.time)?;
        write!(f, "机器状态：\n{}", self.machine_status)
    }
}
