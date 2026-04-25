use super::*;

pub(super) struct WorkflowMergePlanningInput<'a> {
    pub target_workflow: &'a WorkflowSpec,
    pub target_reflection: &'a EvaluationArtifactWorkflowReflection,
    pub target_evidence: &'a [WorkflowRunRecord],
    pub source_workflow: &'a WorkflowSpec,
    pub source_reflection: &'a EvaluationArtifactWorkflowReflection,
    pub source_evidence: &'a [WorkflowRunRecord],
}

pub(super) struct WorkflowFrontierReplayInput<'a> {
    pub entry: &'a WorkflowFrontierEntry,
    pub target_workflow: &'a WorkflowSpec,
    pub target_reflection: Option<&'a EvaluationArtifactWorkflowReflection>,
    pub target_evidence: &'a [WorkflowRunRecord],
    pub source_workflow: Option<&'a WorkflowSpec>,
    pub source_reflection: Option<&'a EvaluationArtifactWorkflowReflection>,
    pub source_evidence: &'a [WorkflowRunRecord],
}

#[async_trait]
pub(super) trait SleepPlannerRuntime: Send + Sync {
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
        input: WorkflowMergePlanningInput<'_>,
    ) -> Result<WorkflowMergePlanningResult>;

    async fn replay_prompt_candidate(
        &self,
        context: &mut Context,
        candidate: &EvaluationArtifactRuntimePromptCandidate,
        failure_patterns: &[EvaluationArtifactFailurePattern],
        turn_demos: &[EvaluationArtifactTurnDemo],
    ) -> Result<EvaluationArtifactRuntimePromptCandidateEvaluation>;

    async fn replay_workflow_frontier_entry(
        &self,
        context: &mut Context,
        input: WorkflowFrontierReplayInput<'_>,
    ) -> Result<EvaluationArtifactWorkflowCandidateEvaluation>;
}

pub(super) struct LlmSleepPlannerRuntime;

async fn load_runtime_trace_records() -> Result<RuntimeTraceBatch> {
    load_runtime_trace_batch().await
}

pub(super) async fn load_sleep_inputs() -> Result<SleepInputs> {
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
        input: WorkflowMergePlanningInput<'_>,
    ) -> Result<WorkflowMergePlanningResult> {
        let WorkflowMergePlanningInput {
            target_workflow,
            target_reflection,
            target_evidence,
            source_workflow,
            source_reflection,
            source_evidence,
        } = input;

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
        turn_demos: &[EvaluationArtifactTurnDemo],
    ) -> Result<EvaluationArtifactRuntimePromptCandidateEvaluation> {
        let rollout_demos = select_prompt_rollout_demos(turn_demos, 8);
        let evaluations = evaluate_runtime_prompt_candidate_rollout(
            context.config.clone(),
            context.compiled_prompts.clone(),
            candidate,
            &rollout_demos,
        )
        .await?;
        Ok(aggregate_prompt_executable_rollout_evaluation(
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
            &evaluations,
        ))
    }

    async fn replay_workflow_frontier_entry(
        &self,
        context: &mut Context,
        input: WorkflowFrontierReplayInput<'_>,
    ) -> Result<EvaluationArtifactWorkflowCandidateEvaluation> {
        let WorkflowFrontierReplayInput {
            entry,
            target_workflow,
            target_reflection,
            target_evidence,
            source_workflow,
            source_reflection,
            source_evidence,
        } = input;

        let renderer = OpenAIToolRenderer;
        let program = WorkflowCandidateRolloutEvaluatorProgram;
        let tuning = resolve_program_tuning(context, &program).await;
        let candidate_json = serde_json::to_string_pretty(entry).into_diagnostic()?;
        let rollout =
            execute_workflow_candidate_rollout(context, entry, target_workflow, source_workflow)
                .await?;
        let target_task_cases = select_workflow_task_cases(
            &target_evidence
                .iter()
                .map(workflow_task_case_from_record)
                .collect::<Vec<_>>(),
            8,
        );
        let source_task_cases = select_workflow_task_cases(
            &source_evidence
                .iter()
                .map(workflow_task_case_from_record)
                .collect::<Vec<_>>(),
            8,
        );
        let case_count = target_task_cases.len().max(source_task_cases.len()).max(1);
        let target_workflow_spec = render_workflow_spec_markdown(&rollout.target_workflow);
        let target_reflection_json =
            serde_json::to_string_pretty(&target_reflection.cloned()).into_diagnostic()?;
        let source_workflow_spec = source_workflow
            .map(render_workflow_spec_markdown)
            .unwrap_or_else(|| "none".to_string());
        let source_reflection_json =
            serde_json::to_string_pretty(&source_reflection.cloned()).into_diagnostic()?;
        let mut outputs = Vec::<WorkflowCandidateRolloutEvaluatorOutput>::new();
        for index in 0..case_count {
            let target_task = target_task_cases
                .get(index)
                .cloned()
                .or_else(|| target_task_cases.last().cloned())
                .unwrap_or_else(blank_workflow_task_case);
            let rolled_out_target_case =
                run_workflow_task_rollout(&rollout.target_workflow, &target_task);
            let source_task = source_task_cases
                .get(index)
                .cloned()
                .or_else(|| source_task_cases.last().cloned())
                .unwrap_or_else(blank_workflow_task_case);
            let rolled_out_source_case =
                source_workflow.map(|workflow| run_workflow_task_rollout(workflow, &source_task));
            let outcome = execute_program_with_ir_report(
                context.judge_llm.as_ref(),
                context,
                &renderer,
                &program,
                program.dataset_ir(
                    entry.candidate_kind.clone(),
                    target_workflow_spec.clone(),
                    format!("{} | {}", rollout.summary, rolled_out_target_case.summary),
                    target_reflection_json.clone(),
                    render_workflow_rollout_case_json(&rolled_out_target_case)?,
                    source_workflow_spec.clone(),
                    source_reflection_json.clone(),
                    if let Some(source_case) = rolled_out_source_case.as_ref() {
                        render_workflow_rollout_case_json(source_case)?
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
