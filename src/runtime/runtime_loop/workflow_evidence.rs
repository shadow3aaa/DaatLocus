use super::*;

pub(crate) struct AgentLoopStepExecution {
    pub(crate) output: AgentLoopStepOutput,
    pub(crate) history_messages: Vec<HistoryMessage>,
}

pub(crate) struct AgentLoopStepOutput {
    pub(crate) observation: String,
    pub(crate) description: String,
    pub(crate) current_doing: String,
    pub(crate) actions: Vec<EpisodeActionRecord>,
}

pub(super) async fn record_runtime_history_messages(
    context: &mut Context,
    draft: RuntimeTurnDraft,
) {
    let retain_plan = context.memory.commit_runtime_turn(draft).await;
    for job in retain_plan.jobs {
        if let Err(err) = context.hindsight_retain.enqueue(job) {
            tracing::error!("failed to enqueue hindsight retain job: {err:?}");
            return;
        }
    }
    if retain_plan.must_flush_before_continue {
        match context.hindsight_retain.flush().await {
            Ok(submitted_handoffs) => {
                context
                    .memory
                    .mark_handoffs_submitted(&submitted_handoffs)
                    .await;
            }
            Err(err) => {
                tracing::error!("failed to flush hindsight handoff queue: {err:?}");
            }
        }
    }
}

fn detect_runtime_rollback(output: &AgentLoopStepOutput) -> bool {
    let text = format!(
        "{}\n{}\n{}",
        output.description,
        output.observation,
        output
            .actions
            .iter()
            .map(|action| action.summary.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    )
    .to_ascii_lowercase();
    text.contains("rollback") || text.contains("revert")
}

fn detect_runtime_manual_fix(output: &AgentLoopStepOutput) -> bool {
    output.actions.iter().any(|action| {
        matches!(
            action.kind.as_str(),
            "apply_patch" | "terminal_exec" | "terminal_write_stdin"
        )
    })
}

fn classify_runtime_failure_type(output: &AgentLoopStepOutput) -> Option<String> {
    let text = format!("{}\n{}", output.description, output.observation).to_ascii_lowercase();
    if text.contains("timeout") {
        return Some("timeout".to_string());
    }
    if text.contains("schema") || text.contains("deserialize") || text.contains("json") {
        return Some("schema_drift".to_string());
    }
    if text.contains("permission") || text.contains("forbidden") || text.contains("denied") {
        return Some("permission".to_string());
    }
    if text.contains("tool") && text.contains("failed") {
        return Some("tool_failure".to_string());
    }
    if text.contains("error") || text.contains("failed") || text.contains("failure") {
        return Some("runtime_error".to_string());
    }
    None
}

fn workflow_tool_action_count(output: &AgentLoopStepOutput) -> usize {
    output
        .actions
        .iter()
        .filter(|action| {
            !matches!(
                action.kind.as_str(),
                "assistant_message" | "empty_tool_calls"
            )
        })
        .count()
}

fn workflow_run_summary(output: &AgentLoopStepOutput) -> String {
    format!(
        "{} | {} | {}",
        output.current_doing.trim(),
        output.description.trim(),
        output.observation.trim()
    )
}

fn accumulate_workflow_session_from_output(
    session: &mut ActiveWorkflowRunSession,
    output: &AgentLoopStepOutput,
) {
    session.turn_count = session.turn_count.saturating_add(1);
    session.tool_action_count = session
        .tool_action_count
        .saturating_add(workflow_tool_action_count(output));
    session.manual_fix_detected |= detect_runtime_manual_fix(output);
    session.rollback_detected |= detect_runtime_rollback(output);
    if let Some(failure_type) = classify_runtime_failure_type(output) {
        session.failure_types.insert(failure_type);
    }
    session.final_summary = workflow_run_summary(output);
}

fn workflow_run_record_from_pending_flush(
    flush: PendingWorkflowRunFlush,
    ended_at_ms: i64,
) -> WorkflowRunRecord {
    WorkflowRunRecord {
        run_id: flush.session.run_id,
        workflow_id: flush.session.workflow_id,
        started_at_ms: flush.session.started_at_ms,
        ended_at_ms,
        origin: flush.session.origin,
        outcome: flush.outcome,
        turn_count: flush.session.turn_count,
        tool_action_count: flush.session.tool_action_count,
        manual_fix_detected: flush.session.manual_fix_detected,
        rollback_detected: flush.session.rollback_detected,
        failure_types: flush.session.failure_types.into_iter().collect(),
        final_summary: flush.session.final_summary,
    }
}

pub(super) async fn record_workflow_run_evidence(
    context: &mut Context,
    output: &AgentLoopStepOutput,
) {
    let target_workflow_id = context
        .workflow_step_started_bound_id
        .clone()
        .or_else(|| context.bound_workflow_id.clone())
        .or_else(|| {
            context
                .pending_workflow_run_flushes
                .last()
                .map(|flush| flush.session.workflow_id.clone())
        });

    if let Some(workflow_id) = target_workflow_id {
        let mut matched_pending = false;
        for flush in context.pending_workflow_run_flushes.iter_mut().rev() {
            if flush.session.workflow_id == workflow_id {
                accumulate_workflow_session_from_output(&mut flush.session, output);
                matched_pending = true;
                break;
            }
        }
        if !matched_pending
            && let Some(session) = context.active_workflow_run.as_mut()
            && session.workflow_id == workflow_id
        {
            accumulate_workflow_session_from_output(session, output);
        }
    }

    if context.pending_workflow_run_flushes.is_empty() {
        return;
    }

    let ended_at_ms = Utc::now().timestamp_millis();
    let records = context
        .pending_workflow_run_flushes
        .drain(..)
        .map(|flush| workflow_run_record_from_pending_flush(flush, ended_at_ms))
        .collect::<Vec<_>>();
    if let Err(err) = append_workflow_run_records(&records).await {
        tracing::error!("failed to append workflow run records at runtime boundary: {err:?}");
    }
}
