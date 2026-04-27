use super::*;

pub(super) fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, ':' | ' ' | '-' | '_' | '.') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

pub(super) fn dedupe_vec(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for item in items {
        let normalized = item.trim();
        if normalized.is_empty() {
            continue;
        }
        if seen.insert(normalized.to_string()) {
            deduped.push(normalized.to_string());
        }
    }
    deduped
}

pub(super) fn dedupe_prompt_candidates(
    candidates: Vec<EvaluationArtifactRuntimePromptCandidate>,
) -> Vec<EvaluationArtifactRuntimePromptCandidate> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for candidate in candidates {
        let key = candidate.prompt_patches.join("\n");
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        deduped.push(candidate);
    }
    deduped
}

pub(super) fn dedupe_workflow_patches(
    patches: Vec<EvaluationArtifactWorkflowPatch>,
) -> Vec<EvaluationArtifactWorkflowPatch> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for patch in patches {
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            patch.workflow_id,
            patch.when_to_use_additions.join("\n"),
            patch.precondition_additions.join("\n"),
            patch.workflow_step_additions.join("\n"),
            patch.done_criteria_additions.join("\n"),
            patch.recovery_additions.join("\n")
        );
        if !seen.insert(key) {
            continue;
        }
        deduped.push(patch);
    }
    deduped
}

