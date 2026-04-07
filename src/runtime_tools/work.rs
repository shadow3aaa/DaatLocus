use miette::Result;
use serde_json::json;
use uuid::Uuid;

use crate::{
    apply_patch::{PatchOperationKind, parse_apply_patch, summarize_patch_ops},
    context::Context,
    core::{
        DeepRecallArgs, EventResolveArgs, FocusAppArgs, PutAwayAppArgs, ReadSkillArgs,
        TodoCompleteArgs, TodoCreateArgs, TodoDropArgs, TodoUpdateArgs,
    },
    events::{EventDisposition, EventPayload, EventStatus},
    hindsight::HindsightReflectOptions,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    todo_board::{TodoOrigin, TodoStatus},
    tool_ui::{ToolCallUiEvent, ToolUiEvent},
};

use super::{
    RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    summarize_inline_text,
};

fn extract_apply_patch_text(call: &AgentToolCall) -> Result<String> {
    if let Some(input) = call
        .arguments
        .as_object()
        .and_then(|value| value.get("input"))
        && let Some(text) = input.as_str()
    {
        return Ok(text.to_string());
    }
    if let Some(patch) = call
        .arguments
        .as_object()
        .and_then(|value| value.get("patch"))
        && let Some(text) = patch.as_str()
    {
        return Ok(text.to_string());
    }
    if let Some(text) = call.arguments.as_str() {
        return Ok(text.to_string());
    }
    Err(miette::miette!(
        "invalid arguments for tool `apply_patch`: expected a patch string in `input`"
    ))
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![
        Box::new(StaticRuntimeTool::new::<FocusAppArgs>(
            "focus_app",
            "将指定应用切到前景。",
            None,
            summarize_focus_app_tool,
            render_focus_app_call_ui,
            execute_focus_app_tool,
        )),
        Box::new(StaticRuntimeTool::new::<PutAwayAppArgs>(
            "put_away_app",
            "把当前前景应用放回后台。",
            None,
            summarize_put_away_app_tool,
            render_put_away_app_call_ui,
            execute_put_away_app_tool,
        )),
        Box::new(StaticRuntimeTool::new::<EventResolveArgs>(
            "finish_and_send",
            "显式终结一个事件并发送最终回复。对需要回复用户的常规成功收尾，应调用此工具并提供 `reply_message`；dismissed 或 failed 也通过此工具提交。",
            None,
            summarize_event_resolve_tool,
            render_event_resolve_call_ui,
            execute_event_resolve_tool,
        )),
        Box::new(StaticRuntimeTool::new::<TodoCreateArgs>(
            "todo_create",
            "创建一个新的 todo。",
            None,
            summarize_todo_create_tool,
            render_todo_create_call_ui,
            execute_todo_create_tool,
        )),
        Box::new(StaticRuntimeTool::new::<TodoUpdateArgs>(
            "todo_update",
            "更新一个 todo 的标题、完成标准、备注或状态。",
            None,
            summarize_todo_update_tool,
            render_todo_update_call_ui,
            execute_todo_update_tool,
        )),
        Box::new(
            StaticRuntimeTool::new_with_availability::<TodoCompleteArgs>(
                "todo_complete",
                "将 todo 标记为完成，仅改变内部 memo 状态。",
                None,
                todo_complete_is_available,
                summarize_todo_complete_tool,
                render_todo_complete_call_ui,
                execute_todo_complete_tool,
            ),
        ),
        Box::new(StaticRuntimeTool::new::<TodoDropArgs>(
            "todo_drop",
            "将 todo 标记为放弃。",
            None,
            summarize_todo_drop_tool,
            render_todo_drop_call_ui,
            execute_todo_drop_tool,
        )),
        Box::new(StaticRuntimeTool::new::<DeepRecallArgs>(
            "deep_recall",
            "对长期记忆执行一次较慢但更深的 reflect 查询，用于高层经验归纳或线程恢复。",
            None,
            summarize_deep_recall_tool,
            render_deep_recall_call_ui,
            execute_deep_recall_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ReadSkillArgs>(
            "read_skill",
            "读取当前前景 app 上指定 skill 的完整说明正文。调用前应先 `focus_app` 到匹配的 app；skill 列表会出现在应用快照里。",
            None,
            summarize_read_skill_tool,
            render_read_skill_call_ui,
            execute_read_skill_tool,
        )),
    ]
}

fn todo_complete_is_available(context: &Context) -> bool {
    context.work_state.item_id.is_some()
}

fn event_disposition_kind(disposition: EventDisposition) -> &'static str {
    match disposition {
        EventDisposition::Resolved => "resolved",
        EventDisposition::Dismissed => "dismissed",
        EventDisposition::Failed => "failed",
    }
}

