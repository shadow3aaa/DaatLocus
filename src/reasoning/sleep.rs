use std::{collections::HashMap, env, path::PathBuf};

use crate::{
    context::Context,
    hindsight::{HindsightRecallOptions, HindsightRetainItem},
    reasoning::{
        compiled::{
            RUNTIME_SYSTEM_PROMPT_COMPILE_KEY,
            load_previous_compiled_runtime_system_prompt_for_model,
            save_compiled_runtime_system_prompt_for_model,
            save_previous_compiled_runtime_system_prompt_for_model,
        },
        episode::{EpisodeActionRecord, EpisodeOutcome, EpisodeStatus, EpisodeStep},
        examples::ExampleField,
        runtime::{PromptMessage, PromptRequest, PromptRole},
    },
    tool_ui::{ToolCallUiEvent, ToolUiEvent},
};
use miette::{Result, miette};
use serde_json::json;
use tracing::warn;

use super::{
    programs::runtime_system_prompt_judge::{
        RuntimeSystemPromptJudgeOutput, RuntimeSystemPromptJudgeProgram,
    },
    programs::runtime_system_prompt_patch_builder::{
        RuntimeSystemPromptPatchBuilderOutput, RuntimeSystemPromptPatchBuilderProgram,
    },
    programs::evaluation_artifact_builder::{EvaluationArtifactBuilderOutput, EvaluationArtifactBuilderProgram},
    programs::sleep_review_synthesizer::{
        SleepReviewSynthesizerOutput, SleepReviewSynthesizerProgram,
    },
    render::openai_tools::OpenAIToolRenderer,
    runtime::{execute_program_with_ir_report, resolve_program_tuning},
    runtime_review::{
        RuntimeReviewSpan, RuntimeTurnRecord, build_runtime_review_spans,
        compact_runtime_review_file, load_runtime_review_batch,
    },
    evaluation_artifacts::{
        EvaluationArtifactBootstrapDemo, EvaluationArtifactFailurePattern,
        EvaluationArtifactInstructionHypothesis, EvaluationArtifactRuntimeDemo,
        EvaluationArtifactRuntimeDemoEvaluation, EvaluationArtifactRuntimePromptCandidate,
        EvaluationArtifactRuntimePromptEvolutionReport, EvaluationArtifactRuntimePromptEvolutionRound,
        EvaluationArtifactRuntimePromptSuggestion, EvaluationArtifactStressCase,
        EvaluationArtifactSuggestedFixKind, EvaluationArtifactTurnDemo, EvaluationArtifactTurnDemoEvaluation,
        EvaluationArtifactsStore,
    },
    trace::{
        ProgramTraceRecord, RuntimeTraceBatch, TraceOrigin, compact_runtime_trace_file,
        load_runtime_trace_batch,
    },
    turn_compile::{
        TurnCompileEngine, apply_runtime_prompt_candidate_shared,
        build_compiled_runtime_system_prompt_report, build_runtime_prompt_evolution_report,
        choose_best_non_regressing_prompt_shared,
        current_runtime_system_prompt_artifact_from_store, generate_turn_prompt_candidates,
        is_acceptable_turn_round, runtime_system_prompt_text as render_runtime_system_prompt_text,
        turn_evaluation_stats, turn_evaluation_summary_lines,
        turn_prompt_suggestions_from_evaluations,
    },
};

#[derive(Clone)]
pub struct SleepSummary {
    pub consumed_trace_events: usize,
    pub consumed_runtime_reviews: usize,
    pub failure_patterns: Vec<EvaluationArtifactFailurePattern>,
    pub bootstrap_demos: usize,
    pub stress_cases: usize,
    pub instruction_hypotheses: usize,
    pub runtime_demos: usize,
    pub turn_demos: usize,
    pub runtime_prompt_suggestions: usize,
    pub runtime_prompt_candidates: usize,
    pub runtime_demo_evaluations: usize,
    pub turn_demo_evaluations: usize,
    pub runtime_demo_passed: usize,
    pub runtime_demo_regressions: usize,
    pub runtime_prompt_rolled_back: bool,
    pub runtime_prompt_evolution_rounds: usize,
    pub runtime_prompt_accepted: bool,
    pub retained_reflections: usize,
}

