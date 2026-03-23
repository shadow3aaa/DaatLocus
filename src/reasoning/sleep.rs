use std::{collections::HashMap, env, path::PathBuf};

use miette::{Result, miette};
use serde_json::json;

use crate::{
    context::Context,
    hindsight::{HindsightRecallOptions, HindsightRetainItem},
    reasoning::{
        compiled::{
            RUNTIME_SYSTEM_PROMPT_COMPILE_KEY, load_previous_compiled_runtime_system_prompt,
            save_compiled_runtime_system_prompt, save_previous_compiled_runtime_system_prompt,
        },
        episode::{EpisodeOutcome, EpisodeStatus, EpisodeStep},
        examples::ExampleField,
        runtime::{PromptRequest, PromptRole},
    },
};

use super::{
    programs::sleep_artifact_builder::{SleepArtifactBuilderOutput, SleepArtifactBuilderProgram},
    programs::runtime_system_prompt_judge::{
        RuntimeSystemPromptJudgeOutput, RuntimeSystemPromptJudgeProgram,
    },
    programs::runtime_system_prompt_patch_builder::{
        RuntimeSystemPromptPatchBuilderOutput, RuntimeSystemPromptPatchBuilderProgram,
    },
    programs::sleep_episode_synthesizer::{
        SleepEpisodeSynthesizerOutput, SleepEpisodeSynthesizerProgram,
    },
    render::openai_tools::OpenAIToolRenderer,
    runtime::{execute_program_with_ir_report, resolve_program_tuning},
    sleep_artifacts::{
        SleepArtifactBootstrapDemo, SleepArtifactFailurePattern,
        SleepArtifactInstructionHypothesis, SleepArtifactStressCase, SleepArtifactSuggestedFixKind,
        SleepArtifactRuntimeDemo, SleepArtifactRuntimeDemoEvaluation,
        SleepArtifactRuntimePromptCandidate, SleepArtifactRuntimePromptEvolutionReport,
        SleepArtifactRuntimePromptEvolutionRound,
        SleepArtifactRuntimePromptSuggestion, SleepArtifactsStore,
    },
    trace::{
        ProgramTraceRecord, RuntimeTraceBatch, TraceOrigin, compact_runtime_trace_file,
        load_runtime_trace_batch,
    },
};

#[derive(Clone)]
pub struct SleepSummary {
    pub consumed_trace_events: usize,
    pub failure_patterns: Vec<SleepArtifactFailurePattern>,
    pub bootstrap_demos: usize,
    pub stress_cases: usize,
    pub instruction_hypotheses: usize,
    pub runtime_demos: usize,
    pub runtime_prompt_suggestions: usize,
    pub runtime_prompt_candidates: usize,
    pub runtime_demo_evaluations: usize,
    pub runtime_demo_passed: usize,
    pub runtime_demo_regressions: usize,
    pub runtime_prompt_rolled_back: bool,
    pub runtime_prompt_evolution_rounds: usize,
    pub runtime_prompt_accepted: bool,
    pub retained_reflections: usize,
}

#[derive(serde::Serialize)]
struct SleepActionOutput {
    observation: String,
    description: String,
    current_doing: String,
    action_kind: String,
    action_summary: String,
}

pub async fn run_sleep(context: &mut Context) -> Result<SleepSummary> {
    let trace_batch = load_runtime_trace_records().await?;
    let consumed_trace_events = trace_batch.records.len();
    let records = trace_batch.records;
    let mut failure_patterns = derive_failure_patterns(&records);
    let episode_outcomes = load_recent_learn_episode_outcomes().await?;
    let episode_synthesis = synthesize_episode_outcomes(context, &episode_outcomes).await?;
    failure_patterns.extend(episode_synthesis.failure_patterns.clone());
    let store = SleepArtifactsStore::open().await?;
    store.replace_failure_patterns(&failure_patterns).await?;
    let mut derived = derive_sleep_artifacts(context, &failure_patterns).await?;
    derived
        .bootstrap_demos
        .extend(derive_success_bootstrap_demos(&records));
    derived
        .bootstrap_demos
        .extend(episode_synthesis.bootstrap_demos.clone());
    derived
        .stress_cases
        .extend(episode_synthesis.stress_cases.clone());
    derived
        .instruction_hypotheses
        .extend(episode_synthesis.instruction_hypotheses.clone());
    derived
        .runtime_demos
        .extend(episode_synthesis.runtime_demos.clone());
    store.replace_bootstrap_demos(&derived.bootstrap_demos).await?;
    store.replace_stress_cases(&derived.stress_cases).await?;
    store
        .replace_instruction_hypotheses(&derived.instruction_hypotheses)
        .await?;
    store.replace_runtime_demos(&derived.runtime_demos).await?;
    let runtime_evolution = evolve_runtime_system_prompt(
        context,
        &derived.runtime_demos,
        &derived.instruction_hypotheses,
    )
    .await?;
    store
        .replace_runtime_demo_evaluations(&runtime_evolution.evaluations)
        .await?;
    store
        .replace_runtime_prompt_suggestions(&runtime_evolution.suggestions)
        .await?;
    store
        .replace_runtime_prompt_candidates(&runtime_evolution.candidates)
        .await?;
    store
        .replace_runtime_prompt_evolution_reports(std::slice::from_ref(&runtime_evolution.report))
        .await?;
    let retained_reflections =
        retain_sleep_reflections(context, &episode_synthesis.reflections).await?;
    compact_runtime_trace_file(trace_batch.next_offset).await?;
    Ok(SleepSummary {
        consumed_trace_events,
        failure_patterns,
        bootstrap_demos: derived.bootstrap_demos.len(),
        stress_cases: derived.stress_cases.len(),
        instruction_hypotheses: derived.instruction_hypotheses.len(),
        runtime_demos: derived.runtime_demos.len(),
        runtime_prompt_suggestions: runtime_evolution.suggestions.len(),
        runtime_prompt_candidates: runtime_evolution.candidates.len(),
        runtime_demo_evaluations: runtime_evolution.evaluations.len(),
        runtime_demo_passed: runtime_evolution.passed,
        runtime_demo_regressions: runtime_evolution.regressions,
        runtime_prompt_rolled_back: runtime_evolution.rolled_back,
        runtime_prompt_evolution_rounds: runtime_evolution.rounds,
        runtime_prompt_accepted: runtime_evolution.accepted,
        retained_reflections,
    })
}

