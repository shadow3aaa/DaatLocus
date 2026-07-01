use miette::Result;
use serde_json::json;

use crate::{
    activity_event::{
        PlanActivityDescriptor, PlanActivityKind, PlanStepActivityDescriptor,
        PlanStepActivityStatus, ReplyDisposition, ReplySubject, ToolCallActivityEvent,
    },
    context::Context,
    core::{EventResolveArgs, NoticeResolvedArgs, UpdatePlanArgs},
    dashboard::SessionActivityEvent,
    dashboard::render::{current_plan_step_for_dashboard, status_command_snapshot_for_dashboard},
    events::{EventDisposition, EventPayload, EventStatus},
    plan::{Plan, PlanStatus, PlanStep},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    schema_utils::model_schema_for,
};

use super::{
    RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    summarize_inline_text,
};

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![
        Box::new(StaticRuntimeTool::new_with_schema(
            "finish_and_send",
            "Explicitly finish an event and send the final reply when a user reply is needed. `resolved` and `failed` both require `reply_message`; `dismissed` silently ends without sending a message.",
            model_schema_for::<EventResolveArgs>(),
            summarize_event_resolve_tool,
            render_event_resolve_call_ui,
            execute_event_resolve_tool,
        )),
        Box::new(StaticRuntimeTool::new_with_schema(
            "notice_resolved",
            "Explicitly resolve an app notice claimed by the current turn. This completes the notice without sending an external reply.",
            model_schema_for::<NoticeResolvedArgs>(),
            summarize_notice_resolved_tool,
            render_notice_resolved_call_ui,
            execute_notice_resolved_tool,
        )),
        Box::new(StaticRuntimeTool::new_with_schema(
            "update_plan",
            "Submit the complete step-by-step plan for the current task.",
            model_schema_for::<UpdatePlanArgs>(),
            summarize_update_plan_tool,
            render_update_plan_call_ui,
            execute_update_plan_tool,
        )),
    ]
}

fn reply_activity_event(
    disposition: ReplyDisposition,
    subject: ReplySubject,
    message_lines: Vec<String>,
) -> SessionActivityEvent {
    SessionActivityEvent::Reply(
        crate::activity_event::ReplyActivityDescriptor {
            disposition,
            subject,
            message_lines,
        }
        .into(),
    )
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

fn render_event_resolve_call_ui(call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
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
    Ok(ToolCallActivityEvent::app("finish_and_send", lines))
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
        let reply_lines = reply_message
            .as_deref()
            .map(|message| message.lines().map(ToString::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        context.queue_active_skill_run_for_flush(crate::context::SkillRunOutcome::Completed);
        let result_payload = json!({
            "event_id": event_id,
            "disposition": event_disposition_kind(args.disposition),
            "reply_message": reply_message.clone(),
            "note": args.note.clone(),
        });
        Ok(ToolExecutionResult::from_activity_event(
            summary.clone(),
            result_payload,
            Some(reply_activity_event(
                match args.disposition {
                    EventDisposition::Resolved => ReplyDisposition::Resolved,
                    EventDisposition::Dismissed => ReplyDisposition::Dismissed,
                    EventDisposition::Failed => ReplyDisposition::Failed,
                },
                ReplySubject::Message,
                reply_lines,
            )),
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

fn render_notice_resolved_call_ui(call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
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
    Ok(ToolCallActivityEvent::app("notice_resolved", lines))
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

        context.queue_active_skill_run_for_flush(crate::context::SkillRunOutcome::Completed);
        let result_lines = vec![format!("Reason: {}", summarize_inline_text(&key.reason))];
        Ok(ToolExecutionResult::from_activity_event(
            format!("resolved app notice {}: {}", key.app, key.reason),
            json!({
                "app": key.app,
                "reason": key.reason,
                "note": args.note,
            }),
            Some(reply_activity_event(
                ReplyDisposition::Resolved,
                ReplySubject::Notice,
                result_lines,
            )),
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

fn render_update_plan_call_ui(call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
    let args: UpdatePlanArgs = parse_tool_args(call)?;
    Ok(ToolCallActivityEvent::plan(PlanActivityDescriptor {
        kind: PlanActivityKind::Proposed,
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
        let summary = if effective_steps.is_empty() {
            if changed {
                "cleared plan after completion".to_string()
            } else {
                "plan already clear".to_string()
            }
        } else if changed {
            format!("updated plan with {} steps", effective_steps.len())
        } else {
            format!("plan unchanged with {} steps", effective_steps.len())
        };
        if let Some(tx) = &context.dashboard_tx {
            let current_plan_step = current_plan_step_for_dashboard(context);
            let status_command = status_command_snapshot_for_dashboard(context);
            tx.send_modify(|state| {
                state.current_plan_step = current_plan_step.clone();
                state.status_command = status_command.clone();
            });
        }
        let plan_ui_steps = plan_ui_steps(&context.plan);
        let plan_event = PlanActivityDescriptor {
            kind: PlanActivityKind::Updated,
            explanation: explanation.clone(),
            steps: plan_ui_steps,
        };
        Ok(ToolExecutionResult::from_activity_event(
            summary.clone(),
            json!({
                "explanation": explanation,
                "plan": effective_steps,
            }),
            Some(SessionActivityEvent::PlanResult(plan_event.into())),
        ))
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

fn plan_ui_steps(plan: &Plan) -> Vec<PlanStepActivityDescriptor> {
    plan.steps()
        .iter()
        .take(8)
        .map(|step| PlanStepActivityDescriptor {
            status: match step.status {
                PlanStatus::Pending => PlanStepActivityStatus::Pending,
                PlanStatus::InProgress => PlanStepActivityStatus::InProgress,
                PlanStatus::Completed => PlanStepActivityStatus::Completed,
            },
            text: summarize_inline_text(&step.step),
        })
        .collect()
}

fn plan_ui_step_from_args(step: crate::core::UpdatePlanStepArgs) -> PlanStepActivityDescriptor {
    PlanStepActivityDescriptor {
        status: match step.status {
            PlanStatus::Pending => PlanStepActivityStatus::Pending,
            PlanStatus::InProgress => PlanStepActivityStatus::InProgress,
            PlanStatus::Completed => PlanStepActivityStatus::Completed,
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
            ToolCallActivityEvent::App(data) => {
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
            ToolCallActivityEvent::Plan(plan) => {
                assert_eq!(plan.kind, PlanActivityKind::Proposed);
                assert_eq!(
                    plan.explanation.as_deref(),
                    Some("Need a short setup sequence.")
                );
                assert_eq!(plan.steps.len(), 2);
                assert_eq!(plan.steps[0].status, PlanStepActivityStatus::InProgress);
            }
            other => panic!("expected plan call ui, got {other:?}"),
        }
    }
}
