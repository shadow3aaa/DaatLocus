use std::{collections::HashMap, env, path::PathBuf};

use miette::{Result, miette};
use serde_json::json;

use crate::{
    context::Context,
    device::DeviceAction,
    core::{Effect, Output},
    hindsight::{HindsightRecallOptions, HindsightRetainItem},
    reasoning::{
        episode::{EpisodeOutcome, EpisodeStatus, EpisodeStep},
        examples::ExampleField,
        runtime::{PromptRequest, PromptRole},
    },
};

use super::{
    program::Program,
    programs::sleep_artifact_builder::{SleepArtifactBuilderOutput, SleepArtifactBuilderProgram},
    programs::sleep_episode_synthesizer::{
        SleepEpisodeSynthesizerOutput, SleepEpisodeSynthesizerProgram,
    },
    render::openai_tools::OpenAIToolRenderer,
    runtime::execute_program_with_ir_report,
    sleep_artifacts::{
        SleepArtifactBootstrapDemo, SleepArtifactFailurePattern,
        SleepArtifactInstructionHypothesis, SleepArtifactStressCase, SleepArtifactSuggestedFixKind,
        SleepArtifactsStore,
    },
    trace::{ProgramTraceRecord, TraceOrigin},
};

#[derive(Clone)]
pub struct SleepSummary {
    pub failure_patterns: Vec<SleepArtifactFailurePattern>,
    pub bootstrap_demos: usize,
    pub stress_cases: usize,
    pub instruction_hypotheses: usize,
    pub retained_reflections: usize,
}

pub async fn run_sleep(context: &mut Context) -> Result<SleepSummary> {
    let records = load_runtime_trace_records().await?;
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
    derived.stress_cases.extend(episode_synthesis.stress_cases.clone());
    derived
        .instruction_hypotheses
        .extend(episode_synthesis.instruction_hypotheses.clone());
    store
        .replace_bootstrap_demos(&derived.bootstrap_demos)
        .await?;
    store.replace_stress_cases(&derived.stress_cases).await?;
    store
        .replace_instruction_hypotheses(&derived.instruction_hypotheses)
        .await?;
    let retained_reflections = retain_sleep_reflections(context, &episode_synthesis.reflections).await?;
    Ok(SleepSummary {
        failure_patterns,
        bootstrap_demos: derived.bootstrap_demos.len(),
        stress_cases: derived.stress_cases.len(),
        instruction_hypotheses: derived.instruction_hypotheses.len(),
        retained_reflections,
    })
}

struct DerivedSleepArtifacts {
    bootstrap_demos: Vec<SleepArtifactBootstrapDemo>,
    stress_cases: Vec<SleepArtifactStressCase>,
    instruction_hypotheses: Vec<SleepArtifactInstructionHypothesis>,
}

