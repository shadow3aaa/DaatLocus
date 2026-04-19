use std::collections::{HashMap, HashSet};

use crate::{
    context::Context,
    hindsight::{HindsightRecallOptions, builtin_hindsight_mental_models},
    reasoning::{
        compiled::{
            RUNTIME_SYSTEM_PROMPT_COMPILE_KEY, save_compiled_runtime_system_prompt_for_model,
        },
        examples::ExampleField,
        frontier::{
            WorkflowFrontierEntry, load_prompt_frontier, load_workflow_frontier,
            mark_prompt_frontier_selected, mark_workflow_frontier_selected,
            prompt_frontier_entry_from_candidate, prompt_frontier_lineage_stats,
            retain_prompt_frontier, retain_workflow_frontier, save_prompt_frontier,
            save_workflow_frontier, select_prompt_frontier_entry,
            select_workflow_merge_frontier_entries, select_workflow_patch_frontier_entries,
            workflow_frontier_lineage_stats, workflow_merge_frontier_entry_from_candidate,
            workflow_patch_frontier_entry_from_candidate,
        },
        programs::{
            prompt_candidate_replay_judge::PromptCandidateReplayJudgeOutput,
            prompt_candidate_replay_judge::PromptCandidateReplayJudgeProgram,
            prompt_evolution_planner::{
                PromptEvolutionPlannerOutput, PromptEvolutionPlannerProgram,
            },
            workflow_candidate_replay_judge::WorkflowCandidateReplayJudgeOutput,
            workflow_candidate_replay_judge::WorkflowCandidateReplayJudgeProgram,
            workflow_evolution_planner::{
                WorkflowEvolutionPlannerOutput, WorkflowEvolutionPlannerProgram,
            },
            workflow_merge_planner::{WorkflowMergePlannerOutput, WorkflowMergePlannerProgram},
        },
        runtime::PromptRequest,
        turn_compile::current_runtime_system_prompt_artifact_from_store,
    },
    workflow::{
        WorkflowPatch, WorkflowRunRecord, WorkflowSpec, WorkflowStore, load_workflow_run_batch,
    },
};
use async_trait::async_trait;
use miette::{IntoDiagnostic, Result};
use serde_json::json;
use tracing::warn;

use super::{
    evaluation_artifacts::{
        EvaluationArtifactBootstrapDemo, EvaluationArtifactFailurePattern,
        EvaluationArtifactInstructionHypothesis, EvaluationArtifactPromptReflection,
        EvaluationArtifactRuntimeDemo, EvaluationArtifactRuntimePromptCandidate,
        EvaluationArtifactRuntimePromptCandidateEvaluation, EvaluationArtifactStressCase,
        EvaluationArtifactSuggestedFixKind, EvaluationArtifactTurnDemo,
        EvaluationArtifactWorkflowCandidateEvaluation, EvaluationArtifactWorkflowMerge,
        EvaluationArtifactWorkflowPatch, EvaluationArtifactWorkflowReflection,
        EvaluationArtifactsStore, PromptImprovementArtifacts, WorkflowImprovementArtifacts,
    },
    programs::evaluation_artifact_builder::{
        EvaluationArtifactBuilderOutput, EvaluationArtifactBuilderProgram,
    },
    render::openai_tools::OpenAIToolRenderer,
    runtime::{execute_program_with_ir_report, resolve_program_tuning},
    trace::{
        ProgramTraceRecord, RuntimeTraceBatch, TraceOrigin, compact_runtime_trace_file,
        load_runtime_trace_batch,
    },
};

#[derive(Clone, Default)]
pub struct PromptImprovementSummary {
    pub consumed_trace_events: usize,
    pub failure_patterns: Vec<EvaluationArtifactFailurePattern>,
    pub prompt_reflections: usize,
    pub prompt_candidates: usize,
    pub prompt_candidate_evaluations: usize,
    pub prompt_frontier_entries: usize,
    pub prompt_frontier_root_entries: usize,
    pub prompt_frontier_branched_entries: usize,
    pub prompt_frontier_max_generation: usize,
    pub bootstrap_demos: usize,
    pub stress_cases: usize,
    pub instruction_hypotheses: usize,
    pub runtime_demos: usize,
    pub turn_demos: usize,
    pub applied_system_additions: usize,
    pub compiled_prompt_updated: bool,
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
    pub prompt_improvement: PromptImprovementSummary,
    pub workflow_improvement: WorkflowImprovementSummary,
    pub refreshed_mental_models: usize,
}

pub async fn run_sleep(context: &mut Context) -> Result<SleepSummary> {
    let planner = LlmSleepPlannerRuntime;
    let store = EvaluationArtifactsStore::open().await?;
    let sleep_inputs = load_sleep_inputs().await?;
    let prompt_improvement = run_prompt_improvement_pipeline(
        context,
        &planner,
        &store,
        &sleep_inputs.trace_batch.records,
        sleep_inputs.trace_batch.records.len(),
    )
    .await?;
    let workflow_improvement = run_workflow_improvement_pipeline(context, &planner, &store).await?;
    let mental_models = builtin_hindsight_mental_models();
    let refreshed_mental_models = match context
        .hindsight
        .sync_mental_models(&mental_models, true)
        .await
    {
        Ok(operation_ids) => operation_ids.len(),
        Err(err) => {
            warn!("failed to refresh hindsight mental models during sleep: {err:?}");
            0
        }
    };
    compact_runtime_trace_file(sleep_inputs.trace_batch.next_offset).await?;
    Ok(SleepSummary {
        prompt_improvement,
        workflow_improvement,
        refreshed_mental_models,
    })
}

