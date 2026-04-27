use super::*;

pub(super) async fn run_workflow_improvement_pipeline(
    context: &mut Context,
    planner: &dyn SleepPlannerRuntime,
    store: &EvaluationArtifactsStore,
) -> Result<WorkflowImprovementSummary> {
    let run_batch = load_workflow_run_batch().await?;
    let workflow_optimization =
        optimize_workflows_from_run_records(context, planner, &run_batch.records).await?;
    store
        .replace_workflow_improvement_artifacts(WorkflowImprovementArtifacts {
            workflow_reflections: &workflow_optimization.reflections,
            workflow_patches: &workflow_optimization.patches,
            workflow_merges: &workflow_optimization.merges,
            workflow_candidate_evaluations: &workflow_optimization.candidate_evaluations,
        })
        .await?;
    compact_workflow_run_record_file(run_batch.next_offset).await?;

    Ok(WorkflowImprovementSummary {
        evidence_run_records: run_batch.unread_record_count,
        workflow_reflections: workflow_optimization.reflections.len(),
        patch_candidates: workflow_optimization.patches.len(),
        merge_candidates: workflow_optimization.merges.len(),
        candidate_evaluations: workflow_optimization.candidate_evaluations.len(),
        frontier_entries: workflow_optimization.frontier_entries,
        frontier_root_entries: workflow_optimization.frontier_root_entries,
        frontier_branched_entries: workflow_optimization.frontier_branched_entries,
        frontier_max_generation: workflow_optimization.frontier_max_generation,
        patch_applied: workflow_optimization.patch_applied,
        merge_applied: workflow_optimization.merge_applied,
        update_rollbacks: workflow_optimization.rollbacks,
        optimization_rounds: workflow_optimization.rounds,
    })
}
