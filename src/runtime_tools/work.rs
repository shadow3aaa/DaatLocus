use miette::Result;
use serde_json::json;

use crate::{
    apply_patch::{PatchOperationKind, parse_apply_patch, summarize_patch_ops},
    context::Context,
    core::{
        ActivateWorkflowArgs, CreateWorkflowArgs, EventResolveArgs, FocusAppArgs,
        NoticeResolvedArgs, PutAwayAppArgs, ReadWorkflowArgs, UpdatePlanArgs, UpdateWorkflowArgs,
    },
    dashboard::render::current_plan_step_for_dashboard,
    events::{EventDisposition, EventPayload, EventStatus},
    plan::{Plan, PlanStatus, PlanStep},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{PlanStepUiData, PlanStepUiStatus, ReplyDisposition, ToolCallUiEvent, ToolUiEvent},
    workflow::{NewWorkflowSpec, WorkflowRunOutcome, WorkflowSpec},
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
            "Bring the specified app to the foreground.",
            None,
            summarize_focus_app_tool,
            render_focus_app_call_ui,
            execute_focus_app_tool,
        )),
        Box::new(StaticRuntimeTool::new::<PutAwayAppArgs>(
            "put_away_app",
            "Put the current foreground app back into the background.",
            None,
            summarize_put_away_app_tool,
            render_put_away_app_call_ui,
            execute_put_away_app_tool,
        )),
        Box::new(StaticRuntimeTool::new::<EventResolveArgs>(
            "finish_and_send",
            "Explicitly finish an event and send the final reply when a user reply is needed. `resolved` and `failed` both require `reply_message`; `dismissed` silently ends without sending a message.",
            None,
            summarize_event_resolve_tool,
            render_event_resolve_call_ui,
            execute_event_resolve_tool,
        )),
        Box::new(StaticRuntimeTool::new::<NoticeResolvedArgs>(
            "notice_resolved",
            "Explicitly resolve an app notice claimed by the current turn. This completes the notice without sending an external reply.",
            None,
            summarize_notice_resolved_tool,
            render_notice_resolved_call_ui,
            execute_notice_resolved_tool,
        )),
        Box::new(StaticRuntimeTool::new::<UpdatePlanArgs>(
            "update_plan",
            "Submit the complete step-by-step plan for the current task.",
            None,
            summarize_update_plan_tool,
            render_update_plan_call_ui,
            execute_update_plan_tool,
        )),
        Box::new(StaticRuntimeTool::new::<CreateWorkflowArgs>(
            "create_workflow",
            "Create an initial workflow draft when no reusable workflow fits.",
            None,
            summarize_create_workflow_tool,
            render_create_workflow_call_ui,
            execute_create_workflow_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ActivateWorkflowArgs>(
            "activate_workflow",
            "Bind a workflow to the current task for subsequent multi-step execution.",
            None,
            summarize_activate_workflow_tool,
            render_activate_workflow_call_ui,
            execute_activate_workflow_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ReadWorkflowArgs>(
            "read_workflow",
            "Read the complete workflow spec for a workflow id, including origin and backing file path when it is a workspace workflow.",
            None,
            summarize_read_workflow_tool,
            render_read_workflow_call_ui,
            execute_read_workflow_tool,
        )),
        Box::new(StaticRuntimeTool::new::<UpdateWorkflowArgs>(
            "update_workflow",
            "Replace a workspace workflow spec with a complete cleaned version. Use this for user-requested workflow maintenance; builtin workflows are read-only.",
            None,
            summarize_update_workflow_tool,
            render_update_workflow_call_ui,
            execute_update_workflow_tool,
        )),
    ]
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

fn disposition_requires_reply(disposition: EventDisposition) -> bool {
    matches!(
        disposition,
        EventDisposition::Resolved | EventDisposition::Failed
    )
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
        let app = args.app.clone();
        context.apps.focus(app.clone()).await?;
        Ok(ToolExecutionResult::new(
            format!("focused app {}", app),
            json!({ "app": app.to_string() }),
            ToolUiEvent::focus_app(app.to_string()),
        )
        .with_turn_boundary(focus_app_turn_boundary_reason(&app)))
    })
}

