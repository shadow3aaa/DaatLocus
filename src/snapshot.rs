//! 本模块定义快照，即LLM应当看到的输入。

use std::fmt::Display;

use crate::{
    context::Context,
    device::{DeviceId, FocusedRender, PeripheralRender},
    memory::Memory,
    obligations::Obligations,
    projects::Projects,
    system_info::SystemInfo,
    tasks::Tasks,
};

/// 快照保存着当前agent的大脑状态
///
/// 这包括记忆、义务、项目、下一步动作和感官输入。
pub struct Snapshot {
    sensory: Sensory,
    current_memory: CurrentMemory,
    obligations: Obligations,
    projects: Projects,
    next_actions: Tasks,
    devices: DeviceSnapshot,
}

impl Snapshot {
    pub async fn new(context: &mut Context) -> Self {
        Self {
            sensory: Sensory::new(),
            current_memory: CurrentMemory::new(&mut context.memory).await,
            obligations: context.obligations.clone(),
            projects: context.projects.clone(),
            next_actions: context.tasks.clone(),
            devices: DeviceSnapshot::new(context),
        }
    }
}

impl Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "感官：")?;
        writeln!(f, "{}", self.sensory)?;
        writeln!(f, "记忆：")?;
        writeln!(f, "{}", self.current_memory)?;
        writeln!(f, "义务列表：")?;
        writeln!(f, "{}", self.obligations)?;
        writeln!(f, "项目列表：")?;
        writeln!(f, "{}", self.projects)?;
        writeln!(f, "下一步动作列表：")?;
        writeln!(f, "{}", self.next_actions)?;
        writeln!(f, "设备：")?;
        write!(f, "{}", self.devices)
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

struct CurrentMemory {
    current_doing: Option<String>,
    trail: Vec<String>,
    associated_memories: Vec<String>,
    learned_experiences: Vec<String>,
}

impl CurrentMemory {
    const EMPTY: Self = Self {
        current_doing: None,
        trail: Vec::new(),
        associated_memories: Vec::new(),
        learned_experiences: Vec::new(),
    };

    async fn new(memory: &mut Memory) -> Self {
        let Some(current_doing) = memory.current_doing() else {
            return Self::EMPTY;
        };
        let trail = memory.trail();
        let query = format!(
            "在【{}】时，发生：【{}】",
            current_doing,
            trail.last().unwrap()
        );
        let associated_memories = memory.search_mem(&query, 5).await;
        let learned_experiences = memory.search_l3(&query, 3);
        Self {
            current_doing: Some(current_doing),
            trail,
            associated_memories,
            learned_experiences,
        }
    }
}

impl Display for CurrentMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(current_doing) = &self.current_doing {
            writeln!(f, "当前正在：{}", current_doing)?;
        } else {
            writeln!(f, "当前没有在干什么")?;
        }
        writeln!(f, "近期经历：")?;
        for event in self.trail.iter() {
            writeln!(f, "{event}")?;
            writeln!(f, "然后")?;
        }
        writeln!(f, "联想回忆：")?;
        let len = self.associated_memories.len();
        for (i, mem) in self.associated_memories.iter().enumerate() {
            if i != len - 1 {
                writeln!(f, "{mem}")?;
            } else {
                write!(f, "{mem}")?;
            }
        }
        if !self.learned_experiences.is_empty() {
            writeln!(f)?;
            writeln!(f, "习得经验：")?;
            for (i, lesson) in self.learned_experiences.iter().enumerate() {
                if i + 1 < self.learned_experiences.len() {
                    writeln!(f, "{lesson}")?;
                } else {
                    write!(f, "{lesson}")?;
                }
            }
        }
        Ok(())
    }
}

struct DeviceSnapshot {
    focused_device: Option<DeviceId>,
    peripheral: Vec<(DeviceId, PeripheralRender)>,
    focused_view: Option<FocusedRender>,
}

impl DeviceSnapshot {
    fn new(context: &Context) -> Self {
        Self {
            focused_device: context.devices.focused(),
            peripheral: context.devices.peripheral_renders(),
            focused_view: context.devices.focused_render(),
        }
    }
}

impl Display for DeviceSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.focused_device {
            Some(device) => writeln!(f, "当前前景设备：{device}")?,
            None => writeln!(f, "当前前景设备：无")?,
        }

        writeln!(f, "设备外围感知：")?;
        for (id, render) in &self.peripheral {
            let focus_state = if render.is_focused {
                "前景"
            } else {
                "后台"
            };
            let action_state = if render.interactive {
                "可操作"
            } else {
                "只读"
            };
            writeln!(
                f,
                "- {id} / {}：{}，{}，注意力等级={}",
                render.title, focus_state, action_state, render.attention
            )?;
            writeln!(f, "  {}", render.summary)?;
        }

        if let Some(view) = &self.focused_view {
            let action_state = if view.interactive {
                "可操作"
            } else {
                "只读"
            };
            writeln!(f, "前景设备画面：")?;
            writeln!(f, "--- {} / {} ---", view.title, action_state)?;
            writeln!(f, "{}", view.content)?;
            write!(f, "--- {} / {} ---", view.title, action_state)?;
        } else {
            write!(f, "当前没有设备处于前景，因此看不到任何设备的完整画面。")?;
        }
        Ok(())
    }
}
