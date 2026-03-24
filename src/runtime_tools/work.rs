use miette::Result;
use serde_json::json;
use uuid::Uuid;

use crate::{
    TelegramResolution,
    apply_patch::{PatchOperationKind, parse_apply_patch, summarize_patch_ops},
    context::Context,
    core::{
        ClearWorkObjectiveArgs, CommitToProjectArgs, DeepRecallArgs, FocusDeviceArgs,
        ObligationSatisfyArgs, ProjectCompleteArgs, PutAwayDeviceArgs, ReportObligationArgs,
        ResolveTelegramChatArgs, SetWorkObjectiveArgs,
    },
    device::DeviceId,
    hindsight::HindsightReflectOptions,
    obligations::{ObligationSource, ObligationStatus},
    projects::ProjectOrigin,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{TelegramUiAction, ToolCallUiEvent, ToolUiEvent},
};

use super::{
    RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    summarize_inline_text,
};

fn extract_apply_patch_text(call: &AgentToolCall) -> Result<String> {
    if let Some(input) = call.arguments.as_object().and_then(|value| value.get("input"))
        && let Some(text) = input.as_str()
    {
        return Ok(text.to_string());
    }
    if let Some(patch) = call.arguments.as_object().and_then(|value| value.get("patch"))
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
        Box::new(StaticRuntimeTool::new::<SetWorkObjectiveArgs>(
            "set_work_objective",
            "设置当前单一工作目标。",
            None,
            summarize_set_work_objective_tool,
            render_set_work_objective_call_ui,
            execute_set_work_objective_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ClearWorkObjectiveArgs>(
            "clear_work_objective",
            "清空当前工作目标。",
            None,
            summarize_clear_work_objective_tool,
            render_clear_work_objective_call_ui,
            execute_clear_work_objective_tool,
        )),
        Box::new(StaticRuntimeTool::new::<FocusDeviceArgs>(
            "focus_device",
            "将指定设备切到前景。",
            None,
            summarize_focus_device_tool,
            render_focus_device_call_ui,
            execute_focus_device_tool,
        )),
        Box::new(StaticRuntimeTool::new::<PutAwayDeviceArgs>(
            "put_away_device",
            "把当前前景设备放回后台。",
            None,
            summarize_put_away_device_tool,
            render_put_away_device_call_ui,
            execute_put_away_device_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ResolveTelegramChatArgs>(
            "resolve_telegram_chat",
            "对 Telegram 会话做语义判断并自动完成后续 bookkeeping。",
            None,
            summarize_resolve_telegram_chat_tool,
            render_resolve_telegram_chat_call_ui,
            execute_resolve_telegram_chat_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ObligationSatisfyArgs>(
            "obligation_satisfy",
            "将不需要外部回复的义务标记为完成。",
            None,
            summarize_obligation_satisfy_tool,
            render_obligation_satisfy_call_ui,
            execute_obligation_satisfy_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ReportObligationArgs>(
            "report_obligation",
            "向义务的回复目标发送结果，并在成功后将该义务标记为完成。",
            None,
            summarize_report_obligation_tool,
            render_report_obligation_call_ui,
            execute_report_obligation_tool,
        )),
        Box::new(StaticRuntimeTool::new::<DeepRecallArgs>(
            "deep_recall",
            "对长期记忆执行一次较慢但更深的 reflect 查询，用于高层经验归纳或线程恢复。",
            None,
            summarize_deep_recall_tool,
            render_deep_recall_call_ui,
            execute_deep_recall_tool,
        )),
        Box::new(StaticRuntimeTool::new::<CommitToProjectArgs>(
            "commit_to_project",
            "接受一项义务并将其升级为项目。",
            None,
            summarize_commit_to_project_tool,
            render_commit_to_project_call_ui,
            execute_commit_to_project_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ProjectCompleteArgs>(
            "project_complete",
            "将项目标记为完成，并记录结果摘要。",
            None,
            summarize_project_complete_tool,
            render_project_complete_call_ui,
            execute_project_complete_tool,
        )),
    ]
}

fn resolution_kind(resolution: &TelegramResolution) -> &'static str {
    match resolution {
        TelegramResolution::ReplyOnly { .. } => "reply_only",
        TelegramResolution::AcceptAsProject { .. } => "accept_as_project",
        TelegramResolution::AskClarification { .. } => "ask_clarification",
        TelegramResolution::Decline { .. } => "decline",
        TelegramResolution::NoReplyNeeded => "no_reply_needed",
    }
}

fn summarize_set_work_objective_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: SetWorkObjectiveArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "set_work_objective".to_string(),
        summary: format!(
            "description={} project_id={}",
            summarize_inline_text(&args.description),
            args.project_id.unwrap_or_else(|| "none".to_string())
        ),
    })
}

