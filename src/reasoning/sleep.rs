use std::collections::{BTreeSet, HashMap, HashSet};

use crate::{
    AgentLoopStepOutput, DaatLocusHomeOverride, build_eval_context_with_compiled,
    context::{ActiveWorkflowRunSession, Context, PendingWorkflowRunFlush},
    reasoning::{
        compiled::{
            RUNTIME_SYSTEM_PROMPT_COMPILE_KEY, save_compiled_runtime_system_prompt_for_model,
        },
        frontier::{
            WorkflowFrontierEntry, load_workflow_frontier, mark_workflow_frontier_selected,
            retain_workflow_frontier, save_workflow_frontier,
            select_workflow_merge_frontier_entries, select_workflow_patch_frontier_entries,
            workflow_frontier_lineage_stats, workflow_merge_frontier_entry_from_candidate,
            workflow_patch_frontier_entry_from_candidate,
        },
        programs::{
            runtime_error_correction_planner::{
                RuntimeErrorCorrectionPlannerOutput, RuntimeErrorCorrectionPlannerProgram,
            },
            workflow_candidate_rollout_evaluator::WorkflowCandidateRolloutEvaluatorOutput,
            workflow_candidate_rollout_evaluator::WorkflowCandidateRolloutEvaluatorProgram,
            workflow_evolution_planner::{
                WorkflowEvolutionPlannerOutput, WorkflowEvolutionPlannerProgram,
            },
            workflow_merge_planner::{WorkflowMergePlannerOutput, WorkflowMergePlannerProgram},
        },
        runtime_error::{RuntimeErrorCase, RuntimeErrorCaseBatch, compact_runtime_error_case_file},
        turn_compile::current_runtime_system_prompt_artifact_from_store,
    },
    workflow::{
        NewWorkflowSpec, WorkflowPatch, WorkflowRunRecord, WorkflowSpec, WorkflowStore,
        compact_workflow_run_record_file, load_workflow_run_batch,
    },
};
use async_trait::async_trait;
use miette::{IntoDiagnostic, Result};
use tracing::warn;

use super::{
    episode::EpisodeActionRecord,
    evaluation_artifacts::{
        EvaluationArtifactPromptReflection, EvaluationArtifactRuntimePromptCandidate,
        EvaluationArtifactRuntimePromptCandidateEvaluation,
        EvaluationArtifactWorkflowCandidateEvaluation, EvaluationArtifactWorkflowMerge,
        EvaluationArtifactWorkflowPatch, EvaluationArtifactWorkflowReflection,
        EvaluationArtifactsStore, RuntimeErrorCorrectionArtifacts, WorkflowImprovementArtifacts,
    },
    render::openai_tools::OpenAIToolRenderer,
    runtime::{execute_program_with_ir_report, resolve_program_tuning},
    trace::TraceOrigin,
};

mod rollout;

use rollout::*;

mod artifacts;
use artifacts::*;
mod planner;
mod prompt_pipeline;
mod workflow_pipeline;
use planner::{
    LlmSleepPlannerRuntime, SleepPlannerRuntime, WorkflowFrontierReplayInput,
    WorkflowMergePlanningInput, load_sleep_inputs,
};
use prompt_pipeline::run_runtime_error_correction_pipeline;
use workflow_pipeline::run_workflow_improvement_pipeline;
#[derive(Clone, Default)]
pub struct RuntimeErrorCorrectionSummary {
    pub consumed_error_cases: usize,
    pub runtime_error_cases: usize,
    pub reflections: usize,
    pub candidates: usize,
    pub candidate_evaluations: usize,
    pub applied_system_additions: usize,
    pub compiled_runtime_contract_updated: bool,
}

#[derive(Clone, Default)]
pub struct WorkflowImprovementSummary {
    pub evidence_run_records: usize,
    pub workflow_reflections: usize,
    pub patch_candidates: usize,
    pub merge_candidates: usize,
    pub candidate_evaluations: usize,
    pub frontier_entries: usize,
    pub frontier_root_entries: usize,
    pub frontier_branched_entries: usize,
    pub frontier_max_generation: usize,
    pub patch_applied: usize,
    pub merge_applied: usize,
    pub update_rollbacks: usize,
    pub optimization_rounds: usize,
}