fn focus_app_turn_boundary_reason(app: &crate::app::AppId) -> String {
    format!("focused app changed to {app}; re-render world state in a new turn")
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
            ToolUiEvent::put_away_app(),
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
            None => format!("disposition={}", event_disposition_kind(args.disposition)),
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
    Ok(ToolCallUiEvent::app("finish_and_send", lines))
}

fn execute_event_resolve_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: EventResolveArgs = parse_tool_args(call)?;
        let event_id = context
            .claimed_event_ids
            .first()
            .ok_or_else(|| miette::miette!("no claimed event in current turn"))?
            .clone();
        let reply_message = trim_optional_field(args.reply_message);
        let event = context.events.view(&event_id)?;
        let required_reply_message = if disposition_requires_reply(args.disposition) {
            Some(reply_message.clone().ok_or_else(|| {
                miette::miette!(
                    "{} requires a non-empty reply_message",
                    event_disposition_kind(args.disposition),
                )
            })?)
        } else {
            None
        };
        let summary = match args.disposition {
            EventDisposition::Resolved | EventDisposition::Failed => {
                let reply_message = required_reply_message
                    .clone()
                    .expect("reply requirement should be validated above");
                let delivery_summary = match &event.payload {
                    EventPayload::TelegramIncoming(_) => {
                        format!(
                            "{} event {} via channel delivery",
                            event_disposition_kind(args.disposition),
                            event_id
                        )
                    }
                    EventPayload::TerminalIncoming(_) => {
                        format!(
                            "{} terminal event {}",
                            event_disposition_kind(args.disposition),
                            event_id
                        )
                    }
                };
                execute_event_resolve_with_reply(
                    context,
                    &event_id,
                    &event,
                    args.disposition,
                    reply_message.clone(),
                    args.note.clone(),
                )?;
                delivery_summary
            }
            EventDisposition::Dismissed => {
                context.events.set_status(
                    &event_id,
                    status_for_event_disposition(args.disposition),
                    args.note.clone(),
                )?;
                format!(
                    "resolved event {} as {}",
                    event_id,
                    event_disposition_kind(args.disposition)
                )
            }
        };
        context.queue_active_workflow_run_for_flush(WorkflowRunOutcome::Completed);
        context.bound_workflow_id = None;
        let reply_lines = reply_message
            .as_deref()
            .map(|message| {
                message
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let result_payload = json!({
            "event_id": event_id,
            "disposition": event_disposition_kind(args.disposition),
            "reply_message": reply_message.clone(),
            "note": args.note.clone(),
        });
        Ok(ToolExecutionResult::new(
            summary.clone(),
            result_payload,
            ToolUiEvent::reply(
                match args.disposition {
                    EventDisposition::Resolved => ReplyDisposition::Resolved,
                    EventDisposition::Dismissed => ReplyDisposition::Dismissed,
                    EventDisposition::Failed => ReplyDisposition::Failed,
                },
                reply_lines,
            ),
        ))
    })
}

fn summarize_notice_resolved_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: NoticeResolvedArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "notice_resolved".to_string(),
        summary: format!(
            "app={} reason={}",
            args.app,
            summarize_inline_text(args.reason.trim())
        ),
    })
}

fn render_notice_resolved_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: NoticeResolvedArgs = parse_tool_args(call)?;
    let mut lines = vec![
        format!("app={}", args.app),
        format!("reason={}", summarize_inline_text(args.reason.trim())),
    ];
    if let Some(note) = args.note.as_deref()
        && !note.trim().is_empty()
    {
        lines.push(format!("note={}", summarize_inline_text(note)));
    }
    Ok(ToolCallUiEvent::app("notice_resolved", lines))
}

fn execute_notice_resolved_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: NoticeResolvedArgs = parse_tool_args(call)?;
        let key = crate::context::AppNoticeKey::new(args.app.clone(), args.reason.clone());
        if !context.resolve_claimed_app_notice(&key) {
            let claimed = context
                .claimed_app_notices
                .iter()
                .map(|notice| format!("{}:{}", notice.app, notice.reason))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(miette::miette!(
                "notice_resolved can only resolve an app notice claimed by the current turn; requested={}:{} claimed=[{}]",
                key.app,
                key.reason,
                claimed,
            ));
        }

        context.queue_active_workflow_run_for_flush(WorkflowRunOutcome::Completed);
        context.bound_workflow_id = None;
        let result_lines = vec![
            format!("App notice resolved: {}", key.app),
            format!("Reason: {}", summarize_inline_text(&key.reason)),
        ];
        Ok(ToolExecutionResult::new(
            format!("resolved app notice {}: {}", key.app, key.reason),
            json!({
                "app": key.app,
                "reason": key.reason,
                "note": args.note,
            }),
            ToolUiEvent::notice_reply(ReplyDisposition::Resolved, result_lines),
        ))
    })
}