#[derive(Clone, serde::Serialize)]
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
    let runtime_review_batch = load_runtime_review_batch().await?;
    let consumed_runtime_reviews = runtime_review_batch.turns.len();
    let runtime_review_spans = build_runtime_review_spans(&runtime_review_batch.turns);
    let mut failure_patterns = derive_failure_patterns(&records);
    let episode_outcomes = load_recent_learn_episode_outcomes().await?;
    let review_inputs = collect_review_inputs(&episode_outcomes, &runtime_review_spans);
    let review_synthesis = synthesize_review_inputs(context, &review_inputs).await?;
    failure_patterns.extend(review_synthesis.failure_patterns.clone());
    let store = EvaluationArtifactsStore::open().await?;
    store.replace_failure_patterns(&failure_patterns).await?;
    let mut derived = derive_evaluation_artifacts(context, &failure_patterns).await?;
    derived
        .bootstrap_demos
        .extend(derive_success_bootstrap_demos(&records));
    derived
        .bootstrap_demos
        .extend(review_synthesis.bootstrap_demos.clone());
    derived
        .stress_cases
        .extend(review_synthesis.stress_cases.clone());
    derived
        .instruction_hypotheses
        .extend(review_synthesis.instruction_hypotheses.clone());
    derived
        .runtime_demos
        .extend(review_synthesis.runtime_demos.clone());
    store
        .replace_bootstrap_demos(&derived.bootstrap_demos)
        .await?;
    store.replace_stress_cases(&derived.stress_cases).await?;
    store
        .replace_instruction_hypotheses(&derived.instruction_hypotheses)
        .await?;
    store.replace_runtime_demos(&derived.runtime_demos).await?;
    store.replace_turn_demos(&derived.turn_demos).await?;
    let runtime_evolution = evolve_runtime_system_prompt(
        context,
        &derived.runtime_demos,
        &derived.turn_demos,
        &runtime_review_spans,
        &derived.instruction_hypotheses,
    )
    .await?;
    store
        .replace_runtime_demo_evaluations(&runtime_evolution.evaluations)
        .await?;
    store
        .replace_turn_demo_evaluations(&runtime_evolution.turn_evaluations)
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
        retain_sleep_reflections(context, &review_synthesis.reflections).await?;
    compact_runtime_trace_file(trace_batch.next_offset).await?;
    compact_runtime_review_file(runtime_review_batch.next_offset).await?;
    Ok(SleepSummary {
        consumed_trace_events,
        consumed_runtime_reviews,
        failure_patterns,
        bootstrap_demos: derived.bootstrap_demos.len(),
        stress_cases: derived.stress_cases.len(),
        instruction_hypotheses: derived.instruction_hypotheses.len(),
        runtime_demos: derived.runtime_demos.len(),
        turn_demos: derived.turn_demos.len(),
        runtime_prompt_suggestions: runtime_evolution.suggestions.len(),
        runtime_prompt_candidates: runtime_evolution.candidates.len(),
        runtime_demo_evaluations: runtime_evolution.evaluations.len(),
        turn_demo_evaluations: runtime_evolution.turn_evaluations.len(),
        runtime_demo_passed: runtime_evolution.passed,
        runtime_demo_regressions: runtime_evolution.regressions,
        runtime_prompt_rolled_back: runtime_evolution.rolled_back,
        runtime_prompt_evolution_rounds: runtime_evolution.rounds,
        runtime_prompt_accepted: runtime_evolution.accepted,
        retained_reflections,
    })
}

struct DerivedEvaluationArtifacts {
    bootstrap_demos: Vec<EvaluationArtifactBootstrapDemo>,
    stress_cases: Vec<EvaluationArtifactStressCase>,
    instruction_hypotheses: Vec<EvaluationArtifactInstructionHypothesis>,
    runtime_demos: Vec<EvaluationArtifactRuntimeDemo>,
    turn_demos: Vec<EvaluationArtifactTurnDemo>,
}

struct RuntimePromptEvolutionResult {
    evaluations: Vec<EvaluationArtifactRuntimeDemoEvaluation>,
    turn_evaluations: Vec<EvaluationArtifactTurnDemoEvaluation>,
    suggestions: Vec<EvaluationArtifactRuntimePromptSuggestion>,
    candidates: Vec<EvaluationArtifactRuntimePromptCandidate>,
    report: EvaluationArtifactRuntimePromptEvolutionReport,
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
struct ReviewSleepSynthesis {
    failure_patterns: Vec<EvaluationArtifactFailurePattern>,
    bootstrap_demos: Vec<EvaluationArtifactBootstrapDemo>,
    stress_cases: Vec<EvaluationArtifactStressCase>,
    instruction_hypotheses: Vec<EvaluationArtifactInstructionHypothesis>,
    runtime_demos: Vec<EvaluationArtifactRuntimeDemo>,
    reflections: Vec<SleepReflectionRecord>,
}

#[derive(Clone)]
struct SleepReflectionRecord {
    document_id: String,
    content: String,
    tags: Vec<String>,
}

#[derive(Clone)]
struct ReviewInput {
    review_id: String,
    review_label: String,
    source_kind: String,
    outcome_status: String,
    task_goal: String,
    done_criteria: String,
    recent_steps: String,
    final_observation: String,
    memory_query: String,
    demo_inputs: Vec<ExampleField>,
    expected_output: SleepActionOutput,
    source_trace_ids: Vec<String>,
    repeat_hint: usize,
    can_create_failure_pattern: bool,
    reflection_tags: Vec<String>,
    reflection_subject: String,
    last_action_kind: String,
    last_action_summary: String,
}

fn collect_review_inputs(
    episode_outcomes: &[EpisodeOutcome],
    runtime_review_spans: &[RuntimeReviewSpan],
) -> Vec<ReviewInput> {
    let mut inputs = Vec::new();
    inputs.extend(
        episode_outcomes
            .iter()
            .filter_map(review_input_from_episode),
    );
    inputs.extend(
        runtime_review_spans
            .iter()
            .filter_map(review_input_from_runtime_span),
    );
    inputs
}

async fn synthesize_review_inputs(
    context: &mut Context,
    inputs: &[ReviewInput],
) -> Result<ReviewSleepSynthesis> {
    let renderer = OpenAIToolRenderer;
    let program = SleepReviewSynthesizerProgram;
    let tuning = resolve_program_tuning(context, &program).await;
    let mut synthesized = ReviewSleepSynthesis::default();

    for review in inputs.iter().cloned() {
        let related_memories = recall_related_memories(context, &review.memory_query, 3).await;
        let outcome_ir = program.dataset_ir(
            review.review_label.clone(),
            review.source_kind.clone(),
            review.review_id.clone(),
            review.outcome_status.clone(),
            review.task_goal.clone(),
            review.done_criteria.clone(),
            review.recent_steps.clone(),
            review.final_observation.clone(),
            render_related_memories(&related_memories).unwrap_or_else(|| "无".to_string()),
        );
        let synthesized_outcome = match execute_program_with_ir_report(
            context.judge_llm.as_ref(),
            context,
            &renderer,
            &program,
            outcome_ir,
            &tuning,
            TraceOrigin::Sleep,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                warn!(
                    "sleep review synthesis skipped for {} (label={}): {err:?}",
                    review.review_id, review.review_label
                );
                continue;
            }
        };

        merge_review_synthesis(
            &mut synthesized,
            &review,
            &related_memories,
            &synthesized_outcome.output,
        );
    }

