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
    context.memory.commit_runtime_turn(draft).await;
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
            "edit_file" | "terminal_exec" | "terminal_write_stdin"
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
                "assistant_message" | "empty_tool_calls" | "runtime_context_compacted"
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
    session: &mut ActivePrimitiveRunSession,
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
    flush: PendingPrimitiveRunFlush,
    ended_at_ms: i64,
) -> PrimitiveRunRecord {
    PrimitiveRunRecord {
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
        .or_else(|| context.bound_primitive_id.clone())
        .or_else(|| {
            context
                .pending_primitive_run_flushes
                .last()
                .map(|flush| flush.session.workflow_id.clone())
        });

    if let Some(workflow_id) = target_workflow_id {
        let mut matched_pending = false;
        for flush in context.pending_primitive_run_flushes.iter_mut().rev() {
            if flush.session.workflow_id == workflow_id {
                accumulate_workflow_session_from_output(&mut flush.session, output);
                matched_pending = true;
                break;
            }
        }
        if !matched_pending
            && let Some(session) = context.active_primitive_run.as_mut()
            && session.workflow_id == workflow_id
        {
            accumulate_workflow_session_from_output(session, output);
        }
    }

    if context.pending_primitive_run_flushes.is_empty() {
        return;
    }

    let ended_at_ms = Utc::now().timestamp_millis();
    let records = context
        .pending_primitive_run_flushes
        .drain(..)
        .map(|flush| workflow_run_record_from_pending_flush(flush, ended_at_ms))
        .collect::<Vec<_>>();
    if let Err(err) = append_primitive_run_records(&records).await {
        tracing::error!("failed to append workflow run records at runtime boundary: {err:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(kind: &str) -> EpisodeActionRecord {
        EpisodeActionRecord {
            kind: kind.to_string(),
            summary: String::new(),
        }
    }

    #[test]
    fn workflow_tool_action_count_ignores_runtime_compaction_boundaries() {
        let output = AgentLoopStepOutput {
            observation: String::new(),
            description: String::new(),
            current_doing: String::new(),
            actions: vec![
                action("runtime_context_compacted"),
                action("assistant_message"),
                action("terminal_exec"),
            ],
        };

        assert_eq!(workflow_tool_action_count(&output), 1);
    }
}
