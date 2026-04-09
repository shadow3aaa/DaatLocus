use crate::{
    app::{AppHowToUse, AppId, AppStateRender, AppUsage},
    context::Context,
    skill::SkillSummary,
};

use super::prompt_text::{PromptTextBuilder, render_bullet_list};

pub const EVENT_UNIT_WHAT: &str = r#"外部输入主要通过事件进入当前 turn。对 event-driven turn 而言，你输出的普通文本不会自动发送给外部用户。
`<world_snapshot> ... </world_snapshot>` 片段不是用户在和你对话，而是当前世界状态的上下文注入。被当前 turn 正式领取的结构化事件或应用 notice，也可能以前序 `user` 消息的形式进入线程上下文；它们同样是待处理的世界输入，不是普通闲聊。"#;

pub const EVENT_UNIT_HOW: &str = r#"只有当你显式调用工具时，世界才会真正改变；对需要回复用户的常规成功收尾，应调用 `finish_and_send` 并提供 `reply_message`。
如果还需要继续推进，就不要调用 `finish_and_send`；应继续调用工具。
当你明显完成了某个中间步骤时，应直接输出文本来解释并记录当前进度；但这类中间记录不是最终提交，不能使用 `finish_and_send` 发送。
凡是动作参数中的 `event_id`，都应优先填写快照中显示的 UUID；不要把中文描述、标题或摘要直接塞进这些字段。
如果当前仍然存在可推进的目标、事件或应用信号，那么仅输出文本回复不会改变世界，不构成有效推进；应直接调用工具。
对于 event-driven turn：
- 只有在准备好最终回复后，才调用 `finish_and_send` 终结当前事件。
- 如果仍需继续推进，就继续调用工具。
- 不要把文本回复本身当作发送动作；真正的回复提交必须通过工具完成。
你必须仔细阅读当前快照，分析所处情况，先做事，再给结论。"#;

pub const APPS_UNIT_WHAT: &str = "App 是一类功能的封装单元。每个 App 都提供独一无二的功能封装。";

pub const APPS_UNIT_WHEN: &str = "当你判定某个任务必须依赖某个 app 时，或者使用这个 app 可以更好地解决任务时，应当切换到这个 app。如果 app 主动向你发出信号，也应当结合当前任务与该信号的重要程度，考虑切换到这个 app 处理。";

pub const APPS_UNIT_HOW: &str = "使用 `focus_app` 切换到目标 app。";

pub const WORKSPACE_UNIT_WHEN: &str = "当你需要进行任何属于你自己的文件操作时，都应默认在这个 workspace 目录下进行。";

pub const WORKSPACE_UNIT_WHY: &str = "一个固定的 workspace 可以让你拥有一片自己的固定空间。让你更好地完成需要操作文件的任务。";

pub const WORKSPACE_UNIT_HOW: &str = "使用相对路径时，不要把 workspace 目录名再写进路径里。快照已经告诉你 workspace 的绝对路径；相对路径默认就是相对于该目录。";

pub const SKILLS_UNIT_WHAT: &str = "每个 skill 都是一份针对特定类任务的执行规范说明。";

pub const SKILLS_UNIT_WHEN: &str = "任务开始执行时或执行过程中，只要快照里出现与任务相关或有帮助的 skill，在继续执行前就必须先调用 `read_skill` 读取它；不要凭猜测直接开始实现。";

pub const SKILLS_UNIT_HOW: &str = "使用 `read_skill(id)` 读取 skill 的完整说明。global skills 会始终出现在快照里；focused app 的 skills 只会在该 app 位于前景时出现。只要某个可见 skill 与当前任务相关，第一步就应是读取它，而不是先写代码或先调用别的实现工具。";

pub const MEMORIES_UNIT_WHAT: &str = "自动召回记忆（对应 `<recall_memories>` 标记）是系统基于当前任务上下文提供的相关长期记忆摘录；`deep_recall` 是显式触发的更深层回忆。";

pub const MEMORIES_UNIT_WHEN: &str = "当你需要上下文中不明确的信息时，应优先尝试 `deep_recall`，先确认这些信息是否已经存在于记忆中；只有在深度回忆仍不足时，才再尝试其他方式。";

pub const MEMORIES_UNIT_HOW: &str = "使用 `deep_recall` 触发更深层回忆。构造 query 时，应使用自然语言问题，并尽量写清你正在寻找的对象、事实、时间范围、任务背景或相关线索，避免只写空泛关键词；query 越具体，越容易召回真正相关的记忆。";

pub const PLAN_UNIT_WHAT: &str =
    "plan 是任务的最新分步计划。它用于记录完成当前任务所需的步骤顺序，以及每一步的当前进展。";

pub const PLAN_UNIT_WHEN: &str = "当任务是非平凡、多步骤、需要持续跟踪推进时，应维护 plan，使当前进展、下一步要做什么，以及整体剩余工作始终清晰。";

pub const PLAN_UNIT_HOW: &str = "使用 `update_plan` 持续维护 plan。每次调用时，都应提交当前完整的 plan，而不是只提交某一个步骤的增量修改。plan 中的步骤应是简短的一句话，尽量控制在 5 到 7 个词，且必须具体、可执行、可验证。只要还有未完成步骤，plan 中就应恰好有一个步骤是 `in_progress`；已完成步骤标记为 `completed`，后续步骤标记为 `pending`。当全部完成时，应将 plan 清空，而不是保留一组已完成步骤。";

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
    format!("你的 workspace 绝对路径是 `{}`。", context.execution_cwd.display())
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

pub fn build_runtime_global_skills_prompt(context: &Context) -> Option<String> {
    let skills = context.global_skills.summaries();
    if skills.is_empty() {
        None
    } else {
        Some(render_skill_summaries(skills))
    }
}

pub fn build_runtime_focused_app_skills_prompt(context: &Context) -> Option<String> {
    let skills = context.apps.focused_skills();
    if skills.is_empty() {
        None
    } else {
        Some(render_skill_summaries(skills))
    }
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

fn render_skill_summaries(skills: Vec<SkillSummary>) -> String {
    let mut lines = Vec::new();
    for skill in skills {
        lines.push(format!("- {}: {}", skill.id, skill.name));
        for when in skill.when_to_use {
            lines.push(format!("  - {}", render_inline_summary(&when)));
        }
    }
    lines.join("\n")
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

fn render_inline_summary(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let compact = text.replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
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