    Ok(synthesized)
}

fn review_input_from_episode(outcome: &EpisodeOutcome) -> Option<ReviewInput> {
    let step = outcome.steps.last()?.clone();
    let review_label = review_label_from_action_kind(&step.action.kind, &outcome.environment_name);
    let task_goal = outcome
        .task
        .task_goal
        .clone()
        .unwrap_or_else(|| outcome.task.title.clone());
    let done_criteria = if outcome.task.done_criteria.is_empty() {
        outcome.task.success_criteria.join("\n")
    } else {
        outcome.task.done_criteria.join("\n")
    };
    let recent_steps = render_recent_episode_steps(outcome);
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
    let review_id = format!("episode:{}", outcome.task.id);
    Some(ReviewInput {
        review_id: review_id.clone(),
        review_label: review_label.clone(),
        source_kind: "train_episode".to_string(),
        outcome_status: format!("{:?}", outcome.status),
        task_goal,
        done_criteria,
        recent_steps,
        final_observation,
        memory_query,
        demo_inputs: episode_example_inputs(outcome, &step),
        expected_output: step_to_output(&step),
        source_trace_ids: vec![review_id.clone()],
        repeat_hint: outcome.metric.repeated_actions.max(2),
        can_create_failure_pattern: !matches!(outcome.status, EpisodeStatus::Succeeded),
        reflection_tags: vec![
            "sleep-reflection".to_string(),
            format!("label:{}", review_label),
            "source:train_episode".to_string(),
            format!("status:{:?}", outcome.status).to_ascii_lowercase(),
        ],
        reflection_subject: format!("train episode {}", outcome.task.id),
        last_action_kind: step.action.kind.clone(),
        last_action_summary: step.action.summary.clone(),
    })
}

fn review_input_from_runtime_span(span: &RuntimeReviewSpan) -> Option<ReviewInput> {
    let first_turn = span.turns.first()?;
    let last_turn = span.last_turn();
    let last_action = last_runtime_turn_action(last_turn);
    let review_label = review_label_from_action_kind(&last_action.kind, "runtime");
    let task_goal = last_turn
        .metadata
        .get("objective")
        .cloned()
        .filter(|item| !item.trim().is_empty())
        .or_else(|| {
            (!last_turn.current_doing.trim().is_empty()).then(|| last_turn.current_doing.clone())
        })
        .unwrap_or_else(|| last_action.summary.clone());
    let outcome_status = infer_runtime_review_status(span);
    let done_criteria = last_turn
        .metadata
        .get("objective")
        .map(|objective| format!("推进目标：{objective}"))
        .unwrap_or_else(|| "推进当前 runtime 世界状态并保持行为边界稳定。".to_string());
    let recent_steps = render_recent_runtime_span_steps(span);
    let final_observation = format!(
        "{}\n\n{}",
        last_turn.observation.trim(),
        last_turn.after_snapshot_text.trim()
    );
    let memory_query = format!(
        "{}\n{}\n{}",
        task_goal.trim(),
        last_turn.observation.trim(),
        recent_steps.trim()
    );
    Some(ReviewInput {
        review_id: format!("runtime_review:{}", span.id),
        review_label: review_label.clone(),
        source_kind: "runtime_review".to_string(),
        outcome_status: outcome_status.clone(),
        task_goal,
        done_criteria,
        recent_steps,
        final_observation,
        memory_query,
        demo_inputs: runtime_span_example_inputs(first_turn, span),
        expected_output: runtime_turn_to_output(last_turn),
        source_trace_ids: span
            .turns
            .iter()
            .map(|turn| format!("runtime_turn:{}", turn.id))
            .collect(),
        repeat_hint: span.turns.len().max(2),
        can_create_failure_pattern: matches!(outcome_status.as_str(), "Blocked" | "NoProgress"),
        reflection_tags: vec![
            "sleep-reflection".to_string(),
            format!("label:{}", review_label),
            "source:runtime_review".to_string(),
            format!("status:{}", outcome_status.to_ascii_lowercase()),
        ],
        reflection_subject: format!("runtime review {}", span.id),
        last_action_kind: last_action.kind.clone(),
        last_action_summary: last_action.summary.clone(),
    })
}

