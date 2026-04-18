use crate::{
    app::{AppHowToUse, AppId, AppStateRender, AppUsage},
    context::Context,
};

use super::prompt_text::{PromptTextBuilder, render_bullet_list};

pub const EVENT_UNIT_WHAT: &str = r#"外部输入主要通过事件进入当前 turn。对 event-driven turn 而言，你输出的普通文本不会自动发送给外部用户。
`<world_snapshot> ... </world_snapshot>` 片段不是用户在和你对话，而是当前世界状态的上下文注入。被当前 turn 正式领取的结构化事件或应用 notice，也可能以前序 `user` 消息的形式进入线程上下文；它们同样是待处理的世界输入，不是普通闲聊。"#;

pub const EVENT_UNIT_HOW: &str = r#"只有当你显式调用工具时，世界才会真正改变；凡是需要向用户提交最终答复的事件收尾，无论 `resolved` 还是 `failed`，都应调用 `finish_and_send` 并提供 `reply_message`。
如果还需要继续推进，就不要调用 `finish_and_send`；应继续调用工具。
当你明显完成了某个中间步骤时，应直接输出文本来解释并记录当前进度；但这类中间记录不是最终提交，不能使用 `finish_and_send` 发送。
凡是动作参数中的 `event_id`，都应优先填写快照中显示的 UUID；不要把中文描述、标题或摘要直接塞进这些字段。
如果当前仍然存在可推进的目标、事件或应用信号，那么仅输出文本回复不会改变世界，不构成有效推进；应直接调用工具。
对于 event-driven turn：
- 只有在准备好最终回复后，才调用 `finish_and_send` 终结当前事件。
- `dismissed` 只用于明确不需要回复用户的静默结束。
- 如果仍需继续推进，就继续调用工具。
- 不要把文本回复本身当作发送动作；真正的回复提交必须通过工具完成。
你必须仔细阅读当前快照，分析所处情况，先做事，再给结论。"#;

pub const APPS_UNIT_WHAT: &str = "App 是一类功能的封装单元。每个 App 都提供独一无二的功能封装。";

pub const APPS_UNIT_WHEN: &str = "当你判定某个任务必须依赖某个 app 时，或者使用这个 app 可以更好地解决任务时，应当切换到这个 app。如果 app 主动向你发出信号，也应当结合当前任务与该信号的重要程度，考虑切换到这个 app 处理。";

pub const APPS_UNIT_HOW: &str = "使用 `focus_app` 切换到目标 app。";

pub const WORKSPACE_UNIT_WHEN: &str =
    "当你需要进行任何属于你自己的文件操作时，都应默认在这个 workspace 目录下进行。";

pub const WORKSPACE_UNIT_WHY: &str =
    "一个固定的 workspace 可以让你拥有一片自己的固定空间。让你更好地完成需要操作文件的任务。";

pub const WORKSPACE_UNIT_HOW: &str = "使用相对路径时，不要把 workspace 目录名再写进路径里。快照已经告诉你 workspace 的绝对路径；相对路径默认就是相对于该目录。";

pub const MEMORIES_UNIT_WHAT: &str = "自动召回记忆（对应 `<recall_memories>` 标记）会优先提供长期 consolidated knowledge，例如 mental models 与 observations，并在需要时补充 raw memories 与 citations；`deep_recall` 是显式触发的更深层回忆。";

pub const MEMORIES_UNIT_WHEN: &str = "当自动召回里的 consolidated knowledge 仍不足以支撑判断，或者你需要更强的证据链、来源说明、历史偏好归纳时，应优先尝试 `deep_recall`；只有在深度回忆仍不足时，才再尝试其他方式。";

pub const MEMORIES_UNIT_HOW: &str = "阅读 `<recall_memories>` 时，应先区分 mental models / observations / raw memories / citations 的角色：优先用 consolidated knowledge 做高层判断，再用 raw evidence 校验细节。使用 `deep_recall` 时，query 应写成自然语言问题，并尽量说明对象、事实、时间范围、任务背景与线索；query 越具体，越容易召回真正相关的记忆。";

pub const PLAN_UNIT_WHAT: &str =
    "plan 是任务的最新分步计划。它用于记录完成当前任务所需的步骤顺序，以及每一步的当前进展。";

pub const PLAN_UNIT_WHEN: &str = "当任务是非平凡、多步骤、需要持续跟踪推进时，应维护 plan，使当前进展、下一步要做什么，以及整体剩余工作始终清晰。";

pub const PLAN_UNIT_HOW: &str = "使用 `update_plan` 持续维护 plan。每次调用时，都应提交当前完整的 plan，而不是只提交某一个步骤的增量修改。plan 中的步骤应是简短的一句话，尽量控制在 5 到 7 个词，且必须具体、可执行、可验证。只要还有未完成步骤，plan 中就应恰好有一个步骤是 `in_progress`；已完成步骤标记为 `completed`，后续步骤标记为 `pending`。当全部完成时，应将 plan 清空，而不是保留一组已完成步骤。";