struct DerivedSleepArtifacts {
    bootstrap_demos: Vec<SleepArtifactBootstrapDemo>,
    stress_cases: Vec<SleepArtifactStressCase>,
    instruction_hypotheses: Vec<SleepArtifactInstructionHypothesis>,
    runtime_demos: Vec<SleepArtifactRuntimeDemo>,
}

struct RuntimePromptEvolutionResult {
    evaluations: Vec<SleepArtifactRuntimeDemoEvaluation>,
    suggestions: Vec<SleepArtifactRuntimePromptSuggestion>,
    candidates: Vec<SleepArtifactRuntimePromptCandidate>,
    report: SleepArtifactRuntimePromptEvolutionReport,
    passed: usize,
    regressions: usize,
    rolled_back: bool,
    accepted: bool,
    rounds: usize,
}

async fn load_runtime_trace_records() -> Result<RuntimeTraceBatch> {
    load_runtime_trace_batch().await
}

async fn latest_train_source_learn_session_root() -> Result<Option<PathBuf>> {
    let train_root = env::current_dir()
        .map_err(|err| miette!("failed to get current dir for learn outcomes: {err}"))?
        .join("tmp")
        .join("train_source_learn");
    if !train_root.exists() {
        return Ok(None);
    }

    let mut latest_session: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut entries = tokio::fs::read_dir(&train_root).await.map_err(|err| {
        miette!(
            "failed to read train_source_learn dir {}: {err}",
            train_root.display()
        )
    })?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| miette!("failed to read train_source_learn entry: {err}"))?
    {
        let path = entry.path();
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if latest_session
            .as_ref()
            .is_none_or(|(latest_modified, _)| modified > *latest_modified)
        {
            latest_session = Some((modified, path));
        }
    }

    Ok(latest_session.map(|(_, path)| path))
}

async fn load_recent_learn_episode_outcomes() -> Result<Vec<EpisodeOutcome>> {
    let Some(session_root) = latest_train_source_learn_session_root().await? else {
        return Ok(Vec::new());
    };
    let episodes_root = session_root.join("episodes");
    if !episodes_root.exists() {
        return Ok(Vec::new());
    }

    let mut outcomes = Vec::new();
    let mut episode_dirs = tokio::fs::read_dir(&episodes_root).await.map_err(|err| {
        miette!(
            "failed to read learn episodes dir {}: {err}",
            episodes_root.display()
        )
    })?;
    while let Some(entry) = episode_dirs
        .next_entry()
        .await
        .map_err(|err| miette!("failed to read learn episode entry: {err}"))?
    {
        let outcome_path = entry.path().join("episode_outcome.json");
        let payload = match tokio::fs::read_to_string(&outcome_path).await {
            Ok(payload) => payload,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(miette!(
                    "failed to read episode outcome {}: {err}",
                    outcome_path.display()
                ));
            }
        };
        let outcome = serde_json::from_str::<EpisodeOutcome>(&payload).map_err(|err| {
            miette!(
                "failed to parse episode outcome {}: {err}",
                outcome_path.display()
            )
        })?;
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

#[derive(Default)]
struct EpisodeSleepSynthesis {
    failure_patterns: Vec<SleepArtifactFailurePattern>,
    bootstrap_demos: Vec<SleepArtifactBootstrapDemo>,
    stress_cases: Vec<SleepArtifactStressCase>,
    instruction_hypotheses: Vec<SleepArtifactInstructionHypothesis>,
    runtime_demos: Vec<SleepArtifactRuntimeDemo>,
    reflections: Vec<SleepReflectionRecord>,
}

#[derive(Clone)]
struct SleepReflectionRecord {
    document_id: String,
    content: String,
    tags: Vec<String>,
}

async fn synthesize_episode_outcomes(
    context: &mut Context,
    outcomes: &[EpisodeOutcome],
) -> Result<EpisodeSleepSynthesis> {
    let renderer = OpenAIToolRenderer;
    let program = SleepEpisodeSynthesizerProgram;
    let tuning = resolve_program_tuning(context, &program).await;
    let mut synthesized = EpisodeSleepSynthesis::default();

    for outcome in outcomes.iter().cloned() {
        let Some(step) = outcome
            .steps
            .iter()
            .rev()
            .find(|step| infer_episode_suite_from_step(step).is_some())
            .cloned()
        else {
            continue;
        };
        let Some(suite) = infer_episode_suite_from_step(&step).map(str::to_string) else {
            continue;
        };

        let episode_id = format!("episode:{}", outcome.task.id);
        let recent_steps = render_recent_episode_steps(&outcome);
        let task_goal = outcome
            .task
            .task_goal
            .clone()
            .unwrap_or_else(|| outcome.task.title.clone());
        let final_observation = format!(
            "{}\n\n{}",
            outcome.final_observation.summary.trim(),
            outcome.final_observation.snapshot_text.trim()
        );
        let memory_query = format!(
            "{}\n{}\n{}",
            task_goal.trim(),
            outcome.final_observation.summary.trim(),
            recent_steps.trim()
        );
        let related_memories = recall_related_memories(context, &memory_query, 3).await;
        let outcome_ir = program.dataset_ir(
            suite.to_string(),
            episode_id.clone(),
            format!("{:?}", outcome.status),
            task_goal,
            outcome.task.done_criteria.join("\n"),
            recent_steps.clone(),
            final_observation.clone(),
            render_related_memories(&related_memories).unwrap_or_else(|| "无".to_string()),
        );
        let synthesized_outcome = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            outcome_ir,
            &tuning,
            TraceOrigin::Sleep,
        )
        .await?;

        merge_episode_synthesis(
            &mut synthesized,
            &outcome,
            &suite,
            &step,
            &episode_id,
            &related_memories,
            &synthesized_outcome.output,
        );
    }

    Ok(synthesized)
}

