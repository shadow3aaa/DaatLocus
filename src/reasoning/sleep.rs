use std::collections::HashMap;

use crate::{
    context::Context,
    hindsight::{HindsightRecallOptions, builtin_hindsight_mental_models},
    reasoning::{
        compiled::{
            RUNTIME_SYSTEM_PROMPT_COMPILE_KEY, save_compiled_runtime_system_prompt_for_model,
        },
        examples::ExampleField,
        runtime::PromptRequest,
        turn_compile::current_runtime_system_prompt_artifact_from_store,
    },
    workflow::{
        WorkflowPatch, WorkflowRunOutcome, WorkflowRunRecord, WorkflowSpec, WorkflowStore,
        load_workflow_run_batch,
    },
};
use miette::Result;
use serde_json::json;
use tracing::warn;

use super::{
    evaluation_artifacts::{
        EvaluationArtifactBootstrapDemo, EvaluationArtifactFailurePattern,
        EvaluationArtifactInstructionHypothesis, EvaluationArtifactRuntimeDemo,
        EvaluationArtifactStressCase, EvaluationArtifactSuggestedFixKind,
        EvaluationArtifactTurnDemo, EvaluationArtifactWorkflowMerge,
        EvaluationArtifactWorkflowPatch, EvaluationArtifactsStore, PromptImprovementArtifacts,
        WorkflowImprovementArtifacts,
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
    pub patch_candidates: usize,
    pub merge_candidates: usize,
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
    let store = EvaluationArtifactsStore::open().await?;
    let sleep_inputs = load_sleep_inputs().await?;
    let prompt_improvement = run_prompt_improvement_pipeline(
        context,
        &store,
        &sleep_inputs.trace_batch.records,
        sleep_inputs.trace_batch.records.len(),
    )
    .await?;
    let workflow_improvement =
        run_workflow_improvement_pipeline(&mut context.workflows, &store).await?;
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
    patches: Vec<EvaluationArtifactWorkflowPatch>,
    merges: Vec<EvaluationArtifactWorkflowMerge>,
    patch_applied: usize,
    merge_applied: usize,
    rollbacks: usize,
    rounds: usize,
}

#[derive(Default)]
struct WorkflowExecutionAggregate {
    workflow_id: String,
    run_count: usize,
    blocked_count: usize,
    no_progress_count: usize,
    action_total: usize,
    manual_fix_signals: usize,
    rollback_signals: usize,
    failure_type_counts: HashMap<String, usize>,
    source_run_ids: Vec<String>,
}

async fn load_runtime_trace_records() -> Result<RuntimeTraceBatch> {
    load_runtime_trace_batch().await
}

async fn load_sleep_inputs() -> Result<SleepInputs> {
    let trace_batch = load_runtime_trace_records().await?;
    Ok(SleepInputs { trace_batch })
}

async fn run_prompt_improvement_pipeline(
    context: &mut Context,
    store: &EvaluationArtifactsStore,
    records: &[ProgramTraceRecord],
    consumed_trace_events: usize,
) -> Result<PromptImprovementSummary> {
    let failure_patterns = derive_failure_patterns(records);

    let mut derived = derive_evaluation_artifacts(context, &failure_patterns).await?;
    derived
        .bootstrap_demos
        .extend(derive_success_bootstrap_demos(records));
    let prompt_update = apply_trace_driven_runtime_prompt_patch(context, &failure_patterns).await?;

    store
        .replace_prompt_improvement_artifacts(PromptImprovementArtifacts {
            failure_patterns: &failure_patterns,
            bootstrap_demos: &derived.bootstrap_demos,
            stress_cases: &derived.stress_cases,
            instruction_hypotheses: &derived.instruction_hypotheses,
            runtime_demos: &derived.runtime_demos,
            turn_demos: &derived.turn_demos,
        })
        .await?;

    Ok(PromptImprovementSummary {
        consumed_trace_events,
        failure_patterns,
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
    workflows: &mut WorkflowStore,
    store: &EvaluationArtifactsStore,
) -> Result<WorkflowImprovementSummary> {
    let run_batch = load_workflow_run_batch().await?;
    let workflow_optimization =
        optimize_workflows_from_run_records(workflows, &run_batch.records).await?;
    store
        .replace_workflow_improvement_artifacts(WorkflowImprovementArtifacts {
            workflow_patches: &workflow_optimization.patches,
            workflow_merges: &workflow_optimization.merges,
        })
        .await?;

    Ok(WorkflowImprovementSummary {
        evidence_run_records: run_batch.records.len(),
        patch_candidates: workflow_optimization.patches.len(),
        merge_candidates: workflow_optimization.merges.len(),
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

async fn apply_trace_driven_runtime_prompt_patch(
    context: &mut Context,
    failure_patterns: &[EvaluationArtifactFailurePattern],
) -> Result<PromptPatchUpdate> {
    let new_additions = build_trace_driven_runtime_prompt_additions(failure_patterns);
    if new_additions.is_empty() {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    }

    let mut compiled = current_runtime_system_prompt_artifact_from_store(&context.compiled_prompts);
    let previous_len = compiled.system_additions.len();
    for addition in new_additions {
        if !compiled
            .system_additions
            .iter()
            .any(|line| line == &addition)
        {
            compiled.system_additions.push(addition);
        }
    }
    let applied_system_additions = compiled.system_additions.len().saturating_sub(previous_len);
    if applied_system_additions == 0 {
        return Ok(PromptPatchUpdate {
            applied_system_additions: 0,
            compiled_prompt_updated: false,
        });
    }

    compiled.best_candidate = format!("sleep_trace_patch_{}", chrono::Utc::now().timestamp());
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

fn build_trace_driven_runtime_prompt_additions(
    failure_patterns: &[EvaluationArtifactFailurePattern],
) -> Vec<String> {
    let mut additions = Vec::new();
    if failure_patterns.is_empty() {
        return additions;
    }

    if failure_patterns.iter().any(|pattern| {
        pattern.pattern_id.contains("missing-field")
            || pattern.pattern_id.contains("unknown-variant")
            || pattern.pattern_id.contains("invalid-type")
            || pattern.pattern_id.contains("malformed-json")
    }) {
        additions.push(
            "在任何结构化输出场景中，先对照目标 schema 自检必填字段、枚举值和字段类型，再提交最终结果。"
                .to_string(),
        );
    }

    if failure_patterns
        .iter()
        .any(|pattern| pattern.pattern_id.contains("provider-error"))
    {
        additions.push(
            "当 provider 或上游接口报错时，不要盲目重试同一动作；先缩小问题边界并明确记录失败原因。"
                .to_string(),
        );
    }

    if failure_patterns
        .iter()
        .any(|pattern| pattern.severity >= 2 && pattern.frequency >= 2)
    {
        additions.push(
            "如果同类失败重复出现，优先收缩输出、降低自由发挥空间，并显式遵守已有 contract。"
                .to_string(),
        );
    }

    additions
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
    workflows: &mut WorkflowStore,
    run_records: &[WorkflowRunRecord],
) -> Result<SleepWorkflowOptimizationResult> {
    let mut result = SleepWorkflowOptimizationResult {
        rounds: 1,
        ..Default::default()
    };

    let aggregates = collect_workflow_execution_aggregates(run_records);
    let all_workflows = workflows.workspace_list();

    result.patches = build_workflow_patch_candidates(&aggregates, &all_workflows);
    result.merges = build_workflow_merge_candidates(&all_workflows);

    for patch in &mut result.patches {
        if !evaluate_workflow_patch_candidate(workflows, patch) {
            patch.rolled_back = true;
            result.rollbacks += 1;
            continue;
        }
        match workflows
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
                patch.applied = true;
                result.patch_applied += 1;
            }
            Err(err) => {
                patch.rolled_back = true;
                patch.rationale = format!("{}; rollback={}", patch.rationale, err);
                result.rollbacks += 1;
            }
        }
    }

    for merge in &mut result.merges {
        if !evaluate_workflow_merge_candidate(workflows, merge) {
            continue;
        }
        if merge.confidence < 0.75 {
            continue;
        }
        match workflows
            .merge_workflows(
                &merge.target_workflow_id,
                &merge.source_workflow_ids,
                Some(merge.rationale.clone()),
            )
            .await
        {
            Ok(_) => {
                merge.applied = true;
                result.merge_applied += 1;
            }
            Err(err) => {
                merge.rationale = format!("{}; rollback={}", merge.rationale, err);
                result.rollbacks += 1;
            }
        }
    }

    Ok(result)
}

fn collect_workflow_execution_aggregates(
    run_records: &[WorkflowRunRecord],
) -> HashMap<String, WorkflowExecutionAggregate> {
    let mut aggregates = HashMap::<String, WorkflowExecutionAggregate>::new();

    for record in run_records {
        let entry = aggregates
            .entry(record.workflow_id.clone())
            .or_insert_with(|| WorkflowExecutionAggregate {
                workflow_id: record.workflow_id.clone(),
                ..Default::default()
            });
        entry.run_count += 1;
        entry.source_run_ids.push(record.run_id.clone());

        if record.outcome == WorkflowRunOutcome::Blocked {
            entry.blocked_count += 1;
        }
        if record.outcome == WorkflowRunOutcome::NoProgress {
            entry.no_progress_count += 1;
        }

        entry.action_total += record.tool_action_count;
        if record.manual_fix_detected {
            entry.manual_fix_signals += 1;
        }
        if record.rollback_detected {
            entry.rollback_signals += 1;
        }
        for failure_type in &record.failure_types {
            let failure_type = failure_type.trim();
            if !failure_type.is_empty() {
                *entry
                    .failure_type_counts
                    .entry(failure_type.to_string())
                    .or_insert(0) += 1;
            }
        }
    }

    aggregates
}

fn build_workflow_patch_candidates(
    aggregates: &HashMap<String, WorkflowExecutionAggregate>,
    workflows: &[WorkflowSpec],
) -> Vec<EvaluationArtifactWorkflowPatch> {
    let workflow_map = workflows
        .iter()
        .map(|workflow| (workflow.id.clone(), workflow))
        .collect::<HashMap<_, _>>();

    let mut patches = Vec::new();
    for aggregate in aggregates.values() {
        let Some(workflow) = workflow_map.get(&aggregate.workflow_id) else {
            continue;
        };
        if aggregate.run_count == 0 {
            continue;
        }

        let failure_rate = (aggregate.blocked_count + aggregate.no_progress_count) as f64
            / aggregate.run_count as f64;
        let needs_patch = failure_rate >= 0.3
            || aggregate.manual_fix_signals > 0
            || aggregate.rollback_signals > 0;
        if !needs_patch {
            continue;
        }

        let mut precondition_additions = Vec::new();
        let mut workflow_step_additions = Vec::new();
        let mut done_criteria_additions = Vec::new();
        let mut recovery_additions = Vec::new();

        if aggregate.blocked_count > 0 {
            precondition_additions.push("执行前确认关键依赖、输入和权限条件都已满足".to_string());
            recovery_additions
                .push("出现阻塞时，先回退到上一个稳定步骤，再重新验证关键前提".to_string());
        }
        if aggregate.no_progress_count > 0 {
            workflow_step_additions
                .push("如果连续多个回合没有实质推进，明确记录阻塞点并收缩目标".to_string());
            done_criteria_additions
                .push("如果无法继续推进，也必须产出明确的阻塞说明或下一步条件".to_string());
        }
        if aggregate.manual_fix_signals > 0 {
            workflow_step_additions
                .push("进行手工修复前，先固定修复前提并规划复验步骤".to_string());
        }
        if let Some((failure_type, _)) = aggregate
            .failure_type_counts
            .iter()
            .max_by_key(|(_, count)| *count)
        {
            recovery_additions.push(format!(
                "当 failure_type=`{}` 时，优先执行对应的标准恢复路径",
                failure_type
            ));
        }

        patches.push(EvaluationArtifactWorkflowPatch {
            workflow_id: workflow.id.clone(),
            title: format!("workflow patch {}", workflow.id),
            rationale: format!(
                "run_count={} blocked={} no_progress={} manual_fix={} rollback={} failure_rate={:.2}",
                aggregate.run_count,
                aggregate.blocked_count,
                aggregate.no_progress_count,
                aggregate.manual_fix_signals,
                aggregate.rollback_signals,
                failure_rate,
            ),
            when_to_use_additions: Vec::new(),
            precondition_additions,
            workflow_step_additions,
            done_criteria_additions,
            recovery_additions,
            source_run_ids: aggregate.source_run_ids.clone(),
            confidence: (0.45 + failure_rate).min(0.95),
            applied: false,
            rolled_back: false,
        });
    }

    patches
}

fn build_workflow_merge_candidates(
    workflows: &[WorkflowSpec],
) -> Vec<EvaluationArtifactWorkflowMerge> {
    let mut merges = Vec::new();
    for left in 0..workflows.len() {
        for right in (left + 1)..workflows.len() {
            let left_workflow = &workflows[left];
            let right_workflow = &workflows[right];
            let similarity = workflow_similarity(left_workflow, right_workflow);
            if similarity < 0.72 {
                continue;
            }
            merges.push(EvaluationArtifactWorkflowMerge {
                target_workflow_id: left_workflow.id.clone(),
                source_workflow_ids: vec![right_workflow.id.clone()],
                rationale: format!(
                    "workflow similarity {:.2}: when_to_use/workflow_steps overlap strongly",
                    similarity
                ),
                confidence: similarity.min(0.95),
                applied: false,
            });
        }
    }
    merges
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
}

fn workflow_similarity(left: &WorkflowSpec, right: &WorkflowSpec) -> f64 {
    let left_tokens = workflow_similarity_tokens(left);
    let right_tokens = workflow_similarity_tokens(right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let intersection = left_tokens.intersection(&right_tokens).count() as f64;
    let union = left_tokens.union(&right_tokens).count() as f64;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn workflow_similarity_tokens(workflow: &WorkflowSpec) -> std::collections::HashSet<String> {
    [
        workflow.when_to_use.join(" "),
        workflow.workflow_steps.join(" "),
        workflow.done_criteria.join(" "),
    ]
    .join(" ")
    .split(|ch: char| !ch.is_alphanumeric())
    .map(|token| token.trim().to_ascii_lowercase())
    .filter(|token| !token.is_empty())
    .collect()
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
    use tempfile::TempDir;

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
        let result = optimize_workflows_from_run_records(&mut workflows, &run_records)
            .await
            .expect("optimize workflows from workflow run records");

        assert_eq!(result.patches.len(), 1);
        assert_eq!(result.patch_applied, 1);
        assert_eq!(result.rollbacks, 0);

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
}