fn status_for_event_disposition(disposition: EventDisposition) -> EventStatus {
    match disposition {
        EventDisposition::Resolved => EventStatus::Resolved,
        EventDisposition::Dismissed => EventStatus::Dismissed,
        EventDisposition::Failed => EventStatus::Failed,
    }
}

fn summarize_focus_app_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: FocusAppArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "focus_app".to_string(),
        summary: format!("app={}", args.app),
    })
}

fn render_focus_app_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: FocusAppArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        format!("focus_app {}", args.app),
        Vec::new(),
    ))
}

fn execute_focus_app_tool<'a>(context: &'a mut Context, call: &'a AgentToolCall) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: FocusAppArgs = parse_tool_args(call)?;
        context.apps.focus(args.app).await?;
        Ok(ToolExecutionResult::new(
            format!("focused app {}", args.app),
            json!({ "app": args.app.to_string() }),
            ToolUiEvent::app(
                format!("focused app {}", args.app),
                vec![args.app.to_string()],
            ),
        )
        .with_turn_boundary(format!(
            "focused app changed to {}; re-render world state in a new turn",
            args.app
        )))
    })
}

fn summarize_put_away_app_tool(_call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    Ok(EpisodeActionRecord {
        kind: "put_away_app".to_string(),
        summary: "put away current focused app".to_string(),
    })
}

fn render_put_away_app_call_ui(_call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    Ok(ToolCallUiEvent::app("put_away_app", Vec::new()))
}

fn execute_put_away_app_tool<'a>(
    context: &'a mut Context,
    _call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        context.apps.put_away().await?;
        Ok(ToolExecutionResult::new(
            "put away focused app",
            json!({}),
            ToolUiEvent::app("put away focused app", Vec::new()),
        )
        .with_turn_boundary("focused app was put away; re-render world state in a new turn"))
    })
}

fn summarize_event_resolve_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: EventResolveArgs = parse_tool_args(call)?;
    let reply_summary = args
        .reply_message
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(summarize_inline_text);
    Ok(EpisodeActionRecord {
        kind: "finish_and_send".to_string(),
        summary: match reply_summary {
            Some(reply) => format!(
                "disposition={} reply={}",
                event_disposition_kind(args.disposition),
                reply
            ),
            None => format!(
                "event_id={} disposition={}",
                args.event_id,
                event_disposition_kind(args.disposition)
            ),
        },
    })
}

fn render_event_resolve_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: EventResolveArgs = parse_tool_args(call)?;
    let mut lines = vec![format!(
        "disposition={}",
        event_disposition_kind(args.disposition)
    )];
    if let Some(reply_message) = args.reply_message.as_deref()
        && !reply_message.trim().is_empty()
    {
        lines.push(format!("reply={}", summarize_inline_text(reply_message)));
    }
    Ok(ToolCallUiEvent::work(
        format!("finish_and_send {}", args.event_id),
        lines,
    ))
}

fn execute_event_resolve_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: EventResolveArgs = parse_tool_args(call)?;
        let reply_message = trim_optional_field(args.reply_message);
        let event = context.events.view(&args.event_id)?;
        let resolved_reply_message = reply_message.clone();
        let summary = match args.disposition {
            EventDisposition::Resolved => {
                let reply_message = resolved_reply_message.ok_or_else(|| {
                    miette::miette!(
                        "resolved event {} requires a non-empty reply_message",
                        args.event_id
                    )
                })?;
                execute_event_resolve_with_reply(
                    context,
                    &args.event_id,
                    &event,
                    reply_message.clone(),
                )?;
                format!("resolved event {} via channel delivery", args.event_id)
            }
            EventDisposition::Dismissed | EventDisposition::Failed => {
                context.events.set_status(
                    &args.event_id,
                    status_for_event_disposition(args.disposition),
                    args.note.clone(),
                )?;
                format!(
                    "resolved event {} as {}",
                    args.event_id,
                    event_disposition_kind(args.disposition)
                )
            }
        };
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "event_id": args.event_id,
                "disposition": event_disposition_kind(args.disposition),
                "reply_message": reply_message,
                "note": args.note,
            }),
            ToolUiEvent::work(summary, Vec::new()),
        ))
    })
}

fn summarize_todo_create_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TodoCreateArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "todo_create".to_string(),
        summary: summarize_inline_text(&args.title),
    })
}

fn render_todo_create_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TodoCreateArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        "todo_create",
        vec![
            summarize_inline_text(&args.title),
            summarize_inline_text(&args.done_criteria),
        ],
    ))
}