fn summarize_update_plan_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: UpdatePlanArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "update_plan".to_string(),
        summary: format!("steps={}", args.plan.len()),
    })
}

fn render_update_plan_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: UpdatePlanArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::plan(
        "update_plan",
        args.plan
            .into_iter()
            .take(6)
            .map(|step| format!("[{}] {}", step.status, summarize_inline_text(&step.step)))
            .collect(),
    ))
}

fn execute_update_plan_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: UpdatePlanArgs = parse_tool_args(call)?;
        let plan = build_plan_from_args(args)?;
        let changed = context.plan.replace(plan.steps().to_vec());
        if changed {
            context.plan.sync_to_disk().await?;
        }
        let effective_steps = context.plan.steps().to_vec();
        if let Some(tx) = &context.dashboard_tx {
            let current_plan_step = current_plan_step_for_dashboard(context);
            tx.send_modify(|state| {
                state.current_plan_step = current_plan_step.clone();
            });
        }
        let summary = if effective_steps.is_empty() {
            if changed {
                context.queue_active_workflow_run_for_flush(WorkflowRunOutcome::Completed);
                "cleared plan after completion".to_string()
            } else {
                "plan already clear".to_string()
            }
        } else if changed {
            format!("updated plan with {} steps", effective_steps.len())
        } else {
            format!("plan unchanged with {} steps", effective_steps.len())
        };
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "plan": effective_steps,
            }),
            ToolUiEvent::plan(plan_ui_steps(&context.plan)),
        ))
    })
}

fn summarize_create_workflow_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: CreateWorkflowArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "create_workflow".to_string(),
        summary: format!("workflow_id={}", args.id),
    })
}

fn render_create_workflow_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: CreateWorkflowArgs = parse_tool_args(call)?;
    let lines = vec![
        format!("id={}", args.id),
        format!("when_to_use={}", args.when_to_use.len()),
        format!("workflow_steps={}", args.workflow_steps.len()),
    ];
    Ok(ToolCallUiEvent::create_workflow("create_workflow", lines))
}

fn execute_create_workflow_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: CreateWorkflowArgs = parse_tool_args(call)?;
        let created = context
            .workflows
            .create_workflow(NewWorkflowSpec {
                id: args.id,
                when_to_use: args.when_to_use,
                preconditions: args.preconditions,
                workflow_steps: args.workflow_steps,
                done_criteria: args.done_criteria,
                recovery: args.recovery,
            })
            .await?;
        let summary = format!("created workflow {}", created.id);
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "created": created,
                "bound_workflow_id": context.bound_workflow_id,
            }),
            ToolUiEvent::create_workflow(created.id),
        ))
    })
}

fn summarize_activate_workflow_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ActivateWorkflowArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "activate_workflow".to_string(),
        summary: format!("workflow_id={}", args.workflow_id),
    })
}

fn render_activate_workflow_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ActivateWorkflowArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::activate_workflow(
        "activate_workflow",
        vec![format!("workflow_id={}", args.workflow_id)],
    ))
}

fn execute_activate_workflow_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ActivateWorkflowArgs = parse_tool_args(call)?;
        let workflow_id = require_field(args.workflow_id, "workflow_id")?;
        let activated = context
            .workflows
            .get(&workflow_id)
            .cloned()
            .ok_or_else(|| miette::miette!("unknown workflow_id `{workflow_id}`"))?;
        if context.bound_workflow_id.as_deref() == Some(activated.id.as_str()) {
            context.begin_workflow_run_session(activated.id.clone());
            let summary = format!("workflow {} is already active", activated.id);
            return Ok(ToolExecutionResult::new(
                summary.clone(),
                json!({
                    "bound_workflow_id": context.bound_workflow_id,
                    "activated": activated,
                    "already_active": true,
                }),
                ToolUiEvent::activate_workflow(workflow_id),
            )
            .with_model_content(format!(
                "summary={summary}\nalready_active=true\nContinue the task using the currently bound workflow; do not call activate_workflow again for this workflow."
            )));
        }
        if context.bound_workflow_id.as_deref() != Some(activated.id.as_str()) {
            context.queue_active_workflow_run_for_flush(WorkflowRunOutcome::Superseded);
        }
        context.bound_workflow_id = Some(activated.id.clone());
        context.begin_workflow_run_session(activated.id.clone());
        let summary = format!("activated workflow {}", activated.id);
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "bound_workflow_id": context.bound_workflow_id,
                "activated": activated,
            }),
            ToolUiEvent::activate_workflow(workflow_id),
        )
        .with_turn_boundary(activate_workflow_turn_boundary_reason()))
    })
}

