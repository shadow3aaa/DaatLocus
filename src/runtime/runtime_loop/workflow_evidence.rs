use std::path::Path;

use chrono::Utc;

use super::*;
use crate::{
    context::{ActiveSkillRunSession, PendingSkillRunFlush, SkillRunOutcome},
    skill_run_records::{SkillRunRecord, append_skill_run_records},
};

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

/// Called after each tool execution: if the call read a known SKILL.md,
/// begin or continue a skill run session for that skill.
pub(super) fn maybe_record_skill_read(context: &mut Context, call: &AgentToolCall) {
    let path = skill_read_path(call);
    let Some(path) = path else { return };
    let Some(skill) = context.openskills.skill_for_path(path) else {
        return;
    };
    let skill_name = skill.name.clone();
    let origin = context
        .current_work_origin
        .clone()
        .unwrap_or_else(|| "runtime_work".to_string());
    context.begin_skill_run_session(skill_name, origin);
}

/// Extract the file path being read from a read_file or coding__read_code call.
fn skill_read_path(call: &AgentToolCall) -> Option<&Path> {
    let is_read_tool = matches!(call.name.as_str(), "read_file" | "coding__read_code")
        || call.name.ends_with("__read_code");

    if !is_read_tool {
        return None;
    }

    let path_str = call.arguments.get("path").and_then(|v| v.as_str())?;
    let path = Path::new(path_str);

    // Only track reads of SKILL.md files
    if path.file_name().and_then(|f| f.to_str()) == Some("SKILL.md") {
        Some(path)
    } else {
        None
    }
}

/// Called at end of each turn to accumulate skill run evidence.
pub(super) async fn record_skill_run_evidence(context: &mut Context, output: &AgentLoopStepOutput) {
    // Accumulate into active skill session if one is running
    if let Some(session) = context.active_skill_run.as_mut() {
        accumulate_skill_session_from_output(session, output);
    }

    // Accumulate into pending flushes too (for already-completed sessions)
    for flush in context.pending_skill_run_flushes.iter_mut() {
        accumulate_skill_session_from_output(&mut flush.session, output);
    }

    // Flush any pending skill run records to disk
    flush_pending_skill_run_records(context).await;
}

fn accumulate_skill_session_from_output(
    session: &mut ActiveSkillRunSession,
    output: &AgentLoopStepOutput,
) {
    session.turn_count = session.turn_count.saturating_add(1);
    session.tool_action_count = session
        .tool_action_count
        .saturating_add(skill_tool_action_count(output));
    session.manual_fix_detected |= detect_manual_fix(output);
    session.rollback_detected |= detect_rollback(output);
    if let Some(failure_type) = classify_failure_type(output) {
        session.failure_types.insert(failure_type);
    }
    session.final_summary = skill_run_summary(output);
}

async fn flush_pending_skill_run_records(context: &mut Context) {
    if context.pending_skill_run_flushes.is_empty() {
        return;
    }
    let ended_at_ms = Utc::now().timestamp_millis();
    let records: Vec<SkillRunRecord> = context
        .pending_skill_run_flushes
        .drain(..)
        .map(|flush| skill_run_record_from_flush(flush, ended_at_ms))
        .collect();
    if let Err(err) = append_skill_run_records(&records).await {
        tracing::error!("failed to append skill run records: {err:?}");
    }
}

fn skill_run_record_from_flush(flush: PendingSkillRunFlush, ended_at_ms: i64) -> SkillRunRecord {
    SkillRunRecord {
        run_id: flush.session.run_id,
        skill_name: flush.session.skill_name,
        started_at_ms: flush.session.started_at_ms,
        ended_at_ms,
        origin: flush.session.origin,
        outcome: match flush.outcome {
            SkillRunOutcome::Completed => "completed",
            SkillRunOutcome::Blocked => "blocked",
            SkillRunOutcome::Abandoned => "abandoned",
            SkillRunOutcome::Superseded => "superseded",
            SkillRunOutcome::NoProgress => "no_progress",
        }
        .to_string(),
        turn_count: flush.session.turn_count,
        tool_action_count: flush.session.tool_action_count,
        manual_fix_detected: flush.session.manual_fix_detected,
        rollback_detected: flush.session.rollback_detected,
        failure_types: flush.session.failure_types.into_iter().collect(),
        final_summary: flush.session.final_summary,
    }
}

fn skill_tool_action_count(output: &AgentLoopStepOutput) -> usize {
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

fn detect_rollback(output: &AgentLoopStepOutput) -> bool {
    let text = format!(
        "{}\n{}\n{}",
        output.description,
        output.observation,
        output
            .actions
            .iter()
            .map(|a| a.summary.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    )
    .to_ascii_lowercase();
    text.contains("rollback") || text.contains("revert")
}

fn detect_manual_fix(output: &AgentLoopStepOutput) -> bool {
    output.actions.iter().any(|action| {
        matches!(
            action.kind.as_str(),
            "edit_file" | "terminal_exec" | "terminal_write_stdin"
        )
    })
}

fn classify_failure_type(output: &AgentLoopStepOutput) -> Option<String> {
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

fn skill_run_summary(output: &AgentLoopStepOutput) -> String {
    format!(
        "{} | {} | {}",
        output.current_doing.trim(),
        output.description.trim(),
        output.observation.trim()
    )
}