fn render_set_work_objective_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: SetWorkObjectiveArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        "set_work_objective",
        vec![
            summarize_inline_text(&args.description),
            format!(
                "project_id={}",
                args.project_id.unwrap_or_else(|| "none".to_string())
            ),
        ],
    ))
}

fn execute_set_work_objective_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: SetWorkObjectiveArgs = parse_tool_args(call)?;
        let project_id = args
            .project_id
            .as_deref()
            .map(|project_id| resolve_project_reference(context, project_id))
            .transpose()?;
        context
            .work_state
            .set_objective(args.description.clone(), project_id);
        Ok(ToolExecutionResult::new(
            format!(
                "set work objective: {}",
                summarize_inline_text(&args.description)
            ),
            json!({
                "project_id": project_id.map(|id| id.to_string()),
                "description": args.description,
            }),
            ToolUiEvent::work(
                format!(
                    "set work objective: {}",
                    summarize_inline_text(&args.description)
                ),
                vec![args.description],
            ),
        ))
    })
}

fn summarize_clear_work_objective_tool(_call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    Ok(EpisodeActionRecord {
        kind: "clear_work_objective".to_string(),
        summary: "clear current work objective".to_string(),
    })
}

fn render_clear_work_objective_call_ui(_call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    Ok(ToolCallUiEvent::work("clear_work_objective", Vec::new()))
}

fn execute_clear_work_objective_tool<'a>(
    context: &'a mut Context,
    _call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        context.work_state.clear();
        Ok(ToolExecutionResult::new(
            "cleared current work objective",
            json!({}),
            ToolUiEvent::work("cleared current work objective", Vec::new()),
        ))
    })
}

fn summarize_focus_device_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: FocusDeviceArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "focus_device".to_string(),
        summary: format!("device={}", args.device),
    })
}

fn render_focus_device_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: FocusDeviceArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::device(
        format!("focus_device {}", args.device),
        Vec::new(),
    ))
}

fn execute_focus_device_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: FocusDeviceArgs = parse_tool_args(call)?;
        context.devices.focus(args.device).await?;
        Ok(ToolExecutionResult::new(
            format!("focused device {}", args.device),
            json!({ "device": args.device.to_string() }),
            ToolUiEvent::device(
                format!("focused device {}", args.device),
                vec![args.device.to_string()],
            ),
        ))
    })
}

fn summarize_put_away_device_tool(_call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    Ok(EpisodeActionRecord {
        kind: "put_away_device".to_string(),
        summary: "put away current focused device".to_string(),
    })
}

fn render_put_away_device_call_ui(_call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    Ok(ToolCallUiEvent::device("put_away_device", Vec::new()))
}

fn execute_put_away_device_tool<'a>(
    context: &'a mut Context,
    _call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        context.devices.put_away().await?;
        Ok(ToolExecutionResult::new(
            "put away focused device",
            json!({}),
            ToolUiEvent::device("put away focused device", Vec::new()),
        ))
    })
}

fn summarize_resolve_telegram_chat_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ResolveTelegramChatArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "resolve_telegram_chat".to_string(),
        summary: format!(
            "chat_id={} resolution={}",
            args.chat_id,
            resolution_kind(&args.resolution)
        ),
    })
}

