use super::*;

pub(super) struct WorkflowExecutableRolloutResult {
    pub(super) target_workflow: WorkflowSpec,
    pub(super) summary: String,
}

#[derive(serde::Serialize)]
pub(super) struct WorkflowTaskRolloutCase {
    pub(super) record: WorkflowRunRecord,
    pub(super) executed_steps: Vec<WorkflowTaskRolloutStep>,
    pub(super) boundary_events: Vec<String>,
    pub(super) summary: String,
}

#[derive(Clone, serde::Serialize)]
pub(super) struct WorkflowTaskRolloutStep {
    pub(super) step_index: usize,
    pub(super) step: String,
    pub(super) status: String,
    pub(super) evidence: String,
}

struct WorkflowTaskRolloutOutput {
    output: AgentLoopStepOutput,
    turn_increment: usize,
    tool_action_increment: usize,
    manual_fix_detected: bool,
    rollback_detected: bool,
    failure_types: Vec<String>,
    final_summary: Option<String>,
}

#[derive(Clone)]
pub(super) struct WorkflowTaskCase {
    pub(super) task_summary: String,
    pub(super) origin: String,
    pub(super) baseline_outcome: crate::workflow::WorkflowRunOutcome,
    pub(super) baseline_turns: usize,
    pub(super) baseline_tool_actions: usize,
    pub(super) manual_fix_detected: bool,
    pub(super) rollback_detected: bool,
    pub(super) failure_types: Vec<String>,
    pub(super) started_at_ms: i64,
    pub(super) ended_at_ms: i64,
    #[cfg(test)]
    pub(super) baseline_run_id: String,
}

#[derive(Default)]
struct WorkflowTaskRolloutRunnerState {
    bound_workflow_id: Option<String>,
    active_workflow_run: Option<ActiveWorkflowRunSession>,
    pending_workflow_run_flushes: Vec<PendingWorkflowRunFlush>,
    current_work_origin: Option<String>,
}

impl WorkflowTaskRolloutRunnerState {
    fn begin_bound_workflow_session(&mut self, workflow: &WorkflowSpec, task: &WorkflowTaskCase) {
        self.bound_workflow_id = Some(workflow.id.clone());
        self.current_work_origin = Some(task.origin.clone());
        if self
            .active_workflow_run
            .as_ref()
            .is_some_and(|session| session.workflow_id == workflow.id)
        {
            return;
        }
        self.active_workflow_run = Some(ActiveWorkflowRunSession {
            run_id: format!("workflow-rollout:{}", uuid::Uuid::new_v4()),
            workflow_id: workflow.id.clone(),
            started_at_ms: task.started_at_ms,
            origin: self
                .current_work_origin
                .clone()
                .unwrap_or_else(|| "workflow_rollout".to_string()),
            turn_count: 0,
            tool_action_count: 0,
            manual_fix_detected: false,
            rollback_detected: false,
            failure_types: BTreeSet::new(),
            final_summary: String::new(),
        });
    }

    fn accumulate_task(&mut self, workflow: &WorkflowSpec, task: &WorkflowTaskCase) {
        let Some(session) = self.active_workflow_run.as_mut() else {
            return;
        };
        if session.workflow_id != workflow.id {
            return;
        }
        let executed_steps = simulated_executed_workflow_steps(workflow, task);
        let outputs = workflow_rollout_outputs_from_task(workflow, task, &executed_steps);
        for rollout_output in &outputs {
            accumulate_workflow_rollout_session_from_output(session, rollout_output);
        }
    }

    fn queue_active_workflow_run_for_flush(
        &mut self,
        outcome: crate::workflow::WorkflowRunOutcome,
    ) {
        if let Some(session) = self.active_workflow_run.take() {
            self.pending_workflow_run_flushes
                .push(PendingWorkflowRunFlush { session, outcome });
        }
    }

    fn flush_records(&mut self, ended_at_ms: i64) -> Vec<WorkflowRunRecord> {
        self.pending_workflow_run_flushes
            .drain(..)
            .map(|flush| workflow_rollout_record_from_pending_flush(flush, ended_at_ms))
            .collect()
    }
}