fn review_label_from_action_kind(action_kind: &str, fallback: &str) -> String {
    let action_kind = action_kind.trim();
    if action_kind.is_empty() {
        fallback.to_string()
    } else {
        action_kind.to_string()
    }
}

fn infer_runtime_review_status(span: &RuntimeReviewSpan) -> String {
    if span.turns.iter().all(|turn| {
        let action = last_runtime_turn_action(turn);
        matches!(action.kind.as_str(), "assistant_message" | "empty_tool_calls")
    }) {
        return "NoProgress".to_string();
    }
    if span.turns.iter().any(|turn| {
        turn.observation.contains("failed")
            || turn.description.contains("没有可执行动作")
            || turn.description.contains("empty tool call")
    }) {
        return "Blocked".to_string();
    }
    "Observed".to_string()
}

fn derive_failure_patterns(records: &[ProgramTraceRecord]) -> Vec<EvaluationArtifactFailurePattern> {
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

fn render_recent_runtime_span_steps(span: &RuntimeReviewSpan) -> String {
    span.turns
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            let phase = turn
                .metadata
                .get("work_phase")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let history_summary = render_runtime_turn_history_summary(&turn.history_messages);
            format!(
                "{}. phase={} actions={} current_doing={} observation={} history={}",
                index + 1,
                phase,
                render_runtime_turn_actions(turn),
                compact_review_text(&turn.current_doing),
                compact_review_text(&turn.observation),
                compact_review_text(&history_summary)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_runtime_turn_history_summary(messages: &[PromptMessage]) -> String {
    let mut lines = Vec::new();
    for message in messages {
        if let Some(summary) = render_prompt_message_summary_for_review(message) {
            lines.push(summary);
        }
    }
    lines.join(" | ")
}

fn render_prompt_message_summary_for_review(message: &PromptMessage) -> Option<String> {
    let mut parts = Vec::new();
    if !message.content.trim().is_empty() {
        parts.push(compact_review_text(&message.content));
    }
    for event in &message.tool_call_ui_events {
        parts.push(render_tool_call_ui_event_summary(event));
    }
    if let Some(event) = &message.tool_ui_event {
        parts.push(render_tool_ui_event_summary(event));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" / "))
    }
}

fn render_tool_call_ui_event_summary(event: &ToolCallUiEvent) -> String {
    match event {
        ToolCallUiEvent::Exec(data)
        | ToolCallUiEvent::Work(data)
        | ToolCallUiEvent::App(data)
        | ToolCallUiEvent::Error(data) => compact_review_text(&data.title),
        ToolCallUiEvent::Terminal(data) => compact_review_text(&data.title),
        ToolCallUiEvent::Patch(data) => compact_review_text(&data.summary_line),
        ToolCallUiEvent::Telegram(data) => compact_review_text(&data.title),
    }
}

fn render_tool_ui_event_summary(event: &ToolUiEvent) -> String {
    match event {
        ToolUiEvent::Exec(data)
        | ToolUiEvent::Work(data)
        | ToolUiEvent::App(data)
        | ToolUiEvent::Error(data) => compact_review_text(&data.title),
        ToolUiEvent::Terminal(data) => compact_review_text(&data.title),
        ToolUiEvent::Patch(data) => compact_review_text(&data.summary_line),
        ToolUiEvent::Telegram(data) => compact_review_text(&data.title),
    }
}

fn compact_review_text(text: &str) -> String {
    const MAX_CHARS: usize = 180;
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}…")
}