#[derive(Clone, Default)]
pub struct SleepSummary {
    pub runtime_error_correction: RuntimeErrorCorrectionSummary,
    pub workflow_improvement: WorkflowImprovementSummary,
}

pub async fn run_sleep(context: &mut Context) -> Result<SleepSummary> {
    let planner = LlmSleepPlannerRuntime;
    let store = EvaluationArtifactsStore::open().await?;
    let sleep_inputs = load_sleep_inputs().await?;
    let runtime_error_correction = if sleep_inputs.runtime_error_cases.cases.is_empty() {
        tracing::info!(
            "[sleep] no runtime error cases, skipping runtime error correction pipeline"
        );
        RuntimeErrorCorrectionSummary::default()
    } else {
        match run_runtime_error_correction_pipeline(
            context,
            &planner,
            &store,
            &sleep_inputs.runtime_error_cases.cases,
            sleep_inputs.runtime_error_cases.cases.len(),
        )
        .await
        {
            Ok(summary) => summary,
            Err(err) => {
                warn!(
                    "runtime error correction pipeline failed, continuing with defaults: {err:?}"
                );
                RuntimeErrorCorrectionSummary::default()
            }
        }
    };
    let workflow_improvement =
        match run_workflow_improvement_pipeline(context, &planner, &store).await {
            Ok(summary) => summary,
            Err(err) => {
                warn!("workflow improvement pipeline failed, continuing with defaults: {err:?}");
                WorkflowImprovementSummary::default()
            }
        };
    compact_runtime_error_case_file(sleep_inputs.runtime_error_cases.next_offset).await?;
    Ok(SleepSummary {
        runtime_error_correction,
        workflow_improvement,
    })
}

struct SleepInputs {
    runtime_error_cases: RuntimeErrorCaseBatch,
}

#[derive(Default)]
struct SleepWorkflowOptimizationResult {
    reflections: Vec<EvaluationArtifactWorkflowReflection>,
    patches: Vec<EvaluationArtifactWorkflowPatch>,
    merges: Vec<EvaluationArtifactWorkflowMerge>,
    candidate_evaluations: Vec<EvaluationArtifactWorkflowCandidateEvaluation>,
    frontier_entries: usize,
    frontier_root_entries: usize,
    frontier_branched_entries: usize,
    frontier_max_generation: usize,
    patch_applied: usize,
    merge_applied: usize,
    rollbacks: usize,
    rounds: usize,
}

#[derive(Default)]
struct PromptPlanningResult {
    reflections: Vec<EvaluationArtifactPromptReflection>,
    candidates: Vec<EvaluationArtifactRuntimePromptCandidate>,
    evaluations: Vec<EvaluationArtifactRuntimePromptCandidateEvaluation>,
}

struct WorkflowPlanningResult {
    reflection: EvaluationArtifactWorkflowReflection,
    patches: Vec<EvaluationArtifactWorkflowPatch>,
    evaluations: Vec<EvaluationArtifactWorkflowCandidateEvaluation>,
}

struct WorkflowMergePlanningResult {
    merge: Option<EvaluationArtifactWorkflowMerge>,
    evaluation: Option<EvaluationArtifactWorkflowCandidateEvaluation>,
}

struct PromptPatchUpdate {
    applied_system_additions: usize,
    compiled_prompt_updated: bool,
}

