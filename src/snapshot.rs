//! 本模块定义快照，即LLM应当看到的输入。

use std::fmt::Display;

use crate::{
    context::Context,
    device::{DeviceId, FocusedRender, PeripheralRender},
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
    obligations: Obligations,
    projects: Projects,
    next_actions: Tasks,
    devices: DeviceSnapshot,
}

impl Snapshot {
    pub async fn new(context: &mut Context) -> Self {
        let devices = DeviceSnapshot::new(context);
        Self {
            sensory: Sensory::new(),
            obligations: context.obligations.clone(),
            projects: context.projects.clone(),
            next_actions: context.tasks.clone(),
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

pub fn summarize_terminal_screen(content: &str) -> String {
    let lower = content.to_ascii_lowercase();
    let prompt_visible = lower.contains("<cursor>")
        && (lower.contains("ps>")
            || lower.contains("ps ")
            || lower.contains(">")
            || lower.contains("$")
            || lower.contains("#"));
    let interactive_prompt = lower.contains("username:")
        || lower.contains("password:")
        || lower.contains("passphrase")
        || lower.contains("would you like")
        || lower.contains("[y/n]")
        || lower.contains("(y/n)")
        || lower.contains("press enter to continue")
        || lower.contains("login")
        || lower.contains("authorize")
        || lower.contains("otp")
        || lower.contains("verification code")
        || lower.contains(">>>")
        || lower.contains("... ");
    let terminal_mode = if lower.contains("(end)")
        || lower.contains("press h for help or q to quit")
        || lower.contains("manual page")
    {
        "pager"
    } else if interactive_prompt {
        "interactive_prompt"
    } else if prompt_visible {
        "shell"
    } else if lower.contains("cursor") {
        "running"
    } else {
        "unknown"
    };
    let command_completed = matches!(terminal_mode, "shell");
    let mut lines = vec![
        format!("mode={terminal_mode}"),
        format!("prompt_visible={prompt_visible}"),
        format!("interactive_prompt={interactive_prompt}"),
        format!("command_completed={command_completed}"),
    ];
    match terminal_mode {
        "pager" => lines.push("建议：若目标只是返回 shell，可优先考虑 q。".to_string()),
        "interactive_prompt" => {
            lines.push("建议：终端已进入交互式提示，不要把它误判成普通 shell 输出。".to_string())
        }
        "running" => lines.push("建议：终端可能仍在输出或等待，先确认是否真的回到 prompt。".to_string()),
        "shell" => lines.push("上一条命令大概率已结束，可基于当前 prompt 决定下一步。".to_string()),
        _ => {}
    }
    lines.join("\n")
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
