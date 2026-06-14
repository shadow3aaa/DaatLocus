use miette::Result;
use serde_json::json;

use crate::{
    context::Context,
    core::{
        ActivateComposedPrimitiveArgs, CreatePrimitiveSpecArgs, EventResolveArgs,
        NoticeResolvedArgs, ReadPrimitiveSpecArgs, UpdatePlanArgs, UpdatePrimitiveSpecArgs,
    },
    dashboard::render::current_plan_step_for_dashboard,
    events::{EventDisposition, EventPayload, EventStatus},
    plan::{Plan, PlanStatus, PlanStep},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{
        PlanStepUiData, PlanStepUiStatus, PlanUiData, PlanUiKind, ReplyDisposition,
        ToolCallUiEvent, ToolUiEvent,
    },
    workflow::{NewPrimitiveSpec, PrimitiveActivation, PrimitiveRunOutcome, PrimitiveSpec},
};

use super::{
    RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    summarize_inline_text,
};

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![
        Box::new(StaticRuntimeTool::new::<EventResolveArgs>(
            "finish_and_send",
            "Explicitly finish an event and send the final reply when a user reply is needed. `resolved` and `failed` both require `reply_message`; `dismissed` silently ends without sending a message.",
            summarize_event_resolve_tool,
            render_event_resolve_call_ui,
            execute_event_resolve_tool,
        )),
        Box::new(StaticRuntimeTool::new::<NoticeResolvedArgs>(
            "notice_resolved",
            "Explicitly resolve an app notice claimed by the current turn. This completes the notice without sending an external reply.",
            summarize_notice_resolved_tool,
            render_notice_resolved_call_ui,
            execute_notice_resolved_tool,
        )),
        Box::new(StaticRuntimeTool::new::<UpdatePlanArgs>(
            "update_plan",
            "Submit the complete step-by-step plan for the current task.",
            summarize_update_plan_tool,
            render_update_plan_call_ui,
            execute_update_plan_tool,
        )),
        Box::new(StaticRuntimeTool::new::<CreatePrimitiveSpecArgs>(
            "create_primitive_spec",
            "Create an initial reusable SOP primitive draft when no reusable primitive fits.",
            summarize_create_primitive_spec_tool,
            render_create_primitive_spec_call_ui,
            execute_create_primitive_spec_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ActivateComposedPrimitiveArgs>(
            "activate_composed_primitive",
            "Bind one existing SOP primitive or a temporary composition of existing primitives to the current task.",
            summarize_activate_primitive_tool,
            render_activate_primitive_call_ui,
            execute_activate_primitive_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ReadPrimitiveSpecArgs>(
            "read_primitive_spec",
            "Read the complete SOP primitive spec for a primitive id, including origin and backing file path when it is a workspace primitive.",
            summarize_read_workflow_tool,
            render_read_workflow_call_ui,
            execute_read_workflow_tool,
        )),
        Box::new(StaticRuntimeTool::new::<UpdatePrimitiveSpecArgs>(
            "update_primitive_spec",
            "Replace a workspace SOP primitive spec with a complete cleaned version. Use this for user-requested primitive maintenance; builtin primitives are read-only.",
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
        context.queue_active_primitive_run_for_flush(PrimitiveRunOutcome::Completed);
        context.bound_primitive_id = None;
        context.bound_primitive_composition = None;
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

        context.queue_active_primitive_run_for_flush(PrimitiveRunOutcome::Completed);
        context.bound_primitive_id = None;
        context.bound_primitive_composition = None;
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
    Ok(ToolCallUiEvent::plan(PlanUiData {
        kind: PlanUiKind::Proposed,
        explanation: args.explanation,
        steps: args
            .plan
            .into_iter()
            .take(8)
            .map(plan_ui_step_from_args)
            .collect(),
    }))
}

fn execute_update_plan_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: UpdatePlanArgs = parse_tool_args(call)?;
        let explanation = args.explanation.clone();
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
                context.queue_active_primitive_run_for_flush(PrimitiveRunOutcome::Completed);
                context.bound_primitive_id = None;
                context.bound_primitive_composition = None;
                "cleared plan after completion".to_string()
            } else {
                "plan already clear".to_string()
            }
        } else if changed {
            format!("updated plan with {} steps", effective_steps.len())
        } else {
            format!("plan unchanged with {} steps", effective_steps.len())
        };
        let plan_ui_steps = plan_ui_steps(&context.plan);
        let plan_ui_event = match explanation.clone() {
            Some(explanation) => {
                ToolUiEvent::plan_with_explanation(Some(explanation), plan_ui_steps)
            }
            None => ToolUiEvent::plan(plan_ui_steps),
        };
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "explanation": explanation,
                "plan": effective_steps,
            }),
            plan_ui_event,
        ))
    })
}