pub(super) fn aggregate_workflow_replay_evaluation(
    mut base: EvaluationArtifactWorkflowCandidateEvaluation,
    outputs: &[WorkflowCandidateRolloutEvaluatorOutput],
) -> EvaluationArtifactWorkflowCandidateEvaluation {
    if outputs.is_empty() {
        return base;
    }
    let accepted_count = outputs.iter().filter(|output| output.accepted_case).count();
    let improvement_count = outputs
        .iter()
        .filter(|output| output.improves_upon_baseline)
        .count();
    let regression_count = outputs
        .iter()
        .filter(|output| output.regression_risk)
        .count();
    let score_sum = outputs.iter().map(|output| output.score).sum::<f64>();
    base.score = score_sum / outputs.len() as f64;
    base.accepted = accepted_count * 2 >= outputs.len() && improvement_count >= regression_count;
    base.rationale = format!(
        "rollout_acceptance={}/{}; improvements={} regressions={}; {}",
        accepted_count,
        outputs.len(),
        improvement_count,
        regression_count,
        outputs
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, output)| format!("case{}: {}", index + 1, output.reason.trim()))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    base
}

pub(super) fn render_workflow_rollout_case_json(case: &WorkflowTaskRolloutCase) -> Result<String> {
    serde_json::to_string_pretty(case).into_diagnostic()
}

fn blank_workflow_run_record() -> WorkflowRunRecord {
    WorkflowRunRecord {
        run_id: "none".to_string(),
        workflow_id: "none".to_string(),
        started_at_ms: 0,
        ended_at_ms: 0,
        origin: "none".to_string(),
        outcome: crate::workflow::WorkflowRunOutcome::NoProgress,
        turn_count: 0,
        tool_action_count: 0,
        manual_fix_detected: false,
        rollback_detected: false,
        failure_types: Vec::new(),
        final_summary: "none".to_string(),
    }
}

pub(super) fn blank_workflow_task_case() -> WorkflowTaskCase {
    WorkflowTaskCase {
        task_summary: String::new(),
        origin: "workflow_rollout".to_string(),
        baseline_outcome: crate::workflow::WorkflowRunOutcome::NoProgress,
        baseline_turns: 0,
        baseline_tool_actions: 0,
        manual_fix_detected: false,
        rollback_detected: false,
        failure_types: Vec::new(),
        started_at_ms: 0,
        ended_at_ms: 0,
        #[cfg(test)]
        baseline_run_id: "none".to_string(),
    }
}

pub(super) fn workflow_task_case_from_record(record: &WorkflowRunRecord) -> WorkflowTaskCase {
    WorkflowTaskCase {
        task_summary: record.final_summary.clone(),
        origin: record.origin.clone(),
        baseline_outcome: record.outcome,
        baseline_turns: record.turn_count,
        baseline_tool_actions: record.tool_action_count,
        manual_fix_detected: record.manual_fix_detected,
        rollback_detected: record.rollback_detected,
        failure_types: record.failure_types.clone(),
        started_at_ms: record.started_at_ms,
        ended_at_ms: record.ended_at_ms,
        #[cfg(test)]
        baseline_run_id: record.run_id.clone(),
    }
}

pub(super) fn select_workflow_task_cases(
    cases: &[WorkflowTaskCase],
    max_cases: usize,
) -> Vec<WorkflowTaskCase> {
    let mut ordered = cases.to_vec();
    ordered.sort_by(|left, right| {
        right
            .ended_at_ms
            .cmp(&left.ended_at_ms)
            .then_with(|| right.started_at_ms.cmp(&left.started_at_ms))
    });
    ordered.truncate(max_cases);
    ordered
}