fn runtime_error_correction_planning_result_from_output(
    output: &RuntimeErrorCorrectionPlannerOutput,
    runtime_error_cases: &[RuntimeErrorCase],
) -> PromptPlanningResult {
    let source_case_ids = runtime_error_cases
        .iter()
        .map(|case| case.case_id.clone())
        .collect::<Vec<_>>();
    let reflections = output
        .reflections
        .iter()
        .map(|reflection| EvaluationArtifactPromptReflection {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            title: reflection.title.trim().to_string(),
            rationale: reflection.rationale.trim().to_string(),
            missing_instructions: dedupe_vec(reflection.missing_runtime_contracts.clone()),
            over_constraints: dedupe_vec(reflection.over_constraints.clone()),
            source_trace_ids: if reflection.source_case_ids.is_empty() {
                source_case_ids.clone()
            } else {
                dedupe_vec(reflection.source_case_ids.clone())
            },
            confidence: reflection.confidence,
        })
        .collect::<Vec<_>>();
    let candidates = output
        .candidates
        .iter()
        .map(|candidate| EvaluationArtifactRuntimePromptCandidate {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            title: candidate.title.trim().to_string(),
            rationale: candidate.rationale.trim().to_string(),
            prompt_patches: dedupe_vec(candidate.runtime_contract_additions.clone()),
            source_demo_titles: Vec::new(),
            source_hypotheses: dedupe_vec(candidate.source_reflection_titles.clone()),
        })
        .filter(|candidate| !candidate.title.is_empty() && !candidate.prompt_patches.is_empty())
        .collect::<Vec<_>>();
    let evaluations = output
        .evaluations
        .iter()
        .map(
            |evaluation| EvaluationArtifactRuntimePromptCandidateEvaluation {
                compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
                candidate_title: evaluation.candidate_title.trim().to_string(),
                rationale: evaluation.rationale.trim().to_string(),
                score: evaluation.score,
                accepted: evaluation.accepted,
                selected: evaluation.selected,
                regressions_detected: evaluation.regressions_detected,
                source_trace_ids: source_case_ids.clone(),
            },
        )
        .filter(|evaluation| !evaluation.candidate_title.is_empty())
        .collect::<Vec<_>>();

    PromptPlanningResult {
        reflections,
        candidates: dedupe_prompt_candidates(candidates),
        evaluations,
    }
}

fn workflow_planning_result_from_output(
    workflow: &WorkflowSpec,
    evidence: &[WorkflowRunRecord],
    output: &WorkflowEvolutionPlannerOutput,
) -> Option<WorkflowPlanningResult> {
    if !output.should_optimize {
        return None;
    }
    let reflection = EvaluationArtifactWorkflowReflection {
        workflow_id: workflow.id.clone(),
        rationale: output.reflection.rationale.trim().to_string(),
        missing_preconditions: dedupe_vec(output.reflection.missing_preconditions.clone()),
        weak_workflow_steps: dedupe_vec(output.reflection.weak_workflow_steps.clone()),
        weak_done_criteria: dedupe_vec(output.reflection.weak_done_criteria.clone()),
        weak_recovery: dedupe_vec(output.reflection.weak_recovery.clone()),
        recurring_failure_patterns: dedupe_vec(
            output.reflection.recurring_failure_patterns.clone(),
        ),
        source_run_ids: evidence
            .iter()
            .map(|record| record.run_id.clone())
            .collect(),
        confidence: output.reflection.confidence,
    };
    let patches = output
        .patch_candidates
        .iter()
        .map(|candidate| EvaluationArtifactWorkflowPatch {
            workflow_id: workflow.id.clone(),
            title: candidate.title.trim().to_string(),
            rationale: candidate.rationale.trim().to_string(),
            when_to_use_additions: dedupe_vec(candidate.when_to_use_additions.clone()),
            precondition_additions: dedupe_vec(candidate.precondition_additions.clone()),
            workflow_step_additions: dedupe_vec(candidate.workflow_step_additions.clone()),
            done_criteria_additions: dedupe_vec(candidate.done_criteria_additions.clone()),
            recovery_additions: dedupe_vec(candidate.recovery_additions.clone()),
            source_run_ids: evidence
                .iter()
                .map(|record| record.run_id.clone())
                .collect(),
            confidence: candidate.confidence,
            applied: false,
            rolled_back: false,
        })
        .filter(|patch| !patch.title.is_empty() && has_workflow_patch_content(patch))
        .collect::<Vec<_>>();
    let evaluations = output
        .evaluations
        .iter()
        .map(|evaluation| EvaluationArtifactWorkflowCandidateEvaluation {
            workflow_id: workflow.id.clone(),
            candidate_kind: "patch".to_string(),
            candidate_title: evaluation.candidate_title.trim().to_string(),
            rationale: evaluation.rationale.trim().to_string(),
            score: evaluation.score,
            accepted: evaluation.accepted,
            selected: evaluation.selected,
            source_run_ids: evidence
                .iter()
                .map(|record| record.run_id.clone())
                .collect(),
        })
        .filter(|evaluation| !evaluation.candidate_title.is_empty())
        .collect::<Vec<_>>();

    Some(WorkflowPlanningResult {
        reflection,
        patches: dedupe_workflow_patches(patches),
        evaluations,
    })
}

