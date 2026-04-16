use miette::Result;
use serde_json::json;

use crate::{
    apply_patch::{PatchOperationKind, parse_apply_patch, summarize_patch_ops},
    context::Context,
    core::{
        ActivateSkillArgs, CreateSkillArgs, DeepRecallArgs, EventResolveArgs, FocusAppArgs,
        LogSkillOutcomeArgs, PutAwayAppArgs, SelectSkillArgs, UpdatePlanArgs,
    },
    events::{EventDisposition, EventPayload, EventStatus},
    hindsight::HindsightReflectOptions,
    plan::{Plan, PlanStatus, PlanStep},
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    skill::{NewSkillRecord, SkillOutcomeLog},
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
        Box::new(StaticRuntimeTool::new::<SelectSkillArgs>(
            "select_skill",
            "按任务语义检索可复用 skill，并返回排序后的匹配结果。",
            None,
            summarize_select_skill_tool,
            render_select_skill_call_ui,
            execute_select_skill_tool,
        )),
        Box::new(StaticRuntimeTool::new::<CreateSkillArgs>(
            "create_skill",
            "当没有可复用 skill 时创建一个新 skill 草稿。",
            None,
            summarize_create_skill_tool,
            render_create_skill_call_ui,
            execute_create_skill_tool,
        )),
        Box::new(StaticRuntimeTool::new::<ActivateSkillArgs>(
            "activate_skill",
            "将一个 skill 绑定到当前任务，供后续多步骤执行复用。",
            None,
            summarize_activate_skill_tool,
            render_activate_skill_call_ui,
            execute_activate_skill_tool,
        )),
        Box::new(StaticRuntimeTool::new::<LogSkillOutcomeArgs>(
            "log_skill_outcome",
            "在任务后记录执行结果，更新 skill 指标与版本。",
            None,
            summarize_log_skill_outcome_tool,
            render_log_skill_outcome_call_ui,
            execute_log_skill_outcome_tool,
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
        let required_reply_message = if disposition_requires_reply(args.disposition) {
            Some(reply_message.clone().ok_or_else(|| {
                miette::miette!(
                    "{} event {} requires a non-empty reply_message",
                    event_disposition_kind(args.disposition),
                    args.event_id,
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
                execute_event_resolve_with_reply(
                    context,
                    &args.event_id,
                    &event,
                    args.disposition,
                    reply_message.clone(),
                    args.note.clone(),
                )?;
                format!(
                    "{} event {} via channel delivery",
                    event_disposition_kind(args.disposition),
                    args.event_id
                )
            }
            EventDisposition::Dismissed => {
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

fn summarize_update_plan_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: UpdatePlanArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "update_plan".to_string(),
        summary: format!("steps={}", args.plan.len()),
    })
}

fn render_update_plan_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: UpdatePlanArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
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
        if requires_skill_binding(&plan) && context.active_skill_id.is_none() {
            return Err(miette::miette!(
                "multi-step plan requires active skill; call select_skill/create_skill and activate_skill before update_plan"
            ));
        }
        let changed = context.plan.replace(plan.steps().to_vec());
        let effective_steps = context.plan.steps();
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
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "plan": effective_steps,
            }),
            ToolUiEvent::work(summary, render_plan_ui_lines(&context.plan)),
        ))
    })
}

fn summarize_select_skill_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: SelectSkillArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "select_skill".to_string(),
        summary: summarize_inline_text(&args.query),
    })
}

fn render_select_skill_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: SelectSkillArgs = parse_tool_args(call)?;
    let mut lines = vec![format!("query={}", summarize_inline_text(&args.query))];
    if let Some(limit) = args.limit {
        lines.push(format!("limit={limit}"));
    }
    Ok(ToolCallUiEvent::work("select_skill", lines))
}

