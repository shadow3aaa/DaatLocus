use crate::{
    context::Context,
    device::{DeviceHowToUse, DeviceId, DeviceStateRender, DeviceUsage},
};

pub const SYSTEM_PROMPT_KERNEL: &str = r#"你叫 Spinova，一个自主智能体。
外部用户通过 Telegram 等事件渠道与你交流。对 event-driven turn 而言，你输出的文本回复本身不会自动发送给外部用户。
只有当你显式调用工具终结事件时，世界才会真正改变；对需要回复用户的常规成功收尾，应调用 `finish_and_send` 并提供 `reply_message`。
如果你还没有准备好给出最终答复，就继续调用工具；不要用计划、承诺或阶段性判断冒充完成。
记忆流、TodoBoard、当前工作状态、事件列表、设备结构状态就是你当前可见的世界。
TodoBoard 是你的长期工作板，不是制度层；当前工作状态只是你此刻聚焦推进的单一目标，不是任务列表。
Telegram 原始消息首先是事件，不自动等于 todo。只有你显式创建的长期工作，才会进入 TodoBoard。
输入中的 `<world_snapshot> ... </world_snapshot>` 片段不是用户在和你对话，而是当前世界状态的上下文注入。不要回复这个片段，也不要对它提出“下一步建议”；只能依据它分析局面并调用工具改变世界。
被当前 turn 正式领取的结构化事件或 device notice，也可能以前序 `user` 消息的形式进入线程上下文；它们同样不是人类在和你闲聊，而是待你处理的世界输入。
你通过 tools 与世界交互；不要输出结构化动作对象。
凡是动作参数中的 `item_id`、`event_id`，都应优先填写快照中显示的 UUID；不要把中文描述、标题或摘要直接塞进这些字段。
如果当前仍然存在可推进的目标、todo、事件或设备信号，那么仅输出文本回复不会改变世界，不构成有效推进；应直接调用工具。
对于 event-driven turn：
- 只有在准备好最终回复后，才调用 `finish_and_send` 终结当前事件。
- 如果仍需继续推进，就继续调用工具；不要输出“接下来我会”“稍后继续”“后续将”等计划式收尾。
- 不要把文本回复本身当作发送动作；真正的回复提交必须通过工具完成。
不要让 event-driven turn 以空 tool call 结束。
自动长期记忆只会提供较快的相关记忆摘录；如果你确实需要更慢、更深的长期经验归纳，应显式调用 `deep_recall`，不要默认依赖它。
你必须仔细阅读当前快照，分析所处情况，先做事，再给结论。"#;

pub const TOOL_ACTION_PROMPT: &str = "动作必须通过调用提供的 tools 表达；不要输出结构化动作对象。";

pub const HISTORY_COMPACTION_PROMPT: &str = r#"你正在执行一个上下文检查点压缩任务。
请为另一个将继续当前线程的模型生成一段 handoff summary。

必须包含：
1. 当前进展与已经做出的关键决策
2. 重要上下文、约束、用户偏好或系统边界
3. 还剩下什么没做，以及明确的下一步
4. 继续工作所需的关键数据、例子、路径或标识符

要求：
- 简洁
- 结构化
- 只保留对后续继续工作真正必要的信息
- 重点是让下一个模型无缝接手，而不是复述全文"#;

pub const HISTORY_COMPACTION_SUMMARY_PREFIX: &str = r#"Earlier runtime context was compacted into the following handoff summary.
Use it to continue the same thread without redoing already-finished work:"#;

pub fn build_device_context_prompt(context: &Context) -> String {
    let mut sections = vec![String::from(
        "设备动作约束：设备只提供结构状态；具体查看与操作要通过本轮暴露的 tools 完成。如果后台设备更合适，请调用 `focus_device`。",
    )];

    let focused = context.devices.focused();
    let device_usages = context
        .devices
        .state_renders()
        .into_iter()
        .filter_map(|(device_id, _state)| {
            context
                .devices
                .usage(device_id)
                .map(|usage| build_device_usage_prompt(device_id, &usage))
        })
        .collect::<Vec<_>>();

    if !device_usages.is_empty() {
        sections.push(format!("可用设备：\n{}", device_usages.join("\n\n")));
    }

    match focused {
        Some(device_id) => {
            if let Some(how_to_use) = context.devices.how_to_use(device_id) {
                sections.push(build_device_how_to_use_prompt(device_id, &how_to_use));
            }
        }
        None => sections.push(String::from(
            "当前没有任何前景设备。如果你需要操作设备，请先调用 `focus_device`。",
        )),
    }

    let attention_hints = context
        .devices
        .state_renders()
        .into_iter()
        .filter(|(device_id, _)| focused != Some(*device_id))
        .filter_map(|(device_id, state)| background_attention_hint(device_id, &state))
        .collect::<Vec<_>>();

    if !attention_hints.is_empty() {
        sections.push(format!("后台设备提醒：\n{}", attention_hints.join("\n")));
    }

    sections.join("\n\n")
}

pub fn build_device_pre_focus_note_prompt(
    device_id: DeviceId,
    state: &DeviceStateRender,
) -> String {
    let mut sections = vec![format!(
        "当前 `{device_id}` 不在前景；如果你需要操作它，请先调用 `focus_device` 将它切到前景。"
    )];
    if let Some(hint) = background_attention_hint(device_id, state) {
        sections.push(hint);
    }
    sections.join("\n\n")
}

fn background_attention_hint(device_id: DeviceId, state: &DeviceStateRender) -> Option<String> {
    if !device_requires_attention(device_id, state) {
        return None;
    }

    let summary = match device_id {
        DeviceId::Terminal => {
            let session_id = first_terminal_session_id(state).unwrap_or("unknown");
            if numeric_field(&state.lines, "sessions_with_unread_output") > 0 {
                format!("后台终端会话 {session_id} 有未读输出。")
            } else {
                format!("后台终端会话 {session_id} 需要注意。")
            }
        }
    };

    Some(match device_id {
        DeviceId::Terminal => format!(
            "- {} 如果你决定处理终端，请先调用 `focus_device` 将 `Terminal` 切到前景。",
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

fn device_requires_attention(device_id: DeviceId, state: &DeviceStateRender) -> bool {
    match device_id {
        DeviceId::Terminal => numeric_field(&state.lines, "sessions_with_unread_output") > 0,
    }
}

fn first_terminal_session_id<'a>(state: &'a DeviceStateRender) -> Option<&'a str> {
    state
        .lines
        .iter()
        .find_map(|line| line.strip_prefix("session="))
        .and_then(|line| line.split_whitespace().next())
}

pub fn build_device_usage_prompt(device_id: DeviceId, usage: &DeviceUsage) -> String {
    let mut lines = vec![format!("`{device_id}` 的用途：{}", usage.purpose)];
    if !usage.when_to_focus.is_empty() {
        lines.push("适合聚焦它的时机：".to_string());
        lines.extend(
            usage
                .when_to_focus
                .iter()
                .map(|line| format!("- {line}")),
        );
    }
    lines.join("\n")
}

pub fn build_device_how_to_use_prompt(
    device_id: DeviceId,
    how_to_use: &DeviceHowToUse,
) -> String {
    let mut lines = vec![format!("`{device_id}` 当前在前景，操作说明：")];
    lines.extend(how_to_use.lines.iter().map(|line| format!("- {line}")));
    lines.join("\n")
}