fn workflow_merge_planning_result_from_output(
    target_workflow: &WorkflowSpec,
    source_workflow: &WorkflowSpec,
    target_reflection: &EvaluationArtifactWorkflowReflection,
    source_reflection: &EvaluationArtifactWorkflowReflection,
    target_evidence: &[WorkflowRunRecord],
    source_evidence: &[WorkflowRunRecord],
    output: &WorkflowMergePlannerOutput,
) -> WorkflowMergePlanningResult {
    let merge = if output.should_merge {
        Some(EvaluationArtifactWorkflowMerge {
            target_workflow_id: target_workflow.id.clone(),
            source_workflow_ids: vec![source_workflow.id.clone()],
            rationale: output.rationale.trim().to_string(),
            confidence: output.confidence,
            applied: false,
        })
    } else {
        None
    };
    let evaluation = Some(EvaluationArtifactWorkflowCandidateEvaluation {
        workflow_id: target_workflow.id.clone(),
        candidate_kind: "merge".to_string(),
        candidate_title: format!("{}<-{}", target_workflow.id, source_workflow.id),
        rationale: format!(
            "{} | target_reflection={} source_reflection={}",
            output.rationale.trim(),
            target_reflection.rationale.trim(),
            source_reflection.rationale.trim()
        ),
        score: output.confidence,
        accepted: output.accepted && output.should_merge,
        selected: output.selected && output.should_merge,
        source_run_ids: target_evidence
            .iter()
            .chain(source_evidence.iter())
            .map(|record| record.run_id.clone())
            .collect(),
    });
    WorkflowMergePlanningResult { merge, evaluation }
}