fn execute_select_skill_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: SelectSkillArgs = parse_tool_args(call)?;
        let query = require_field(args.query, "query")?;
        let limit = args.limit.unwrap_or(5).clamp(1, 12);
        let mut matches = context.skills.query(&query, limit);
        let mut auto_created = None;
        if matches.is_empty() {
            let created = context.skills.create_skill(NewSkillRecord {
                name: query.clone(),
                trigger_conditions: vec![query.clone()],
                preconditions: vec!["明确任务范围与输入约束".to_string()],
                workflow_steps: vec![
                    "分析任务目标与边界".to_string(),
                    "执行核心步骤并记录证据".to_string(),
                    "验证结果并沉淀经验".to_string(),
                ],
                done_criteria: vec!["结果可验证并可复用".to_string()],
                failure_recovery: vec!["失败时回退到上一个稳定步骤".to_string()],
            })?;
            context.skills.persist_if_dirty().await?;
            auto_created = Some(created.clone());
            matches = context.skills.query(&query, limit);
        }

        let summary = if matches.is_empty() {
            format!(
                "no skill matched query and auto-create failed: {}",
                summarize_inline_text(&query)
            )
        } else if let Some(created) = auto_created.as_ref() {
            format!(
                "no match found; auto-created skill {} ({})",
                created.id,
                summarize_inline_text(&created.name)
            )
        } else {
            format!("found {} skill matches", matches.len())
        };
        let ui_lines = if matches.is_empty() {
            vec![summary.clone()]
        } else {
            matches
                .iter()
                .take(6)
                .map(|matched| {
                    format!(
                        "score={:.2} {}",
                        matched.score,
                        format_skill_summary_line(
                            &matched.summary.id,
                            &matched.summary.name,
                            matched.summary.version,
                            matched.summary.success_rate,
                        )
                    )
                })
                .collect()
        };

        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "query": query,
                "limit": limit,
                "matches": matches,
                "auto_created": auto_created,
                "active_skill_id": context.active_skill_id,
            }),
            ToolUiEvent::work(summary, ui_lines),
        ))
    })
}

fn summarize_create_skill_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: CreateSkillArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "create_skill".to_string(),
        summary: summarize_inline_text(&args.name),
    })
}

fn render_create_skill_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: CreateSkillArgs = parse_tool_args(call)?;
    let mut lines = vec![format!("name={}", summarize_inline_text(&args.name))];
    lines.push(format!("workflow_steps={}", args.workflow_steps.len()));
    Ok(ToolCallUiEvent::work("create_skill", lines))
}

fn execute_create_skill_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: CreateSkillArgs = parse_tool_args(call)?;
        let created = context.skills.create_skill(NewSkillRecord {
            name: args.name,
            trigger_conditions: args.trigger_conditions,
            preconditions: args.preconditions,
            workflow_steps: args.workflow_steps,
            done_criteria: args.done_criteria,
            failure_recovery: args.failure_recovery,
        })?;
        context.skills.persist_if_dirty().await?;

        let summary = format!("created skill {} ({})", created.id, created.name);
        let ui_lines = vec![
            format!("id={}", created.id),
            format!("name={}", created.name),
            format!("version={}", created.version),
        ];

        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "created": created,
                "active_skill_id": context.active_skill_id,
            }),
            ToolUiEvent::work(summary, ui_lines),
        ))
    })
}

fn summarize_activate_skill_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ActivateSkillArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "activate_skill".to_string(),
        summary: format!("skill_id={}", args.skill_id),
    })
}

fn render_activate_skill_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ActivateSkillArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::work(
        "activate_skill",
        vec![format!("skill_id={}", args.skill_id)],
    ))
}

fn execute_activate_skill_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ActivateSkillArgs = parse_tool_args(call)?;
        let skill_id = require_field(args.skill_id, "skill_id")?;
        let activated = context.skills.activate_skill(&skill_id)?;
        context.active_skill_id = Some(activated.id.clone());
        context.skills.persist_if_dirty().await?;

        let summary = format!(
            "activated skill {} ({})",
            activated.id,
            summarize_inline_text(&activated.name)
        );
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "active_skill_id": context.active_skill_id,
                "activated": activated,
            }),
            ToolUiEvent::work(summary, vec![format!("active_skill_id={}", skill_id)]),
        ))
    })
}

