//! 本模块定义快照，即LLM应当看到的输入。

use std::fmt::Display;

use crate::{
    context::Context,
    device::{AttentionLevel, DeviceId, DeviceStateRender},
    obligations::Obligations,
    projects::Projects,
    system_info::SystemInfo,
    work_state::WorkState,
};

/// 快照保存着当前agent的大脑状态
///
/// 这包括义务、项目、当前工作状态和感官输入。
pub struct Snapshot {
    sensory: Sensory,
    obligations: Obligations,
    projects: Projects,
    work_state: WorkState,
    devices: DeviceSnapshot,
}

impl Snapshot {
    pub async fn new(context: &mut Context) -> Self {
        let devices = DeviceSnapshot::new(context);
        Self {
            sensory: Sensory::new(),
            obligations: context.obligations.clone(),
            projects: context.projects.clone(),
            work_state: context.work_state.clone(),
            devices,
        }
    }
}

impl Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "感官：")?;
        writeln!(f, "{}", self.sensory)?;
        writeln!(f, "义务列表：")?;
        writeln!(f, "{}", self.obligations)?;
        writeln!(f, "项目列表：")?;
        writeln!(f, "{}", self.projects)?;
        writeln!(f, "当前工作状态：")?;
        writeln!(f, "{}", self.work_state)?;
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

struct DeviceSnapshot {
    focused_device: Option<DeviceId>,
    states: Vec<(DeviceId, DeviceStateRender)>,
}

impl DeviceSnapshot {
    fn new(context: &Context) -> Self {
        Self {
            focused_device: context.devices.focused(),
            states: context.devices.state_renders(),
        }
    }
}

impl Display for DeviceSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.focused_device {
            Some(device) => writeln!(f, "当前前景设备：{device}")?,
            None => writeln!(f, "当前前景设备：无")?,
        }

        let attention_hints = self
            .states
            .iter()
            .filter(|(_, state)| !state.is_focused)
            .filter_map(|(id, state)| device_attention_hint(*id, state));
        let attention_hints = attention_hints.collect::<Vec<_>>();
        if !attention_hints.is_empty() {
            writeln!(f, "后台设备提醒：")?;
            for hint in attention_hints {
                writeln!(f, "- {hint}")?;
            }
        }

        writeln!(f, "设备结构状态：")?;
        for (id, state) in &self.states {
            let focus_state = if state.is_focused { "前景" } else { "后台" };
            writeln!(
                f,
                "- {id} / {}：{}，注意力等级={}",
                state.title, focus_state, state.attention
            )?;
            for line in &state.lines {
                writeln!(f, "  {line}")?;
            }
        }
        Ok(())
    }
}

fn device_attention_hint(device_id: DeviceId, state: &DeviceStateRender) -> Option<String> {
    match device_id {
        DeviceId::Terminal
            if !state.is_focused && matches!(state.attention, AttentionLevel::Notice) =>
        {
            let session_id = state
                .lines
                .iter()
                .find_map(|line| line.strip_prefix("active_session="))
                .unwrap_or("unknown");
            if numeric_field(&state.lines, "sessions_with_unread_output") > 0 {
                Some(format!("Terminal 会话 {session_id} 有未读输出"))
            } else {
                Some(format!("Terminal 会话 {session_id} 需要注意"))
            }
        }
        DeviceId::Telegram
            if !state.is_focused && matches!(state.attention, AttentionLevel::Notice) =>
        {
            let pending_resolution = numeric_field(&state.lines, "pending_resolution");
            let pending_reply = numeric_field(&state.lines, "pending_reply");
            let unread_messages = numeric_field(&state.lines, "unread_messages");
            if pending_resolution > 0 {
                Some(format!("Telegram 有 {pending_resolution} 个会话待判断"))
            } else if pending_reply > 0 {
                Some(format!("Telegram 有 {pending_reply} 个会话待回复"))
            } else if unread_messages > 0 {
                Some(format!("Telegram 有 {unread_messages} 条未读消息"))
            } else {
                Some("Telegram 有需要注意的状态变化".to_string())
            }
        }
        _ => None,
    }
}

fn numeric_field(lines: &[String], key: &str) -> usize {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}