fn workflow_rollout_detect_runtime_rollback(output: &AgentLoopStepOutput) -> bool {
    let text = format!(
        "{}\n{}\n{}",
        output.description,
        output.observation,
        output
            .actions
            .iter()
            .map(|action| action.summary.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    )
    .to_ascii_lowercase();
    text.contains("rollback") || text.contains("回滚") || text.contains("revert")
}

fn workflow_rollout_detect_runtime_manual_fix(output: &AgentLoopStepOutput) -> bool {
    output.actions.iter().any(|action| {
        matches!(
            action.kind.as_str(),
            "apply_patch" | "terminal_exec" | "terminal_write_stdin"
        )
    })
}

fn workflow_rollout_classify_runtime_failure_type(output: &AgentLoopStepOutput) -> Option<String> {
    let text = format!("{}\n{}", output.description, output.observation).to_ascii_lowercase();
    if text.contains("timeout") || text.contains("超时") {
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
    if text.contains("error") || text.contains("失败") {
        return Some("runtime_error".to_string());
    }
    None
}

fn workflow_rollout_tool_action_count(output: &AgentLoopStepOutput) -> usize {
    output
        .actions
        .iter()
        .filter(|action| {
            !matches!(
                action.kind.as_str(),
                "assistant_message" | "empty_tool_calls"
            )
        })
        .count()
}

fn workflow_rollout_run_summary(output: &AgentLoopStepOutput) -> String {
    format!(
        "{} | {} | {}",
        output.current_doing.trim(),
        output.description.trim(),
        output.observation.trim()
    )
}

fn distribute_rollout_count(total: usize, buckets: usize, index: usize) -> usize {
    if buckets == 0 {
        return 0;
    }
    let base = total / buckets;
    let remainder = total % buckets;
    base + usize::from(index < remainder)
}

fn workflow_rollout_outputs_from_task(
    workflow: &WorkflowSpec,
    task: &WorkflowTaskCase,
    executed_steps: &[WorkflowTaskRolloutStep],
) -> Vec<WorkflowTaskRolloutOutput> {
    if executed_steps.is_empty() {
        return vec![WorkflowTaskRolloutOutput {
            output: AgentLoopStepOutput {
                observation: format!(
                    "workflow rollout replay for {} | {}",
                    workflow.id, task.task_summary
                ),
                description: format!(
                    "workflow rollout boundary for {} outcome={:?} origin={}",
                    workflow.id, task.baseline_outcome, task.origin
                ),
                current_doing: task.task_summary.clone(),
                actions: Vec::new(),
            },
            turn_increment: task.baseline_turns.max(1),
            tool_action_increment: task.baseline_tool_actions,
            manual_fix_detected: task.manual_fix_detected,
            rollback_detected: task.rollback_detected,
            failure_types: task.failure_types.clone(),
            final_summary: Some(task.task_summary.clone()),
        }];
    }

    executed_steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let turn_increment =
                distribute_rollout_count(task.baseline_turns.max(1), executed_steps.len(), index);
            let tool_action_increment =
                distribute_rollout_count(task.baseline_tool_actions, executed_steps.len(), index);
            let is_boundary = index + 1 == executed_steps.len();
            let mut actions = Vec::with_capacity(tool_action_increment.max(usize::from(
                is_boundary && (task.manual_fix_detected || task.rollback_detected),
            )));
            for action_index in 0..tool_action_increment {
                let (kind, summary) =
                    if is_boundary && action_index == 0 && task.manual_fix_detected {
                        (
                            "terminal_exec".to_string(),
                            format!("manual fix step executed for {}", workflow.id),
                        )
                    } else if is_boundary && action_index == 0 && task.rollback_detected {
                        (
                            "tool_call".to_string(),
                            format!("rollback executed while running {}", workflow.id),
                        )
                    } else {
                        (
                            "tool_call".to_string(),
                            format!(
                                "workflow tool step {}.{} for {}",
                                step.step_index,
                                action_index + 1,
                                workflow.id
                            ),
                        )
                    };
                actions.push(EpisodeActionRecord { kind, summary });
            }
            if actions.is_empty() && is_boundary && task.manual_fix_detected {
                actions.push(EpisodeActionRecord {
                    kind: "terminal_exec".to_string(),
                    summary: format!("manual fix step executed for {}", workflow.id),
                });
            }
            if is_boundary
                && task.rollback_detected
                && !actions
                    .iter()
                    .any(|action| action.summary.contains("rollback"))
            {
                actions.push(EpisodeActionRecord {
                    kind: "tool_call".to_string(),
                    summary: format!("rollback executed while running {}", workflow.id),
                });
            }
            let mut observation_lines = vec![
                format!("workflow rollout replay for {}", workflow.id),
                step.evidence.clone(),
            ];
            if is_boundary && !task.failure_types.is_empty() {
                observation_lines.push(format!("failure_types={}", task.failure_types.join(",")));
            }
            if is_boundary && task.rollback_detected {
                observation_lines.push("rollback detected".to_string());
            }
            if is_boundary && task.manual_fix_detected {
                observation_lines.push("manual fix detected".to_string());
            }
            WorkflowTaskRolloutOutput {
                output: AgentLoopStepOutput {
                    observation: observation_lines.join(" | "),
                    description: format!(
                        "workflow rollout step {}/{} for {} status={} origin={} outcome={:?}",
                        step.step_index,
                        executed_steps.len(),
                        workflow.id,
                        step.status,
                        task.origin,
                        task.baseline_outcome
                    ),
                    current_doing: step.step.clone(),
                    actions,
                },
                turn_increment,
                tool_action_increment,
                manual_fix_detected: is_boundary && task.manual_fix_detected,
                rollback_detected: is_boundary && task.rollback_detected,
                failure_types: if is_boundary {
                    task.failure_types.clone()
                } else {
                    Vec::new()
                },
                final_summary: is_boundary.then(|| task.task_summary.clone()),
            }
        })
        .collect()
}