fn execute_resolve_telegram_chat_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ResolveTelegramChatArgs = parse_tool_args(call)?;
        execute_resolve_telegram_chat(context, &args.chat_id, args.resolution.clone()).await?;
        Ok(ToolExecutionResult::new(
            format!(
                "resolved telegram chat {} via {}",
                args.chat_id,
                resolution_kind(&args.resolution)
            ),
            json!({
                "chat_id": args.chat_id,
                "resolution": resolution_kind(&args.resolution),
            }),
            ToolUiEvent::telegram(
                TelegramUiAction::ResolveChat,
                format!(
                    "resolved telegram chat {} via {}",
                    args.chat_id,
                    resolution_kind(&args.resolution)
                ),
                vec![
                    format!("chat_id={}", args.chat_id),
                    format!("resolution={}", resolution_kind(&args.resolution)),
                ],
                Vec::new(),
            ),
        ))
    })
}

fn render_resolve_telegram_chat_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ResolveTelegramChatArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::telegram(
        TelegramUiAction::ResolveChat,
        format!("resolve_telegram_chat {}", args.chat_id),
        vec![format!("resolution={}", resolution_kind(&args.resolution))],
        Vec::new(),
    ))
}

fn summarize_obligation_satisfy_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ObligationSatisfyArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "obligation_satisfy".to_string(),
        summary: format!("obligation_id={}", args.obligation_id),
    })
}

fn summarize_report_obligation_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ReportObligationArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "report_obligation".to_string(),
        summary: format!(
            "obligation_id={} reply={}",
            args.obligation_id,
            summarize_inline_text(&args.reply)
        ),
    })
}

fn render_obligation_satisfy_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ObligationSatisfyArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        format!("obligation_satisfy {}", args.obligation_id),
        Vec::new(),
    ))
}

fn render_report_obligation_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ReportObligationArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        format!("report_obligation {}", args.obligation_id),
        vec![summarize_inline_text(&args.reply)],
    ))
}

fn execute_obligation_satisfy_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ObligationSatisfyArgs = parse_tool_args(call)?;
        let obligation_id = resolve_obligation_reference(context, &args.obligation_id)?;
        let Some(obligation) = context.obligations.get(obligation_id).cloned() else {
            return Err(miette::miette!("unknown obligation: {obligation_id}").into());
        };
        if obligation.requires_reply {
            return Err(miette::miette!(
                "obligation {obligation_id} requires an external reply; use report_obligation instead"
            )
            .into());
        }
        context
            .obligations
            .set_status(obligation_id, ObligationStatus::Satisfied);
        Ok(ToolExecutionResult::new(
            format!("satisfied obligation {}", obligation_id),
            json!({ "obligation_id": obligation_id.to_string() }),
            ToolUiEvent::work(
                format!("satisfied obligation {}", obligation_id),
                vec![obligation_id.to_string()],
            ),
        ))
    })
}

fn execute_report_obligation_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ReportObligationArgs = parse_tool_args(call)?;
        let obligation_id = resolve_obligation_reference(context, &args.obligation_id)?;
        execute_report_obligation(context, obligation_id, args.reply.clone()).await?;
        Ok(ToolExecutionResult::new(
            format!("reported obligation {}", obligation_id),
            json!({
                "obligation_id": obligation_id.to_string(),
                "reply": args.reply,
            }),
            ToolUiEvent::work(
                format!("reported obligation {}", obligation_id),
                vec![obligation_id.to_string()],
            ),
        ))
    })
}

fn summarize_commit_to_project_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: CommitToProjectArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "commit_to_project".to_string(),
        summary: format!(
            "obligation_id={} title={}",
            args.obligation_id,
            summarize_inline_text(&args.title)
        ),
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

fn render_commit_to_project_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: CommitToProjectArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        format!("commit_to_project {}", args.obligation_id),
        vec![
            summarize_inline_text(&args.title),
            summarize_inline_text(&args.success_criteria),
        ],
    ))
}

fn execute_commit_to_project_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: CommitToProjectArgs = parse_tool_args(call)?;
        execute_commit_to_project(
            context,
            &args.obligation_id,
            args.title.clone(),
            args.success_criteria.clone(),
            args.initial_next_action.clone(),
            args.acknowledgment.clone(),
        )
        .await?;
        Ok(ToolExecutionResult::new(
            format!("committed obligation {} to project", args.obligation_id),
            json!({
                "obligation_id": args.obligation_id,
                "title": args.title,
                "success_criteria": args.success_criteria,
                "initial_next_action": args.initial_next_action,
            }),
            ToolUiEvent::work(
                format!("committed obligation {} to project", args.obligation_id),
                vec![
                    args.title,
                    args.success_criteria,
                    args.initial_next_action.unwrap_or_default(),
                ]
                .into_iter()
                .filter(|line| !line.trim().is_empty())
                .collect(),
            ),
        ))
    })
}