fn render_workflow_spec_markdown(workflow: &WorkflowSpec) -> String {
    let render_section = |title: &str, items: &[String]| -> String {
        let body = if items.is_empty() {
            "- <empty>".to_string()
        } else {
            items
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!("## {title}\n{body}")
    };

    [
        format!("---\nid: {}\n---", workflow.id),
        render_section("When To Use", &workflow.when_to_use),
        render_section("Preconditions", &workflow.preconditions),
        render_section("Workflow", &workflow.workflow_steps),
        render_section("Done Criteria", &workflow.done_criteria),
        render_section("Recovery", &workflow.recovery),
    ]
    .join("\n\n")
}

fn render_workflow_run_evidence_json(evidence: &[WorkflowRunRecord]) -> Result<String> {
    serde_json::to_string_pretty(evidence).into_diagnostic()
}

fn group_run_records_by_workflow(
    run_records: &[WorkflowRunRecord],
) -> HashMap<String, Vec<WorkflowRunRecord>> {
    let mut grouped = HashMap::<String, Vec<WorkflowRunRecord>>::new();
    for record in run_records {
        grouped
            .entry(record.workflow_id.clone())
            .or_default()
            .push(record.clone());
    }
    grouped
}

async fn replay_workflow_frontier_entries(
    context: &mut Context,
    planner: &dyn SleepPlannerRuntime,
    entries: &[WorkflowFrontierEntry],
    workflows: &[WorkflowSpec],
    reflection_by_workflow: &HashMap<String, EvaluationArtifactWorkflowReflection>,
    evidence_by_workflow: &HashMap<String, Vec<WorkflowRunRecord>>,
) -> Result<Vec<WorkflowFrontierEntry>> {
    let workflow_map = workflows
        .iter()
        .map(|workflow| (workflow.id.clone(), workflow))
        .collect::<HashMap<_, _>>();
    let mut replayed = Vec::new();
    for entry in entries {
        let Some(target_workflow) = workflow_map.get(&entry.evaluation.workflow_id) else {
            continue;
        };
        let target_reflection = reflection_by_workflow.get(&entry.evaluation.workflow_id);
        let target_evidence = evidence_by_workflow
            .get(&entry.evaluation.workflow_id)
            .map(|items| items.as_slice())
            .unwrap_or(&[]);
        let (source_workflow, source_reflection, source_evidence) =
            if let Some(merge) = entry.merge.as_ref() {
                let source_id = merge
                    .source_workflow_ids
                    .first()
                    .cloned()
                    .unwrap_or_default();
                (
                    workflow_map.get(&source_id).copied(),
                    reflection_by_workflow.get(&source_id),
                    evidence_by_workflow
                        .get(&source_id)
                        .map(|items| items.as_slice())
                        .unwrap_or(&[]),
                )
            } else {
                (None, None, &[][..])
            };
        let mut updated = entry.clone();
        updated.evaluation = planner
            .replay_workflow_frontier_entry(
                context,
                WorkflowFrontierReplayInput {
                    entry: &updated,
                    target_workflow,
                    target_reflection,
                    target_evidence,
                    source_workflow,
                    source_reflection,
                    source_evidence,
                },
            )
            .await?;
        replayed.push(updated);
    }
    Ok(replayed)
}

fn infer_workflow_patch_lineage(
    existing: &[WorkflowFrontierEntry],
    patch: &EvaluationArtifactWorkflowPatch,
) -> (Vec<String>, usize) {
    let patch_set = [
        patch.when_to_use_additions.clone(),
        patch.precondition_additions.clone(),
        patch.workflow_step_additions.clone(),
        patch.done_criteria_additions.clone(),
        patch.recovery_additions.clone(),
    ]
    .concat()
    .into_iter()
    .collect::<HashSet<_>>();
    let mut overlaps = existing
        .iter()
        .filter(|entry| entry.candidate_kind == "patch")
        .filter_map(|entry| {
            let existing_patch = entry.patch.as_ref()?;
            if existing_patch.workflow_id != patch.workflow_id {
                return None;
            }
            let entry_set = [
                existing_patch.when_to_use_additions.clone(),
                existing_patch.precondition_additions.clone(),
                existing_patch.workflow_step_additions.clone(),
                existing_patch.done_criteria_additions.clone(),
                existing_patch.recovery_additions.clone(),
            ]
            .concat()
            .into_iter()
            .collect::<HashSet<_>>();
            let intersection = patch_set.intersection(&entry_set).count();
            if intersection == 0 {
                return None;
            }
            Some((entry.key.clone(), entry.generation, intersection))
        })
        .collect::<Vec<_>>();
    overlaps.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| right.1.cmp(&left.1)));
    let parent_keys = overlaps
        .iter()
        .take(2)
        .map(|(key, _, _)| key.clone())
        .collect::<Vec<_>>();
    let generation = overlaps
        .iter()
        .take(2)
        .map(|(_, generation, _)| *generation)
        .max()
        .unwrap_or(0)
        + usize::from(!parent_keys.is_empty());
    (parent_keys, generation)
}