struct DerivedEvaluationArtifacts {
    bootstrap_demos: Vec<EvaluationArtifactBootstrapDemo>,
    stress_cases: Vec<EvaluationArtifactStressCase>,
    instruction_hypotheses: Vec<EvaluationArtifactInstructionHypothesis>,
    runtime_demos: Vec<EvaluationArtifactRuntimeDemo>,
    turn_demos: Vec<EvaluationArtifactTurnDemo>,
}

struct SleepInputs {
    trace_batch: RuntimeTraceBatch,
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

#[async_trait]
trait SleepPlannerRuntime: Send + Sync {
    async fn plan_prompt_improvement(
        &self,
        context: &mut Context,
        failure_patterns: &[EvaluationArtifactFailurePattern],
    ) -> Result<PromptPlanningResult>;

    async fn plan_workflow_improvement(
        &self,
        context: &mut Context,
        workflow: &WorkflowSpec,
        evidence: &[WorkflowRunRecord],
    ) -> Result<Option<WorkflowPlanningResult>>;

    async fn plan_workflow_merge(
        &self,
        context: &mut Context,
        target_workflow: &WorkflowSpec,
        target_reflection: &EvaluationArtifactWorkflowReflection,
        target_evidence: &[WorkflowRunRecord],
        source_workflow: &WorkflowSpec,
        source_reflection: &EvaluationArtifactWorkflowReflection,
        source_evidence: &[WorkflowRunRecord],
    ) -> Result<WorkflowMergePlanningResult>;

    async fn replay_prompt_candidate(
        &self,
        context: &mut Context,
        candidate: &EvaluationArtifactRuntimePromptCandidate,
        failure_patterns: &[EvaluationArtifactFailurePattern],
        records: &[ProgramTraceRecord],
    ) -> Result<EvaluationArtifactRuntimePromptCandidateEvaluation>;

    async fn replay_workflow_frontier_entry(
        &self,
        context: &mut Context,
        entry: &WorkflowFrontierEntry,
        target_workflow: &WorkflowSpec,
        target_reflection: Option<&EvaluationArtifactWorkflowReflection>,
        target_evidence: &[WorkflowRunRecord],
        source_workflow: Option<&WorkflowSpec>,
        source_reflection: Option<&EvaluationArtifactWorkflowReflection>,
        source_evidence: &[WorkflowRunRecord],
    ) -> Result<EvaluationArtifactWorkflowCandidateEvaluation>;
}

struct LlmSleepPlannerRuntime;

async fn load_runtime_trace_records() -> Result<RuntimeTraceBatch> {
    load_runtime_trace_batch().await
}

async fn load_sleep_inputs() -> Result<SleepInputs> {
    let trace_batch = load_runtime_trace_records().await?;
    Ok(SleepInputs { trace_batch })
}

#[async_trait]
impl SleepPlannerRuntime for LlmSleepPlannerRuntime {
    async fn plan_prompt_improvement(
        &self,
        context: &mut Context,
        failure_patterns: &[EvaluationArtifactFailurePattern],
    ) -> Result<PromptPlanningResult> {
        if failure_patterns.is_empty() {
            return Ok(PromptPlanningResult {
                reflections: Vec::new(),
                candidates: Vec::new(),
                evaluations: Vec::new(),
            });
        }

        let renderer = OpenAIToolRenderer;
        let program = PromptEvolutionPlannerProgram;
        let tuning = resolve_program_tuning(context, &program).await;
        let current_additions = context
            .compiled_prompts
            .runtime_system_additions()
            .join("\n");
        let failure_patterns_json =
            serde_json::to_string_pretty(failure_patterns).into_diagnostic()?;
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(current_additions, failure_patterns_json),
            &tuning,
            TraceOrigin::Sleep,
        )
        .await?;

        Ok(prompt_planning_result_from_output(
            &outcome.output,
            failure_patterns,
        ))
    }

    async fn plan_workflow_improvement(
        &self,
        context: &mut Context,
        workflow: &WorkflowSpec,
        evidence: &[WorkflowRunRecord],
    ) -> Result<Option<WorkflowPlanningResult>> {
        let renderer = OpenAIToolRenderer;
        let program = WorkflowEvolutionPlannerProgram;
        let tuning = resolve_program_tuning(context, &program).await;
        let workflow_markdown = render_workflow_spec_markdown(workflow);
        let workflow_run_evidence_json = render_workflow_run_evidence_json(evidence)?;
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(
                workflow.id.clone(),
                workflow_markdown,
                workflow_run_evidence_json,
            ),
            &tuning,
            TraceOrigin::Sleep,
        )
        .await?;