fn summarize_project_complete_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ProjectCompleteArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "project_complete".to_string(),
        summary: format!(
            "project_id={} summary={}",
            args.project_id,
            summarize_inline_text(&args.summary)
        ),
    })
}

fn render_project_complete_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ProjectCompleteArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        format!("project_complete {}", args.project_id),
        vec![summarize_inline_text(&args.summary)],
    ))
}

fn execute_project_complete_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ProjectCompleteArgs = parse_tool_args(call)?;
        execute_project_complete(context, &args.project_id, args.summary.clone())?;
        Ok(ToolExecutionResult::new(
            format!("completed project {}", args.project_id),
            json!({
                "project_id": args.project_id,
                "summary": args.summary,
            }),
            ToolUiEvent::work(
                format!("completed project {}", args.project_id),
                vec![args.summary],
            ),
        ))
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
    _context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        super::super::execute_apply_patch_tool(&extract_apply_patch_text(call)?).await
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

fn resolve_obligation_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_string_reference(
        "obligation",
        reference,
        context
            .obligations
            .obligations()
            .flat_map(|(id, obligation)| {
                [id.to_string(), obligation.summary.clone()]
                    .into_iter()
                    .map(move |key| (key, id))
            }),
    )
}

fn resolve_project_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_string_reference(
        "project",
        reference,
        context.projects.projects().flat_map(|(id, project)| {
            [id.to_string(), project.title.clone()]
                .into_iter()
                .map(move |key| (key, id))
        }),
    )
}

