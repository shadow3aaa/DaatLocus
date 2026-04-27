use super::*;

pub(super) async fn run_runtime_error_correction_pipeline(
    context: &mut Context,
    planner: &dyn SleepPlannerRuntime,
    store: &EvaluationArtifactsStore,
    runtime_error_cases: &[RuntimeErrorCase],
    consumed_error_cases: usize,
) -> Result<RuntimeErrorCorrectionSummary> {
    if runtime_error_cases.is_empty() {
        tracing::info!(
            "[sleep] no runtime error cases, skipping runtime error correction planning"
        );
        store
            .replace_runtime_error_correction_artifacts(RuntimeErrorCorrectionArtifacts {
                runtime_error_cases,
                prompt_reflections: &[],
                runtime_prompt_candidates: &[],
                runtime_prompt_candidate_evaluations: &[],
            })
            .await?;
        return Ok(RuntimeErrorCorrectionSummary {
            consumed_error_cases,
            ..Default::default()
        });
    }

    // Planning failures should not prevent workflow optimization from running.
    // If the correction planner cannot produce structured output, keep the
    // cases visible as artifacts and defer correction to a later sleep cycle.
    let PromptPlanningResult {
        reflections,
        candidates,
        mut evaluations,
    } = match planner
        .plan_runtime_error_correction(context, runtime_error_cases)
        .await
    {
        Ok(result) => result,
        Err(err) => {
            warn!("runtime error correction planning failed, using empty plan: {err:?}");
            PromptPlanningResult::default()
        }
    };

    let prompt_update =
        apply_selected_prompt_candidate(context, &candidates, &mut evaluations).await?;

    store
        .replace_runtime_error_correction_artifacts(RuntimeErrorCorrectionArtifacts {
            runtime_error_cases,
            prompt_reflections: &reflections,
            runtime_prompt_candidates: &candidates,
            runtime_prompt_candidate_evaluations: &evaluations,
        })
        .await?;

    Ok(RuntimeErrorCorrectionSummary {
        consumed_error_cases,
        runtime_error_cases: runtime_error_cases.len(),
        reflections: reflections.len(),
        candidates: candidates.len(),
        candidate_evaluations: evaluations.len(),
        applied_system_additions: prompt_update.applied_system_additions,
        compiled_runtime_contract_updated: prompt_update.compiled_prompt_updated,
    })
}