fn infer_workflow_merge_lineage(
    existing: &[WorkflowFrontierEntry],
    merge: &EvaluationArtifactWorkflowMerge,
) -> (Vec<String>, usize) {
    let mut overlaps = existing
        .iter()
        .filter(|entry| entry.candidate_kind == "merge")
        .filter_map(|entry| {
            let existing_merge = entry.merge.as_ref()?;
            if existing_merge.target_workflow_id != merge.target_workflow_id {
                return None;
            }
            let intersection = existing_merge
                .source_workflow_ids
                .iter()
                .filter(|source| merge.source_workflow_ids.iter().any(|item| item == *source))
                .count();
            if intersection == 0 {
                return None;
            }
            Some((entry.key.clone(), entry.generation, intersection))
        })
        .collect::<Vec<_>>();
    overlaps.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| right.1.cmp(&left.1)));
    let parent_keys = overlaps
        .iter()
        .take(2)
        .map(|(key, _, _)| key.clone())
        .collect::<Vec<_>>();
    let generation = overlaps
        .iter()
        .take(2)
        .map(|(_, generation, _)| *generation)
        .max()
        .unwrap_or(0)
        + usize::from(!parent_keys.is_empty());
    (parent_keys, generation)
}

#[cfg(test)]
fn selected_candidate_titles(
    evaluations: &[EvaluationArtifactWorkflowCandidateEvaluation],
    candidate_kind: &str,
) -> HashSet<String> {
    evaluations
        .iter()
        .filter(|evaluation| evaluation.candidate_kind == candidate_kind && evaluation.selected)
        .map(|evaluation| evaluation.candidate_title.clone())
        .collect()
}

