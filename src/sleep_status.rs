use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};

use crate::{
    daat_locus_paths::daat_locus_paths,
    persistence::{PersistenceFileMode, read_json_or_default, write_bytes_atomic},
    reasoning::{sleep::SleepSummary, trace::unread_runtime_trace_count},
    workflow::workflow_run_record_count,
};

const SLEEP_STATUS_FILE_NAME: &str = "sleep_status.json";

/// Runtime-owned sleep status snapshot.
///
/// Renderers may display this, but they should not derive their own counters.
#[derive(Clone, Debug, Default)]
pub struct SleepStatusSnapshot {
    pub running: bool,
    pub current_trigger: Option<&'static str>,
    pub last_result: Option<String>,
    pub last_started_at_ms: Option<i64>,
    pub last_completed_at_ms: Option<i64>,
    pub unread_trace_backlog: usize,
    pub workflow_evidence_records: usize,
    pub total_runs: usize,
    pub total_prompt_consumed_trace_events: usize,
    pub total_failure_patterns: usize,
    pub total_prompt_reflections: usize,
    pub total_prompt_candidates: usize,
    pub total_prompt_candidate_evaluations: usize,
    pub total_prompt_frontier_entries: usize,
    pub latest_prompt_frontier_root_entries: usize,
    pub latest_prompt_frontier_branched_entries: usize,
    pub latest_prompt_frontier_max_generation: usize,
    pub total_bootstrap_demos: usize,
    pub total_stress_cases: usize,
    pub total_instruction_hypotheses: usize,
    pub total_runtime_demos: usize,
    pub total_turn_demos: usize,
    pub total_prompt_system_additions: usize,
    pub total_compiled_prompt_updates: usize,
    pub total_workflow_evidence_run_records: usize,
    pub total_workflow_reflections: usize,
    pub total_workflow_patch_candidates: usize,
    pub total_workflow_merge_candidates: usize,
    pub total_workflow_candidate_evaluations: usize,
    pub total_workflow_frontier_entries: usize,
    pub latest_workflow_frontier_root_entries: usize,
    pub latest_workflow_frontier_branched_entries: usize,
    pub latest_workflow_frontier_max_generation: usize,
    pub total_workflow_patch_applied: usize,
    pub total_workflow_merge_applied: usize,
    pub total_workflow_update_rollbacks: usize,
    pub total_workflow_optimization_rounds: usize,
}

impl SleepStatusSnapshot {
    pub fn mark_started(&mut self, trigger: &'static str) {
        self.running = true;
        self.current_trigger = Some(trigger);
        self.last_started_at_ms = Some(Utc::now().timestamp_millis());
    }

    pub fn mark_completed(&mut self, result: String) {
        self.running = false;
        self.current_trigger = None;
        self.last_result = Some(result);
        self.last_completed_at_ms = Some(Utc::now().timestamp_millis());
    }