fn summarize_log_skill_outcome_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: LogSkillOutcomeArgs = parse_tool_args(call)?;
    let label = args
        .skill_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("<active_skill>");
    Ok(EpisodeActionRecord {
        kind: "log_skill_outcome".to_string(),
        summary: format!("skill_id={label} success={}", args.success),
    })
}

fn render_log_skill_outcome_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: LogSkillOutcomeArgs = parse_tool_args(call)?;
    let mut lines = vec![format!("success={}", args.success)];
    if let Some(skill_id) = args.skill_id.as_deref()
        && !skill_id.trim().is_empty()
    {
        lines.push(format!("skill_id={skill_id}"));
    } else {
        lines.push("skill_id=<active_skill>".to_string());
    }
    if let Some(steps_executed) = args.steps_executed {
        lines.push(format!("steps_executed={steps_executed}"));
    }
    Ok(ToolCallUiEvent::work("log_skill_outcome", lines))
}

fn execute_log_skill_outcome_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: LogSkillOutcomeArgs = parse_tool_args(call)?;
        let skill_id = args
            .skill_id
            .as_deref()
            .and_then(|value| trim_required_field(value.to_string()))
            .or_else(|| context.active_skill_id.clone())
            .ok_or_else(|| {
                miette::miette!("log_skill_outcome requires skill_id when no active skill is bound")
            })?;

        let updated = context.skills.log_outcome(
            &skill_id,
            SkillOutcomeLog {
                success: args.success,
                steps_executed: args.steps_executed,
                regression: args.regression.unwrap_or(false),
                summary: trim_optional_field(args.summary),
                failure_type: trim_optional_field(args.failure_type),
            },
        )?;
        context.skills.persist_if_dirty().await?;

        let success_rate = updated
            .quality_metrics
            .success_rate()
            .map(|rate| format!("{:.2}%", rate * 100.0))
            .unwrap_or_else(|| "n/a".to_string());
        let summary = format!(
            "logged skill outcome for {} (runs={}, success_rate={})",
            updated.id, updated.quality_metrics.total_runs, success_rate
        );
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "skill_id": skill_id,
                "updated": updated,
                "active_skill_id": context.active_skill_id,
            }),
            ToolUiEvent::work(summary, vec![format!("skill_id={skill_id}")]),
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
            body_lines.push(format!(
                "sources: memories={} mental_models={} directives={}",
                based_on.memories.len(),
                based_on.mental_models.len(),
                based_on.directives.len()
            ));
            body_lines.extend(based_on.mental_models.iter().take(3).map(|model| {
                format!(
                    "mental_model: {} ({})",
                    model.id,
                    summarize_inline_text(&model.text)
                )
            }));
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
            body_lines.extend(
                based_on
                    .directives
                    .iter()
                    .take(3)
                    .map(|directive| format!("directive: {} ({})", directive.id, directive.name)),
            );
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
            ToolUiEvent::work(title, body_lines),
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

fn requires_skill_binding(plan: &Plan) -> bool {
    let active_steps = plan
        .steps()
        .iter()
        .filter(|step| !matches!(step.status, PlanStatus::Completed))
        .count();
    active_steps > 1
}

fn format_skill_summary_line(
    id: &str,
    name: &str,
    version: u64,
    success_rate: Option<f64>,
) -> String {
    let success_rate = success_rate
        .map(|rate| format!("{:.1}%", rate * 100.0))
        .unwrap_or_else(|| "n/a".to_string());
    format!(
        "{id} v{version} ({success_rate}) - {}",
        summarize_inline_text(name)
    )
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