pub(super) async fn optimize_workflows_from_run_records(
    context: &mut Context,
    planner: &dyn SleepPlannerRuntime,
    run_records: &[WorkflowRunRecord],
) -> Result<SleepWorkflowOptimizationResult> {
    let mut result = SleepWorkflowOptimizationResult {
        rounds: 1,
        ..Default::default()
    };

    let evidence_by_workflow = group_run_records_by_workflow(run_records);
    let all_workflows = context.workflows.workspace_list();
    let mut reflection_by_workflow = HashMap::<String, EvaluationArtifactWorkflowReflection>::new();

    // Skip planning, merge, and frontier replay without run records; there is no new
    // evidence, so LLM calls are not meaningful. Existing frontier candidates can
    // still be selected and applied later.
    if !run_records.is_empty() {
        for workflow in &all_workflows {
            let evidence = evidence_by_workflow
                .get(&workflow.id)
                .cloned()
                .unwrap_or_default();
            let Some(plan) = planner
                .plan_workflow_improvement(context, workflow, &evidence)
                .await?
            else {
                continue;
            };
            reflection_by_workflow.insert(workflow.id.clone(), plan.reflection.clone());
            result.reflections.push(plan.reflection);
            result.patches.extend(plan.patches);
            result.candidate_evaluations.extend(plan.evaluations);
        }

        for left in 0..all_workflows.len() {
            for right in (left + 1)..all_workflows.len() {
                let target = &all_workflows[left];
                let source = &all_workflows[right];
                let Some(target_reflection) = reflection_by_workflow.get(&target.id) else {
                    continue;
                };
                let Some(source_reflection) = reflection_by_workflow.get(&source.id) else {
                    continue;
                };
                let target_evidence = evidence_by_workflow
                    .get(&target.id)
                    .cloned()
                    .unwrap_or_default();
                let source_evidence = evidence_by_workflow
                    .get(&source.id)
                    .cloned()
                    .unwrap_or_default();
                let merge_plan = planner
                    .plan_workflow_merge(
                        context,
                        WorkflowMergePlanningInput {
                            target_workflow: target,
                            target_reflection,
                            target_evidence: &target_evidence,
                            source_workflow: source,
                            source_reflection,
                            source_evidence: &source_evidence,
                        },
                    )
                    .await?;
                if let Some(evaluation) = merge_plan.evaluation {
                    result.candidate_evaluations.push(evaluation);
                }
                if let Some(merge) = merge_plan.merge {
                    result.merges.push(merge);
                }
            }
        }
    } else {
        tracing::info!(
            "[sleep] no workflow run records, skipping workflow planning and frontier replay"
        );
    }

    let mut workflow_frontier = load_workflow_frontier().await?;
    let mut frontier_incoming = Vec::<WorkflowFrontierEntry>::new();
    for patch in &result.patches {
        if let Some(evaluation) = result.candidate_evaluations.iter().find(|evaluation| {
            evaluation.candidate_kind == "patch" && evaluation.candidate_title == patch.title
        }) {
            let mut entry = workflow_patch_frontier_entry_from_candidate(patch, evaluation);
            let (parent_keys, generation) = infer_workflow_patch_lineage(&workflow_frontier, patch);
            entry.parent_keys = parent_keys;
            entry.generation = generation;
            frontier_incoming.push(entry);
        }
    }
    for merge in &result.merges {
        let merge_title = workflow_merge_title(merge);
        if let Some(evaluation) = result.candidate_evaluations.iter().find(|evaluation| {
            evaluation.candidate_kind == "merge" && evaluation.candidate_title == merge_title
        }) {
            let mut entry = workflow_merge_frontier_entry_from_candidate(merge, evaluation);
            let (parent_keys, generation) = infer_workflow_merge_lineage(&workflow_frontier, merge);
            entry.parent_keys = parent_keys;
            entry.generation = generation;
            frontier_incoming.push(entry);
        }
    }
    workflow_frontier = retain_workflow_frontier(&workflow_frontier, &frontier_incoming, 4);
    if !run_records.is_empty() {
        workflow_frontier = replay_workflow_frontier_entries(
            context,
            planner,
            &workflow_frontier,
            &all_workflows,
            &reflection_by_workflow,
            &evidence_by_workflow,
        )
        .await?;
    }
    let workflow_frontier_stats = workflow_frontier_lineage_stats(&workflow_frontier);
    result.frontier_entries = workflow_frontier.len();
    result.frontier_root_entries = workflow_frontier_stats.root_entries;
    result.frontier_branched_entries = workflow_frontier_stats.branched_entries;
    result.frontier_max_generation = workflow_frontier_stats.max_generation;

    let selected_patch_entries = select_workflow_patch_frontier_entries(&workflow_frontier);
    let mut selected_workflow_frontier_keys = Vec::<String>::new();
    for entry in selected_patch_entries {
        let Some(patch) = entry.patch.as_ref() else {
            continue;
        };
        if !evaluate_workflow_patch_candidate(&context.workflows, patch)
            || !patch_has_novel_content(&context.workflows, patch)
        {
            continue;
        }
        match context
            .workflows
            .apply_patch(WorkflowPatch {
                workflow_id: patch.workflow_id.clone(),
                when_to_use_additions: patch.when_to_use_additions.clone(),
                precondition_additions: patch.precondition_additions.clone(),
                workflow_step_additions: patch.workflow_step_additions.clone(),
                done_criteria_additions: patch.done_criteria_additions.clone(),
                recovery_additions: patch.recovery_additions.clone(),
            })
            .await
        {
            Ok(_) => {
                if let Some(local_patch) = result
                    .patches
                    .iter_mut()
                    .find(|candidate| candidate.title == patch.title)
                {
                    local_patch.applied = true;
                }
                selected_workflow_frontier_keys.push(entry.key.clone());
                result.patch_applied += 1;
            }
            Err(err) => {
                if let Some(local_patch) = result
                    .patches
                    .iter_mut()
                    .find(|candidate| candidate.title == patch.title)
                {
                    local_patch.rolled_back = true;
                    local_patch.rationale = format!("{}; rollback={}", local_patch.rationale, err);
                }
                result.rollbacks += 1;
            }
        }
    }

    let selected_merge_entries = select_workflow_merge_frontier_entries(&workflow_frontier);
    for entry in selected_merge_entries {
        let Some(merge) = entry.merge.as_ref() else {
            continue;
        };
        if !evaluate_workflow_merge_candidate(&context.workflows, merge) {
            continue;
        }
        match context
            .workflows
            .merge_workflows(
                &merge.target_workflow_id,
                &merge.source_workflow_ids,
                Some(merge.rationale.clone()),
            )
            .await
        {
            Ok(_) => {
                if let Some(local_merge) = result.merges.iter_mut().find(|candidate| {
                    workflow_merge_title(candidate) == workflow_merge_title(merge)
                }) {
                    local_merge.applied = true;
                }
                selected_workflow_frontier_keys.push(entry.key.clone());
                result.merge_applied += 1;
            }
            Err(err) => {
                if let Some(local_merge) = result.merges.iter_mut().find(|candidate| {
                    workflow_merge_title(candidate) == workflow_merge_title(merge)
                }) {
                    local_merge.rationale = format!("{}; rollback={}", local_merge.rationale, err);
                }
                result.rollbacks += 1;
            }
        }
    }
    mark_workflow_frontier_selected(&mut workflow_frontier, &selected_workflow_frontier_keys);
    save_workflow_frontier(&workflow_frontier).await?;

    Ok(result)
}