async fn apply_selected_prompt_candidate(
    context: &mut Context,
    candidates: &[EvaluationArtifactRuntimePromptCandidate],
    evaluations: &mut [EvaluationArtifactRuntimePromptCandidateEvaluation],
) -> Result<PromptPatchUpdate> {
    let Some(selected) = evaluations
        .iter_mut()
        .filter(|evaluation| evaluation.accepted)
        .max_by(|left, right| left.score.total_cmp(&right.score))
    else {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    };
    selected.selected = true;

    let Some(candidate) = candidates
        .iter()
        .find(|candidate| candidate.title == selected.candidate_title)
    else {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    };

    let mut compiled = current_runtime_system_prompt_artifact_from_store(&context.compiled_prompts);
    let previous_len = compiled.system_additions.len();
    for addition in &candidate.prompt_patches {
        if !compiled
            .system_additions
            .iter()
            .any(|line| line == addition)
        {
            compiled.system_additions.push(addition.clone());
        }
    }
    let applied_system_additions = compiled.system_additions.len().saturating_sub(previous_len);
    if applied_system_additions == 0 {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    }

    compiled.best_candidate = format!(
        "sleep_prompt_candidate_{}_{}",
        slugify(&candidate.title),
        chrono::Utc::now().timestamp()
    );
    save_compiled_runtime_system_prompt_for_model(
        &context.config.main_model_config().model_id,
        &compiled,
    )
    .await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(compiled));

    Ok(PromptPatchUpdate {
        applied_system_additions,
        compiled_prompt_updated: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::reasoning::programs::workflow_evolution_planner::{
        WorkflowEvolutionPlannerOutput, WorkflowPlannerCandidateEvaluation,
        WorkflowPlannerPatchCandidate, WorkflowPlannerReflection,
    };
    use crate::workflow::{NewWorkflowSpec, WorkflowRunOutcome, WorkflowRunRecord};

    #[tokio::test]
    async fn sleep_workflow_optimizer_updates_workflow_content_from_run_records() {
        let temp_dir = TempDir::new().expect("create temporary workflow dir");
        let primary = temp_dir.path().join("workflows");

        let mut workflows = WorkflowStore::open_scoped(primary.clone()).await;
        let created = workflows
            .create_workflow(NewWorkflowSpec {
                id: "repair-flaky-test-pipeline".to_string(),
                when_to_use: vec!["test flaky".to_string()],
                preconditions: vec!["failing test logs available".to_string()],
                workflow_steps: vec![
                    "collect flaky failure evidence".to_string(),
                    "pinpoint unstable assertion".to_string(),
                ],
                done_criteria: vec!["test runs stable".to_string()],
                recovery: vec!["rollback last risky change".to_string()],
            })
            .await
            .expect("create workflow");

        let run_records = vec![WorkflowRunRecord {
            run_id: "workflow-run:test".to_string(),
            workflow_id: created.id.clone(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            ended_at_ms: chrono::Utc::now().timestamp_millis(),
            origin: "event:test".to_string(),
            outcome: WorkflowRunOutcome::Blocked,
            turn_count: 1,
            tool_action_count: 3,
            manual_fix_detected: true,
            rollback_detected: true,
            failure_types: vec!["tool_failure".to_string()],
            final_summary: "tool failed while applying patch".to_string(),
        }];
        let plan = workflow_planning_result_from_output(
            &created,
            &run_records,
            &WorkflowEvolutionPlannerOutput {
                should_optimize: true,
                reflection: WorkflowPlannerReflection {
                    rationale: "blocked + manual fix".to_string(),
                    missing_preconditions: vec![
                        "Confirm key dependencies, inputs, and permission conditions before execution."
                            .to_string(),
                    ],
                    weak_workflow_steps: vec![
                        "Before manual repair, lock the repair premise and plan verification steps."
                            .to_string(),
                    ],
                    weak_done_criteria: vec![],
                    weak_recovery: vec![
                        "When blocked, return to the previous stable step and revalidate key premises."
                            .to_string(),
                    ],
                    recurring_failure_patterns: vec!["tool_failure".to_string()],
                    confidence: 0.88,
                },
                patch_candidates: vec![WorkflowPlannerPatchCandidate {
                    title: "repair flaky test patch".to_string(),
                    rationale: "add recovery and manual-fix guardrails".to_string(),
                    when_to_use_additions: vec![],
                    precondition_additions: vec![
                        "Confirm key dependencies, inputs, and permission conditions before execution."
                            .to_string(),
                    ],
                    workflow_step_additions: vec![
                        "Before manual repair, lock the repair premise and plan verification steps."
                            .to_string(),
                    ],
                    done_criteria_additions: vec![],
                    recovery_additions: vec![
                        "When blocked, return to the previous stable step and revalidate key premises."
                            .to_string(),
                    ],
                    confidence: 0.91,
                }],
                evaluations: vec![WorkflowPlannerCandidateEvaluation {
                    candidate_title: "repair flaky test patch".to_string(),
                    rationale: "covers reflection weaknesses".to_string(),
                    score: 0.92,
                    accepted: true,
                    selected: true,
                }],
            },
        )
        .expect("planner output should produce workflow plan");

        assert_eq!(plan.patches.len(), 1);
        assert_eq!(plan.evaluations.len(), 1);

        let selected_titles = selected_candidate_titles(&plan.evaluations, "patch");
        let patch = plan
            .patches
            .iter()
            .find(|patch| selected_titles.contains(&patch.title))
            .expect("selected patch should exist");
        workflows
            .apply_patch(WorkflowPatch {
                workflow_id: patch.workflow_id.clone(),
                when_to_use_additions: patch.when_to_use_additions.clone(),
                precondition_additions: patch.precondition_additions.clone(),
                workflow_step_additions: patch.workflow_step_additions.clone(),
                done_criteria_additions: patch.done_criteria_additions.clone(),
                recovery_additions: patch.recovery_additions.clone(),
            })
            .await
            .expect("selected patch should apply");

        let updated = workflows
            .get(&created.id)
            .expect("updated workflow should exist");
        assert_ne!(updated.workflow_steps, created.workflow_steps);
        assert!(
            updated
                .workflow_steps
                .iter()
                .any(|step| step.contains("manual repair") || step.contains("blocked"))
                || updated
                    .recovery
                    .iter()
                    .any(|step| step.contains("blocked") || step.contains("stable step"))
        );
    }

    #[tokio::test]
    async fn workflow_rollout_uses_empty_isolated_store_even_when_target_exists() {
        let temp_dir = TempDir::new().expect("create temporary workflow dir");
        let primary = temp_dir.path().join("primary");
        let isolated = temp_dir.path().join("isolated");

        let mut workflows = WorkflowStore::open_scoped(primary).await;
        let existing = workflows
            .create_workflow(NewWorkflowSpec {
                id: "hermes-agent-analysis".to_string(),
                when_to_use: vec!["analyze agent repos".to_string()],
                preconditions: Vec::new(),
                workflow_steps: vec!["inspect repo".to_string()],
                done_criteria: vec!["summary is produced".to_string()],
                recovery: Vec::new(),
            })
            .await
            .expect("create existing workflow");

        let (installed, source_ids) =
            install_rollout_workflows(&mut workflows, isolated.clone(), &existing, None)
                .await
                .expect("install isolated rollout workflow");

        assert_eq!(installed.id, existing.id);
        assert!(source_ids.is_empty());
        assert_eq!(workflows.workspace_list().len(), 1);
        assert!(isolated.join("hermes-agent-analysis.md").exists());
    }

    #[test]
    fn workflow_task_cases_prefer_most_recent_runs() {
        let records = (0..10)
            .map(|index| WorkflowRunRecord {
                run_id: format!("run-{index}"),
                workflow_id: "repair-flaky-test-pipeline".to_string(),
                started_at_ms: index,
                ended_at_ms: index + 100,
                origin: "event:test".to_string(),
                outcome: WorkflowRunOutcome::Completed,
                turn_count: 1,
                tool_action_count: 1,
                manual_fix_detected: false,
                rollback_detected: false,
                failure_types: Vec::new(),
                final_summary: format!("summary-{index}"),
            })
            .collect::<Vec<_>>();
        let cases = records
            .iter()
            .map(workflow_task_case_from_record)
            .collect::<Vec<_>>();

        let selected = select_workflow_task_cases(&cases, 4);
        let run_ids = selected
            .iter()
            .map(|task| task.baseline_run_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(run_ids, vec!["run-9", "run-8", "run-7", "run-6"]);
    }

    #[test]
    fn workflow_task_rollout_case_simulates_bind_and_flush_boundary() {
        let workflow = WorkflowSpec {
            id: "repair-flaky-test-pipeline".to_string(),
            when_to_use: vec!["repair flaky tests".to_string()],
            preconditions: Vec::new(),
            workflow_steps: vec![
                "Collect failing traces".to_string(),
                "Apply minimal patch".to_string(),
            ],
            done_criteria: vec!["Root cause identified".to_string()],
            recovery: vec!["Fallback to evidence collection".to_string()],
        };
        let case = WorkflowRunRecord {
            run_id: "run-1".to_string(),
            workflow_id: "old-id".to_string(),
            started_at_ms: 100,
            ended_at_ms: 220,
            origin: "event:test".to_string(),
            outcome: WorkflowRunOutcome::Blocked,
            turn_count: 3,
            tool_action_count: 2,
            manual_fix_detected: true,
            rollback_detected: false,
            failure_types: vec!["tool_failure".to_string()],
            final_summary: "patch attempt failed".to_string(),
        };

        let task = workflow_task_case_from_record(&case);
        let rollout = run_workflow_task_rollout(&workflow, &task);

        assert!(rollout.record.run_id.starts_with("workflow-rollout:"));
        assert_eq!(rollout.record.workflow_id, workflow.id);
        assert_eq!(rollout.record.origin, task.origin);
        assert_eq!(rollout.record.outcome, task.baseline_outcome);
        assert_eq!(rollout.record.turn_count, task.baseline_turns);
        assert_eq!(rollout.record.tool_action_count, task.baseline_tool_actions);
        assert!(rollout.record.manual_fix_detected);
        assert_eq!(rollout.record.failure_types, task.failure_types);
        assert_eq!(rollout.executed_steps.len(), 2);
        assert_eq!(rollout.executed_steps[0].status, "completed");
        assert_eq!(rollout.executed_steps[1].status, "blocked_boundary");
        assert!(
            rollout
                .boundary_events
                .iter()
                .any(|event| event == "manual_fix_detected")
        );
        assert!(
            rollout
                .boundary_events
                .iter()
                .any(|event| event == "outcome:Blocked")
        );
        assert!(rollout.summary.contains("workflow_bound=true"));
        assert!(rollout.summary.contains("session_accumulated=true"));
        assert!(rollout.summary.contains("outcome_collected=true"));
    }
}