fn derive_failure_patterns(records: &[ProgramTraceRecord]) -> Vec<SleepArtifactFailurePattern> {
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
        .map(|bucket| SleepArtifactFailurePattern {
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

fn infer_episode_suite_from_step(step: &EpisodeStep) -> Option<&'static str> {
    match step.action.kind.as_str() {
        "resolve_telegram_chat" => Some("resolve_telegram_chat"),
        _ => None,
    }
}

fn render_recent_episode_steps(outcome: &EpisodeOutcome) -> String {
    outcome
        .steps
        .iter()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .enumerate()
        .map(|(index, step)| {
            let phase = step
                .metadata
                .get("work_phase")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let reason = step
                .metadata
                .get("completion_reason")
                .cloned()
                .unwrap_or_default();
            format!(
                "{}. phase={} action={} ({}) observation={} reason={}",
                index + 1,
                phase,
                step.action.kind,
                step.action.summary,
                step.observation_summary.trim(),
                reason.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn merge_episode_synthesis(
    synthesized: &mut EpisodeSleepSynthesis,
    outcome: &EpisodeOutcome,
    suite: &str,
    step: &EpisodeStep,
    episode_id: &str,
    related_memories: &[String],
    output: &SleepEpisodeSynthesizerOutput,
) {
    let has_case_artifact = output.create_bootstrap_demo || output.create_stress_case;

    if output.create_failure_pattern
        && !output.failure_pattern_summary.trim().is_empty()
        && !matches!(outcome.status, EpisodeStatus::Succeeded)
    {
        synthesized
            .failure_patterns
            .push(SleepArtifactFailurePattern {
                suite: suite.to_string(),
                pattern_id: format!(
                    "episode:{}:{}:{}",
                    slugify(suite),
                    slugify(output.failure_pattern_summary.trim()),
                    slugify(&outcome.task.id)
                ),
                description: output.failure_pattern_summary.trim().to_string(),
                supporting_trace_ids: vec![episode_id.to_string()],
                frequency: outcome.metric.repeated_actions.max(1),
                severity: 4,
                suggested_fix_kind: match output
                    .suggested_fix_kind
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "demo" => SleepArtifactSuggestedFixKind::Demo,
                    "stress" | "stress_case" | "stresscase" => {
                        SleepArtifactSuggestedFixKind::StressCase
                    }
                    _ => SleepArtifactSuggestedFixKind::Instruction,
                },
            });
    }

    if output.create_bootstrap_demo
        && !output.bootstrap_demo_title.trim().is_empty()
        && !output.bootstrap_demo_summary.trim().is_empty()
    {
        synthesized
            .bootstrap_demos
            .push(SleepArtifactBootstrapDemo {
                suite: suite.to_string(),
                title: output.bootstrap_demo_title.trim().to_string(),
                input_summary: output.synthesized_summary.trim().to_string(),
                inputs: episode_example_inputs(outcome, step),
                expected_output: serde_json::to_value(step_to_output(step))
                    .unwrap_or_else(|_| json!({})),
                reference_case_names: Vec::new(),
                source_trace_ids: vec![episode_id.to_string()],
                confidence: output.reflection_confidence.clamp(0.0, 1.0) as f32,
            });
    }

    if output.create_stress_case && !output.stress_case_name.trim().is_empty() {
        synthesized.stress_cases.push(SleepArtifactStressCase {
            suite: suite.to_string(),
            name: output.stress_case_name.trim().to_string(),
            input_ir: json!({
                "task_title": outcome.task.title,
                "task_goal": outcome.task.task_goal,
                "summary": output.synthesized_summary,
                "last_action": format!("{} ({})", step.action.kind, step.action.summary),
                "related_memories": related_memories,
            }),
            expected_constraints: output.stress_constraints.clone(),
            reference_case_names: Vec::new(),
            source_pattern_id: episode_id.to_string(),
            repeat: outcome.metric.repeated_actions.max(2),
            weight: 2,
        });
    }

    if output.create_instruction_hypothesis
        && !output.instruction_text.trim().is_empty()
        && !has_case_artifact
    {
        synthesized
            .instruction_hypotheses
            .push(SleepArtifactInstructionHypothesis {
                suite: suite.to_string(),
                text: output.instruction_text.trim().to_string(),
                justification: output.reason.trim().to_string(),
                source_pattern_ids: vec![episode_id.to_string()],
            });
    }

    if let Some(runtime_demo) = episode_runtime_demo(outcome, step, episode_id, output) {
        synthesized.runtime_demos.push(runtime_demo);
    }

    if !output.synthesized_summary.trim().is_empty() || !output.strategy_lesson.trim().is_empty() {
        synthesized.reflections.push(SleepReflectionRecord {
            document_id: format!("sleep-reflection:{}", slugify(episode_id)),
            content: format!(
                "Episode: {}\nSuite: {}\nStatus: {:?}\nSummary: {}\nStrategy lesson: {}\nReason: {}",
                outcome.task.id,
                suite,
                outcome.status,
                output.synthesized_summary.trim(),
                output.strategy_lesson.trim(),
                output.reason.trim(),
            ),
            tags: vec![
                "sleep-reflection".to_string(),
                format!("suite:{}", suite),
                format!("status:{:?}", outcome.status).to_ascii_lowercase(),
            ],
        });
    }
}

fn episode_example_inputs(outcome: &EpisodeOutcome, step: &EpisodeStep) -> Vec<ExampleField> {
    vec![
        ExampleField {
            name: "训练任务".to_string(),
            value: outcome.task.instruction.clone(),
        },
        ExampleField {
            name: "当前状态".to_string(),
            value: step.snapshot_text.clone(),
        },
    ]
}

fn step_to_output(step: &EpisodeStep) -> SleepActionOutput {
    SleepActionOutput {
        observation: step.observation_summary.clone(),
        description: step
            .metadata
            .get("description")
            .cloned()
            .unwrap_or_default(),
        current_doing: step
            .metadata
            .get("current_doing")
            .cloned()
            .unwrap_or_default(),
        action_kind: step.action.kind.clone(),
        action_summary: step.action.summary.clone(),
    }
}

struct PatternAccumulator {
    suite: String,
    label: String,
    description: String,
    supporting_trace_ids: Vec<String>,
    frequency: usize,
    severity: u8,
    suggested_fix_kind: SleepArtifactSuggestedFixKind,
}

fn classify_failure(record: &ProgramTraceRecord, error: &str) -> String {
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
    if record.program_name == "resolve_telegram_chat" && error.contains("action") {
        return "resolve_chat_schema_drift".to_string();
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
        "resolve_chat_schema_drift" => {
            "resolve_telegram_chat 在运行时出现了扁平 action/结构漂移，需用 stress case 与 demo 固化输出边界。"
                .to_string()
        }
        _ => format!(
            "{} 在运行时出现结构化输出失败：{}",
            record.program_name, error
        ),
    }
}

fn suggested_fix_kind(label: &str) -> SleepArtifactSuggestedFixKind {
    if label.starts_with("missing_field:") || label.starts_with("unknown_variant:") {
        return SleepArtifactSuggestedFixKind::StressCase;
    }
    if label == "resolve_chat_schema_drift" {
        return SleepArtifactSuggestedFixKind::Demo;
    }
    SleepArtifactSuggestedFixKind::Instruction
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

async fn derive_sleep_artifacts(
    context: &mut Context,
    patterns: &[SleepArtifactFailurePattern],
) -> Result<DerivedSleepArtifacts> {
    if patterns.is_empty() {
        return Ok(DerivedSleepArtifacts {
            bootstrap_demos: Vec::new(),
            stress_cases: Vec::new(),
            instruction_hypotheses: Vec::new(),
            runtime_demos: Vec::new(),
        });
    }

    let renderer = OpenAIToolRenderer;
    let program = SleepArtifactBuilderProgram;
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

    Ok(DerivedSleepArtifacts {
        bootstrap_demos,
        stress_cases,
        instruction_hypotheses,
        runtime_demos,
    })
}

async fn evaluate_runtime_demos(
    context: &mut Context,
    runtime_demos: &[SleepArtifactRuntimeDemo],
) -> Result<Vec<SleepArtifactRuntimeDemoEvaluation>> {
    if runtime_demos.is_empty() {
        return Ok(Vec::new());
    }

    let renderer = OpenAIToolRenderer;
    let program = RuntimeSystemPromptJudgeProgram;
    let tuning = resolve_program_tuning(context, &program).await;
    let current_system_prompt = current_runtime_system_prompt_text(context);
    let previous_system_prompt = previous_runtime_system_prompt_text().await?;
    let mut evaluations = Vec::with_capacity(runtime_demos.len());

    for demo in runtime_demos.iter().cloned() {
        let judge_focus = if demo.judge_focus.is_empty() {
            String::from("none")
        } else {
            demo.judge_focus.join("\n")
        };
        let output = execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            program.dataset_ir(
                current_system_prompt.clone(),
                previous_system_prompt.clone(),
                demo.title.clone(),
                demo.scenario_summary.clone(),
                demo.expected_behavior.clone(),
                judge_focus,
            ),
            &tuning,
            TraceOrigin::Sleep,
        )
        .await?;

        evaluations.push(runtime_demo_evaluation_from_output(&demo, &output.output));
    }

    Ok(evaluations)
}

const MAX_RUNTIME_PROMPT_EVOLUTION_ROUNDS: usize = 3;

async fn evolve_runtime_system_prompt(
    context: &mut Context,
    runtime_demos: &[SleepArtifactRuntimeDemo],
    instruction_hypotheses: &[SleepArtifactInstructionHypothesis],
) -> Result<RuntimePromptEvolutionResult> {
    if runtime_demos.is_empty() {
        return Ok(RuntimePromptEvolutionResult {
            evaluations: Vec::new(),
            suggestions: Vec::new(),
            candidates: Vec::new(),
            report: SleepArtifactRuntimePromptEvolutionReport {
                compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
                rounds: 0,
                accepted: true,
                rolled_back: false,
                passed: 0,
                total_demos: 0,
                regressions: 0,
                selected_candidate: "current".to_string(),
                selected_demo_titles: Vec::new(),
                final_system_additions: context.compiled_prompts.runtime_system_additions().to_vec(),
                round_history: Vec::new(),
            },
            passed: 0,
            regressions: 0,
            rolled_back: false,
            accepted: true,
            rounds: 0,
        });
    }

    let mut current_prompt = current_runtime_system_prompt_artifact(context);
    let mut best_prompt = current_prompt.clone();
    let mut best_passed = 0usize;
    let mut rounds = 0usize;
    let mut rolled_back = false;
    let mut accepted = false;
    let mut all_candidates = Vec::new();
    let mut latest_evaluations = Vec::new();
    let mut latest_suggestions = Vec::new();
    let mut latest_regressions = 0usize;
    let mut round_history = Vec::new();

    save_compiled_runtime_system_prompt(&current_prompt).await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(current_prompt.clone()));

    for _ in 0..MAX_RUNTIME_PROMPT_EVOLUTION_ROUNDS {
        rounds += 1;
        latest_evaluations = evaluate_runtime_demos(context, runtime_demos).await?;
        latest_suggestions = runtime_prompt_suggestions_from_evaluations(&latest_evaluations);
        latest_regressions = latest_evaluations
            .iter()
            .filter(|item| item.regression_detected)
            .count();
        let passed = latest_evaluations.iter().filter(|item| item.passed).count();
        let has_regression = latest_regressions > 0;

        let round_accepted = is_acceptable_runtime_round(passed, latest_evaluations.len(), has_regression);
        let (next_best_prompt, next_best_passed) =
            choose_best_non_regressing_prompt(&best_prompt, best_passed, &current_prompt, passed, has_regression);
        best_prompt = next_best_prompt;
        best_passed = next_best_passed;

        round_history.push(SleepArtifactRuntimePromptEvolutionRound {
            round: rounds,
            candidate: current_prompt.best_candidate.clone(),
            passed,
            total_demos: latest_evaluations.len(),
            regressions: latest_regressions,
            rolled_back,
            accepted: round_accepted,
            suggestion_titles: latest_suggestions
                .iter()
                .map(|item| item.title.clone())
                .collect(),
            candidate_titles: Vec::new(),
        });

        if round_accepted {
            accepted = true;
            best_prompt = current_prompt.clone();
            break;
        }

        if has_regression
            && rollback_runtime_system_prompt_if_regressed(context, &latest_evaluations).await?
        {
            rolled_back = true;
            current_prompt = current_runtime_system_prompt_artifact(context);
        }

        let next_candidates = generate_runtime_prompt_candidates(
            context,
            &latest_evaluations,
            instruction_hypotheses,
        )
        .await?;
        if next_candidates.is_empty() {
            break;
        }
        let candidate_titles = next_candidates
            .iter()
            .map(|item| item.title.clone())
            .collect::<Vec<_>>();
        all_candidates.extend(next_candidates.clone());
        if let Some(last_round) = round_history.last_mut() {
            last_round.candidate_titles = candidate_titles;
        }

        let next_prompt = apply_runtime_prompt_candidate(&current_prompt, &next_candidates[0]);
        save_previous_compiled_runtime_system_prompt(&current_prompt).await?;
        save_compiled_runtime_system_prompt(&next_prompt).await?;
        context.compiled_prompts = context
            .compiled_prompts
            .clone()
            .with_runtime_system_prompt(Some(next_prompt.clone()));
        current_prompt = next_prompt;
    }

    save_compiled_runtime_system_prompt(&best_prompt).await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(best_prompt.clone()));

    Ok(RuntimePromptEvolutionResult {
        evaluations: latest_evaluations,
        suggestions: latest_suggestions,
        candidates: all_candidates,
        report: SleepArtifactRuntimePromptEvolutionReport {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            rounds,
            accepted,
            rolled_back,
            passed: best_passed,
            total_demos: runtime_demos.len(),
            regressions: latest_regressions,
            selected_candidate: best_prompt.best_candidate.clone(),
            selected_demo_titles: best_prompt.selected_demo_titles.clone(),
            final_system_additions: best_prompt.system_additions.clone(),
            round_history,
        },
        passed: best_passed,
        regressions: latest_regressions,
        rolled_back,
        accepted,
        rounds,
    })
}

async fn generate_runtime_prompt_candidates(
    context: &mut Context,
    evaluations: &[SleepArtifactRuntimeDemoEvaluation],
    instruction_hypotheses: &[SleepArtifactInstructionHypothesis],
) -> Result<Vec<SleepArtifactRuntimePromptCandidate>> {
    let failed = evaluations
        .iter()
        .filter(|item| !item.passed)
        .cloned()
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return Ok(Vec::new());
    }

    let renderer = OpenAIToolRenderer;
    let program = RuntimeSystemPromptPatchBuilderProgram;
    let tuning = resolve_program_tuning(context, &program).await;
    let current_system_prompt = current_runtime_system_prompt_text(context);
    let output = execute_program_with_ir_report(
        context.judge_llm.as_ref(),
        context,
        &renderer,
        &program,
        program.dataset_ir(
            current_system_prompt,
            render_failed_runtime_demos(&failed),
            render_runtime_judge_feedback(&failed),
            render_runtime_hypotheses(instruction_hypotheses),
        ),
        &tuning,
        TraceOrigin::Sleep,
    )
    .await?;

    let Some(candidate) = runtime_prompt_candidate_from_output(&output.output, &failed, instruction_hypotheses) else {
        return Ok(Vec::new());
    };
    Ok(vec![candidate])
}

fn current_runtime_system_prompt_artifact(
    context: &Context,
) -> crate::reasoning::compiled::CompiledRuntimeSystemPrompt {
    crate::reasoning::compiled::CompiledRuntimeSystemPrompt {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        best_candidate: "current".to_string(),
        system_additions: context.compiled_prompts.runtime_system_additions().to_vec(),
        selected_demo_titles: Vec::new(),
        report: None,
    }
}

fn apply_runtime_prompt_candidate(
    current: &crate::reasoning::compiled::CompiledRuntimeSystemPrompt,
    candidate: &SleepArtifactRuntimePromptCandidate,
) -> crate::reasoning::compiled::CompiledRuntimeSystemPrompt {
    let mut system_additions = current.system_additions.clone();
    for patch in &candidate.prompt_patches {
        if !patch.trim().is_empty() && !system_additions.iter().any(|line| line == patch) {
            system_additions.push(patch.clone());
        }
    }
    crate::reasoning::compiled::CompiledRuntimeSystemPrompt {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        best_candidate: candidate.title.clone(),
        system_additions,
        selected_demo_titles: candidate.source_demo_titles.clone(),
        report: None,
    }
}

fn is_acceptable_runtime_round(passed: usize, total: usize, has_regression: bool) -> bool {
    !has_regression && passed == total
}

fn choose_best_non_regressing_prompt(
    best_prompt: &crate::reasoning::compiled::CompiledRuntimeSystemPrompt,
    best_passed: usize,
    current_prompt: &crate::reasoning::compiled::CompiledRuntimeSystemPrompt,
    current_passed: usize,
    has_regression: bool,
) -> (
    crate::reasoning::compiled::CompiledRuntimeSystemPrompt,
    usize,
) {
    if !has_regression && current_passed >= best_passed {
        (current_prompt.clone(), current_passed)
    } else {
        (best_prompt.clone(), best_passed)
    }
}

async fn previous_runtime_system_prompt_text() -> Result<String> {
    let Some(previous) = load_previous_compiled_runtime_system_prompt().await? else {
        return Ok(String::from("none"));
    };
    let mut lines = vec![
        crate::reasoning::prompts::SYSTEM_PROMPT_KERNEL.to_string(),
        crate::reasoning::prompts::TOOL_ACTION_PROMPT.to_string(),
    ];
    lines.extend(
        previous
            .system_additions
            .into_iter()
            .filter(|line| !line.trim().is_empty()),
    );
    Ok(lines.join("\n\n"))
}

fn current_runtime_system_prompt_text(context: &Context) -> String {
    let mut lines = vec![
        crate::reasoning::prompts::SYSTEM_PROMPT_KERNEL.to_string(),
        crate::reasoning::prompts::TOOL_ACTION_PROMPT.to_string(),
    ];
    lines.extend(
        context
            .compiled_prompts
            .runtime_system_additions()
            .iter()
            .filter(|line| !line.trim().is_empty())
            .cloned(),
    );
    lines.join("\n\n")
}

fn runtime_demo_evaluation_from_output(
    demo: &SleepArtifactRuntimeDemo,
    output: &RuntimeSystemPromptJudgeOutput,
) -> SleepArtifactRuntimeDemoEvaluation {
    SleepArtifactRuntimeDemoEvaluation {
        compile_key: demo.compile_key.clone(),
        demo_title: demo.title.clone(),
        passed: output.passed,
        regression_detected: output.regression_detected,
        confidence: output.confidence,
        needed_changes: output.needed_changes.clone(),
        reason: output.reason.clone(),
    }
}

fn runtime_prompt_suggestions_from_evaluations(
    evaluations: &[SleepArtifactRuntimeDemoEvaluation],
) -> Vec<SleepArtifactRuntimePromptSuggestion> {
    evaluations
        .iter()
        .filter(|item| !item.passed)
        .filter(|item| !item.needed_changes.is_empty())
        .map(|item| SleepArtifactRuntimePromptSuggestion {
            compile_key: item.compile_key.clone(),
            title: format!("runtime prompt suggestion {}", item.demo_title),
            rationale: item.reason.clone(),
            suggested_additions: item.needed_changes.clone(),
            source_demo_titles: vec![item.demo_title.clone()],
            source_pattern_ids: Vec::new(),
        })
        .collect()
}

fn render_failed_runtime_demos(evaluations: &[SleepArtifactRuntimeDemoEvaluation]) -> String {
    evaluations
        .iter()
        .map(|item| format!("- {}: {}", item.demo_title, item.reason.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_runtime_judge_feedback(evaluations: &[SleepArtifactRuntimeDemoEvaluation]) -> String {
    evaluations
        .iter()
        .map(|item| {
            let changes = if item.needed_changes.is_empty() {
                "none".to_string()
            } else {
                item.needed_changes.join(" | ")
            };
            format!(
                "- {}: regression={} changes={}",
                item.demo_title, item.regression_detected, changes
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_runtime_hypotheses(
    instruction_hypotheses: &[SleepArtifactInstructionHypothesis],
) -> String {
    if instruction_hypotheses.is_empty() {
        return String::from("none");
    }
    instruction_hypotheses
        .iter()
        .map(|item| format!("- {}: {}", item.suite, item.text.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn runtime_prompt_candidate_from_output(
    output: &RuntimeSystemPromptPatchBuilderOutput,
    evaluations: &[SleepArtifactRuntimeDemoEvaluation],
    instruction_hypotheses: &[SleepArtifactInstructionHypothesis],
) -> Option<SleepArtifactRuntimePromptCandidate> {
    if output.prompt_patches.is_empty() {
        return None;
    }
    Some(SleepArtifactRuntimePromptCandidate {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        title: if output.title.trim().is_empty() {
            String::from("runtime prompt candidate")
        } else {
            output.title.trim().to_string()
        },
        rationale: output.rationale.trim().to_string(),
        prompt_patches: output
            .prompt_patches
            .iter()
            .filter(|item| !item.trim().is_empty())
            .cloned()
            .collect(),
        source_demo_titles: evaluations.iter().map(|item| item.demo_title.clone()).collect(),
        source_hypotheses: instruction_hypotheses
            .iter()
            .map(|item| item.text.clone())
            .collect(),
    })
}

async fn rollback_runtime_system_prompt_if_regressed(
    context: &mut Context,
    evaluations: &[SleepArtifactRuntimeDemoEvaluation],
) -> Result<bool> {
    if !evaluations.iter().any(|item| item.regression_detected) {
        return Ok(false);
    }
    let Some(previous) = load_previous_compiled_runtime_system_prompt().await? else {
        return Ok(false);
    };
    save_compiled_runtime_system_prompt(&previous).await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(previous.with_compile_key(
            RUNTIME_SYSTEM_PROMPT_COMPILE_KEY,
        )));
    Ok(true)
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
    pattern: &SleepArtifactFailurePattern,
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
    pattern: &SleepArtifactFailurePattern,
    output: &SleepArtifactBuilderOutput,
) -> Option<SleepArtifactInstructionHypothesis> {
    if !output.create_instruction_hypothesis || output.instruction_text.trim().is_empty() {
        return None;
    }
    Some(SleepArtifactInstructionHypothesis {
        suite: pattern.suite.clone(),
        text: output.instruction_text.trim().to_string(),
        justification: output.reason.trim().to_string(),
        source_pattern_ids: vec![pattern.pattern_id.clone()],
    })
}

fn to_bootstrap_demo(
    pattern: &SleepArtifactFailurePattern,
    related_memories: &[String],
    evidence_summary: Option<&str>,
    output: &SleepArtifactBuilderOutput,
) -> Option<SleepArtifactBootstrapDemo> {
    if !output.create_bootstrap_demo
        || output.bootstrap_demo_title.trim().is_empty()
        || output.reference_case_names.is_empty()
    {
        return None;
    }
    Some(SleepArtifactBootstrapDemo {
        suite: pattern.suite.clone(),
        title: output.bootstrap_demo_title.trim().to_string(),
        input_summary: render_input_summary(pattern, evidence_summary),
        inputs: vec![ExampleField {
            name: "sleep artifact summary".to_string(),
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
    pattern: &SleepArtifactFailurePattern,
    related_memories: &[String],
    evidence_summary: Option<&str>,
    output: &SleepArtifactBuilderOutput,
) -> Option<SleepArtifactRuntimeDemo> {
    if !output.create_bootstrap_demo
        || output.bootstrap_demo_title.trim().is_empty()
        || output.bootstrap_demo_summary.trim().is_empty()
    {
        return None;
    }
    Some(SleepArtifactRuntimeDemo {
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
    pattern: &SleepArtifactFailurePattern,
    related_memories: &[String],
    output: &SleepArtifactBuilderOutput,
) -> Option<SleepArtifactStressCase> {
    if !output.create_stress_case
        || output.stress_case_name.trim().is_empty()
        || output.reference_case_names.is_empty()
    {
        return None;
    }
    Some(SleepArtifactStressCase {
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

fn episode_runtime_demo(
    outcome: &EpisodeOutcome,
    step: &EpisodeStep,
    episode_id: &str,
    output: &SleepEpisodeSynthesizerOutput,
) -> Option<SleepArtifactRuntimeDemo> {
    let expected_behavior = if !output.strategy_lesson.trim().is_empty() {
        output.strategy_lesson.trim()
    } else if !output.reflection_lesson.trim().is_empty() {
        output.reflection_lesson.trim()
    } else {
        return None;
    };
    let task_goal = outcome
        .task
        .task_goal
        .clone()
        .unwrap_or_else(|| outcome.task.title.clone());
    Some(SleepArtifactRuntimeDemo {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        title: format!("runtime demo {}", outcome.task.id),
        scenario_summary: format!(
            "task: {}\noutcome: {:?}\nsummary: {}",
            task_goal.trim(),
            outcome.status,
            output.synthesized_summary.trim()
        ),
        inputs: episode_example_inputs(outcome, step),
        expected_behavior: expected_behavior.to_string(),
        judge_focus: [
            output.failure_pattern_summary.trim(),
            output.reason.trim(),
            output.reflection_evidence_summary.trim(),
        ]
        .into_iter()
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect(),
        source_trace_ids: vec![episode_id.to_string()],
        confidence: output.reflection_confidence.clamp(0.0, 1.0) as f32,
    })
}


fn derive_success_bootstrap_demos(
    records: &[ProgramTraceRecord],
) -> Vec<SleepArtifactBootstrapDemo> {
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
        demos.push(SleepArtifactBootstrapDemo {
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

fn infer_runtime_suite(record: &ProgramTraceRecord) -> Option<String> {
    if record.program_name == "resolve_telegram_chat" {
        return Some("resolve_telegram_chat".to_string());
    }
    None
}

fn extract_inputs_from_request(request: &PromptRequest) -> Vec<ExampleField> {
    let mut inputs = Vec::new();
    for message in request.all_messages() {
        if !matches!(message.role, PromptRole::User) {
            continue;
        }
        inputs.extend(parse_user_sections(&message.content));
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

async fn retain_sleep_reflections(
    context: &Context,
    reflections: &[SleepReflectionRecord],
) -> Result<usize> {
    if reflections.is_empty() {
        return Ok(0);
    }

    let items = reflections
        .iter()
        .map(|reflection| HindsightRetainItem {
            content: reflection.content.clone(),
            timestamp: None,
            context: Some("sleep reflection".to_string()),
            metadata: None,
            document_id: Some(reflection.document_id.clone()),
            tags: Some(reflection.tags.clone()),
        })
        .collect::<Vec<_>>();
    context.hindsight_retain.enqueue(crate::hindsight::HindsightRetainJob {
        items,
        document_id: None,
    })?;
    Ok(reflections.len())
}

async fn recall_related_memories(context: &Context, query: &str, top_k: usize) -> Vec<String> {
    let response = context
        .hindsight
        .recall(
            query,
            HindsightRecallOptions {
                max_tokens: 1200,
                budget: Some("low".to_string()),
                include_source_facts: true,
                max_source_facts_tokens: 1200,
                ..Default::default()
            },
        )
        .await;
    let Ok(response) = response else {
        return Vec::new();
    };
    response
        .results
        .into_iter()
        .take(top_k)
        .map(|item| item.text)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_prompt(best_candidate: &str, additions: &[&str]) -> crate::reasoning::compiled::CompiledRuntimeSystemPrompt {
        crate::reasoning::compiled::CompiledRuntimeSystemPrompt {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            best_candidate: best_candidate.to_string(),
            system_additions: additions.iter().map(|item| item.to_string()).collect(),
            selected_demo_titles: Vec::new(),
            report: None,
        }
    }

    #[test]
    fn apply_runtime_prompt_candidate_appends_only_new_patches() {
        let current = test_prompt("current", &["rule a", "rule b"]);
        let candidate = SleepArtifactRuntimePromptCandidate {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            title: "candidate".to_string(),
            rationale: "test".to_string(),
            prompt_patches: vec!["rule b".to_string(), "rule c".to_string()],
            source_demo_titles: vec!["demo".to_string()],
            source_hypotheses: Vec::new(),
        };

        let next = apply_runtime_prompt_candidate(&current, &candidate);
        assert_eq!(next.best_candidate, "candidate");
        assert_eq!(next.system_additions, vec!["rule a", "rule b", "rule c"]);
    }

    #[test]
    fn runtime_prompt_suggestions_come_only_from_failed_evaluations_with_changes() {
        let suggestions = runtime_prompt_suggestions_from_evaluations(&[
            SleepArtifactRuntimeDemoEvaluation {
                compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
                demo_title: "passed-demo".to_string(),
                passed: true,
                regression_detected: false,
                confidence: 0.9,
                needed_changes: vec!["unused".to_string()],
                reason: "ok".to_string(),
            },
            SleepArtifactRuntimeDemoEvaluation {
                compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
                demo_title: "failed-demo".to_string(),
                passed: false,
                regression_detected: false,
                confidence: 0.6,
                needed_changes: vec!["add rule".to_string()],
                reason: "missing boundary".to_string(),
            },
        ]);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].title, "runtime prompt suggestion failed-demo");
        assert_eq!(suggestions[0].suggested_additions, vec!["add rule"]);
    }

    #[test]
    fn acceptable_runtime_round_requires_full_pass_without_regression() {
        assert!(is_acceptable_runtime_round(2, 2, false));
        assert!(!is_acceptable_runtime_round(2, 2, true));
        assert!(!is_acceptable_runtime_round(1, 2, false));
    }

    #[test]
    fn choose_best_non_regressing_prompt_prefers_more_passed_without_regression() {
        let best = test_prompt("best", &["rule a"]);
        let current = test_prompt("current", &["rule a", "rule b"]);

        let (selected, passed) =
            choose_best_non_regressing_prompt(&best, 1, &current, 2, false);
        assert_eq!(selected.best_candidate, "current");
        assert_eq!(passed, 2);

        let (selected, passed) =
            choose_best_non_regressing_prompt(&best, 2, &current, 3, true);
        assert_eq!(selected.best_candidate, "best");
        assert_eq!(passed, 2);
    }
}