pub const WORKFLOW_UNIT_WHAT: &str = "workflow 是可迭代的任务执行规范。每个 workflow 都描述何时适用、前置条件、可复用步骤、完成标准与稳定恢复路径。";

pub const WORKFLOW_UNIT_WHEN: &str = "当任务是非平凡、多步骤、且适合复用稳定流程时，应先查看快照里的候选 workflow；如果没有合适项，再调用 `create_workflow` 创建初稿，并用 `activate_workflow` 绑定到当前任务。";

pub const WORKFLOW_UNIT_HOW: &str = "workflow 绑定只是当前任务的 runtime 状态，不会自动改写 workflow 规范本体。白天无需手工记录 workflow outcome；runtime 会在 work 完成边界直接写入 `WorkflowRunRecord`，供 sleep 在夜间进行 patch 或 merge。";

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

pub fn build_workspace_unit_what(context: &Context) -> String {
    format!(
        "你的 workspace 绝对路径是 `{}`。",
        context.execution_cwd.display()
    )
}

pub fn build_runtime_app_usages(context: &Context) -> Vec<(AppId, AppUsage)> {
    let focused = context.apps.focused();
    context
        .apps
        .state_renders()
        .into_iter()
        .filter(|(app_id, _state)| focused.as_ref() != Some(app_id))
        .filter_map(|(app_id, _state)| context.apps.usage(&app_id).map(|usage| (app_id, usage)))
        .collect()
}

pub fn build_runtime_focused_app_how_to_use_prompt(context: &Context) -> Option<String> {
    let app_id = context.apps.focused()?;
    let how_to_use = context.apps.how_to_use(&app_id)?;
    Some(build_app_how_to_use_prompt(app_id, &how_to_use))
}

pub fn build_runtime_background_hint_items(context: &Context) -> Vec<String> {
    let focused = context.apps.focused();
    context
        .apps
        .state_renders()
        .into_iter()
        .filter(|(app_id, _)| focused.as_ref() != Some(app_id))
        .filter_map(|(app_id, state)| background_app_attention_hint(app_id, &state))
        .collect()
}

pub fn build_app_pre_focus_note_prompt(app_id: AppId, state: &AppStateRender) -> String {
    let mut builder = PromptTextBuilder::new();
    builder.push_paragraph(format!(
        "当前 `{app_id}` 不在前景；如果你需要操作它，请先调用 `focus_app` 将它切到前景。"
    ));
    if let Some(hint) = background_app_attention_hint(app_id, state) {
        builder.push_paragraph(hint);
    }
    builder.build()
}

fn background_app_attention_hint(app_id: AppId, state: &AppStateRender) -> Option<String> {
    if !app_requires_attention(app_id.clone(), state) {
        return None;
    }

    if app_id.is_terminal() {
        let summary = if !list_field(&state.lines, "unread_sessions").is_empty() {
            "后台终端有未读输出。".to_string()
        } else {
            "后台终端需要注意。".to_string()
        };
        return Some(format!(
            "{} 如果你决定处理终端，请先调用 `focus_app` 将 `Terminal` 切到前景。",
            summary
        ));
    }

    None
}

fn list_field(lines: &[String], key: &str) -> Vec<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(|value| {
            if value == "none" {
                Vec::new()
            } else {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToString::to_string)
                    .collect()
            }
        })
        .unwrap_or_default()
}

fn app_requires_attention(app_id: AppId, state: &AppStateRender) -> bool {
    if app_id.is_terminal() {
        !list_field(&state.lines, "unread_sessions").is_empty()
    } else {
        false
    }
}

pub fn build_app_usage_prompt(_app_id: AppId, usage: &AppUsage) -> String {
    if let Some(body) = usage.body_markdown.as_deref()
        && !body.trim().is_empty()
    {
        return body.trim().to_string();
    }
    let mut builder = PromptTextBuilder::new();
    builder.push_labeled_section("what", usage.description.clone());
    if !usage.when_to_focus.is_empty() {
        builder.push_labeled_section("when", render_bullet_list(usage.when_to_focus.clone()));
    }
    builder.build()
}

pub fn build_app_how_to_use_prompt(app_id: AppId, how_to_use: &AppHowToUse) -> String {
    if let Some(body) = how_to_use.body_markdown.as_deref()
        && !body.trim().is_empty()
    {
        return body.trim().to_string();
    }
    let mut builder = PromptTextBuilder::new();
    let _ = app_id;
    builder.push_paragraph(render_bullet_list(how_to_use.lines.clone()));
    builder.build()
}