fn activate_workflow_turn_boundary_reason() -> &'static str {
    "workflow binding changed; re-render world state in a new turn before continuing"
}

fn summarize_read_workflow_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ReadWorkflowArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "read_workflow".to_string(),
        summary: format!("workflow_id={}", args.workflow_id),
    })
}

fn render_read_workflow_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ReadWorkflowArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "read_workflow",
        vec![format!("workflow_id={}", args.workflow_id)],
    ))
}

fn execute_read_workflow_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ReadWorkflowArgs = parse_tool_args(call)?;
        let workflow_id = require_field(args.workflow_id, "workflow_id")?;
        let spec = context
            .workflows
            .get(&workflow_id)
            .cloned()
            .ok_or_else(|| miette::miette!("unknown workflow_id `{workflow_id}`"))?;
        let origin = context.workflows.workflow_origin(&workflow_id);
        let path = context
            .workflows
            .workflow_path(&workflow_id)
            .map(|path| path.display().to_string());
        let summary = format!("read workflow {}", spec.id);
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "workflow_id": spec.id,
                "origin": origin,
                "path": path,
                "spec": spec,
            }),
            ToolUiEvent::app(
                "Read Workflow",
                vec![
                    format!("workflow_id={workflow_id}"),
                    format!(
                        "origin={}",
                        origin
                            .map(|origin| format!("{origin:?}").to_ascii_lowercase())
                            .unwrap_or_else(|| "unknown".to_string())
                    ),
                ],
            ),
        ))
    })
}

fn summarize_update_workflow_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: UpdateWorkflowArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "update_workflow".to_string(),
        summary: format!(
            "workflow_id={} steps={} reason={}",
            args.workflow_id,
            args.workflow_steps.len(),
            args.reason.as_deref().unwrap_or("")
        ),
    })
}

fn render_update_workflow_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: UpdateWorkflowArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "update_workflow",
        vec![
            format!("workflow_id={}", args.workflow_id),
            format!("when_to_use={}", args.when_to_use.len()),
            format!("workflow_steps={}", args.workflow_steps.len()),
            format!("done_criteria={}", args.done_criteria.len()),
        ],
    ))
}

fn execute_update_workflow_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: UpdateWorkflowArgs = parse_tool_args(call)?;
        let workflow_id = require_field(args.workflow_id, "workflow_id")?;
        let updated = context
            .workflows
            .replace_workspace_workflow(
                &workflow_id,
                WorkflowSpec {
                    id: workflow_id.clone(),
                    when_to_use: args.when_to_use,
                    preconditions: args.preconditions,
                    workflow_steps: args.workflow_steps,
                    done_criteria: args.done_criteria,
                    recovery: args.recovery,
                },
            )
            .await?;
        let summary = format!("updated workflow {}", updated.id);
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "updated": updated,
                "reason": trim_optional_field(args.reason),
            }),
            ToolUiEvent::app(
                "Updated Workflow",
                vec![
                    format!("workflow_id={workflow_id}"),
                    format!("summary={summary}"),
                ],
            ),
        )
        .with_turn_boundary("workflow spec updated; re-render world state in a new turn"))
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
                    PatchOperationKind::Add => crate::tool_ui::PatchFileOperation::Add,
                    PatchOperationKind::Delete => crate::tool_ui::PatchFileOperation::Delete,
                    PatchOperationKind::Update => crate::tool_ui::PatchFileOperation::Update,
                },
                added_lines: file.added_lines,
                removed_lines: file.removed_lines,
                diff_lines: Vec::new(),
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
    disposition: EventDisposition,
    reply_message: String,
    note: Option<String>,
) -> miette::Result<()> {
    match &event.payload {
        EventPayload::TelegramIncoming(payload) => {
            context.events.prepare_telegram_delivery(event_id)?;
            context.telegram.enqueue_outgoing_message(
                payload.chat_id.clone(),
                reply_message,
                Some(event_id.to_string()),
                Some(status_for_event_disposition(disposition)),
                note.filter(|_| matches!(disposition, EventDisposition::Failed)),
            )?;
            Ok(())
        }
        EventPayload::TerminalIncoming(_) => {
            context.events.set_status(
                event_id,
                status_for_event_disposition(disposition),
                note.filter(|_| matches!(disposition, EventDisposition::Failed)),
            )?;
            Ok(())
        }
    }
}