fn accumulate_workflow_rollout_session_from_output(
    session: &mut ActiveWorkflowRunSession,
    rollout_output: &WorkflowTaskRolloutOutput,
) {
    let output = &rollout_output.output;
    session.turn_count = session
        .turn_count
        .saturating_add(rollout_output.turn_increment);
    session.tool_action_count = session
        .tool_action_count
        .saturating_add(rollout_output.tool_action_increment);
    session.manual_fix_detected |=
        rollout_output.manual_fix_detected || workflow_rollout_detect_runtime_manual_fix(output);
    session.rollback_detected |=
        rollout_output.rollback_detected || workflow_rollout_detect_runtime_rollback(output);
    if rollout_output.failure_types.is_empty() {
        if let Some(failure_type) = workflow_rollout_classify_runtime_failure_type(output) {
            session.failure_types.insert(failure_type);
        }
    } else {
        session
            .failure_types
            .extend(rollout_output.failure_types.iter().cloned());
    }
    if session.tool_action_count == 0 {
        session.tool_action_count = workflow_rollout_tool_action_count(output);
    }
    if let Some(final_summary) = rollout_output.final_summary.as_deref() {
        session.final_summary = if final_summary.trim().is_empty() {
            workflow_rollout_run_summary(output)
        } else {
            final_summary.to_string()
        };
    } else if session.final_summary.is_empty() {
        session.final_summary = workflow_rollout_run_summary(output);
    }
}

fn workflow_rollout_record_from_pending_flush(
    flush: PendingWorkflowRunFlush,
    ended_at_ms: i64,
) -> WorkflowRunRecord {
    WorkflowRunRecord {
        run_id: flush.session.run_id,
        workflow_id: flush.session.workflow_id,
        started_at_ms: flush.session.started_at_ms,
        ended_at_ms,
        origin: flush.session.origin,
        outcome: flush.outcome,
        turn_count: flush.session.turn_count,
        tool_action_count: flush.session.tool_action_count,
        manual_fix_detected: flush.session.manual_fix_detected,
        rollback_detected: flush.session.rollback_detected,
        failure_types: flush.session.failure_types.into_iter().collect(),
        final_summary: flush.session.final_summary,
    }
}

fn workflow_rollout_step_status(
    outcome: crate::workflow::WorkflowRunOutcome,
    step_index: usize,
    executed_steps: usize,
) -> &'static str {
    if step_index + 1 < executed_steps {
        return "completed";
    }
    match outcome {
        crate::workflow::WorkflowRunOutcome::Completed => "completed",
        crate::workflow::WorkflowRunOutcome::Blocked => "blocked_boundary",
        crate::workflow::WorkflowRunOutcome::Abandoned => "abandoned_boundary",
        crate::workflow::WorkflowRunOutcome::Superseded => "superseded_boundary",
        crate::workflow::WorkflowRunOutcome::NoProgress => "no_progress_boundary",
    }
}

fn simulated_executed_workflow_steps(
    workflow: &WorkflowSpec,
    task: &WorkflowTaskCase,
) -> Vec<WorkflowTaskRolloutStep> {
    if workflow.workflow_steps.is_empty() {
        return Vec::new();
    }
    let executed_count = workflow
        .workflow_steps
        .len()
        .min(task.baseline_turns.max(task.baseline_tool_actions).max(1));
    workflow
        .workflow_steps
        .iter()
        .take(executed_count)
        .enumerate()
        .map(|(index, step)| WorkflowTaskRolloutStep {
            step_index: index + 1,
            step: step.clone(),
            status: workflow_rollout_step_status(task.baseline_outcome, index, executed_count)
                .to_string(),
            evidence: format!(
                "origin={} turns={} tool_actions={} summary={}",
                task.origin, task.baseline_turns, task.baseline_tool_actions, task.task_summary
            ),
        })
        .collect()
}

fn workflow_rollout_boundary_events(task: &WorkflowTaskCase) -> Vec<String> {
    let mut events = vec![
        "workflow_bound".to_string(),
        "session_accumulated".to_string(),
        "work_boundary_flushed".to_string(),
        format!("outcome:{:?}", task.baseline_outcome),
    ];
    if task.manual_fix_detected {
        events.push("manual_fix_detected".to_string());
    }
    if task.rollback_detected {
        events.push("rollback_detected".to_string());
    }
    if !task.failure_types.is_empty() {
        events.push(format!("failure_types:{}", task.failure_types.join(",")));
    }
    events
}

