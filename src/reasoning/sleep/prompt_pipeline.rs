use super::*;

pub(super) async fn run_prompt_improvement_pipeline(
    context: &mut Context,
    planner: &dyn SleepPlannerRuntime,
    store: &EvaluationArtifactsStore,
    records: &[ProgramTraceRecord],
    consumed_trace_events: usize,
) -> Result<PromptImprovementSummary> {
    let failure_patterns = derive_failure_patterns(records);
    if failure_patterns.is_empty() {
        tracing::info!(
            "[sleep] no prompt failure patterns, skipping prompt planning and frontier replay"
        );
        store
            .replace_prompt_improvement_artifacts(PromptImprovementArtifacts {
                failure_patterns: &failure_patterns,
                bootstrap_demos: &[],
                stress_cases: &[],
                instruction_hypotheses: &[],
                runtime_demos: &[],
                turn_demos: &[],
                prompt_reflections: &[],
                runtime_prompt_candidates: &[],
                runtime_prompt_candidate_evaluations: &[],
            })
            .await?;
        return Ok(PromptImprovementSummary {
            consumed_trace_events,
            failure_patterns,
            ..Default::default()
        });
    }
    // LLM 调用失败（如推理模型返回 reasoning text 而非 tool_calls）时，降级为空规划，
    // 不中断整个 pipeline：derive_artifacts、frontier replay 仍能正常执行。
    let PromptPlanningResult {
        reflections: prompt_reflections,
        candidates: prompt_candidates,
        evaluations: prompt_candidate_evaluations,
    } = match planner
        .plan_prompt_improvement(context, &failure_patterns)
        .await
    {
        Ok(result) => result,
        Err(err) => {
            warn!("prompt improvement planning failed, using empty plan: {err:?}");
            PromptPlanningResult::default()
        }
    };

    let mut derived = match derive_evaluation_artifacts(context, &failure_patterns).await {
        Ok(artifacts) => artifacts,
        Err(err) => {
            warn!("derive_evaluation_artifacts failed, using empty artifacts: {err:?}");
            DerivedEvaluationArtifacts {
                bootstrap_demos: Vec::new(),
                stress_cases: Vec::new(),
                instruction_hypotheses: Vec::new(),
                runtime_demos: Vec::new(),
                turn_demos: Vec::new(),
            }
        }
    };
    derived
        .bootstrap_demos
        .extend(derive_success_bootstrap_demos(records));
    let mut prompt_frontier = load_prompt_frontier().await?;
    let prompt_frontier_incoming = prompt_candidates
        .iter()
        .filter_map(|candidate| {
            prompt_candidate_evaluations
                .iter()
                .find(|evaluation| evaluation.candidate_title == candidate.title)
                .map(|evaluation| {
                    let mut entry = prompt_frontier_entry_from_candidate(candidate, evaluation);
                    let (parent_keys, generation) =
                        infer_prompt_lineage(&prompt_frontier, candidate);
                    entry.parent_keys = parent_keys;
                    entry.generation = generation;
                    entry
                })
        })
        .collect::<Vec<_>>();
    prompt_frontier = retain_prompt_frontier(&prompt_frontier, &prompt_frontier_incoming, 16);
    prompt_frontier = match replay_prompt_frontier_entries(
        context,
        planner,
        &prompt_frontier,
        &failure_patterns,
        &derived.turn_demos,
    )
    .await
    {
        Ok(frontier) => frontier,
        Err(err) => {
            warn!("replay_prompt_frontier_entries failed, skipping replay: {err:?}");
            prompt_frontier
        }
    };
    let prompt_frontier_choice = select_prompt_frontier_entry(&prompt_frontier);
    let prompt_update = apply_selected_prompt_frontier_candidate(
        context,
        &mut prompt_frontier,
        prompt_frontier_choice,
    )
    .await?;
    save_prompt_frontier(&prompt_frontier).await?;
    let prompt_frontier_stats = prompt_frontier_lineage_stats(&prompt_frontier);

    store
        .replace_prompt_improvement_artifacts(PromptImprovementArtifacts {
            failure_patterns: &failure_patterns,
            bootstrap_demos: &derived.bootstrap_demos,
            stress_cases: &derived.stress_cases,
            instruction_hypotheses: &derived.instruction_hypotheses,
            runtime_demos: &derived.runtime_demos,
            turn_demos: &derived.turn_demos,
            prompt_reflections: &prompt_reflections,
            runtime_prompt_candidates: &prompt_candidates,
            runtime_prompt_candidate_evaluations: &prompt_candidate_evaluations,
        })
        .await?;

    Ok(PromptImprovementSummary {
        consumed_trace_events,
        failure_patterns,
        prompt_reflections: prompt_reflections.len(),
        prompt_candidates: prompt_candidates.len(),
        prompt_candidate_evaluations: prompt_candidate_evaluations.len(),
        prompt_frontier_entries: prompt_frontier.len(),
        prompt_frontier_root_entries: prompt_frontier_stats.root_entries,
        prompt_frontier_branched_entries: prompt_frontier_stats.branched_entries,
        prompt_frontier_max_generation: prompt_frontier_stats.max_generation,
        bootstrap_demos: derived.bootstrap_demos.len(),
        stress_cases: derived.stress_cases.len(),
        instruction_hypotheses: derived.instruction_hypotheses.len(),
        runtime_demos: derived.runtime_demos.len(),
        turn_demos: derived.turn_demos.len(),
        applied_system_additions: prompt_update.applied_system_additions,
        compiled_prompt_updated: prompt_update.compiled_prompt_updated,
    })
}