fn build_plan_from_args(args: UpdatePlanArgs) -> miette::Result<Plan> {
    let now = chrono::Utc::now().timestamp_millis();
    let steps = args
        .plan
        .into_iter()
        .map(|step| {
            Ok(PlanStep {
                step: require_field(step.step, "plan[].step")?,
                status: step.status,
                created_at_ms: now,
                last_updated_at_ms: now,
            })
        })
        .collect::<miette::Result<Vec<_>>>()?;

    validate_plan_steps(&steps)?;
    let mut plan = Plan::default();
    let _ = plan.replace(steps);
    Ok(plan)
}

fn validate_plan_steps(steps: &[PlanStep]) -> miette::Result<()> {
    let in_progress = steps
        .iter()
        .filter(|step| matches!(step.status, PlanStatus::InProgress))
        .count();
    let all_completed = !steps.is_empty()
        && steps
            .iter()
            .all(|step| matches!(step.status, PlanStatus::Completed));

    if steps.is_empty() {
        return Ok(());
    }
    if all_completed {
        if in_progress != 0 {
            return Err(miette::miette!(
                "update_plan cannot contain `in_progress` steps when all steps are completed"
            ));
        }
        return Ok(());
    }
    if in_progress != 1 {
        return Err(miette::miette!(
            "update_plan must contain exactly one `in_progress` step until all steps are completed"
        ));
    }
    Ok(())
}

fn plan_ui_steps(plan: &Plan) -> Vec<PlanStepUiData> {
    plan.steps()
        .iter()
        .take(8)
        .map(|step| PlanStepUiData {
            status: match step.status {
                PlanStatus::Pending => PlanStepUiStatus::Pending,
                PlanStatus::InProgress => PlanStepUiStatus::InProgress,
                PlanStatus::Completed => PlanStepUiStatus::Completed,
            },
            text: summarize_inline_text(&step.step),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_is_required_for_resolved_and_failed_but_not_dismissed() {
        assert!(disposition_requires_reply(EventDisposition::Resolved));
        assert!(disposition_requires_reply(EventDisposition::Failed));
        assert!(!disposition_requires_reply(EventDisposition::Dismissed));
    }

    #[test]
    fn notice_resolved_call_ui_renders_claimed_notice_identity() {
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "notice_resolved".to_string(),
            arguments: json!({
                "app": "Terminal",
                "reason": "terminal output reviewed",
                "note": "handled",
            }),
        };

        let event = render_notice_resolved_call_ui(&call).expect("render notice resolved call");
        match event {
            ToolCallUiEvent::App(data) => {
                assert_eq!(data.title, "notice_resolved");
                assert_eq!(
                    data.body_lines,
                    vec![
                        "app=Terminal".to_string(),
                        "reason=terminal output reviewed".to_string(),
                        "note=handled".to_string(),
                    ]
                );
            }
            other => panic!("expected app call ui, got {other:?}"),
        }
    }

    #[test]
    fn focus_app_tool_declares_turn_boundary_reason() {
        assert_eq!(
            focus_app_turn_boundary_reason(&crate::app::AppId::terminal()),
            "focused app changed to Terminal; re-render world state in a new turn"
        );
    }

    #[test]
    fn activate_workflow_declares_turn_boundary_reason() {
        assert_eq!(
            activate_workflow_turn_boundary_reason(),
            "workflow binding changed; re-render world state in a new turn before continuing"
        );
    }
}