pub(super) async fn execute_workflow_candidate_rollout(
    context: &Context,
    entry: &WorkflowFrontierEntry,
    target_workflow: &WorkflowSpec,
    source_workflow: Option<&WorkflowSpec>,
) -> Result<WorkflowExecutableRolloutResult> {
    let rollout_home = std::env::temp_dir().join(format!(
        "daat-locus-workflow-rollout-{}",
        uuid::Uuid::new_v4()
    ));
    tokio::fs::create_dir_all(&rollout_home)
        .await
        .into_diagnostic()?;
    let home_override = DaatLocusHomeOverride::set(rollout_home.clone());
    let mut isolated =
        build_eval_context_with_compiled(context.config.clone(), context.compiled_prompts.clone())
            .await;

    let target_spec = create_isolated_workflow(&mut isolated.workflows, target_workflow).await?;
    let mut source_ids = Vec::<String>::new();
    if let Some(source) = source_workflow {
        let source_spec = create_isolated_workflow(&mut isolated.workflows, source).await?;
        source_ids.push(source_spec.id.clone());
    }

    let (rolled_out_target, summary) = if let Some(patch) = entry.patch.as_ref() {
        let updated = isolated
            .workflows
            .apply_patch(WorkflowPatch {
                workflow_id: patch.workflow_id.clone(),
                when_to_use_additions: patch.when_to_use_additions.clone(),
                precondition_additions: patch.precondition_additions.clone(),
                workflow_step_additions: patch.workflow_step_additions.clone(),
                done_criteria_additions: patch.done_criteria_additions.clone(),
                recovery_additions: patch.recovery_additions.clone(),
            })
            .await?;
        (
            updated,
            format!(
                "patch_applied=true additions={}",
                patch.when_to_use_additions.len()
                    + patch.precondition_additions.len()
                    + patch.workflow_step_additions.len()
                    + patch.done_criteria_additions.len()
                    + patch.recovery_additions.len()
            ),
        )
    } else if let Some(merge) = entry.merge.as_ref() {
        let updated = isolated
            .workflows
            .merge_workflows(
                &merge.target_workflow_id,
                &source_ids,
                Some(merge.rationale.clone()),
            )
            .await?;
        (
            updated,
            format!(
                "merge_applied=true target={} merged_sources={}",
                merge.target_workflow_id,
                source_ids.join(",")
            ),
        )
    } else {
        let current = isolated
            .workflows
            .get(&target_spec.id)
            .cloned()
            .ok_or_else(|| miette::miette!("missing rolled out target workflow"))?;
        (current, "no_candidate_applied".to_string())
    };

    isolated.shutdown().await;
    drop(home_override);
    let _ = tokio::fs::remove_dir_all(&rollout_home).await;

    Ok(WorkflowExecutableRolloutResult {
        target_workflow: rolled_out_target,
        summary,
    })
}

async fn create_isolated_workflow(
    store: &mut WorkflowStore,
    workflow: &WorkflowSpec,
) -> Result<WorkflowSpec> {
    store
        .create_workflow(NewWorkflowSpec {
            id: workflow.id.clone(),
            when_to_use: workflow.when_to_use.clone(),
            preconditions: workflow.preconditions.clone(),
            workflow_steps: workflow.workflow_steps.clone(),
            done_criteria: workflow.done_criteria.clone(),
            recovery: workflow.recovery.clone(),
        })
        .await
}

pub(super) fn run_workflow_task_rollout(
    workflow: &WorkflowSpec,
    task: &WorkflowTaskCase,
) -> WorkflowTaskRolloutCase {
    let mut runner = WorkflowTaskRolloutRunnerState::default();
    runner.begin_bound_workflow_session(workflow, task);
    runner.accumulate_task(workflow, task);
    runner.queue_active_workflow_run_for_flush(task.baseline_outcome);
    let record = runner
        .flush_records(task.ended_at_ms.max(task.started_at_ms))
        .into_iter()
        .next()
        .unwrap_or_else(blank_workflow_run_record);
    let executed_steps = simulated_executed_workflow_steps(workflow, task);
    let boundary_events = workflow_rollout_boundary_events(task);
    WorkflowTaskRolloutCase {
        executed_steps,
        boundary_events: boundary_events.clone(),
        summary: format!(
            "workflow_bound=true session_accumulated=true flush_count=1 outcome_collected=true run_id={} workflow_id={} outcome={:?} turns={} tool_actions={} step_count={} boundary_events={}",
            record.run_id,
            record.workflow_id,
            record.outcome,
            record.turn_count,
            record.tool_action_count,
            workflow
                .workflow_steps
                .len()
                .min(task.baseline_turns.max(task.baseline_tool_actions).max(1)),
            boundary_events.join("|")
        ),
        record,
    }
}