fn merge_review_synthesis(
    synthesized: &mut ReviewSleepSynthesis,
    review: &ReviewInput,
    related_memories: &[String],
    output: &SleepReviewSynthesizerOutput,
) {
    let has_case_artifact = output.create_bootstrap_demo || output.create_stress_case;

    if output.create_failure_pattern
        && !output.failure_pattern_summary.trim().is_empty()
        && review.can_create_failure_pattern
    {
        synthesized
            .failure_patterns
            .push(EvaluationArtifactFailurePattern {
                suite: review.review_label.clone(),
                pattern_id: format!(
                    "review:{}:{}:{}",
                    slugify(&review.review_label),
                    slugify(output.failure_pattern_summary.trim()),
                    slugify(&review.review_id)
                ),
                description: output.failure_pattern_summary.trim().to_string(),
                supporting_trace_ids: review.source_trace_ids.clone(),
                frequency: review.repeat_hint.max(1),
                severity: if review.source_kind == "runtime_review" {
                    3
                } else {
                    4
                },
                suggested_fix_kind: match output
                    .suggested_fix_kind
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "demo" => EvaluationArtifactSuggestedFixKind::Demo,
                    "stress" | "stress_case" | "stresscase" => {
                        EvaluationArtifactSuggestedFixKind::StressCase
                    }
                    _ => EvaluationArtifactSuggestedFixKind::Instruction,
                },
            });
    }

    if output.create_bootstrap_demo
        && !output.bootstrap_demo_title.trim().is_empty()
        && !output.bootstrap_demo_summary.trim().is_empty()
    {
        synthesized
            .bootstrap_demos
            .push(EvaluationArtifactBootstrapDemo {
                suite: review.review_label.clone(),
                title: output.bootstrap_demo_title.trim().to_string(),
                input_summary: output.synthesized_summary.trim().to_string(),
                inputs: review.demo_inputs.clone(),
                expected_output: serde_json::to_value(&review.expected_output)
                    .unwrap_or_else(|_| json!({})),
                reference_case_names: Vec::new(),
                source_trace_ids: review.source_trace_ids.clone(),
                confidence: output.reflection_confidence.clamp(0.0, 1.0) as f32,
            });
    }

    if output.create_stress_case && !output.stress_case_name.trim().is_empty() {
        synthesized.stress_cases.push(EvaluationArtifactStressCase {
            suite: review.review_label.clone(),
            name: output.stress_case_name.trim().to_string(),
            input_ir: json!({
                "review_label": review.review_label,
                "source_kind": review.source_kind,
                "task_goal": review.task_goal,
                "summary": output.synthesized_summary,
                "last_action": format!("{} ({})", review.last_action_kind, review.last_action_summary),
                "related_memories": related_memories,
            }),
            expected_constraints: output.stress_constraints.clone(),
            reference_case_names: Vec::new(),
            source_pattern_id: review.review_id.clone(),
            repeat: review.repeat_hint.max(2),
            weight: 2,
        });
    }

    if output.create_instruction_hypothesis
        && !output.instruction_text.trim().is_empty()
        && !has_case_artifact
    {
        synthesized
            .instruction_hypotheses
            .push(EvaluationArtifactInstructionHypothesis {
                suite: review.review_label.clone(),
                text: output.instruction_text.trim().to_string(),
                justification: output.reason.trim().to_string(),
                source_pattern_ids: review.source_trace_ids.clone(),
            });
    }

    if let Some(runtime_demo) = review_runtime_demo(review, output) {
        synthesized.runtime_demos.push(runtime_demo);
    }

    if !output.synthesized_summary.trim().is_empty() || !output.strategy_lesson.trim().is_empty() {
        synthesized.reflections.push(SleepReflectionRecord {
            document_id: format!("sleep-reflection:{}", slugify(&review.review_id)),
            content: format!(
                "Source: {}\nLabel: {}\nStatus: {}\nSummary: {}\nStrategy lesson: {}\nReason: {}",
                review.reflection_subject,
                review.review_label,
                review.outcome_status,
                output.synthesized_summary.trim(),
                output.strategy_lesson.trim(),
                output.reason.trim(),
            ),
            tags: review.reflection_tags.clone(),
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

fn runtime_span_example_inputs(
    first_turn: &RuntimeTurnRecord,
    span: &RuntimeReviewSpan,
) -> Vec<ExampleField> {
    vec![
        ExampleField {
            name: "当前状态".to_string(),
            value: first_turn.before_snapshot_text.clone(),
        },
        ExampleField {
            name: "最近交互".to_string(),
            value: render_recent_runtime_span_steps(span),
        },
    ]
}

fn runtime_turn_to_output(turn: &RuntimeTurnRecord) -> SleepActionOutput {
    let action = last_runtime_turn_action(turn);
    SleepActionOutput {
        observation: turn.observation.clone(),
        description: turn.description.clone(),
        current_doing: turn.current_doing.clone(),
        action_kind: action.kind.clone(),
        action_summary: action.summary.clone(),
    }
}

fn last_runtime_turn_action(turn: &RuntimeTurnRecord) -> &EpisodeActionRecord {
    turn.actions
        .last()
        .expect("runtime review turn should contain at least one action")
}

fn render_runtime_turn_actions(turn: &RuntimeTurnRecord) -> String {
    turn.actions
        .iter()
        .map(|action| format!("{}({})", action.kind, compact_review_text(&action.summary)))
        .collect::<Vec<_>>()
        .join(" -> ")
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

async fn evaluate_runtime_demos(
    context: &mut Context,
    runtime_demos: &[EvaluationArtifactRuntimeDemo],
) -> Result<Vec<EvaluationArtifactRuntimeDemoEvaluation>> {
    if runtime_demos.is_empty() {
        return Ok(Vec::new());
    }

    let renderer = OpenAIToolRenderer;
    let program = RuntimeSystemPromptJudgeProgram;
    let tuning = resolve_program_tuning(context, &program).await;
    let current_system_prompt = current_runtime_system_prompt_text(context);
    let previous_system_prompt = previous_runtime_system_prompt_text(context).await?;
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
    runtime_demos: &[EvaluationArtifactRuntimeDemo],
    turn_demos: &[EvaluationArtifactTurnDemo],
    runtime_review_spans: &[RuntimeReviewSpan],
    instruction_hypotheses: &[EvaluationArtifactInstructionHypothesis],
) -> Result<RuntimePromptEvolutionResult> {
    if runtime_demos.is_empty() && turn_demos.is_empty() {
        return Ok(RuntimePromptEvolutionResult {
            evaluations: Vec::new(),
            turn_evaluations: Vec::new(),
            suggestions: Vec::new(),
            candidates: Vec::new(),
            report: EvaluationArtifactRuntimePromptEvolutionReport {
                compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
                rounds: 0,
                accepted: true,
                rolled_back: false,
                passed: 0,
                total_demos: 0,
                regressions: 0,
                selected_candidate: "current".to_string(),
                selected_demo_titles: Vec::new(),
                final_system_additions: context
                    .compiled_prompts
                    .runtime_system_additions()
                    .to_vec(),
                round_history: Vec::new(),
            },
            passed: 0,
            regressions: 0,
            rolled_back: false,
            accepted: false,
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
    let mut latest_turn_evaluations: Option<Vec<EvaluationArtifactTurnDemoEvaluation>> = None;
    let mut latest_suggestions = Vec::new();
    let mut latest_regressions = 0usize;
    let mut round_history = Vec::new();

    save_compiled_runtime_system_prompt_for_model(
        &context.config.main_model.model_name,
        &current_prompt,
    )
    .await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(current_prompt.clone()));

    for _ in 0..MAX_RUNTIME_PROMPT_EVOLUTION_ROUNDS {
        rounds += 1;
        latest_evaluations = evaluate_runtime_demos(context, runtime_demos).await?;
        latest_turn_evaluations = Some(
            TurnCompileEngine::evaluate_from_review_spans(
                context,
                turn_demos,
                runtime_review_spans,
                current_runtime_system_prompt_text(context),
                previous_runtime_system_prompt_text(context).await?,
            )
            .await?,
        );
        let latest_turn_evaluations_ref = latest_turn_evaluations
            .as_ref()
            .expect("latest_turn_evaluations just populated");
        let runtime_suggestions = runtime_prompt_suggestions_from_evaluations(&latest_evaluations);
        let turn_suggestions =
            turn_prompt_suggestions_from_evaluations(latest_turn_evaluations_ref)
                .into_iter()
                .map(|title| EvaluationArtifactRuntimePromptSuggestion {
                    compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
                    title,
                    rationale: "turn demo failed".to_string(),
                    suggested_additions: Vec::new(),
                    source_demo_titles: latest_turn_evaluations_ref
                        .iter()
                        .filter(|item| !item.passed)
                        .map(|item| item.demo_title.clone())
                        .collect(),
                    source_pattern_ids: Vec::new(),
                })
                .collect::<Vec<_>>();
        latest_suggestions = runtime_suggestions
            .into_iter()
            .chain(turn_suggestions.into_iter())
            .collect();
        let runtime_regressions = latest_evaluations
            .iter()
            .filter(|item| item.regression_detected)
            .count();
        let (turn_passed, turn_regressions) = turn_evaluation_stats(latest_turn_evaluations_ref);
        let runtime_passed = latest_evaluations.iter().filter(|item| item.passed).count();
        let passed = runtime_passed + turn_passed;
        latest_regressions = runtime_regressions + turn_regressions;
        let has_regression = latest_regressions > 0;

        let runtime_round_accepted = is_acceptable_runtime_round(
            runtime_passed,
            latest_evaluations.len(),
            runtime_regressions > 0,
        );
        let turn_round_accepted = is_acceptable_turn_round(
            turn_passed,
            latest_turn_evaluations_ref.len(),
            turn_regressions > 0,
        );
        let round_accepted = runtime_round_accepted && turn_round_accepted;
        let (next_best_prompt, next_best_passed) = choose_best_non_regressing_prompt_shared(
            &best_prompt,
            best_passed,
            &current_prompt,
            passed,
            has_regression,
        );
        best_prompt = next_best_prompt;
        best_passed = next_best_passed;

        round_history.push(EvaluationArtifactRuntimePromptEvolutionRound {
            round: rounds,
            candidate: current_prompt.best_candidate.clone(),
            passed,
            total_demos: latest_evaluations.len() + latest_turn_evaluations_ref.len(),
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
            && rollback_runtime_system_prompt_if_regressed(
                context,
                &latest_evaluations,
                latest_turn_evaluations_ref,
            )
            .await?
        {
            rolled_back = true;
            current_prompt = current_runtime_system_prompt_artifact(context);
        }

        let mut next_candidates = generate_runtime_prompt_candidates(
            context,
            &latest_evaluations,
            instruction_hypotheses,
        )
        .await?;
        next_candidates.extend(
            generate_turn_prompt_candidates(
                context,
                latest_turn_evaluations_ref,
                render_runtime_hypotheses(instruction_hypotheses),
            )
            .await?,
        );
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

        let next_prompt =
            apply_runtime_prompt_candidate_shared(&current_prompt, &next_candidates[0]);
        save_previous_compiled_runtime_system_prompt_for_model(
            &context.config.main_model.model_name,
            &current_prompt,
        )
        .await?;
        save_compiled_runtime_system_prompt_for_model(
            &context.config.main_model.model_name,
            &next_prompt,
        )
        .await?;
        context.compiled_prompts = context
            .compiled_prompts
            .clone()
            .with_runtime_system_prompt(Some(next_prompt.clone()));
        current_prompt = next_prompt;
    }

    save_compiled_runtime_system_prompt_for_model(
        &context.config.main_model.model_name,
        &best_prompt,
    )
    .await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(best_prompt.clone()));

    let mut best_prompt_with_report = best_prompt.clone();
    let runtime_summary_lines = latest_evaluations
        .iter()
        .map(|item| {
            format!(
                "- {}: passed={} regression={} reason={}",
                item.demo_title,
                item.passed,
                item.regression_detected,
                item.reason.trim()
            )
        })
        .chain(turn_evaluation_summary_lines(
            latest_turn_evaluations.as_deref().unwrap_or(&[]),
        ))
        .collect::<Vec<_>>();
    best_prompt_with_report.report = Some(build_compiled_runtime_system_prompt_report(
        best_passed,
        runtime_demos.len() + turn_demos.len(),
        &runtime_summary_lines,
    ));
    save_compiled_runtime_system_prompt_for_model(
        &context.config.main_model.model_name,
        &best_prompt_with_report,
    )
    .await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(best_prompt_with_report.clone()));

    Ok(RuntimePromptEvolutionResult {
        evaluations: latest_evaluations,
        turn_evaluations: latest_turn_evaluations.unwrap_or_else(Vec::new),
        suggestions: latest_suggestions,
        candidates: all_candidates,
        report: build_runtime_prompt_evolution_report(
            runtime_demos.len() + turn_demos.len(),
            &best_prompt_with_report,
            &round_history,
            accepted,
            rolled_back,
            latest_regressions,
            best_passed,
        ),
        passed: best_passed,
        regressions: latest_regressions,
        rolled_back,
        accepted,
        rounds,
    })
}

async fn generate_runtime_prompt_candidates(
    context: &mut Context,
    evaluations: &[EvaluationArtifactRuntimeDemoEvaluation],
    instruction_hypotheses: &[EvaluationArtifactInstructionHypothesis],
) -> Result<Vec<EvaluationArtifactRuntimePromptCandidate>> {
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

    let Some(candidate) =
        runtime_prompt_candidate_from_output(&output.output, &failed, instruction_hypotheses)
    else {
        return Ok(Vec::new());
    };
    Ok(vec![candidate])
}

fn current_runtime_system_prompt_artifact(
    context: &Context,
) -> crate::reasoning::compiled::CompiledRuntimeSystemPrompt {
    current_runtime_system_prompt_artifact_from_store(&context.compiled_prompts)
}

fn is_acceptable_runtime_round(passed: usize, total: usize, has_regression: bool) -> bool {
    !has_regression && passed == total
}

async fn previous_runtime_system_prompt_text(context: &Context) -> Result<String> {
    let Some(previous) = load_previous_compiled_runtime_system_prompt_for_model(
        &context.config.main_model.model_name,
    )
    .await?
    else {
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
    render_runtime_system_prompt_text(&context.compiled_prompts)
}

fn runtime_demo_evaluation_from_output(
    demo: &EvaluationArtifactRuntimeDemo,
    output: &RuntimeSystemPromptJudgeOutput,
) -> EvaluationArtifactRuntimeDemoEvaluation {
    EvaluationArtifactRuntimeDemoEvaluation {
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
    evaluations: &[EvaluationArtifactRuntimeDemoEvaluation],
) -> Vec<EvaluationArtifactRuntimePromptSuggestion> {
    evaluations
        .iter()
        .filter(|item| !item.passed)
        .filter(|item| !item.needed_changes.is_empty())
        .map(|item| EvaluationArtifactRuntimePromptSuggestion {
            compile_key: item.compile_key.clone(),
            title: format!("runtime prompt suggestion {}", item.demo_title),
            rationale: item.reason.clone(),
            suggested_additions: item.needed_changes.clone(),
            source_demo_titles: vec![item.demo_title.clone()],
            source_pattern_ids: Vec::new(),
        })
        .collect()
}

fn render_failed_runtime_demos(evaluations: &[EvaluationArtifactRuntimeDemoEvaluation]) -> String {
    evaluations
        .iter()
        .map(|item| format!("- {}: {}", item.demo_title, item.reason.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_runtime_judge_feedback(evaluations: &[EvaluationArtifactRuntimeDemoEvaluation]) -> String {
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
    instruction_hypotheses: &[EvaluationArtifactInstructionHypothesis],
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
    evaluations: &[EvaluationArtifactRuntimeDemoEvaluation],
    instruction_hypotheses: &[EvaluationArtifactInstructionHypothesis],
) -> Option<EvaluationArtifactRuntimePromptCandidate> {
    if output.prompt_patches.is_empty() {
        return None;
    }
    Some(EvaluationArtifactRuntimePromptCandidate {
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
        source_demo_titles: evaluations
            .iter()
            .map(|item| item.demo_title.clone())
            .collect(),
        source_hypotheses: instruction_hypotheses
            .iter()
            .map(|item| item.text.clone())
            .collect(),
    })
}

async fn rollback_runtime_system_prompt_if_regressed(
    context: &mut Context,
    evaluations: &[EvaluationArtifactRuntimeDemoEvaluation],
    turn_evaluations: &[EvaluationArtifactTurnDemoEvaluation],
) -> Result<bool> {
    if !evaluations.iter().any(|item| item.regression_detected)
        && !turn_evaluations.iter().any(|item| item.regression_detected)
    {
        return Ok(false);
    }
    let Some(previous) = load_previous_compiled_runtime_system_prompt_for_model(
        &context.config.main_model.model_name,
    )
    .await?
    else {
        return Ok(false);
    };
    save_compiled_runtime_system_prompt_for_model(&context.config.main_model.model_name, &previous)
        .await?;
    context.compiled_prompts = context
        .compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(
            previous.with_compile_key(RUNTIME_SYSTEM_PROMPT_COMPILE_KEY),
        ));
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

fn review_runtime_demo(
    review: &ReviewInput,
    output: &SleepReviewSynthesizerOutput,
) -> Option<EvaluationArtifactRuntimeDemo> {
    let expected_behavior = if !output.strategy_lesson.trim().is_empty() {
        output.strategy_lesson.trim()
    } else if !output.reflection_lesson.trim().is_empty() {
        output.reflection_lesson.trim()
    } else {
        return None;
    };
    Some(EvaluationArtifactRuntimeDemo {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        title: format!("runtime demo {}", review.review_id),
        scenario_summary: format!(
            "source: {}\nlabel: {}\nstatus: {}\ntask: {}\nsummary: {}",
            review.source_kind,
            review.review_label,
            review.outcome_status,
            review.task_goal.trim(),
            output.synthesized_summary.trim()
        ),
        inputs: review.demo_inputs.clone(),
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
        source_trace_ids: review.source_trace_ids.clone(),
        confidence: output.reflection_confidence.clamp(0.0, 1.0) as f32,
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
    context
        .hindsight_retain
        .enqueue(crate::hindsight::HindsightRetainJob {
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

    fn test_prompt(
        best_candidate: &str,
        additions: &[&str],
    ) -> crate::reasoning::compiled::CompiledRuntimeSystemPrompt {
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
        let candidate = EvaluationArtifactRuntimePromptCandidate {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            title: "candidate".to_string(),
            rationale: "test".to_string(),
            prompt_patches: vec!["rule b".to_string(), "rule c".to_string()],
            source_demo_titles: vec!["demo".to_string()],
            source_hypotheses: Vec::new(),
        };

        let next = apply_runtime_prompt_candidate_shared(&current, &candidate);
        assert_eq!(next.best_candidate, "candidate");
        assert_eq!(next.system_additions, vec!["rule a", "rule b", "rule c"]);
    }

    #[test]
    fn runtime_prompt_suggestions_come_only_from_failed_evaluations_with_changes() {
        let suggestions = runtime_prompt_suggestions_from_evaluations(&[
            EvaluationArtifactRuntimeDemoEvaluation {
                compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
                demo_title: "passed-demo".to_string(),
                passed: true,
                regression_detected: false,
                confidence: 0.9,
                needed_changes: vec!["unused".to_string()],
                reason: "ok".to_string(),
            },
            EvaluationArtifactRuntimeDemoEvaluation {
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
        assert_eq!(
            suggestions[0].title,
            "runtime prompt suggestion failed-demo"
        );
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
            choose_best_non_regressing_prompt_shared(&best, 1, &current, 2, false);
        assert_eq!(selected.best_candidate, "current");
        assert_eq!(passed, 2);

        let (selected, passed) =
            choose_best_non_regressing_prompt_shared(&best, 2, &current, 3, true);
        assert_eq!(selected.best_candidate, "best");
        assert_eq!(passed, 2);
    }
}
