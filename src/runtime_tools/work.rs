use miette::Result;
use serde_json::json;

use crate::{
    apply_patch::{PatchOperationKind, parse_apply_patch, summarize_patch_ops},
    context::Context,
    core::{
        ActivateWorkflowArgs, CreateWorkflowArgs, DeepRecallArgs, EventResolveArgs, FocusAppArgs,
        PutAwayAppArgs, UpdatePlanArgs,
    },
    events::{EventDisposition, EventPayload, EventStatus},
    hindsight::HindsightReflectOptions,
    plan::{Plan, PlanStatus, PlanStep},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{ReplyDisposition, ToolCallUiEvent, ToolUiEvent},
    workflow::{NewWorkflowSpec, WorkflowRunOutcome},
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
            "显式终结一个事件，并在需要回复用户时发送最终回复。`resolved` 和 `failed` 都必须提供 `reply_message`；`dismissed` 用于静默结束而不发送消息。",
            None,
            summarize_event_resolve_tool,
            render_event_resolve_call_ui,
            execute_event_resolve_tool,
        )),
        Box::new(StaticRuntimeTool::new::<UpdatePlanArgs>(
            "update_plan",
            "提交当前任务的完整分步 plan。",
            None,
            summarize_update_plan_tool,
            render_update_plan_call_ui,
            execute_update_plan_tool,
        )),
        Box::new(StaticRuntimeTool::new::<CreateWorkflowArgs>(
            "create_workflow",
            "当没有可复用 workflow 时创建一个新 workflow 初稿。",
            None,
            summarize_create_workflow_tool,
            render_create_workflow_call_ui,
            execute_create_workflow_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ActivateWorkflowArgs>(
            "activate_workflow",
            "将一个 workflow 绑定到当前任务，供后续多步骤执行复用。",
            None,
            summarize_activate_workflow_tool,
            render_activate_workflow_call_ui,
            execute_activate_workflow_tool,
        )),
        Box::new(StaticRuntimeTool::new::<DeepRecallArgs>(
            "deep_recall",
            "对长期记忆执行一次较慢但更深的 reflect 查询，用于线程恢复、项目状态判断、用户偏好推断，以及需要证据链的高层建议或风险分析。",
            None,
            summarize_deep_recall_tool,
            render_deep_recall_call_ui,
            execute_deep_recall_tool,
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
            ToolUiEvent::app(format!("focused app {}", app), vec![app.to_string()]),
        )
        .with_turn_boundary(format!(
            "focused app changed to {}; re-render world state in a new turn",
            app
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
    Ok(ToolCallUiEvent::finish("finish_and_send", lines))
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
                    .take(8)
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
        let effective_steps = context.plan.steps().to_vec();
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
            ToolUiEvent::plan(summary, render_plan_ui_lines(&context.plan)),
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
        let ui_lines = vec![format!("id={}", created.id)];
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "created": created,
                "bound_workflow_id": context.bound_workflow_id,
            }),
            ToolUiEvent::create_workflow(summary, ui_lines),
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
            ToolUiEvent::activate_workflow(
                summary,
                vec![format!("bound_workflow_id={}", workflow_id)],
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
    Ok(ToolCallUiEvent::deep_recall("deep_recall", lines))
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
                    include_facts: true,
                    include_tool_calls: true,
                    include_tool_call_output: false,
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
        if let Some(based_on) = &response.based_on {
            body_lines.push(format!("sources: memories={}", based_on.memories.len()));
            body_lines.extend(based_on.memories.iter().take(3).map(|memory| {
                format!(
                    "memory: {} [{}] {}",
                    memory.id,
                    memory
                        .r#type
                        .clone()
                        .unwrap_or_else(|| "memory".to_string()),
                    summarize_inline_text(&memory.text)
                )
            }));
        }
        if let Some(trace) = &response.trace {
            body_lines.push(format!(
                "trace: tool_calls={} llm_calls={}",
                trace.tool_calls.len(),
                trace.llm_calls.len()
            ));
        }
        if let Some(usage) = &response.usage {
            body_lines.push(format!(
                "usage: input={} output={} total={}",
                usage.input_tokens, usage.output_tokens, usage.total_tokens
            ));
        }
        Ok(ToolExecutionResult::new(
            title.clone(),
            json!({
                "query": args.query,
                "budget": args.budget,
                "max_tokens": args.max_tokens,
                "text": response.text,
                "based_on": response.based_on,
                "usage": response.usage,
                "trace": response.trace,
            }),
            ToolUiEvent::deep_recall(title, body_lines),
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

fn render_plan_ui_lines(plan: &Plan) -> Vec<String> {
    plan.steps()
        .iter()
        .take(8)
        .map(|step| format!("[{}] {}", step.status, summarize_inline_text(&step.step)))
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
}