fn resolve_telegram_chat_reference(context: &Context, reference: &str) -> miette::Result<String> {
    resolve_string_reference(
        "telegram chat",
        reference,
        context
            .telegram
            .chat_refs()
            .into_iter()
            .flat_map(|(chat_id, title)| {
                [chat_id.clone(), title]
                    .into_iter()
                    .map(move |key| (key, chat_id.clone()))
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

async fn send_telegram_message(
    context: &mut Context,
    chat_id: &str,
    text: String,
) -> miette::Result<()> {
    context.devices.focus(DeviceId::Telegram).await?;
    context
        .devices
        .telegram_select_chat(chat_id.to_string())
        .await?;
    context.devices.telegram_send_message(text).await?;
    Ok(())
}

async fn execute_resolve_telegram_chat(
    context: &mut Context,
    chat_reference: &str,
    resolution: TelegramResolution,
) -> miette::Result<()> {
    let chat_id = resolve_telegram_chat_reference(context, chat_reference)?;

    match resolution {
        TelegramResolution::ReplyOnly { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::AskClarification { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::Decline { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::NoReplyNeeded => {
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::AcceptAsProject {
            reply,
            project_title,
            success_criteria,
            first_next_action,
        } => {
            let project_title = require_field(project_title, "project_title")?;
            let success_criteria = require_field(success_criteria, "success_criteria")?;
            let project_id = context.projects.add(
                project_title,
                ProjectOrigin::Telegram,
                success_criteria,
                Some(crate::projects::ReportTarget {
                    device: DeviceId::Telegram,
                    target: chat_id.clone(),
                }),
            );

            if let Some(next_action) = trim_optional_field(first_next_action) {
                context
                    .work_state
                    .set_objective(next_action, Some(project_id));
            }

            if let Some(reply) = trim_optional_field(reply) {
                send_telegram_message(context, &chat_id, reply).await?;
                context.telegram.resolve_chat(&chat_id, Some(false))?;
            } else {
                context.telegram.resolve_chat(&chat_id, None)?;
            }
        }
    }

    Ok(())
}

async fn execute_commit_to_project(
    context: &mut Context,
    obligation_id: &str,
    title: String,
    success_criteria: String,
    initial_next_action: Option<String>,
    acknowledgment: Option<String>,
) -> miette::Result<()> {
    let obligation_id = resolve_obligation_reference(context, obligation_id)?;
    let Some(obligation) = context.obligations.get(obligation_id).cloned() else {
        return Err(miette::miette!("unknown obligation: {obligation_id}"));
    };

    let project_id = context.projects.add(
        title,
        project_origin_from(obligation.source),
        success_criteria,
        obligation.reply_target.clone(),
    );
    context.obligations.link_project(obligation_id, project_id);

    if let Some(next_action) = initial_next_action.map(|s| s.trim().to_string())
        && !next_action.is_empty()
    {
        context
            .work_state
            .set_objective(next_action, Some(project_id));
    }

    if let Some(ack) = acknowledgment.map(|s| s.trim().to_string())
        && !ack.is_empty()
    {
        enqueue_obligation_acknowledgment(context, obligation_id, &obligation, ack).await?;
        return Ok(());
    }

    if obligation.requires_reply {
        context
            .obligations
            .set_status(obligation_id, ObligationStatus::Seen);
    } else {
        context
            .obligations
            .set_status(obligation_id, ObligationStatus::Satisfied);
    }
    Ok(())
}

async fn enqueue_obligation_acknowledgment(
    context: &mut Context,
    obligation_id: Uuid,
    obligation: &crate::obligations::Obligation,
    acknowledgment: String,
) -> miette::Result<()> {
    let Some(target) = obligation.reply_target.clone() else {
        return Err(miette::miette!(
            "obligation {obligation_id} has no reply target"
        ));
    };

    match target.device {
        DeviceId::Telegram => {
            context.devices.focus(DeviceId::Telegram).await?;
            context.devices.telegram_select_chat(target.target).await?;
            context
                .devices
                .telegram_send_message(acknowledgment)
                .await?;
            context
                .obligations
                .set_status(obligation_id, ObligationStatus::Seen);
            Ok(())
        }
        DeviceId::Terminal => Err(miette::miette!(
            "terminal obligations do not support external acknowledgment"
        )),
    }
}

async fn execute_report_obligation(
    context: &mut Context,
    obligation_id: Uuid,
    reply: String,
) -> miette::Result<()> {
    let Some(obligation) = context.obligations.get(obligation_id).cloned() else {
        return Err(miette::miette!("unknown obligation: {obligation_id}"));
    };
    if !obligation.requires_reply {
        return Err(miette::miette!(
            "obligation {obligation_id} does not require an external reply"
        ));
    }
    let Some(target) = obligation.reply_target.clone() else {
        return Err(miette::miette!(
            "obligation {obligation_id} has no reply target"
        ));
    };

    match target.device {
        DeviceId::Telegram => {
            send_telegram_message(context, &target.target, reply).await?;
            context
                .obligations
                .set_status(obligation_id, ObligationStatus::Satisfied);
            Ok(())
        }
        DeviceId::Terminal => Err(miette::miette!(
            "terminal obligations do not support external reply reporting"
        )),
    }
}

fn project_origin_from(source: ObligationSource) -> ProjectOrigin {
    match source {
        ObligationSource::Telegram => ProjectOrigin::Telegram,
        ObligationSource::Terminal => ProjectOrigin::Terminal,
        ObligationSource::System => ProjectOrigin::System,
    }
}

fn execute_project_complete(
    context: &mut Context,
    project_id: &str,
    summary: String,
) -> miette::Result<()> {
    let project_id = resolve_project_reference(context, project_id)?;
    let Some(project) = context.projects.get(project_id).cloned() else {
        return Err(miette::miette!("unknown project: {project_id}"));
    };

    context
        .projects
        .set_status(project_id, crate::projects::ProjectStatus::Completed);
    context.work_state.clear_if_project(project_id);

    if let Some(target) = project.report_back_to {
        context.obligations.add(
            match target.device {
                DeviceId::Telegram => ObligationSource::Telegram,
                DeviceId::Terminal => ObligationSource::Terminal,
            },
            format!(
                "把项目《{}》的结果回复给对方：{}",
                project.title,
                summary.trim()
            ),
            true,
            crate::obligations::Urgency::High,
            Some(project_id),
            Some(target),
        );
    }

    Ok(())
}