fn execute_todo_create_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TodoCreateArgs = parse_tool_args(call)?;
        let title = require_field(args.title, "title")?;
        let done_criteria = require_field(args.done_criteria, "done_criteria")?;
        let notes = trim_optional_field(args.notes);
        let todo_id = context.todo_board.add(
            title.clone(),
            TodoOrigin::SelfInitiated,
            done_criteria.clone(),
            notes.clone(),
        );
        Ok(ToolExecutionResult::new(
            format!("created todo {}", todo_id),
            json!({
                "item_id": todo_id.to_string(),
                "title": title,
                "done_criteria": done_criteria,
                "notes": notes,
            }),
            ToolUiEvent::work(
                format!("created todo {}", todo_id),
                vec![title, done_criteria],
            ),
        ))
    })
}

fn summarize_todo_update_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TodoUpdateArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "todo_update".to_string(),
        summary: format!("item_id={}", args.item_id),
    })
}

fn render_todo_update_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TodoUpdateArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        format!("todo_update {}", args.item_id),
        Vec::new(),
    ))
}

fn execute_todo_update_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TodoUpdateArgs = parse_tool_args(call)?;
        let item_id = resolve_item_reference(context, &args.item_id)?;
        let title = args.title.and_then(trim_required_field);
        let done_criteria = args.done_criteria.and_then(trim_required_field);
        let notes = if args.clear_notes.unwrap_or(false) {
            Some(None)
        } else {
            args.notes.map(|notes| trim_required_field(notes))
        };
        let changed = context
            .todo_board
            .update(item_id, title, done_criteria, notes, args.status);
        if !changed {
            return Err(miette::miette!("todo {item_id} was not changed").into());
        }
        Ok(ToolExecutionResult::new(
            format!("updated todo {}", item_id),
            json!({ "item_id": item_id.to_string() }),
            ToolUiEvent::work(
                format!("updated todo {}", item_id),
                vec![item_id.to_string()],
            ),
        ))
    })
}

fn summarize_todo_complete_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TodoCompleteArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "todo_complete".to_string(),
        summary: format!(
            "item_id={} summary={}",
            args.item_id,
            summarize_inline_text(&args.summary)
        ),
    })
}

fn render_todo_complete_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TodoCompleteArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        format!("todo_complete {}", args.item_id),
        vec![summarize_inline_text(&args.summary)],
    ))
}

fn execute_todo_complete_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TodoCompleteArgs = parse_tool_args(call)?;
        let item_id = execute_todo_complete(context, &args.item_id, args.summary.clone()).await?;
        Ok(ToolExecutionResult::new(
            format!("completed todo {}", item_id),
            json!({
                "item_id": item_id.to_string(),
                "summary": args.summary,
            }),
            ToolUiEvent::work(format!("completed todo {}", item_id), vec![args.summary]),
        ))
    })
}

fn summarize_todo_drop_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: TodoDropArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "todo_drop".to_string(),
        summary: format!("item_id={}", args.item_id),
    })
}

fn render_todo_drop_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: TodoDropArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        format!("todo_drop {}", args.item_id),
        Vec::new(),
    ))
}

fn execute_todo_drop_tool<'a>(context: &'a mut Context, call: &'a AgentToolCall) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: TodoDropArgs = parse_tool_args(call)?;
        let item_id = resolve_item_reference(context, &args.item_id)?;
        let note = trim_optional_field(args.note);
        let note_for_update = note.clone();
        let changed = context.todo_board.update(
            item_id,
            None,
            None,
            note_for_update.map(Some),
            Some(TodoStatus::Dropped),
        );
        if !changed {
            return Err(miette::miette!("todo {item_id} was not changed").into());
        }
        context.work_state.clear_if_item(item_id);
        Ok(ToolExecutionResult::new(
            format!("dropped todo {}", item_id),
            json!({
                "item_id": item_id.to_string(),
                "note": note,
            }),
            ToolUiEvent::work(
                format!("dropped todo {}", item_id),
                vec![item_id.to_string()],
            ),
        ))
    })
}

fn summarize_deep_recall_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: DeepRecallArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "deep_recall".to_string(),
        summary: summarize_inline_text(&args.query),
    })
}

fn render_deep_recall_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: DeepRecallArgs = parse_tool_args(call)?;
    let mut lines = vec![summarize_inline_text(&args.query)];
    if let Some(budget) = args.budget.as_deref()
        && !budget.trim().is_empty()
    {
        lines.push(format!("budget={budget}"));
    }
    if let Some(max_tokens) = args.max_tokens {
        lines.push(format!("max_tokens={max_tokens}"));
    }
    Ok(ToolCallUiEvent::work("deep_recall", lines))
}