pub(super) fn evaluate_workflow_patch_candidate(
    workflows: &WorkflowStore,
    patch: &EvaluationArtifactWorkflowPatch,
) -> bool {
    workflows.workflow_origin(&patch.workflow_id)
        == Some(crate::workflow::WorkflowOrigin::Workspace)
        && (!patch.when_to_use_additions.is_empty()
            || !patch.precondition_additions.is_empty()
            || !patch.workflow_step_additions.is_empty()
            || !patch.done_criteria_additions.is_empty()
            || !patch.recovery_additions.is_empty())
}

pub(super) fn evaluate_workflow_merge_candidate(
    workflows: &WorkflowStore,
    merge: &EvaluationArtifactWorkflowMerge,
) -> bool {
    if workflows.workflow_origin(&merge.target_workflow_id)
        != Some(crate::workflow::WorkflowOrigin::Workspace)
    {
        return false;
    }
    !merge.source_workflow_ids.is_empty()
        && merge.source_workflow_ids.iter().all(|source_id| {
            workflows.workflow_origin(source_id) == Some(crate::workflow::WorkflowOrigin::Workspace)
        })
        && merge.confidence > 0.0
}

pub(super) fn total_patch_additions(patch: &EvaluationArtifactWorkflowPatch) -> usize {
    patch.when_to_use_additions.len()
        + patch.precondition_additions.len()
        + patch.workflow_step_additions.len()
        + patch.done_criteria_additions.len()
        + patch.recovery_additions.len()
}

pub(super) fn has_workflow_patch_content(patch: &EvaluationArtifactWorkflowPatch) -> bool {
    total_patch_additions(patch) > 0
}

pub(super) fn patch_has_novel_content(
    workflows: &WorkflowStore,
    patch: &EvaluationArtifactWorkflowPatch,
) -> bool {
    let Some(current) = workflows.get(&patch.workflow_id) else {
        return false;
    };
    patch
        .when_to_use_additions
        .iter()
        .any(|item| !current.when_to_use.iter().any(|existing| existing == item))
        || patch.precondition_additions.iter().any(|item| {
            !current
                .preconditions
                .iter()
                .any(|existing| existing == item)
        })
        || patch.workflow_step_additions.iter().any(|item| {
            !current
                .workflow_steps
                .iter()
                .any(|existing| existing == item)
        })
        || patch.done_criteria_additions.iter().any(|item| {
            !current
                .done_criteria
                .iter()
                .any(|existing| existing == item)
        })
        || patch
            .recovery_additions
            .iter()
            .any(|item| !current.recovery.iter().any(|existing| existing == item))
}

pub(super) fn workflow_merge_title(merge: &EvaluationArtifactWorkflowMerge) -> String {
    format!(
        "{}<-{}",
        merge.target_workflow_id,
        merge.source_workflow_ids.join("+")
    )
}