        Ok(workflow_planning_result_from_output(
            workflow,
            evidence,
            &outcome.output,
        ))
    }

    async fn plan_workflow_merge(
        &self,
        context: &mut Context,
        target_workflow: &WorkflowSpec,
        target_reflection: &EvaluationArtifactWorkflowReflection,
        target_evidence: &[WorkflowRunRecord],
        source_workflow: &WorkflowSpec,
        source_reflection: &EvaluationArtifactWorkflowReflection,
        source_evidence: &[WorkflowRunRecord],
    ) -> Result<WorkflowMergePlanningResult> {
        let renderer = OpenAIToolRenderer;
        let program = WorkflowMergePlannerProgram;
        let tuning = resolve_program_tuning(context, &program).await;
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(
                target_workflow.id.clone(),
                render_workflow_spec_markdown(target_workflow),
                serde_json::to_string_pretty(target_reflection).into_diagnostic()?,
                render_workflow_run_evidence_json(target_evidence)?,
                source_workflow.id.clone(),
                render_workflow_spec_markdown(source_workflow),
                serde_json::to_string_pretty(source_reflection).into_diagnostic()?,
                render_workflow_run_evidence_json(source_evidence)?,
            ),
            &tuning,
            TraceOrigin::Sleep,
        )
        .await?;

        Ok(workflow_merge_planning_result_from_output(
            target_workflow,
            source_workflow,
            target_reflection,
            source_reflection,
            target_evidence,
            source_evidence,
            &outcome.output,
        ))
    }

    async fn replay_prompt_candidate(
        &self,
        context: &mut Context,
        candidate: &EvaluationArtifactRuntimePromptCandidate,
        failure_patterns: &[EvaluationArtifactFailurePattern],
        records: &[ProgramTraceRecord],
    ) -> Result<EvaluationArtifactRuntimePromptCandidateEvaluation> {
        let renderer = OpenAIToolRenderer;
        let program = PromptCandidateReplayJudgeProgram;
        let tuning = resolve_program_tuning(context, &program).await;
        let current_system_additions = context
            .compiled_prompts
            .runtime_system_additions()
            .join("\n");
        let candidate_json = serde_json::to_string_pretty(candidate).into_diagnostic()?;
        let failure_patterns_json =
            serde_json::to_string_pretty(failure_patterns).into_diagnostic()?;
        let replay_batches = select_prompt_replay_rollout_batches(records, 4, 3);
        let mut outputs = Vec::<PromptCandidateReplayJudgeOutput>::new();
        for replay_batch in replay_batches {
            let trace_evidence_summary = render_trace_replay_evidence_summary(&replay_batch);
            let outcome = execute_program_with_ir_report(
                context.judge_llm.as_ref(),
                context,
                &renderer,
                &program,
                program.dataset_ir(
                    current_system_additions.clone(),
                    candidate_json.clone(),
                    failure_patterns_json.clone(),
                    trace_evidence_summary,
                ),
                &tuning,
                TraceOrigin::Sleep,
            )
            .await?;
            outputs.push(outcome.output);
        }
        Ok(aggregate_prompt_replay_evaluation(
            EvaluationArtifactRuntimePromptCandidateEvaluation {
                compile_key: candidate.compile_key.clone(),
                candidate_title: candidate.title.clone(),
                rationale: String::new(),
                score: 0.0,
                accepted: false,
                selected: false,
                regressions_detected: 0,
                source_trace_ids: failure_patterns
                    .iter()
                    .flat_map(|pattern| pattern.supporting_trace_ids.clone())
                    .collect(),
            },
            &outputs,
        ))
    }

    async fn replay_workflow_frontier_entry(
        &self,
        context: &mut Context,
        entry: &WorkflowFrontierEntry,
        target_workflow: &WorkflowSpec,
        target_reflection: Option<&EvaluationArtifactWorkflowReflection>,
        target_evidence: &[WorkflowRunRecord],
        source_workflow: Option<&WorkflowSpec>,
        source_reflection: Option<&EvaluationArtifactWorkflowReflection>,
        source_evidence: &[WorkflowRunRecord],
    ) -> Result<EvaluationArtifactWorkflowCandidateEvaluation> {
        let renderer = OpenAIToolRenderer;
        let program = WorkflowCandidateReplayJudgeProgram;
        let tuning = resolve_program_tuning(context, &program).await;
        let candidate_json = serde_json::to_string_pretty(entry).into_diagnostic()?;
        let target_batches = select_workflow_replay_rollout_batches(target_evidence, 4, 3);
        let source_batches = select_workflow_replay_rollout_batches(source_evidence, 4, 3);
        let batch_count = target_batches.len().max(source_batches.len()).max(1);
        let target_workflow_spec = render_workflow_spec_markdown(target_workflow);
        let target_reflection_json =
            serde_json::to_string_pretty(&target_reflection.cloned()).into_diagnostic()?;
        let source_workflow_spec = source_workflow
            .map(render_workflow_spec_markdown)
            .unwrap_or_else(|| "none".to_string());
        let source_reflection_json =
            serde_json::to_string_pretty(&source_reflection.cloned()).into_diagnostic()?;
        let mut outputs = Vec::<WorkflowCandidateReplayJudgeOutput>::new();
        for index in 0..batch_count {
            let target_batch = target_batches
                .get(index)
                .cloned()
                .or_else(|| target_batches.last().cloned())
                .unwrap_or_default();
            let source_batch = source_batches
                .get(index)
                .cloned()
                .or_else(|| source_batches.last().cloned())
                .unwrap_or_default();
            let outcome = execute_program_with_ir_report(
                context.judge_llm.as_ref(),
                context,
                &renderer,
                &program,
                program.dataset_ir(
                    entry.candidate_kind.clone(),
                    target_workflow_spec.clone(),
                    target_reflection_json.clone(),
                    render_workflow_run_evidence_json(&target_batch)?,
                    source_workflow_spec.clone(),
                    source_reflection_json.clone(),
                    if source_workflow.is_some() {
                        render_workflow_run_evidence_json(&source_batch)?
                    } else {
                        "none".to_string()
                    },
                    candidate_json.clone(),
                ),
                &tuning,
                TraceOrigin::Sleep,
            )
            .await?;
            outputs.push(outcome.output);
        }
        Ok(aggregate_workflow_replay_evaluation(
            EvaluationArtifactWorkflowCandidateEvaluation {
                workflow_id: entry.evaluation.workflow_id.clone(),
                candidate_kind: entry.candidate_kind.clone(),
                candidate_title: entry.evaluation.candidate_title.clone(),
                rationale: String::new(),
                score: 0.0,
                accepted: false,
                selected: false,
                source_run_ids: target_evidence
                    .iter()
                    .chain(source_evidence.iter())
                    .map(|record| record.run_id.clone())
                    .collect(),
            },
            &outputs,
        ))
    }
}

