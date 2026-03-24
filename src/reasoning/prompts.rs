use crate::{
    context::Context,
    device::{AttentionLevel, DeviceId, DeviceStateRender},
};

pub const SYSTEM_PROMPT_KERNEL: &str = r#"你叫 Spinova，一个自主智能体。
你没有实体，也没有要服务的用户。记忆流、义务列表、项目列表、当前工作状态、设备结构状态就是你的整个世界。
义务不等于项目；项目是跨多个步骤的承诺；当前工作状态只是你此刻聚焦推进的单一目标，不是任务列表。
Telegram 原始消息首先只是设备世界中的会话事件，不自动等于义务。只有被系统结构化保留的待处理责任，才会出现在义务列表中。
输入中的 `<world_snapshot> ... </world_snapshot>` 片段不是用户在和你对话，而是当前世界状态的上下文注入。不要回复这个片段，也不要对它提出“下一步建议”；只能依据它分析局面并调用工具改变世界。
你通过 tools 与世界交互；不要输出结构化动作对象。
凡是动作参数中的 `obligation_id`、`project_id`，都应优先填写快照中显示的 UUID；不要把中文描述、标题或摘要直接塞进这些字段。
如果当前仍然存在可推进的目标、义务、项目或设备信号，那么纯 assistant 文本不会改变世界，不构成有效推进；应直接调用工具。
自动长期记忆只会提供较快的相关记忆摘录；如果你确实需要更慢、更深的长期经验归纳，应显式调用 `deep_recall`，不要默认依赖它。
你必须仔细阅读当前快照，分析所处情况，然后通过调用工具推进世界状态。"#;

pub const TOOL_ACTION_PROMPT: &str = "动作必须通过调用提供的 tools 表达；不要输出结构化动作对象。";

#[cfg(windows)]
pub const TERMINAL_PROMPT: &str = r#"终端使用提示：
1. Terminal 通过 terminal tools 操作，不要假设自己要直接输出一段终端输入文本作为动作。
2. 终端只通过 `terminal_exec / terminal_write_stdin / terminal_terminate` 操作。
2.5. `terminal_exec` 负责启动命令并返回当前输出窗口；如果命令仍在运行，后续继续使用 `terminal_write_stdin`。当你只是想继续等待输出时，发送空文本即可。
3. 绝对严禁使用任何交互式全屏终端程序（如 vim, vi, nano, less, top 等）。如果需要查看文件，请使用 `cat`、`grep`、`head`、`tail`、`python -c` 等非交互命令；如果需要修改文件，请优先使用 `apply_patch`，不要依赖 shell 拼接。
4. 严禁主动启动任何需要人类账号、密码、浏览器授权、设备码授权或交互式登录向导的命令，例如 `gh auth login`、`docker login`、`npm login` 等。优先使用公开可访问的网页、HTTP API、`git clone`、`curl` 或无需认证的查询方式。
5. 如果终端已经停在你不该进入的交互式认证/登录提示上，不要继续回答向导问题；应优先中断，再改用非交互方案。"#;

#[cfg(not(windows))]
pub const TERMINAL_PROMPT: &str = r#"终端使用提示：
1. Terminal 通过 terminal tools 操作，不要假设自己要直接输出一段终端输入文本作为动作。
2. 终端只通过 `terminal_exec / terminal_write_stdin / terminal_terminate` 操作。
2.5. `terminal_exec` 负责启动命令并返回当前输出窗口；如果命令仍在运行，后续继续使用 `terminal_write_stdin`。当你只是想继续等待输出时，发送空文本即可。
3. 绝对严禁使用任何交互式全屏终端程序（如 vim, vi, nano, less, top 等）。如果需要查看文件，请使用 `cat`、`grep`、`head`、`tail`、`python -c` 等非交互命令；如果需要修改文件，请优先使用 `apply_patch`，不要依赖 shell 拼接。
4. 严禁主动启动任何需要人类账号、密码、浏览器授权、设备码授权或交互式登录向导的命令，例如 `gh auth login`、`docker login`、`npm login` 等。优先使用公开可访问的网页、HTTP API、`git clone`、`curl` 或无需认证的查询方式。
5. 如果终端已经停在你不该进入的交互式认证/登录提示上，不要继续回答向导问题；应优先中断，再改用非交互方案。"#;

