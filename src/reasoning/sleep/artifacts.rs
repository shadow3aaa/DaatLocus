use super::*;

pub(super) fn derive_failure_patterns(
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
                "{} omitted required field `{}` at runtime; keep the structure stable through demos, stress cases, or instructions.",
                record.program_name, field
            )
        }
        l if l.starts_with("unknown_variant:") => {
            let variant = l.trim_start_matches("unknown_variant:");
            format!(
                "{} emitted unknown enum variant `{}` at runtime, indicating schema drift around action or branch boundaries.",
                record.program_name, variant
            )
        }
        "invalid_type" => format!(
            "{} emitted an invalid field type at runtime, indicating unstable structural constraints.",
            record.program_name
        ),
        "malformed_json" => format!(
            "{} emitted malformed JSON at runtime, indicating unstable output formatting.",
            record.program_name
        ),
        "provider_error" => format!(
            "{} encountered a provider-level error at runtime; distinguish API compatibility from program semantics.",
            record.program_name
        ),
        _ => format!(
            "{} had a structured output failure at runtime: {}",
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

pub(super) async fn derive_evaluation_artifacts(
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
                evidence_summary
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
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
        turn_demos: runtime_demos
            .iter()
            .map(runtime_demo_to_turn_demo)
            .collect::<Vec<_>>(),
        bootstrap_demos,
        stress_cases,
        instruction_hypotheses,
        runtime_demos,
    })
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

pub(super) fn prompt_candidate_has_novel_content(
    existing_additions: &[String],
    candidate: &EvaluationArtifactRuntimePromptCandidate,
) -> bool {
    candidate
        .prompt_patches
        .iter()
        .any(|patch| !existing_additions.iter().any(|existing| existing == patch))
}

pub(super) fn workflow_merge_title(merge: &EvaluationArtifactWorkflowMerge) -> String {
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
            "failure pattern: {}\nrelated L2 memories:\n{}",
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

fn runtime_demo_to_turn_demo(demo: &EvaluationArtifactRuntimeDemo) -> EvaluationArtifactTurnDemo {
    let mut initial_inputs = demo.inputs.clone();
    let has_incoming_text = initial_inputs.iter().any(|field| {
        matches!(
            field.name.as_str(),
            "incoming_text" | "message" | "user_message"
        )
    });
    if !has_incoming_text {
        initial_inputs.push(ExampleField {
            name: "incoming_text".to_string(),
            value: demo.scenario_summary.clone(),
        });
    }
    EvaluationArtifactTurnDemo {
        compile_key: demo.compile_key.clone(),
        title: demo.title.clone(),
        scenario_summary: demo.scenario_summary.clone(),
        initial_inputs,
        expected_behavior: demo.expected_behavior.clone(),
        judge_focus: demo.judge_focus.clone(),
        covered_tests: Vec::new(),
        must_use_tools: false,
        must_not_final_answer_patterns: Vec::new(),
        must_end_with_terminal_answer: true,
    }
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

pub(super) fn derive_success_bootstrap_demos(
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
    if matches!(title.as_str(), "Program Signature" | "Examples") {
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