async fn load_runtime_trace_records() -> Result<Vec<ProgramTraceRecord>> {
    let path = crate::get_spinova_home()
        .await
        .join("reasoning_traces.jsonl");
    let bytes = match tokio::fs::read_to_string(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(miette!(
                "failed to read reasoning trace file {}: {err}",
                path.display()
            ));
        }
    };

    let mut records = Vec::new();
    for line in bytes.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<ProgramTraceRecord>(line) {
            if record.origin == TraceOrigin::Runtime {
                records.push(record);
            }
        }
    }

    Ok(records)
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
    let mut entries = tokio::fs::read_dir(&train_root)
        .await
        .map_err(|err| miette!("failed to read train_source_learn dir {}: {err}", train_root.display()))?;
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
    let mut episode_dirs = tokio::fs::read_dir(&episodes_root)
        .await
        .map_err(|err| miette!("failed to read learn episodes dir {}: {err}", episodes_root.display()))?;
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
    let mut synthesized = EpisodeSleepSynthesis::default();

    for outcome in outcomes {
        let Some(step) = outcome
            .steps
            .iter()
            .rev()
            .find(|step| infer_episode_suite_from_step(step).is_some())
        else {
            continue;
        };
        let Some(suite) = infer_episode_suite_from_step(step) else {
            continue;
        };

        let episode_id = format!("episode:{}", outcome.task.id);
        let recent_steps = render_recent_episode_steps(outcome);
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
            &program.default_tuning(),
            TraceOrigin::Sleep,
        )
        .await?;

        merge_episode_synthesis(
            &mut synthesized,
            outcome,
            suite,
            step,
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
    match &step.effect {
        Effect::FocusDevice {
            device: crate::device::DeviceId::Telegram,
        } => Some("action_phase.attend_notifications"),
        Effect::TaskSelect { .. } => Some("action_phase.execute_task"),
        Effect::TaskAdd {
            project_id: Some(_),
            ..
        } => Some("action_phase.plan_from_project"),
        Effect::TaskAdd {
            project_id: None, ..
        } => Some("action_phase.explore_new_tasks"),
        Effect::DeviceAction {
            action: DeviceAction::TerminalInput { .. },
        }
        | Effect::Wait
        | Effect::SilentWait => Some("terminal_next_step"),
        Effect::FocusDevice {
            device: crate::device::DeviceId::Terminal,
        } => Some("action_phase.execute_task"),
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
                "{}. phase={} effect={:?} observation={} reason={}",
                index + 1,
                phase,
                step.effect,
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
        synthesized.failure_patterns.push(SleepArtifactFailurePattern {
            suite: suite.to_string(),
            pattern_id: format!(
                "episode:{}:{}:{}",
                slugify(suite),
                slugify(output.failure_pattern_summary.trim()),
                slugify(&outcome.task.id)
            ),
            description: output.failure_pattern_summary.trim().to_string(),
            supporting_trace_ids: vec![episode_id.to_string()],
            frequency: outcome.metric.repeated_effects.max(1),
            severity: 4,
            suggested_fix_kind: match output.suggested_fix_kind.trim().to_ascii_lowercase().as_str() {
                "demo" => SleepArtifactSuggestedFixKind::Demo,
                "stress" | "stress_case" | "stresscase" => SleepArtifactSuggestedFixKind::StressCase,
                _ => SleepArtifactSuggestedFixKind::Instruction,
            },
        });
    }

    if output.create_bootstrap_demo
        && !output.bootstrap_demo_title.trim().is_empty()
        && !output.bootstrap_demo_summary.trim().is_empty()
    {
        synthesized.bootstrap_demos.push(SleepArtifactBootstrapDemo {
            suite: suite.to_string(),
            title: output.bootstrap_demo_title.trim().to_string(),
            input_summary: output.synthesized_summary.trim().to_string(),
            inputs: episode_example_inputs(outcome, step),
            expected_output: serde_json::to_value(step_to_output(step)).unwrap_or_else(|_| json!({})),
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
                "last_effect": format!("{:?}", step.effect),
                "related_memories": related_memories,
            }),
            expected_constraints: output.stress_constraints.clone(),
            reference_case_names: Vec::new(),
            source_pattern_id: episode_id.to_string(),
            repeat: outcome.metric.repeated_effects.max(2),
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
            name: "完整快照".to_string(),
            value: step.snapshot_text.clone(),
        },
    ]
}

fn step_to_output(step: &EpisodeStep) -> Output {
    Output {
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
        effect: step.effect.clone(),
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
        });
    }

    let renderer = OpenAIToolRenderer;
    let program = SleepArtifactBuilderProgram;
    let mut bootstrap_demos = Vec::new();
    let mut stress_cases = Vec::new();
    let mut instruction_hypotheses = Vec::new();

    for pattern in patterns {
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
            &program.default_tuning(),
            TraceOrigin::Sleep,
        )
        .await?;

        if let Some(artifact) = to_instruction_hypothesis(pattern, &outcome.output) {
            instruction_hypotheses.push(artifact);
        }
        if let Some(artifact) = to_bootstrap_demo(
            pattern,
            &related_memories,
            evidence_summary.as_deref(),
            &outcome.output,
        ) {
            bootstrap_demos.push(artifact);
        }
        if let Some(artifact) = to_stress_case(pattern, &related_memories, &outcome.output) {
            stress_cases.push(artifact);
        }
    }

    Ok(DerivedSleepArtifacts {
        bootstrap_demos,
        stress_cases,
        instruction_hypotheses,
    })
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
    match suite {
        "resolve_telegram_chat" => crate::reasoning::datasets::resolve_telegram::all_case_names(),
        "action_phase.attend_notifications"
        | "action_phase.execute_task"
        | "action_phase.plan_from_project"
        | "action_phase.explore_new_tasks" => {
            crate::reasoning::datasets::action_phase::all_case_names_for_suite(suite)
        }
        _ => Vec::new(),
    }
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
    if record.program_name != "decide_next_action" {
        return None;
    }
    let inputs = extract_inputs_from_request(&record.request);
    let phase = inputs
        .iter()
        .find(|field| field.name.trim() == "阶段")
        .map(|field| field.value.trim());
    match phase {
        Some("处理提醒") => Some("action_phase.attend_notifications".to_string()),
        Some("执行下一步动作") => Some("action_phase.execute_task".to_string()),
        Some("为项目规划下一步") => Some("action_phase.plan_from_project".to_string()),
        Some("探索与规划新任务") => Some("action_phase.explore_new_tasks".to_string()),
        _ => None,
    }
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
    let Some(retain_handle) = context.hindsight_retain.as_ref() else {
        return Ok(0);
    };
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
    retain_handle.enqueue(crate::hindsight::HindsightRetainJob {
        items,
        document_id: None,
        document_tags: Vec::new(),
    })?;
    Ok(reflections.len())
}

async fn recall_related_memories(
    context: &Context,
    query: &str,
    top_k: usize,
) -> Vec<String> {
    let Some(hindsight) = context.hindsight.as_ref() else {
        return Vec::new();
    };
    let response = hindsight
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