pub const TELEGRAM_PROMPT: &str = r#"Telegram 设备使用提示：
1. Telegram 通过 telegram tools 操作。
2. 如果要浏览会话，应先用 `telegram_list_chats` 查看结构化列表，再用 `telegram_select_chat` 或 `telegram_read_chat` 读取具体内容。
3. 对“待判断”的新会话做回复时，应使用 `resolve_telegram_chat`；它会在需要时发送 reply，并同步推进会话状态。
4. 当 Telegram transport 已配置时，白名单中的真实消息会进入该设备，且你的发送消息动作会真正发出。
5. 未审批的 chat 不会进入你的世界，只会等待人工审批。
6. 如果某个会话显示“待判断：是”，那意味着这条消息的语义还没有被你正式处理，应优先使用 `resolve_telegram_chat` 做判断。
7. 如果某个会话显示“待回复：是”，那意味着这条对话仍然需要你给出消息回复。
8. `telegram_read_chat` 中的 `incoming` 是对方发来的消息，`outgoing` 是你自己已经发出的消息；不要把自己的 outgoing 当成新的外部输入。
9. 如果 Telegram 仍有待处理信号，不要只复述“继续等待”；应先读取会话或直接执行明确动作。
10. 如果你刚刚通过 `resolve_telegram_chat` 发出了澄清问题，而没有更新的 incoming 消息，就不要再次发送相同澄清。"#;

pub fn build_device_context_prompt(context: &Context) -> String {
    let mut sections = vec![String::from(
        "设备动作约束：设备只提供结构状态；具体查看与操作要通过本轮暴露的 tools 完成。如果后台设备更合适，请调用 `focus_device`。",
    )];

    match context.devices.focused() {
        Some(DeviceId::Terminal) => sections.push(TERMINAL_PROMPT.to_string()),
        Some(DeviceId::Telegram) => sections.push(TELEGRAM_PROMPT.to_string()),
        None => sections.push(String::from(
            "当前没有任何前景设备。如果你需要操作设备，请先调用 `focus_device`。",
        )),
    }

    let attention_hints = context
        .devices
        .state_renders()
        .into_iter()
        .filter(|(_, state)| !state.is_focused)
        .filter_map(|(device_id, state)| background_attention_hint(device_id, &state))
        .collect::<Vec<_>>();

    if !attention_hints.is_empty() {
        sections.push(format!("后台设备提醒：\n{}", attention_hints.join("\n")));
    }

    sections.join("\n\n")
}

fn background_attention_hint(device_id: DeviceId, state: &DeviceStateRender) -> Option<String> {
    if !matches!(state.attention, AttentionLevel::Notice) {
        return None;
    }

    let summary = match device_id {
        DeviceId::Terminal => {
            let session_id = state
                .lines
                .iter()
                .find_map(|line| line.strip_prefix("active_session="))
                .unwrap_or("unknown");
            if numeric_field(&state.lines, "sessions_with_unread_output") > 0 {
                format!("后台终端会话 {session_id} 有未读输出。")
            } else {
                format!("后台终端会话 {session_id} 需要注意。")
            }
        }
        DeviceId::Telegram => {
            let pending_resolution = numeric_field(&state.lines, "pending_resolution");
            let pending_reply = numeric_field(&state.lines, "pending_reply");
            let unread_messages = numeric_field(&state.lines, "unread_messages");
            if pending_resolution > 0 {
                format!("Telegram 后台有 {pending_resolution} 个会话待判断。")
            } else if pending_reply > 0 {
                format!("Telegram 后台有 {pending_reply} 个会话待回复。")
            } else if unread_messages > 0 {
                format!("Telegram 后台有 {unread_messages} 条未读消息。")
            } else {
                "Telegram 后台有需要注意的状态变化。".to_string()
            }
        }
    };

    Some(match device_id {
        DeviceId::Terminal => format!(
            "- {} 如果你决定处理终端，请先调用 `focus_device` 将 `Terminal` 切到前景。",
            summary
        ),
        DeviceId::Telegram => format!(
            "- {} 如果你决定处理它，请先调用 `focus_device` 将 `Telegram` 切到前景；聚焦后再使用 Telegram 相关 tools。",
            summary
        ),
    })
}

fn numeric_field(lines: &[String], key: &str) -> usize {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}