fn execute_deep_recall_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: DeepRecallArgs = parse_tool_args(call)?;
        let response = context
            .hindsight
            .reflect(
                &args.query,
                HindsightReflectOptions {
                    budget: args.budget.clone(),
                    max_tokens: args.max_tokens,
                    include_facts: false,
                    ..Default::default()
                },
            )
            .await?;
        let title = format!("deep recall: {}", summarize_inline_text(&args.query));
        let mut body_lines = Vec::new();
        body_lines.extend(
            response
                .text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .take(12)
                .map(ToString::to_string),
        );
        Ok(ToolExecutionResult::new(
            title.clone(),
            json!({
                "query": args.query,
                "budget": args.budget,
                "max_tokens": args.max_tokens,
                "text": response.text,
            }),
            ToolUiEvent::work(title, body_lines),
        ))
    })
}

fn summarize_read_skill_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ReadSkillArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "read_skill".to_string(),
        summary: format!("id={}", args.id),
    })
}

fn render_read_skill_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ReadSkillArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        "read_skill".to_string(),
        vec![format!("id={}", args.id)],
    ))
}

fn execute_read_skill_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ReadSkillArgs = parse_tool_args(call)?;
        let skill = context.apps.read_skill(&args.id)?;
        let content = format!(
            "skill_id={}\ntitle={}\nbody=\n{}",
            skill.id, skill.title, skill.body
        );
        Ok(ToolExecutionResult::new(
            format!("read skill {}", skill.id),
            json!({
                "id": skill.id,
                "title": skill.title,
                "body": skill.body,
            }),
            ToolUiEvent::work("read skill".to_string(), vec![format!("id={}", args.id)]),
        )
        .with_model_content(content))
    })
}

pub(super) fn summarize_apply_patch_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    Ok(EpisodeActionRecord {
        kind: "apply_patch".to_string(),
        summary: summarize_inline_text(&extract_apply_patch_text(call)?),
    })
}

pub(super) fn render_apply_patch_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let ops = parse_apply_patch(&extract_apply_patch_text(call)?)?;
    let summary = summarize_patch_ops(&ops);
    Ok(ToolCallUiEvent::patch(
        "apply_patch",
        format!(
            "{} file(s) changed (+{} -{})",
            summary.changed_files, summary.added_lines, summary.removed_lines
        ),
        summary
            .files
            .iter()
            .cloned()
            .map(|file| crate::tool_ui::PatchFileUiData {
                path: file.path,
                operation: match file.operation {
                    PatchOperationKind::Add => "add".to_string(),
                    PatchOperationKind::Delete => "delete".to_string(),
                    PatchOperationKind::Update => "update".to_string(),
                },
                added_lines: file.added_lines,
                removed_lines: file.removed_lines,
            })
            .collect(),
    ))
}

pub(super) fn execute_apply_patch_runtime_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        super::super::execute_apply_patch_tool(context, &extract_apply_patch_text(call)?).await
    })
}

fn resolve_string_reference<T: Clone>(
    kind: &str,
    reference: &str,
    refs: impl IntoIterator<Item = (String, T)>,
) -> miette::Result<T> {
    refs.into_iter()
        .find(|(key, _)| key == reference)
        .map(|(_, value)| value)
        .ok_or_else(|| miette::miette!("unknown {kind}: {reference}"))
}

fn resolve_item_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_string_reference(
        "todo",
        reference,
        context.todo_board.items().flat_map(|(id, item)| {
            [id.to_string(), item.title.clone()]
                .into_iter()
                .map(move |key| (key, id))
        }),
    )
}

fn trim_optional_field(value: Option<String>) -> Option<String> {
    value.and_then(trim_required_field)
}

fn trim_required_field(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn require_field(value: String, field_name: &str) -> miette::Result<String> {
    trim_required_field(value)
        .ok_or_else(|| miette::miette!("missing required field: {field_name}"))
}

fn execute_event_resolve_with_reply(
    context: &mut Context,
    event_id: &str,
    event: &crate::events::EventView,
    reply_message: String,
) -> miette::Result<()> {
    match &event.payload {
        EventPayload::TelegramIncoming(payload) => {
            context.events.prepare_telegram_delivery(event_id)?;
            context.telegram.enqueue_outgoing_message(
                payload.chat_id.clone(),
                reply_message,
                Some(event_id.to_string()),
                Some(EventStatus::Resolved),
            )?;
            Ok(())
        }
    }
}

async fn execute_todo_complete(
    context: &mut Context,
    item_id: &str,
    summary: String,
) -> miette::Result<Uuid> {
    let item_id = resolve_item_reference(context, item_id)?;
    let Some(_item) = context.todo_board.get(item_id).cloned() else {
        return Err(miette::miette!("unknown todo: {item_id}"));
    };
    let summary = require_field(summary, "summary")?;

    context.todo_board.update(
        item_id,
        None,
        None,
        Some(Some(summary)),
        Some(TodoStatus::Completed),
    );
    context.work_state.clear_if_item(item_id);
    Ok(item_id)
}