    pub fn apply_summary(&mut self, summary: &SleepSummary) {
        let prompt = &summary.prompt_improvement;
        let workflow = &summary.workflow_improvement;
        self.total_runs += 1;
        self.total_prompt_consumed_trace_events += prompt.consumed_trace_events;
        self.total_failure_patterns += prompt.failure_patterns.len();
        self.total_prompt_reflections += prompt.prompt_reflections;
        self.total_prompt_candidates += prompt.prompt_candidates;
        self.total_prompt_candidate_evaluations += prompt.prompt_candidate_evaluations;
        self.total_prompt_frontier_entries += prompt.prompt_frontier_entries;
        self.latest_prompt_frontier_root_entries = prompt.prompt_frontier_root_entries;
        self.latest_prompt_frontier_branched_entries = prompt.prompt_frontier_branched_entries;
        self.latest_prompt_frontier_max_generation = prompt.prompt_frontier_max_generation;
        self.total_bootstrap_demos += prompt.bootstrap_demos;
        self.total_stress_cases += prompt.stress_cases;
        self.total_instruction_hypotheses += prompt.instruction_hypotheses;
        self.total_runtime_demos += prompt.runtime_demos;
        self.total_turn_demos += prompt.turn_demos;
        self.total_prompt_system_additions += prompt.applied_system_additions;
        self.total_compiled_prompt_updates += usize::from(prompt.compiled_prompt_updated);
        self.total_workflow_evidence_run_records += workflow.evidence_run_records;
        self.total_workflow_reflections += workflow.workflow_reflections;
        self.total_workflow_patch_candidates += workflow.patch_candidates;
        self.total_workflow_merge_candidates += workflow.merge_candidates;
        self.total_workflow_candidate_evaluations += workflow.candidate_evaluations;
        self.total_workflow_frontier_entries += workflow.frontier_entries;
        self.latest_workflow_frontier_root_entries = workflow.frontier_root_entries;
        self.latest_workflow_frontier_branched_entries = workflow.frontier_branched_entries;
        self.latest_workflow_frontier_max_generation = workflow.frontier_max_generation;
        self.total_workflow_patch_applied += workflow.patch_applied;
        self.total_workflow_merge_applied += workflow.merge_applied;
        self.total_workflow_update_rollbacks += workflow.update_rollbacks;
        self.total_workflow_optimization_rounds += workflow.optimization_rounds;
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedSleepStatusSnapshot {
    last_result: Option<String>,
    last_started_at_ms: Option<i64>,
    last_completed_at_ms: Option<i64>,
    total_runs: usize,
    total_prompt_consumed_trace_events: usize,
    total_failure_patterns: usize,
    total_prompt_reflections: usize,
    total_prompt_candidates: usize,
    total_prompt_candidate_evaluations: usize,
    total_prompt_frontier_entries: usize,
    latest_prompt_frontier_root_entries: usize,
    latest_prompt_frontier_branched_entries: usize,
    latest_prompt_frontier_max_generation: usize,
    total_bootstrap_demos: usize,
    total_stress_cases: usize,
    total_instruction_hypotheses: usize,
    total_runtime_demos: usize,
    total_turn_demos: usize,
    total_prompt_system_additions: usize,
    total_compiled_prompt_updates: usize,
    total_workflow_evidence_run_records: usize,
    total_workflow_reflections: usize,
    total_workflow_patch_candidates: usize,
    total_workflow_merge_candidates: usize,
    total_workflow_candidate_evaluations: usize,
    total_workflow_frontier_entries: usize,
    latest_workflow_frontier_root_entries: usize,
    latest_workflow_frontier_branched_entries: usize,
    latest_workflow_frontier_max_generation: usize,
    total_workflow_patch_applied: usize,
    total_workflow_merge_applied: usize,
    total_workflow_update_rollbacks: usize,
    total_workflow_optimization_rounds: usize,
}

impl From<PersistedSleepStatusSnapshot> for SleepStatusSnapshot {
    fn from(value: PersistedSleepStatusSnapshot) -> Self {
        Self {
            running: false,
            current_trigger: None,
            last_result: value.last_result,
            last_started_at_ms: value.last_started_at_ms,
            last_completed_at_ms: value.last_completed_at_ms,
            unread_trace_backlog: 0,
            workflow_evidence_records: 0,
            total_runs: value.total_runs,
            total_prompt_consumed_trace_events: value.total_prompt_consumed_trace_events,
            total_failure_patterns: value.total_failure_patterns,
            total_prompt_reflections: value.total_prompt_reflections,
            total_prompt_candidates: value.total_prompt_candidates,
            total_prompt_candidate_evaluations: value.total_prompt_candidate_evaluations,
            total_prompt_frontier_entries: value.total_prompt_frontier_entries,
            latest_prompt_frontier_root_entries: value.latest_prompt_frontier_root_entries,
            latest_prompt_frontier_branched_entries: value.latest_prompt_frontier_branched_entries,
            latest_prompt_frontier_max_generation: value.latest_prompt_frontier_max_generation,
            total_bootstrap_demos: value.total_bootstrap_demos,
            total_stress_cases: value.total_stress_cases,
            total_instruction_hypotheses: value.total_instruction_hypotheses,
            total_runtime_demos: value.total_runtime_demos,
            total_turn_demos: value.total_turn_demos,
            total_prompt_system_additions: value.total_prompt_system_additions,
            total_compiled_prompt_updates: value.total_compiled_prompt_updates,
            total_workflow_evidence_run_records: value.total_workflow_evidence_run_records,
            total_workflow_reflections: value.total_workflow_reflections,
            total_workflow_patch_candidates: value.total_workflow_patch_candidates,
            total_workflow_merge_candidates: value.total_workflow_merge_candidates,
            total_workflow_candidate_evaluations: value.total_workflow_candidate_evaluations,
            total_workflow_frontier_entries: value.total_workflow_frontier_entries,
            latest_workflow_frontier_root_entries: value.latest_workflow_frontier_root_entries,
            latest_workflow_frontier_branched_entries: value
                .latest_workflow_frontier_branched_entries,
            latest_workflow_frontier_max_generation: value.latest_workflow_frontier_max_generation,
            total_workflow_patch_applied: value.total_workflow_patch_applied,
            total_workflow_merge_applied: value.total_workflow_merge_applied,
            total_workflow_update_rollbacks: value.total_workflow_update_rollbacks,
            total_workflow_optimization_rounds: value.total_workflow_optimization_rounds,
        }
    }
}

impl From<&SleepStatusSnapshot> for PersistedSleepStatusSnapshot {
    fn from(value: &SleepStatusSnapshot) -> Self {
        Self {
            last_result: value.last_result.clone(),
            last_started_at_ms: value.last_started_at_ms,
            last_completed_at_ms: value.last_completed_at_ms,
            total_runs: value.total_runs,
            total_prompt_consumed_trace_events: value.total_prompt_consumed_trace_events,
            total_failure_patterns: value.total_failure_patterns,
            total_prompt_reflections: value.total_prompt_reflections,
            total_prompt_candidates: value.total_prompt_candidates,
            total_prompt_candidate_evaluations: value.total_prompt_candidate_evaluations,
            total_prompt_frontier_entries: value.total_prompt_frontier_entries,
            latest_prompt_frontier_root_entries: value.latest_prompt_frontier_root_entries,
            latest_prompt_frontier_branched_entries: value.latest_prompt_frontier_branched_entries,
            latest_prompt_frontier_max_generation: value.latest_prompt_frontier_max_generation,
            total_bootstrap_demos: value.total_bootstrap_demos,
            total_stress_cases: value.total_stress_cases,
            total_instruction_hypotheses: value.total_instruction_hypotheses,
            total_runtime_demos: value.total_runtime_demos,
            total_turn_demos: value.total_turn_demos,
            total_prompt_system_additions: value.total_prompt_system_additions,
            total_compiled_prompt_updates: value.total_compiled_prompt_updates,
            total_workflow_evidence_run_records: value.total_workflow_evidence_run_records,
            total_workflow_reflections: value.total_workflow_reflections,
            total_workflow_patch_candidates: value.total_workflow_patch_candidates,
            total_workflow_merge_candidates: value.total_workflow_merge_candidates,
            total_workflow_candidate_evaluations: value.total_workflow_candidate_evaluations,
            total_workflow_frontier_entries: value.total_workflow_frontier_entries,
            latest_workflow_frontier_root_entries: value.latest_workflow_frontier_root_entries,
            latest_workflow_frontier_branched_entries: value
                .latest_workflow_frontier_branched_entries,
            latest_workflow_frontier_max_generation: value.latest_workflow_frontier_max_generation,
            total_workflow_patch_applied: value.total_workflow_patch_applied,
            total_workflow_merge_applied: value.total_workflow_merge_applied,
            total_workflow_update_rollbacks: value.total_workflow_update_rollbacks,
            total_workflow_optimization_rounds: value.total_workflow_optimization_rounds,
        }
    }
}

pub async fn load_sleep_status_snapshot() -> SleepStatusSnapshot {
    let paths = daat_locus_paths().await;
    let persisted: PersistedSleepStatusSnapshot =
        read_json_or_default(&paths.state_file(SLEEP_STATUS_FILE_NAME), "sleep status").await;
    let mut status = SleepStatusSnapshot::from(persisted);
    refresh_sleep_status_queues(&mut status).await;
    status
}

pub async fn persist_sleep_status_snapshot(status: &SleepStatusSnapshot) -> Result<()> {
    let paths = daat_locus_paths().await;
    let persisted = PersistedSleepStatusSnapshot::from(status);
    let bytes = serde_json::to_vec_pretty(&persisted).into_diagnostic()?;
    write_bytes_atomic(
        paths.state_file(SLEEP_STATUS_FILE_NAME),
        bytes,
        PersistenceFileMode::Default,
    )
    .await
    .into_diagnostic()
}

pub async fn refresh_sleep_status_queues(status: &mut SleepStatusSnapshot) {
    if let Ok(backlog) = unread_runtime_trace_count().await {
        status.unread_trace_backlog = backlog;
    }
    if let Ok(records) = workflow_run_record_count().await {
        status.workflow_evidence_records = records;
    }
}

#[cfg(test)]
mod tests {
    use crate::reasoning::sleep::{PromptImprovementSummary, WorkflowImprovementSummary};

    use super::*;

    #[test]
    fn apply_summary_accumulates_totals_and_tracks_latest_frontiers() {
        let mut status = SleepStatusSnapshot::default();
        let summary = SleepSummary {
            prompt_improvement: PromptImprovementSummary {
                consumed_trace_events: 3,
                prompt_reflections: 2,
                prompt_frontier_root_entries: 1,
                prompt_frontier_branched_entries: 4,
                prompt_frontier_max_generation: 5,
                compiled_prompt_updated: true,
                ..PromptImprovementSummary::default()
            },
            workflow_improvement: WorkflowImprovementSummary {
                evidence_run_records: 7,
                frontier_root_entries: 8,
                frontier_branched_entries: 9,
                frontier_max_generation: 10,
                patch_applied: 1,
                ..WorkflowImprovementSummary::default()
            },
        };

        status.apply_summary(&summary);
        status.apply_summary(&summary);

        assert_eq!(status.total_runs, 2);
        assert_eq!(status.total_prompt_consumed_trace_events, 6);
        assert_eq!(status.total_prompt_reflections, 4);
        assert_eq!(status.total_compiled_prompt_updates, 2);
        assert_eq!(status.latest_prompt_frontier_root_entries, 1);
        assert_eq!(status.latest_prompt_frontier_branched_entries, 4);
        assert_eq!(status.latest_prompt_frontier_max_generation, 5);
        assert_eq!(status.total_workflow_evidence_run_records, 14);
        assert_eq!(status.total_workflow_patch_applied, 2);
        assert_eq!(status.latest_workflow_frontier_root_entries, 8);
        assert_eq!(status.latest_workflow_frontier_branched_entries, 9);
        assert_eq!(status.latest_workflow_frontier_max_generation, 10);
    }
}