async fn run_prompt_improvement_pipeline(
    context: &mut Context,
    planner: &dyn SleepPlannerRuntime,
    store: &EvaluationArtifactsStore,
    records: &[ProgramTraceRecord],
    consumed_trace_events: usize,
) -> Result<PromptImprovementSummary> {
    let failure_patterns = derive_failure_patterns(records);
    let PromptPlanningResult {
        reflections: prompt_reflections,
        candidates: prompt_candidates,
        evaluations: prompt_candidate_evaluations,
    } = planner
        .plan_prompt_improvement(context, &failure_patterns)
        .await?;

    let mut derived = derive_evaluation_artifacts(context, &failure_patterns).await?;
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
    prompt_frontier = replay_prompt_frontier_entries(
        context,
        planner,
        &prompt_frontier,
        &failure_patterns,
        records,
    )
    .await?;
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

async fn run_workflow_improvement_pipeline(
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

    Ok(WorkflowImprovementSummary {
        evidence_run_records: run_batch.records.len(),
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

struct PromptPatchUpdate {
    applied_system_additions: usize,
    compiled_prompt_updated: bool,
}

fn prompt_planning_result_from_output(
    output: &PromptEvolutionPlannerOutput,
    failure_patterns: &[EvaluationArtifactFailurePattern],
) -> PromptPlanningResult {
    let pattern_trace_ids = failure_patterns
        .iter()
        .flat_map(|pattern| pattern.supporting_trace_ids.clone())
        .collect::<Vec<_>>();
    let reflections = output
        .reflections
        .iter()
        .map(|reflection| EvaluationArtifactPromptReflection {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            title: reflection.title.trim().to_string(),
            rationale: reflection.rationale.trim().to_string(),
            missing_instructions: dedupe_vec(reflection.missing_instructions.clone()),
            over_constraints: dedupe_vec(reflection.over_constraints.clone()),
            source_trace_ids: pattern_trace_ids.clone(),
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
            prompt_patches: dedupe_vec(candidate.prompt_patches.clone()),
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
                source_trace_ids: pattern_trace_ids.clone(),
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

async fn replay_prompt_frontier_entries(
    context: &mut Context,
    planner: &dyn SleepPlannerRuntime,
    entries: &[crate::reasoning::frontier::PromptFrontierEntry],
    failure_patterns: &[EvaluationArtifactFailurePattern],
    records: &[ProgramTraceRecord],
) -> Result<Vec<crate::reasoning::frontier::PromptFrontierEntry>> {
    let mut replayed = Vec::new();
    for entry in entries {
        let mut updated = entry.clone();
        updated.evaluation = planner
            .replay_prompt_candidate(context, &updated.candidate, failure_patterns, records)
            .await?;
        replayed.push(updated);
    }
    Ok(replayed)
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
                &updated,
                target_workflow,
                target_reflection,
                target_evidence,
                source_workflow,
                source_reflection,
                source_evidence,
            )
            .await?;
        replayed.push(updated);
    }
    Ok(replayed)
}

fn infer_prompt_lineage(
    existing: &[crate::reasoning::frontier::PromptFrontierEntry],
    candidate: &EvaluationArtifactRuntimePromptCandidate,
) -> (Vec<String>, usize) {
    let candidate_set = candidate
        .prompt_patches
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let mut overlaps = existing
        .iter()
        .filter_map(|entry| {
            let entry_set = entry
                .candidate
                .prompt_patches
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            let intersection = candidate_set.intersection(&entry_set).count();
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

fn render_trace_replay_evidence_summary(records: &[ProgramTraceRecord]) -> String {
    let total_records = records.len();
    let failed_records = records
        .iter()
        .filter(|record| record.deserialization_error.is_some())
        .count();
    let suites = records
        .iter()
        .map(|record| record.program_name.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    format!("total_records={total_records}\nfailed_records={failed_records}\nprograms={suites}")
}

fn select_prompt_replay_rollout_batches(
    records: &[ProgramTraceRecord],
    batch_size: usize,
    max_batches: usize,
) -> Vec<Vec<ProgramTraceRecord>> {
    let mut ordered = records.to_vec();
    ordered.sort_by(|left, right| {
        right
            .timestamp_ms
            .cmp(&left.timestamp_ms)
            .then_with(|| right.attempt.cmp(&left.attempt))
    });
    let mut batches = ordered
        .chunks(batch_size.max(1))
        .take(max_batches.max(1))
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();
    if batches.is_empty() {
        batches.push(Vec::new());
    }
    batches
}

fn select_workflow_replay_rollout_batches(
    records: &[WorkflowRunRecord],
    batch_size: usize,
    max_batches: usize,
) -> Vec<Vec<WorkflowRunRecord>> {
    let mut ordered = records.to_vec();
    ordered.sort_by(|left, right| {
        right
            .ended_at_ms
            .cmp(&left.ended_at_ms)
            .then_with(|| right.started_at_ms.cmp(&left.started_at_ms))
    });
    let mut batches = ordered
        .chunks(batch_size.max(1))
        .take(max_batches.max(1))
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();
    if batches.is_empty() {
        batches.push(Vec::new());
    }
    batches
}

fn aggregate_prompt_replay_evaluation(
    mut base: EvaluationArtifactRuntimePromptCandidateEvaluation,
    outputs: &[PromptCandidateReplayJudgeOutput],
) -> EvaluationArtifactRuntimePromptCandidateEvaluation {
    if outputs.is_empty() {
        return base;
    }
    let accepted_count = outputs.iter().filter(|output| output.accepted).count();
    let score_sum = outputs.iter().map(|output| output.score).sum::<f64>();
    base.score = score_sum / outputs.len() as f64;
    base.accepted = accepted_count * 2 >= outputs.len();
    base.regressions_detected = outputs
        .iter()
        .map(|output| output.regressions_detected)
        .max()
        .unwrap_or(0);
    base.rationale = format!(
        "rollout_acceptance={}/{}; {}",
        accepted_count,
        outputs.len(),
        outputs
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, output)| format!("batch{}: {}", index + 1, output.reason.trim()))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    base
}

fn aggregate_workflow_replay_evaluation(
    mut base: EvaluationArtifactWorkflowCandidateEvaluation,
    outputs: &[WorkflowCandidateReplayJudgeOutput],
) -> EvaluationArtifactWorkflowCandidateEvaluation {
    if outputs.is_empty() {
        return base;
    }
    let accepted_count = outputs.iter().filter(|output| output.accepted).count();
    let score_sum = outputs.iter().map(|output| output.score).sum::<f64>();
    base.score = score_sum / outputs.len() as f64;
    base.accepted = accepted_count * 2 >= outputs.len();
    base.rationale = format!(
        "rollout_acceptance={}/{}; {}",
        accepted_count,
        outputs.len(),
        outputs
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, output)| format!("batch{}: {}", index + 1, output.reason.trim()))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    base
}

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
    save_compiled_runtime_system_prompt_for_model(&context.config.main_model.model_name, &compiled)
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

async fn apply_selected_prompt_frontier_candidate(
    context: &mut Context,
    frontier: &mut [crate::reasoning::frontier::PromptFrontierEntry],
    selected: Option<crate::reasoning::frontier::PromptFrontierEntry>,
) -> Result<PromptPatchUpdate> {
    let Some(selected_entry) = selected else {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    };
    if !prompt_candidate_has_novel_content(
        current_runtime_system_prompt_artifact_from_store(&context.compiled_prompts)
            .system_additions
            .as_slice(),
        &selected_entry.candidate,
    ) {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    }

    let update = apply_selected_prompt_candidate(
        context,
        std::slice::from_ref(&selected_entry.candidate),
        &mut [selected_entry.evaluation.clone()],
    )
    .await?;
    if update.compiled_prompt_updated {
        mark_prompt_frontier_selected(frontier, &selected_entry.key);
    }
    Ok(update)
}

fn derive_failure_patterns(
    records: &[ProgramTraceRecord],
) -> Vec<EvaluationArtifactFailurePattern> {
    let mut buckets: HashMap<(String, String), PatternAccumulator> = HashMap::new();

    for record in records {
        let Some(error) = record.deserialization_error.as_deref() else {
            continue;
        };

        let label = classify_failure(record, error);
        let description = describe_failure(record, error, &label);
        let suggested_fix_kind = suggested_fix_kind(&label);
        let trace_id = format!(
            "{}:{}:{}",
            record.program_name, record.timestamp_ms, record.attempt
        );

        let entry = buckets
            .entry((record.program_name.clone(), label.clone()))
            .or_insert_with(|| PatternAccumulator {
                suite: record.program_name.clone(),
                label,
                description,
                supporting_trace_ids: Vec::new(),
                frequency: 0,
                severity: 1,
                suggested_fix_kind,
            });

        entry.frequency += 1;
        if entry.supporting_trace_ids.len() < 8 {
            entry.supporting_trace_ids.push(trace_id);
        }
        entry.severity = entry.severity.max(derive_severity(error));
    }

    let mut patterns = buckets
        .into_values()
        .map(|bucket| EvaluationArtifactFailurePattern {
            suite: bucket.suite.clone(),
            pattern_id: format!("{}:{}", slugify(&bucket.suite), slugify(&bucket.label)),
            description: bucket.description,
            supporting_trace_ids: bucket.supporting_trace_ids,
            frequency: bucket.frequency,
            severity: bucket.severity,
            suggested_fix_kind: bucket.suggested_fix_kind,
        })
        .collect::<Vec<_>>();

    patterns.sort_by(|left, right| {
        right
            .frequency
            .cmp(&left.frequency)
            .then_with(|| right.severity.cmp(&left.severity))
            .then_with(|| left.pattern_id.cmp(&right.pattern_id))
    });

    patterns
}

struct PatternAccumulator {
    suite: String,
    label: String,
    description: String,
    supporting_trace_ids: Vec<String>,
    frequency: usize,
    severity: u8,
    suggested_fix_kind: EvaluationArtifactSuggestedFixKind,
}

fn classify_failure(_record: &ProgramTraceRecord, error: &str) -> String {
    if error.contains("provider_error") {
        return "provider_error".to_string();
    }
    if let Some(field) = extract_quoted_after(error, "missing field ") {
        return format!("missing_field:{field}");
    }
    if let Some(variant) = extract_quoted_after(error, "unknown variant ") {
        return format!("unknown_variant:{variant}");
    }
    if error.contains("invalid type") {
        return "invalid_type".to_string();
    }
    if error.contains("expected value") || error.contains("EOF while parsing") {
        return "malformed_json".to_string();
    }
    "deserialization_error".to_string()
}

fn describe_failure(record: &ProgramTraceRecord, error: &str, label: &str) -> String {
    match label {
        l if l.starts_with("missing_field:") => {
            let field = l.trim_start_matches("missing_field:");
            format!(
                "{} 在运行时输出缺少关键字段 `{}`，需要通过 demos、stress case 或 instruction 保持结构稳定。",
                record.program_name, field
            )
        }
        l if l.starts_with("unknown_variant:") => {
            let variant = l.trim_start_matches("unknown_variant:");
            format!(
                "{} 在运行时输出了未知枚举 `{}`，说明动作/分支边界仍有 schema 漂移。",
                record.program_name, variant
            )
        }
        "invalid_type" => format!(
            "{} 在运行时输出字段类型错误，说明当前候选对结构约束仍不稳定。",
            record.program_name
        ),
        "malformed_json" => format!(
            "{} 在运行时输出了无法解析的 JSON，说明输出格式稳定性不足。",
            record.program_name
        ),
        "provider_error" => format!(
            "{} 在运行时遇到 provider 级错误，需要区分接口兼容问题与程序语义问题。",
            record.program_name
        ),
        _ => format!(
            "{} 在运行时出现结构化输出失败：{}",
            record.program_name, error
        ),
    }
}

fn suggested_fix_kind(label: &str) -> EvaluationArtifactSuggestedFixKind {
    if label.starts_with("missing_field:") || label.starts_with("unknown_variant:") {
        return EvaluationArtifactSuggestedFixKind::StressCase;
    }
    if label == "resolve_chat_schema_drift" {
        return EvaluationArtifactSuggestedFixKind::Demo;
    }
    EvaluationArtifactSuggestedFixKind::Instruction
}

fn derive_severity(error: &str) -> u8 {
    if error.contains("provider_error") {
        3
    } else if error.contains("unknown variant") || error.contains("missing field") {
        2
    } else {
        1
    }
}

fn extract_quoted_after(text: &str, prefix: &str) -> Option<String> {
    let start = text.find(prefix)? + prefix.len();
    let rest = &text[start..];
    let first_quote = rest.find('`').or_else(|| rest.find('\''))?;
    let quote = rest.as_bytes()[first_quote] as char;
    let after = &rest[first_quote + 1..];
    let end = after.find(quote)?;
    Some(after[..end].to_string())
}

fn slugify(value: &str) -> String {
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

fn dedupe_vec(items: Vec<String>) -> Vec<String> {
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

fn dedupe_prompt_candidates(
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

fn dedupe_workflow_patches(
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

async fn derive_evaluation_artifacts(
    context: &mut Context,
    patterns: &[EvaluationArtifactFailurePattern],
) -> Result<DerivedEvaluationArtifacts> {
    if patterns.is_empty() {
        return Ok(DerivedEvaluationArtifacts {
            bootstrap_demos: Vec::new(),
            stress_cases: Vec::new(),
            instruction_hypotheses: Vec::new(),
            runtime_demos: Vec::new(),
            turn_demos: Vec::new(),
        });
    }

    let renderer = OpenAIToolRenderer;
    let program = EvaluationArtifactBuilderProgram;
    let tuning = resolve_program_tuning(context, &program).await;
    let mut bootstrap_demos = Vec::new();
    let mut stress_cases = Vec::new();
    let mut instruction_hypotheses = Vec::new();
    let mut runtime_demos = Vec::new();

    for pattern in patterns.iter().cloned() {
        let related_memories = recall_related_memories(context, &pattern.description, 3).await;
        let evidence_summary = render_related_memories(&related_memories);
        let available_canonical_cases = suite_reference_case_names(&pattern.suite);
        let outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(
                pattern.suite.clone(),
                pattern.pattern_id.clone(),
                pattern.description.clone(),
                pattern.frequency,
                pattern.severity,
                format!("{:?}", pattern.suggested_fix_kind),
                pattern.supporting_trace_ids.join("\n"),
                evidence_summary.clone().unwrap_or_else(|| "无".to_string()),
                available_canonical_cases.join("\n"),
            ),
            &tuning,
            TraceOrigin::Sleep,
        )
        .await?;

        if let Some(artifact) = to_instruction_hypothesis(&pattern, &outcome.output) {
            instruction_hypotheses.push(artifact);
        }
        if let Some(artifact) = to_bootstrap_demo(
            &pattern,
            &related_memories,
            evidence_summary.as_deref(),
            &outcome.output,
        ) {
            bootstrap_demos.push(artifact);
        }
        if let Some(artifact) = to_runtime_demo(
            &pattern,
            &related_memories,
            evidence_summary.as_deref(),
            &outcome.output,
        ) {
            runtime_demos.push(artifact);
        }
        if let Some(artifact) = to_stress_case(&pattern, &related_memories, &outcome.output) {
            stress_cases.push(artifact);
        }
    }

    Ok(DerivedEvaluationArtifacts {
        bootstrap_demos,
        stress_cases,
        instruction_hypotheses,
        runtime_demos,
        turn_demos: Vec::new(),
    })
}

async fn optimize_workflows_from_run_records(
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
                    target,
                    target_reflection,
                    &target_evidence,
                    source,
                    source_reflection,
                    &source_evidence,
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
    workflow_frontier = replay_workflow_frontier_entries(
        context,
        planner,
        &workflow_frontier,
        &all_workflows,
        &reflection_by_workflow,
        &evidence_by_workflow,
    )
    .await?;
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

fn evaluate_workflow_patch_candidate(
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

fn evaluate_workflow_merge_candidate(
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

fn total_patch_additions(patch: &EvaluationArtifactWorkflowPatch) -> usize {
    patch.when_to_use_additions.len()
        + patch.precondition_additions.len()
        + patch.workflow_step_additions.len()
        + patch.done_criteria_additions.len()
        + patch.recovery_additions.len()
}

fn has_workflow_patch_content(patch: &EvaluationArtifactWorkflowPatch) -> bool {
    total_patch_additions(patch) > 0
}

fn patch_has_novel_content(
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

fn prompt_candidate_has_novel_content(
    existing_additions: &[String],
    candidate: &EvaluationArtifactRuntimePromptCandidate,
) -> bool {
    candidate
        .prompt_patches
        .iter()
        .any(|patch| !existing_additions.iter().any(|existing| existing == patch))
}

fn workflow_merge_title(merge: &EvaluationArtifactWorkflowMerge) -> String {
    format!(
        "{}<-{}",
        merge.target_workflow_id,
        merge.source_workflow_ids.join("+")
    )
}

fn render_related_memories(related_memories: &[String]) -> Option<String> {
    if related_memories.is_empty() {
        return None;
    }
    Some(
        related_memories
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, memory)| format!("{}. {}", index + 1, memory.trim()))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn render_input_summary(
    pattern: &EvaluationArtifactFailurePattern,
    evidence_summary: Option<&str>,
) -> String {
    match evidence_summary {
        Some(evidence) => format!(
            "failure pattern: {}\n相关 L2 记忆：\n{}",
            pattern.description, evidence
        ),
        None => format!("failure pattern: {}", pattern.description),
    }
}

fn suite_reference_case_names(suite: &str) -> Vec<String> {
    let _ = suite;
    Vec::new()
}

fn to_instruction_hypothesis(
    pattern: &EvaluationArtifactFailurePattern,
    output: &EvaluationArtifactBuilderOutput,
) -> Option<EvaluationArtifactInstructionHypothesis> {
    if !output.create_instruction_hypothesis || output.instruction_text.trim().is_empty() {
        return None;
    }
    Some(EvaluationArtifactInstructionHypothesis {
        suite: pattern.suite.clone(),
        text: output.instruction_text.trim().to_string(),
        justification: output.reason.trim().to_string(),
        source_pattern_ids: vec![pattern.pattern_id.clone()],
    })
}

fn to_bootstrap_demo(
    pattern: &EvaluationArtifactFailurePattern,
    related_memories: &[String],
    evidence_summary: Option<&str>,
    output: &EvaluationArtifactBuilderOutput,
) -> Option<EvaluationArtifactBootstrapDemo> {
    if !output.create_bootstrap_demo
        || output.bootstrap_demo_title.trim().is_empty()
        || output.reference_case_names.is_empty()
    {
        return None;
    }
    Some(EvaluationArtifactBootstrapDemo {
        suite: pattern.suite.clone(),
        title: output.bootstrap_demo_title.trim().to_string(),
        input_summary: render_input_summary(pattern, evidence_summary),
        inputs: vec![ExampleField {
            name: "evaluation artifact summary".to_string(),
            value: render_input_summary(pattern, evidence_summary),
        }],
        expected_output: json!({
            "suite": pattern.suite,
            "pattern_id": pattern.pattern_id,
            "target": "avoid_failure_pattern",
            "summary": output.bootstrap_demo_summary.trim(),
            "related_memories": related_memories,
        }),
        reference_case_names: output.reference_case_names.clone(),
        source_trace_ids: pattern.supporting_trace_ids.clone(),
        confidence: output.confidence.clamp(0.0, 1.0) as f32,
    })
}

fn to_runtime_demo(
    pattern: &EvaluationArtifactFailurePattern,
    related_memories: &[String],
    evidence_summary: Option<&str>,
    output: &EvaluationArtifactBuilderOutput,
) -> Option<EvaluationArtifactRuntimeDemo> {
    if !output.create_bootstrap_demo
        || output.bootstrap_demo_title.trim().is_empty()
        || output.bootstrap_demo_summary.trim().is_empty()
    {
        return None;
    }
    Some(EvaluationArtifactRuntimeDemo {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        title: output.bootstrap_demo_title.trim().to_string(),
        scenario_summary: render_input_summary(pattern, evidence_summary),
        inputs: vec![ExampleField {
            name: "sleep target".to_string(),
            value: render_input_summary(pattern, evidence_summary),
        }],
        expected_behavior: output.bootstrap_demo_summary.trim().to_string(),
        judge_focus: output
            .reference_case_names
            .iter()
            .map(|name| format!("align with canonical case `{name}`"))
            .chain(
                related_memories
                    .iter()
                    .take(1)
                    .map(|memory| format!("use recalled precedent: {}", memory.trim())),
            )
            .collect(),
        source_trace_ids: pattern.supporting_trace_ids.clone(),
        confidence: output.confidence.clamp(0.0, 1.0) as f32,
    })
}

fn to_stress_case(
    pattern: &EvaluationArtifactFailurePattern,
    related_memories: &[String],
    output: &EvaluationArtifactBuilderOutput,
) -> Option<EvaluationArtifactStressCase> {
    if !output.create_stress_case
        || output.stress_case_name.trim().is_empty()
        || output.reference_case_names.is_empty()
    {
        return None;
    }
    Some(EvaluationArtifactStressCase {
        suite: pattern.suite.clone(),
        name: output.stress_case_name.trim().to_string(),
        input_ir: json!({
            "suite": pattern.suite,
            "pattern_id": pattern.pattern_id,
            "description": pattern.description,
            "related_memories": related_memories,
        }),
        expected_constraints: output.stress_constraints.clone(),
        reference_case_names: output.reference_case_names.clone(),
        source_pattern_id: pattern.pattern_id.clone(),
        repeat: pattern.frequency.max(2),
        weight: usize::from(pattern.severity.max(1)),
    })
}

fn derive_success_bootstrap_demos(
    records: &[ProgramTraceRecord],
) -> Vec<EvaluationArtifactBootstrapDemo> {
    let mut per_suite = std::collections::HashMap::<String, usize>::new();
    let mut demos = Vec::new();

    for record in records {
        if record.deserialization_error.is_some() || record.attempt != 1 {
            continue;
        }
        let Some(parsed_output) = record.parsed_output.clone() else {
            continue;
        };
        let Some(suite) = infer_runtime_suite(record) else {
            continue;
        };
        let inputs = extract_inputs_from_request(&record.request);
        if inputs.is_empty() {
            continue;
        }
        let count = per_suite.entry(suite.clone()).or_insert(0);
        if *count >= 3 {
            continue;
        }
        *count += 1;
        demos.push(EvaluationArtifactBootstrapDemo {
            suite,
            title: format!("Sleep success trace {} #{}", record.program_name, count),
            input_summary: inputs
                .iter()
                .map(|field| format!("{}: {}", field.name, field.value))
                .collect::<Vec<_>>()
                .join("\n"),
            inputs,
            expected_output: parsed_output,
            reference_case_names: Vec::new(),
            source_trace_ids: vec![format!(
                "{}:{}:{}",
                record.program_name, record.timestamp_ms, record.attempt
            )],
            confidence: 0.8,
        });
    }

    demos
}

fn infer_runtime_suite(_record: &ProgramTraceRecord) -> Option<String> {
    None
}

fn extract_inputs_from_request(request: &PromptRequest) -> Vec<ExampleField> {
    let mut inputs = Vec::new();
    for message in request.all_messages() {
        if !message.is_user() {
            continue;
        }
        inputs.extend(parse_user_sections(
            message.text_content().unwrap_or_default(),
        ));
    }
    inputs
}

fn parse_user_sections(content: &str) -> Vec<ExampleField> {
    let mut fields = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_body = String::new();

    for line in content.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            flush_section(&mut fields, &mut current_title, &mut current_body);
            current_title = Some(title.trim().to_string());
        } else {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        }
    }
    flush_section(&mut fields, &mut current_title, &mut current_body);

    fields
}

fn flush_section(
    fields: &mut Vec<ExampleField>,
    current_title: &mut Option<String>,
    current_body: &mut String,
) {
    let Some(title) = current_title.take() else {
        current_body.clear();
        return;
    };
    let trimmed = current_body.trim();
    if trimmed.is_empty() {
        current_body.clear();
        return;
    }
    if matches!(title.as_str(), "程序签名" | "示例") {
        current_body.clear();
        return;
    }
    fields.push(ExampleField {
        name: title,
        value: trimmed.to_string(),
    });
    current_body.clear();
}

async fn recall_related_memories(context: &Context, query: &str, top_k: usize) -> Vec<String> {
    let observations = context
        .hindsight
        .recall(
            query,
            HindsightRecallOptions {
                types: vec!["observation".to_string()],
                max_tokens: 900,
                budget: Some("low".to_string()),
                include_source_facts: false,
                ..Default::default()
            },
        )
        .await;
    let mut collected = match observations {
        Ok(response) => response
            .results
            .into_iter()
            .take(top_k)
            .map(|item| item.text)
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    if collected.len() >= top_k {
        return collected;
    }

    let response = context
        .hindsight
        .recall(
            query,
            HindsightRecallOptions {
                types: vec!["world".to_string(), "experience".to_string()],
                max_tokens: 1200,
                budget: Some("low".to_string()),
                include_source_facts: true,
                max_source_facts_tokens: 1200,
                ..Default::default()
            },
        )
        .await;
    let Ok(response) = response else {
        return collected;
    };
    collected.extend(
        response
            .results
            .into_iter()
            .take(top_k.saturating_sub(collected.len()))
            .map(|item| item.text),
    );
    collected
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    use crate::reasoning::programs::workflow_evolution_planner::{
        WorkflowEvolutionPlannerOutput, WorkflowPlannerCandidateEvaluation,
        WorkflowPlannerPatchCandidate, WorkflowPlannerReflection,
    };
    use crate::reasoning::{
        runtime::PromptRequest,
        signature::Signature,
        trace::{ProgramTraceRecord, TraceOrigin},
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
                        "执行前确认关键依赖、输入和权限条件都已满足".to_string(),
                    ],
                    weak_workflow_steps: vec![
                        "进行手工修复前，先固定修复前提并规划复验步骤".to_string(),
                    ],
                    weak_done_criteria: vec![],
                    weak_recovery: vec![
                        "出现阻塞时，先回退到上一个稳定步骤，再重新验证关键前提".to_string(),
                    ],
                    recurring_failure_patterns: vec!["tool_failure".to_string()],
                    confidence: 0.88,
                },
                patch_candidates: vec![WorkflowPlannerPatchCandidate {
                    title: "repair flaky test patch".to_string(),
                    rationale: "add recovery and manual-fix guardrails".to_string(),
                    when_to_use_additions: vec![],
                    precondition_additions: vec![
                        "执行前确认关键依赖、输入和权限条件都已满足".to_string(),
                    ],
                    workflow_step_additions: vec![
                        "进行手工修复前，先固定修复前提并规划复验步骤".to_string(),
                    ],
                    done_criteria_additions: vec![],
                    recovery_additions: vec![
                        "出现阻塞时，先回退到上一个稳定步骤，再重新验证关键前提".to_string(),
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
                .any(|step| step.contains("手工修复") || step.contains("阻塞"))
                || updated
                    .recovery
                    .iter()
                    .any(|step| step.contains("阻塞") || step.contains("标准恢复路径"))
        );
    }

    #[test]
    fn prompt_replay_rollout_batches_chunk_recent_records() {
        let records = (0..10)
            .map(|index| ProgramTraceRecord {
                timestamp_ms: index,
                origin: TraceOrigin::Runtime,
                program_name: format!("program-{index}"),
                attempt: index as usize,
                signature: Signature::new("test_program"),
                request: PromptRequest {
                    tool_name: "test_tool".to_string(),
                    tool_description: "test description".to_string(),
                    output_schema: json!({ "type": "object" }),
                    system_messages: Vec::new(),
                    long_term_memory_messages: Vec::new(),
                    history_messages: Vec::new(),
                    current_user_message: format!("message-{index}"),
                    retry_messages: Vec::new(),
                },
                raw_response: json!({ "index": index }),
                parsed_output: None,
                deserialization_error: None,
            })
            .collect::<Vec<_>>();

        let selected = select_prompt_replay_rollout_batches(&records, 3, 2);
        let timestamps = selected
            .iter()
            .map(|batch| {
                batch
                    .iter()
                    .map(|record| record.timestamp_ms)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(timestamps, vec![vec![9, 8, 7], vec![6, 5, 4]]);
    }

    #[test]
    fn workflow_replay_rollout_batches_chunk_recent_runs() {
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

        let selected = select_workflow_replay_rollout_batches(&records, 4, 2);
        let run_ids = selected
            .iter()
            .map(|batch| {
                batch
                    .iter()
                    .map(|record| record.run_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            run_ids,
            vec![
                vec!["run-9", "run-8", "run-7", "run-6"],
                vec!["run-5", "run-4", "run-3", "run-2"]
            ]
        );
    }
}
