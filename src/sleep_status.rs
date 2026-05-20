use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};

use crate::{
    daat_locus_paths::daat_locus_paths,
    persistence::{PersistenceFileMode, read_json_or_default, write_bytes_atomic},
    reasoning::{runtime_error::unread_runtime_error_case_count, sleep::SleepSummary},
    workflow::primitive_run_record_count,
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
    pub unread_runtime_error_backlog: usize,
    pub primitive_evidence_records: usize,
    pub total_runs: usize,
    pub total_runtime_error_cases_consumed: usize,
    pub total_runtime_error_cases: usize,
    pub total_runtime_error_reflections: usize,
    pub total_runtime_contract_candidates: usize,
    pub total_runtime_contract_candidate_evaluations: usize,
    pub total_runtime_contract_system_additions: usize,
    pub total_runtime_contract_updates: usize,
    pub total_primitive_evidence_run_records: usize,
    pub total_primitive_reflections: usize,
    pub total_primitive_patch_candidates: usize,
    pub total_primitive_merge_candidates: usize,
    pub total_primitive_candidate_evaluations: usize,
    pub total_primitive_frontier_entries: usize,
    pub latest_primitive_frontier_root_entries: usize,
    pub latest_primitive_frontier_branched_entries: usize,
    pub latest_primitive_frontier_max_generation: usize,
    pub total_primitive_patch_applied: usize,
    pub total_primitive_merge_applied: usize,
    pub total_primitive_update_rollbacks: usize,
    pub total_primitive_optimization_rounds: usize,
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
        let correction = &summary.runtime_error_correction;
        let workflow = &summary.workflow_improvement;
        self.total_runs += 1;
        self.total_runtime_error_cases_consumed += correction.consumed_error_cases;
        self.total_runtime_error_cases += correction.runtime_error_cases;
        self.total_runtime_error_reflections += correction.reflections;
        self.total_runtime_contract_candidates += correction.candidates;
        self.total_runtime_contract_candidate_evaluations += correction.candidate_evaluations;
        self.total_runtime_contract_system_additions += correction.applied_system_additions;
        self.total_runtime_contract_updates +=
            usize::from(correction.compiled_runtime_contract_updated);
        self.total_primitive_evidence_run_records += workflow.evidence_run_records;
        self.total_primitive_reflections += workflow.workflow_reflections;
        self.total_primitive_patch_candidates += workflow.patch_candidates;
        self.total_primitive_merge_candidates += workflow.merge_candidates;
        self.total_primitive_candidate_evaluations += workflow.candidate_evaluations;
        self.total_primitive_frontier_entries += workflow.frontier_entries;
        self.latest_primitive_frontier_root_entries = workflow.frontier_root_entries;
        self.latest_primitive_frontier_branched_entries = workflow.frontier_branched_entries;
        self.latest_primitive_frontier_max_generation = workflow.frontier_max_generation;
        self.total_primitive_patch_applied += workflow.patch_applied;
        self.total_primitive_merge_applied += workflow.merge_applied;
        self.total_primitive_update_rollbacks += workflow.update_rollbacks;
        self.total_primitive_optimization_rounds += workflow.optimization_rounds;
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedSleepStatusSnapshot {
    last_result: Option<String>,
    last_started_at_ms: Option<i64>,
    last_completed_at_ms: Option<i64>,
    total_runs: usize,
    total_runtime_error_cases_consumed: usize,
    total_runtime_error_cases: usize,
    total_runtime_error_reflections: usize,
    total_runtime_contract_candidates: usize,
    total_runtime_contract_candidate_evaluations: usize,
    total_runtime_contract_system_additions: usize,
    total_runtime_contract_updates: usize,
    total_primitive_evidence_run_records: usize,
    total_primitive_reflections: usize,
    total_primitive_patch_candidates: usize,
    total_primitive_merge_candidates: usize,
    total_primitive_candidate_evaluations: usize,
    total_primitive_frontier_entries: usize,
    latest_primitive_frontier_root_entries: usize,
    latest_primitive_frontier_branched_entries: usize,
    latest_primitive_frontier_max_generation: usize,
    total_primitive_patch_applied: usize,
    total_primitive_merge_applied: usize,
    total_primitive_update_rollbacks: usize,
    total_primitive_optimization_rounds: usize,
}

impl From<PersistedSleepStatusSnapshot> for SleepStatusSnapshot {
    fn from(value: PersistedSleepStatusSnapshot) -> Self {
        Self {
            running: false,
            current_trigger: None,
            last_result: value.last_result,
            last_started_at_ms: value.last_started_at_ms,
            last_completed_at_ms: value.last_completed_at_ms,
            unread_runtime_error_backlog: 0,
            primitive_evidence_records: 0,
            total_runs: value.total_runs,
            total_runtime_error_cases_consumed: value.total_runtime_error_cases_consumed,
            total_runtime_error_cases: value.total_runtime_error_cases,
            total_runtime_error_reflections: value.total_runtime_error_reflections,
            total_runtime_contract_candidates: value.total_runtime_contract_candidates,
            total_runtime_contract_candidate_evaluations: value
                .total_runtime_contract_candidate_evaluations,
            total_runtime_contract_system_additions: value.total_runtime_contract_system_additions,
            total_runtime_contract_updates: value.total_runtime_contract_updates,
            total_primitive_evidence_run_records: value.total_primitive_evidence_run_records,
            total_primitive_reflections: value.total_primitive_reflections,
            total_primitive_patch_candidates: value.total_primitive_patch_candidates,
            total_primitive_merge_candidates: value.total_primitive_merge_candidates,
            total_primitive_candidate_evaluations: value.total_primitive_candidate_evaluations,
            total_primitive_frontier_entries: value.total_primitive_frontier_entries,
            latest_primitive_frontier_root_entries: value.latest_primitive_frontier_root_entries,
            latest_primitive_frontier_branched_entries: value
                .latest_primitive_frontier_branched_entries,
            latest_primitive_frontier_max_generation: value
                .latest_primitive_frontier_max_generation,
            total_primitive_patch_applied: value.total_primitive_patch_applied,
            total_primitive_merge_applied: value.total_primitive_merge_applied,
            total_primitive_update_rollbacks: value.total_primitive_update_rollbacks,
            total_primitive_optimization_rounds: value.total_primitive_optimization_rounds,
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
            total_runtime_error_cases_consumed: value.total_runtime_error_cases_consumed,
            total_runtime_error_cases: value.total_runtime_error_cases,
            total_runtime_error_reflections: value.total_runtime_error_reflections,
            total_runtime_contract_candidates: value.total_runtime_contract_candidates,
            total_runtime_contract_candidate_evaluations: value
                .total_runtime_contract_candidate_evaluations,
            total_runtime_contract_system_additions: value.total_runtime_contract_system_additions,
            total_runtime_contract_updates: value.total_runtime_contract_updates,
            total_primitive_evidence_run_records: value.total_primitive_evidence_run_records,
            total_primitive_reflections: value.total_primitive_reflections,
            total_primitive_patch_candidates: value.total_primitive_patch_candidates,
            total_primitive_merge_candidates: value.total_primitive_merge_candidates,
            total_primitive_candidate_evaluations: value.total_primitive_candidate_evaluations,
            total_primitive_frontier_entries: value.total_primitive_frontier_entries,
            latest_primitive_frontier_root_entries: value.latest_primitive_frontier_root_entries,
            latest_primitive_frontier_branched_entries: value
                .latest_primitive_frontier_branched_entries,
            latest_primitive_frontier_max_generation: value
                .latest_primitive_frontier_max_generation,
            total_primitive_patch_applied: value.total_primitive_patch_applied,
            total_primitive_merge_applied: value.total_primitive_merge_applied,
            total_primitive_update_rollbacks: value.total_primitive_update_rollbacks,
            total_primitive_optimization_rounds: value.total_primitive_optimization_rounds,
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
    if let Ok(backlog) = unread_runtime_error_case_count().await {
        status.unread_runtime_error_backlog = backlog;
    }
    if let Ok(records) = primitive_run_record_count().await {
        status.primitive_evidence_records = records;
    }
}

#[cfg(test)]
mod tests {
    use crate::reasoning::sleep::{RuntimeErrorCorrectionSummary, WorkflowImprovementSummary};

    use super::*;

    #[test]
    fn apply_summary_accumulates_totals_and_tracks_latest_frontiers() {
        let mut status = SleepStatusSnapshot::default();
        let summary = SleepSummary {
            runtime_error_correction: RuntimeErrorCorrectionSummary {
                consumed_error_cases: 3,
                runtime_error_cases: 2,
                reflections: 2,
                candidates: 1,
                candidate_evaluations: 1,
                applied_system_additions: 1,
                compiled_runtime_contract_updated: true,
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
        assert_eq!(status.total_runtime_error_cases_consumed, 6);
        assert_eq!(status.total_runtime_error_cases, 4);
        assert_eq!(status.total_runtime_error_reflections, 4);
        assert_eq!(status.total_runtime_contract_candidates, 2);
        assert_eq!(status.total_runtime_contract_candidate_evaluations, 2);
        assert_eq!(status.total_runtime_contract_system_additions, 2);
        assert_eq!(status.total_runtime_contract_updates, 2);
        assert_eq!(status.total_primitive_evidence_run_records, 14);
        assert_eq!(status.total_primitive_patch_applied, 2);
        assert_eq!(status.latest_primitive_frontier_root_entries, 8);
        assert_eq!(status.latest_primitive_frontier_branched_entries, 9);
        assert_eq!(status.latest_primitive_frontier_max_generation, 10);
    }
}