fn summarize_create_primitive_spec_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: CreatePrimitiveSpecArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "create_primitive_spec".to_string(),
        summary: format!("primitive_id={}", args.id),
    })
}

fn render_create_primitive_spec_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: CreatePrimitiveSpecArgs = parse_tool_args(call)?;
    let lines = vec![
        format!("id={}", args.id),
        format!("when_to_use={}", args.when_to_use.len()),
        format!("primitive_steps={}", args.primitive_steps.len()),
    ];
    Ok(ToolCallUiEvent::create_primitive_spec(
        "create_primitive_spec",
        lines,
    ))
}

fn execute_create_primitive_spec_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: CreatePrimitiveSpecArgs = parse_tool_args(call)?;
        let created = context
            .workflows
            .create_workflow(NewPrimitiveSpec {
                id: args.id,
                when_to_use: args.when_to_use,
                preconditions: args.preconditions,
                primitive_steps: args.primitive_steps,
                done_criteria: args.done_criteria,
                recovery: args.recovery,
            })
            .await?;
        let summary = format!("created primitive spec {}", created.id);
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "created": created,
                "bound_primitive_id": context.bound_primitive_id,
            }),
            ToolUiEvent::create_primitive_spec(created.id),
        ))
    })
}

fn summarize_activate_primitive_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ActivateComposedPrimitiveArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "activate_composed_primitive".to_string(),
        summary: format!("primitive_id={}", args.workflow_id),
    })
}

fn render_activate_primitive_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ActivateComposedPrimitiveArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::activate_primitive(
        "activate_composed_primitive",
        vec![format!("primitive_id={}", args.workflow_id)],
    ))
}

fn execute_activate_primitive_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ActivateComposedPrimitiveArgs = parse_tool_args(call)?;
        let workflow_id = require_field(args.workflow_id, "primitive_id")?;
        let activation = context
            .workflows
            .activate_composed_primitive(&workflow_id)?;
        let (bound_id, primitive_ids, activated_value, is_composition) = match activation {
            PrimitiveActivation::Single { primitive } => {
                let primitive_id = primitive.id.clone();
                (
                    primitive_id,
                    vec![primitive.id.clone()],
                    json!(primitive),
                    false,
                )
            }
            PrimitiveActivation::Composition { composition } => {
                let composition_id = composition.composition_id.clone();
                (
                    composition_id,
                    composition.primitive_ids.clone(),
                    json!(composition),
                    true,
                )
            }
        };
        if context.bound_primitive_id.as_deref() == Some(bound_id.as_str()) {
            context.begin_composed_primitive_run_session(bound_id.clone());
            let summary = if is_composition {
                format!("primitive composition {bound_id} is already active")
            } else {
                format!("primitive {bound_id} is already active")
            };
            return Ok(ToolExecutionResult::new(
                summary.clone(),
                json!({
                    "bound_primitive_id": context.bound_primitive_id,
                    "bound_primitive_composition": context.bound_primitive_composition,
                    "primitive_ids": primitive_ids,
                    "activated": activated_value,
                    "is_composition": is_composition,
                    "already_active": true,
                }),
                ToolUiEvent::activate_primitive(workflow_id),
            )
            .with_model_content(format!(
                "summary={summary}\nalready_active=true\nbound_primitive_id={bound_id}\nprimitive_ids={}\nContinue the task using the currently bound primitive or composition; do not call activate_composed_primitive again for this binding.",
                primitive_ids.join(",")
            )));
        }
        if context.bound_primitive_id.as_deref() != Some(bound_id.as_str()) {
            context.queue_active_primitive_run_for_flush(PrimitiveRunOutcome::Superseded);
        }
        context.bound_primitive_id = Some(bound_id.clone());
        context.bound_primitive_composition =
            is_composition.then(|| crate::workflow::PrimitiveComposition {
                composition_id: bound_id.clone(),
                primitive_ids: primitive_ids.clone(),
            });
        context.begin_composed_primitive_run_session(bound_id.clone());
        let summary = if is_composition {
            format!("activated primitive composition {bound_id}")
        } else {
            format!("activated primitive {bound_id}")
        };
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "bound_primitive_id": context.bound_primitive_id,
                "bound_primitive_composition": context.bound_primitive_composition,
                "primitive_ids": primitive_ids,
                "activated": activated_value,
                "is_composition": is_composition,
            }),
            ToolUiEvent::activate_primitive(workflow_id),
        )
        .with_turn_boundary(activate_primitive_turn_boundary_reason()))
    })
}

fn activate_primitive_turn_boundary_reason() -> &'static str {
    "primitive binding changed; re-render world state in a new turn before continuing"
}

fn summarize_read_workflow_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ReadPrimitiveSpecArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "read_primitive_spec".to_string(),
        summary: format!("primitive_id={}", args.workflow_id),
    })
}

fn render_read_workflow_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ReadPrimitiveSpecArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "read_primitive_spec",
        vec![format!("primitive_id={}", args.workflow_id)],
    ))
}

fn execute_read_workflow_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ReadPrimitiveSpecArgs = parse_tool_args(call)?;
        let workflow_id = require_field(args.workflow_id, "primitive_id")?;
        let spec = context
            .workflows
            .get(&workflow_id)
            .cloned()
            .ok_or_else(|| miette::miette!("unknown primitive_id `{workflow_id}`"))?;
        let origin = context.workflows.workflow_origin(&workflow_id);
        let path = context
            .workflows
            .workflow_path(&workflow_id)
            .map(|path| path.display().to_string());
        let summary = format!("read primitive spec {}", spec.id);
        let primitive_id = spec.id.clone();
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "primitive_id": primitive_id,
                "origin": origin,
                "path": path,
                "spec": spec,
            }),
            ToolUiEvent::app(
                "Read Primitive Spec",
                vec![
                    format!("primitive_id={workflow_id}"),
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
    let args: UpdatePrimitiveSpecArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "update_primitive_spec".to_string(),
        summary: format!(
            "primitive_id={} steps={} reason={}",
            args.workflow_id,
            args.primitive_steps.len(),
            args.reason.as_deref().unwrap_or("")
        ),
    })
}

fn render_update_workflow_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: UpdatePrimitiveSpecArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "update_primitive_spec",
        vec![
            format!("primitive_id={}", args.workflow_id),
            format!("when_to_use={}", args.when_to_use.len()),
            format!("primitive_steps={}", args.primitive_steps.len()),
            format!("done_criteria={}", args.done_criteria.len()),
        ],
    ))
}

fn execute_update_workflow_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: UpdatePrimitiveSpecArgs = parse_tool_args(call)?;
        let workflow_id = require_field(args.workflow_id, "primitive_id")?;
        let updated = context
            .workflows
            .replace_workspace_workflow(
                &workflow_id,
                PrimitiveSpec {
                    id: workflow_id.clone(),
                    when_to_use: args.when_to_use,
                    preconditions: args.preconditions,
                    primitive_steps: args.primitive_steps,
                    done_criteria: args.done_criteria,
                    recovery: args.recovery,
                },
            )
            .await?;
        let summary = format!("updated primitive spec {}", updated.id);
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "primitive_id": updated.id,
                "updated": updated,
                "reason": trim_optional_field(args.reason),
            }),
            ToolUiEvent::app(
                "Updated Primitive Spec",
                vec![
                    format!("primitive_id={workflow_id}"),
                    format!("summary={summary}"),
                ],
            ),
        )
        .with_turn_boundary("primitive spec updated; re-render world state in a new turn"))
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
            context.events.finish_with_reply(
                event_id,
                status_for_event_disposition(disposition),
                reply_message,
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

fn plan_ui_step_from_args(step: crate::core::UpdatePlanStepArgs) -> PlanStepUiData {
    PlanStepUiData {
        status: match step.status {
            PlanStatus::Pending => PlanStepUiStatus::Pending,
            PlanStatus::InProgress => PlanStepUiStatus::InProgress,
            PlanStatus::Completed => PlanStepUiStatus::Completed,
        },
        text: summarize_inline_text(&step.step),
    }
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
    fn update_plan_call_ui_renders_proposed_plan() {
        let call = AgentToolCall {
            id: "call_1".to_string(),
            name: "update_plan".to_string(),
            arguments: json!({
                "explanation": "Need a short setup sequence.",
                "plan": [
                    { "step": "Inspect state", "status": "in_progress" },
                    { "step": "Apply fix", "status": "pending" }
                ],
            }),
        };

        let event = render_update_plan_call_ui(&call).expect("render update_plan call ui");
        match event {
            ToolCallUiEvent::Plan(plan) => {
                assert_eq!(plan.kind, PlanUiKind::Proposed);
                assert_eq!(
                    plan.explanation.as_deref(),
                    Some("Need a short setup sequence.")
                );
                assert_eq!(plan.steps.len(), 2);
                assert_eq!(plan.steps[0].status, PlanStepUiStatus::InProgress);
            }
            other => panic!("expected plan call ui, got {other:?}"),
        }
    }

    #[test]
    fn activate_composed_primitive_declares_turn_boundary_reason() {
        assert_eq!(
            activate_primitive_turn_boundary_reason(),
            "primitive binding changed; re-render world state in a new turn before continuing"
        );
    }
}
