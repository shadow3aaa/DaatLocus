//! Turn-compile evaluation pipeline infrastructure.
//! Many items in this module exist for offline evaluation and training runs
//! that are not linked into the main binary path.
#![allow(dead_code)]

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::warn;
use uuid::Uuid;

use crate::{
    DaatLocusHomeOverride, build_eval_context_with_compiled,
    config::Config,
    context::Context,
    daat_locus_paths::daat_locus_paths_sync,
    events::TelegramIncomingEvent,
    execute_agent_loop_step,
    pending_work::PendingWork,
    reasoning::{
        compiled::{
            CompiledPromptStore, CompiledRuntimeSystemPrompt, RUNTIME_SYSTEM_PROMPT_COMPILE_KEY,
        },
        episode::EpisodeActionRecord,
        evaluation_artifacts::{
            EvaluationArtifactRuntimePromptCandidate, EvaluationArtifactTurnDemo,
            EvaluationArtifactTurnDemoEvaluation,
        },
        examples::ExampleField,
        programs::runtime_turn_trace_judge::{
            RuntimeTurnTraceJudgeOutput, RuntimeTurnTraceJudgeProgram,
        },
        prompt_assembler::runtime_system_prompt_text_from_additions,
        prompts::PERSONA_DEFAULT,
        render::openai_tools::OpenAIToolRenderer,
        runtime::HistoryMessage,
        runtime::{execute_program_with_ir_report, resolve_program_tuning},
        trace::TraceOrigin,
    },
};

pub const PROMPT_PERSONA_FILE_NAME: &str = "persona.md";
const PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE: &str = "configured-locale";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PromptPersonaSpec {
    pub name: String,
    #[serde(default = "default_prompt_persona_language")]
    pub language: String,
    pub identity_summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PromptPersonaFrontmatter {
    pub name: String,
    #[serde(default = "default_prompt_persona_language")]
    pub language: String,
}

fn default_prompt_persona_language() -> String {
    PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE.to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnCompileSpec {
    pub compile_key: String,
    pub title: String,
    pub scenario_summary: String,
    #[serde(default)]
    pub initial_inputs: Vec<ExampleField>,
    pub expected_behavior: String,
    #[serde(default)]
    pub judge_focus: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnTraceStep {
    pub turn_id: String,
    pub current_doing: String,
    pub description: String,
    pub observation: String,
    #[serde(default)]
    pub actions: Vec<EpisodeActionRecord>,
    pub assistant_message: Option<String>,
    pub reply_message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnTraceArtifact {
    pub span_id: String,
    pub turn_count: usize,
    #[serde(default)]
    pub steps: Vec<TurnTraceStep>,
    pub final_assistant_message: Option<String>,
    pub final_reply_message: Option<String>,
}

#[cfg(test)]
pub struct TurnRolloutRunner;

#[cfg(test)]
struct TurnTraceSourceTurn {
    id: String,
    current_doing: String,
    description: String,
    observation: String,
    actions: Vec<EpisodeActionRecord>,
    history_messages: Vec<HistoryMessage>,
}

struct IsolatedEvalContext {
    context: Context,
    home_override: DaatLocusHomeOverride,
    home_path: PathBuf,
}

impl IsolatedEvalContext {
    async fn new(config: Config, compiled_prompts: CompiledPromptStore) -> Result<Self> {
        let home_path =
            std::env::temp_dir().join(format!("daat-locus-turn-compile-{}", Uuid::new_v4()));
        fs::create_dir_all(&home_path).await.map_err(|err| {
            miette!(
                "failed to create isolated turn-compile home '{}': {err}",
                home_path.display()
            )
        })?;
        let home_override = DaatLocusHomeOverride::set(home_path.clone()).await;
        let context = build_eval_context_with_compiled(config, compiled_prompts).await;
        Ok(Self {
            context,
            home_override,
            home_path,
        })
    }

    async fn shutdown(self) {
        let Self {
            context,
            home_override,
            home_path,
        } = self;
        context.shutdown().await;
        drop(home_override);
        if let Err(err) = fs::remove_dir_all(&home_path).await {
            warn!(
                "failed to remove isolated turn-compile home '{}': {err}",
                home_path.display()
            );
        }
    }
}

#[cfg(test)]
impl TurnRolloutRunner {
    fn trace_from_turns(span_id: &str, turns: &[TurnTraceSourceTurn]) -> TurnTraceArtifact {
        let steps = turns
            .iter()
            .map(turn_trace_step_from_source_turn)
            .collect::<Vec<_>>();
        let final_turn = turns
            .last()
            .expect("turn trace source should contain at least one turn");
        let final_assistant_message = final_turn
            .history_messages
            .iter()
            .rev()
            .find(|message| message.is_assistant())
            .and_then(|message| message.text_content().map(str::to_string))
            .filter(|message| !message.trim().is_empty());
        let final_reply_message = last_finish_and_send_reply_message(&final_turn.history_messages);
        TurnTraceArtifact {
            span_id: span_id.to_string(),
            turn_count: turns.len(),
            steps,
            final_assistant_message,
            final_reply_message,
        }
    }
}

pub struct TurnCompileEngine;

impl TurnCompileEngine {
    async fn evaluate_turn_demos(
        config: Config,
        compiled_prompts: CompiledPromptStore,
        turn_demos: &[EvaluationArtifactTurnDemo],
        current_system_prompt: String,
        previous_system_prompt: String,
    ) -> Result<Vec<EvaluationArtifactTurnDemoEvaluation>> {
        if turn_demos.is_empty() {
            return Ok(Vec::new());
        }

        let renderer = OpenAIToolRenderer;
        let program = RuntimeTurnTraceJudgeProgram;
        let mut evaluations = Vec::with_capacity(turn_demos.len());

        for demo in turn_demos.iter().cloned() {
            let mut isolated_context =
                IsolatedEvalContext::new(config.clone(), compiled_prompts.clone()).await?;
            let tuning = resolve_program_tuning(&isolated_context.context, &program).await;
            let trace = run_turn_demo(
                &mut isolated_context.context,
                &TurnCompileSpec::from_demo(&demo),
            )
            .await?;
            let judge_focus = if demo.judge_focus.is_empty() {
                String::from("none")
            } else {
                demo.judge_focus.join("\n")
            };
            let rendered_trace = render_turn_trace_for_judge(&trace);
            let output = execute_program_with_ir_report(
                isolated_context.context.judge_llm.as_ref(),
                &isolated_context.context,
                &renderer,
                &program,
                program.dataset_ir(
                    current_system_prompt.clone(),
                    previous_system_prompt.clone(),
                    demo.title.clone(),
                    demo.scenario_summary.clone(),
                    demo.expected_behavior.clone(),
                    judge_focus,
                    rendered_trace.clone(),
                ),
                &tuning,
                TraceOrigin::Sleep,
            )
            .await?;
            evaluations.push(turn_demo_evaluation_from_output(
                &demo,
                &trace,
                &rendered_trace,
                &output.output,
            ));
            isolated_context.shutdown().await;
        }

        Ok(evaluations)
    }
}

pub async fn evaluate_runtime_prompt_candidate_rollout(
    config: Config,
    compiled_prompts: CompiledPromptStore,
    candidate: &EvaluationArtifactRuntimePromptCandidate,
    turn_demos: &[EvaluationArtifactTurnDemo],
) -> Result<Vec<EvaluationArtifactTurnDemoEvaluation>> {
    if turn_demos.is_empty() {
        return Ok(Vec::new());
    }
    let previous_system_prompt = runtime_system_prompt_text(&compiled_prompts);
    let current_prompt = current_runtime_system_prompt_artifact_from_store(&compiled_prompts);
    let candidate_prompt = apply_runtime_prompt_candidate_shared(&current_prompt, candidate);
    let candidate_compiled_prompts =
        compiled_prompts_with_runtime_prompt(&compiled_prompts, candidate_prompt);
    let current_system_prompt = runtime_system_prompt_text(&candidate_compiled_prompts);
    TurnCompileEngine::evaluate_turn_demos(
        config,
        candidate_compiled_prompts,
        turn_demos,
        current_system_prompt,
        previous_system_prompt,
    )
    .await
}

pub fn prompt_persona_path_sync() -> PathBuf {
    daat_locus_paths_sync().config_file(PROMPT_PERSONA_FILE_NAME)
}

pub fn load_prompt_persona_spec_sync() -> PromptPersonaSpec {
    let path = prompt_persona_path_sync();
    load_prompt_persona_spec_from_path_sync(&path, None, false)
}

pub fn load_or_create_prompt_persona_spec_sync(locale: &str) -> PromptPersonaSpec {
    let path = prompt_persona_path_sync();
    load_prompt_persona_spec_from_path_sync(&path, Some(locale), true)
}

fn load_prompt_persona_spec_from_path_sync(
    path: &Path,
    locale_hint: Option<&str>,
    create_if_missing: bool,
) -> PromptPersonaSpec {
    if !path.exists() {
        let default = prompt_persona_spec_from_default_prompt(locale_hint);
        if create_if_missing {
            write_default_prompt_persona_file_sync(path, &default);
        }
        return default;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            warn!(
                "failed to read prompt persona spec '{}': {error}",
                path.display()
            );
            return prompt_persona_spec_from_default_prompt(locale_hint);
        }
    };

    match parse_prompt_persona_markdown(&content) {
        Ok(parsed) => parsed,
        Err(error) => {
            warn!(
                "failed to parse prompt persona spec '{}': {error}",
                path.display()
            );
            prompt_persona_spec_from_default_prompt(locale_hint)
        }
    }
}

pub fn resolve_prompt_persona_language(
    persona: &PromptPersonaSpec,
    configured_locale: &str,
) -> String {
    let language = persona.language.trim();
    if language.is_empty() || language == PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE {
        configured_locale.trim().to_string()
    } else {
        language.to_string()
    }
}

fn write_default_prompt_persona_file_sync(path: &Path, spec: &PromptPersonaSpec) {
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        warn!(
            "failed to create prompt persona config dir '{}': {error}",
            parent.display()
        );
        return;
    }

    let content = render_prompt_persona_markdown(spec);
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return,
        Err(error) => {
            warn!(
                "failed to create default prompt persona spec '{}': {error}",
                path.display()
            );
            return;
        }
    };

    if let Err(error) = file.write_all(content.as_bytes()) {
        warn!(
            "failed to write default prompt persona spec '{}': {error}",
            path.display()
        );
    }
}

fn parse_prompt_persona_markdown(content: &str) -> Result<PromptPersonaSpec> {
    let (frontmatter_text, body) = split_prompt_persona_frontmatter(content)?;
    let frontmatter: PromptPersonaFrontmatter = serde_yaml::from_str(frontmatter_text)
        .map_err(|error| miette!("parse persona frontmatter failed: {error}"))?;
    let identity_summary = body.trim().to_string();
    if frontmatter.name.trim().is_empty() {
        return Err(miette!(
            "persona frontmatter field 'name' must not be empty"
        ));
    }
    if identity_summary.is_empty() {
        return Err(miette!("persona markdown body must not be empty"));
    }
    Ok(PromptPersonaSpec {
        name: frontmatter.name.trim().to_string(),
        language: normalized_persona_language(&frontmatter.language),
        identity_summary,
    })
}

fn normalized_persona_language(language: &str) -> String {
    let language = language.trim();
    if language.is_empty() {
        default_prompt_persona_language()
    } else {
        language.to_string()
    }
}

fn split_prompt_persona_frontmatter(content: &str) -> Result<(&str, &str)> {
    let rest = content
        .strip_prefix("---\r\n")
        .or_else(|| {
            content
                .strip_prefix("---\n")
                .or_else(|| content.strip_prefix("---"))
        })
        .ok_or_else(|| miette!("persona file missing frontmatter start"))?;
    let delimiter = rest
        .find("\n---\n")
        .map(|index| (index, 5))
        .or_else(|| rest.find("\r\n---\r\n").map(|index| (index, 7)))
        .or_else(|| rest.find("\n---\r\n").map(|index| (index, 6)))
        .or_else(|| rest.find("\r\n---\n").map(|index| (index, 6)))
        .ok_or_else(|| miette!("persona file missing frontmatter end"))?;
    Ok((&rest[..delimiter.0], &rest[delimiter.0 + delimiter.1..]))
}

pub fn render_prompt_persona_markdown(spec: &PromptPersonaSpec) -> String {
    let frontmatter = PromptPersonaFrontmatter {
        name: spec.name.clone(),
        language: spec.language.clone(),
    };
    let frontmatter_text = serde_yaml::to_string(&frontmatter)
        .unwrap_or_else(|_| format!("name: {}\nlanguage: {}\n", spec.name, spec.language));
    format!(
        "---\n{}---\n\n{}\n",
        frontmatter_text,
        spec.identity_summary.trim()
    )
}

impl TurnCompileSpec {
    pub fn from_demo(demo: &EvaluationArtifactTurnDemo) -> Self {
        Self {
            compile_key: demo.compile_key.clone(),
            title: demo.title.clone(),
            scenario_summary: demo.scenario_summary.clone(),
            initial_inputs: demo.initial_inputs.clone(),
            expected_behavior: demo.expected_behavior.clone(),
            judge_focus: demo.judge_focus.clone(),
        }
    }
}

pub fn render_turn_trace_for_judge(trace: &TurnTraceArtifact) -> String {
    let mut lines = vec![
        format!("span_id={}", trace.span_id),
        format!("turn_count={}", trace.turn_count),
        format!(
            "final_assistant_message={}",
            trace
                .final_assistant_message
                .as_deref()
                .map(single_line)
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "final_reply_message={}",
            trace
                .final_reply_message
                .as_deref()
                .map(single_line)
                .unwrap_or_else(|| "none".to_string())
        ),
    ];

    for (index, step) in trace.steps.iter().enumerate() {
        let turn_number = index + 1;
        lines.push(format!("turn[{turn_number}].id={}", step.turn_id));
        lines.push(format!(
            "turn[{turn_number}].current_doing={}",
            single_line(&step.current_doing)
        ));
        lines.push(format!(
            "turn[{turn_number}].description={}",
            single_line(&step.description)
        ));
        lines.push(format!(
            "turn[{turn_number}].observation={}",
            single_line(&step.observation)
        ));
        lines.push(format!(
            "turn[{turn_number}].actions={}",
            render_actions_inline(&step.actions)
        ));
        lines.push(format!(
            "turn[{turn_number}].assistant_message={}",
            step.assistant_message
                .as_deref()
                .map(single_line)
                .unwrap_or_else(|| "none".to_string())
        ));
        lines.push(format!(
            "turn[{turn_number}].reply_message={}",
            step.reply_message
                .as_deref()
                .map(single_line)
                .unwrap_or_else(|| "none".to_string())
        ));
    }

    lines.join("\n")
}

#[cfg(test)]
fn turn_trace_step_from_source_turn(turn: &TurnTraceSourceTurn) -> TurnTraceStep {
    TurnTraceStep {
        turn_id: turn.id.clone(),
        current_doing: turn.current_doing.clone(),
        description: turn.description.clone(),
        observation: turn.observation.clone(),
        actions: turn.actions.clone(),
        assistant_message: last_assistant_message(turn),
        reply_message: last_finish_and_send_reply_message(&turn.history_messages),
    }
}

async fn run_turn_demo(context: &mut Context, spec: &TurnCompileSpec) -> Result<TurnTraceArtifact> {
    let synthetic_update_id = unique_synthetic_telegram_id();
    let incoming_text = field_value(
        &spec.initial_inputs,
        &["incoming_text", "message", "user_message"],
    )
    .unwrap_or_else(|| spec.scenario_summary.clone());
    let chat_id = field_value(&spec.initial_inputs, &["chat_id"])
        .and_then(|value| value.parse::<i64>().ok().map(|_| value))
        .unwrap_or_else(|| synthetic_update_id.to_string());
    let chat_title = "Turn Compile Demo".to_string();
    let sender = field_value(&spec.initial_inputs, &["sender", "user_name"])
        .unwrap_or_else(|| "demo-user".to_string());

    context
        .telegram
        .register_known_chat(chat_id.clone(), chat_title.clone());

    let event_id = context
        .events
        .register_telegram_incoming(TelegramIncomingEvent {
            chat_id,
            chat_kind: "private".to_string(),
            chat_title,
            sender,
            incoming_text,
            telegram_update_id: synthetic_update_id,
            telegram_message_id: Some(synthetic_update_id),
            telegram_message_date: None,
            attachments: Vec::new(),
        })?;
    context
        .pending_work
        .enqueue(PendingWork::Event { event_id })?;
    let execution = execute_agent_loop_step(context, None).await;

    Ok(TurnTraceArtifact {
        span_id: format!("turn-demo:{}", spec.title),
        turn_count: 1,
        steps: vec![TurnTraceStep {
            turn_id: format!("turn-demo:{event_id}"),
            current_doing: execution.output.current_doing.clone(),
            description: execution.output.description.clone(),
            observation: execution.output.observation.clone(),
            actions: execution.output.actions.clone(),
            assistant_message: execution
                .history_messages
                .iter()
                .rev()
                .find(|message| message.is_assistant())
                .and_then(|message| message.text_content().map(str::to_string))
                .filter(|message| !message.trim().is_empty()),
            reply_message: last_finish_and_send_reply_message(&execution.history_messages),
        }],
        final_assistant_message: execution
            .history_messages
            .iter()
            .rev()
            .find(|message| message.is_assistant())
            .and_then(|message| message.text_content().map(str::to_string))
            .filter(|message| !message.trim().is_empty()),
        final_reply_message: last_finish_and_send_reply_message(&execution.history_messages),
    })
}

fn unique_synthetic_telegram_id() -> i64 {
    let bytes = Uuid::new_v4().into_bytes();
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[..8]);
    let id = (u64::from_be_bytes(raw) & (i64::MAX as u64)) as i64;
    if id == 0 { 1 } else { id }
}

fn turn_demo_evaluation_from_output(
    demo: &EvaluationArtifactTurnDemo,
    trace: &TurnTraceArtifact,
    rendered_trace: &str,
    output: &RuntimeTurnTraceJudgeOutput,
) -> EvaluationArtifactTurnDemoEvaluation {
    EvaluationArtifactTurnDemoEvaluation {
        compile_key: demo.compile_key.clone(),
        demo_title: demo.title.clone(),
        passed: output.passed,
        regression_detected: output.regression_detected,
        confidence: output.confidence,
        needed_changes: output.needed_changes.clone(),
        reason: output.reason.clone(),
        trace_summary: demo.scenario_summary.clone(),
        incoming_text: field_value(
            &demo.initial_inputs,
            &["incoming_text", "message", "user_message"],
        )
        .unwrap_or_default(),
        expected_behavior: demo.expected_behavior.clone(),
        judge_focus: demo.judge_focus.clone(),
        must_use_tools: demo.must_use_tools,
        must_not_final_answer_patterns: demo.must_not_final_answer_patterns.clone(),
        trace_rendered: rendered_trace.to_string(),
        final_assistant_message: trace.final_assistant_message.clone().unwrap_or_default(),
        final_reply_message: trace.final_reply_message.clone().unwrap_or_default(),
        actions_rendered: trace
            .steps
            .last()
            .map(|step| render_actions_inline(&step.actions))
            .unwrap_or_else(|| "none".to_string()),
    }
}

fn prompt_message_finish_and_send_reply_message(message: &HistoryMessage) -> Option<String> {
    let content = message.text_content().unwrap_or_default();
    if !message.is_tool() || !content.contains("\nname=finish_and_send\n") {
        return None;
    }
    let payload = content.split_once("payload=\n")?.1;
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    value
        .get("reply_message")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn last_finish_and_send_reply_message(history_messages: &[HistoryMessage]) -> Option<String> {
    history_messages
        .iter()
        .rev()
        .find_map(prompt_message_finish_and_send_reply_message)
}

pub fn apply_runtime_prompt_candidate_shared(
    current: &CompiledRuntimeSystemPrompt,
    candidate: &EvaluationArtifactRuntimePromptCandidate,
) -> CompiledRuntimeSystemPrompt {
    let mut system_additions = current.system_additions.clone();
    for patch in &candidate.prompt_patches {
        if !patch.trim().is_empty() && !system_additions.iter().any(|line| line == patch) {
            system_additions.push(patch.clone());
        }
    }
    CompiledRuntimeSystemPrompt {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        best_candidate: candidate.title.clone(),
        system_additions,
        selected_demo_titles: candidate.source_demo_titles.clone(),
        report: None,
    }
}

fn compiled_prompts_with_runtime_prompt(
    compiled_prompts: &CompiledPromptStore,
    runtime_prompt: CompiledRuntimeSystemPrompt,
) -> CompiledPromptStore {
    compiled_prompts
        .clone()
        .with_runtime_system_prompt(Some(runtime_prompt))
}

pub fn current_runtime_system_prompt_artifact_from_store(
    compiled_prompts: &CompiledPromptStore,
) -> CompiledRuntimeSystemPrompt {
    CompiledRuntimeSystemPrompt {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        best_candidate: "runtime_baseline".to_string(),
        system_additions: compiled_prompts.runtime_system_additions().to_vec(),
        selected_demo_titles: Vec::new(),
        report: None,
    }
}

pub fn runtime_system_prompt_text(compiled_prompts: &CompiledPromptStore) -> String {
    runtime_system_prompt_text_from_additions(compiled_prompts.runtime_system_additions())
}

impl Default for PromptPersonaSpec {
    fn default() -> Self {
        prompt_persona_spec_from_default_prompt(None)
    }
}

fn prompt_persona_spec_from_default_prompt(locale_hint: Option<&str>) -> PromptPersonaSpec {
    let language = match PERSONA_DEFAULT.language.trim() {
        "" => default_prompt_persona_language(),
        PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE => locale_hint
            .map(str::trim)
            .filter(|locale| !locale.is_empty())
            .unwrap_or(PROMPT_PERSONA_CONFIGURED_LOCALE_LANGUAGE)
            .to_string(),
        language => language.to_string(),
    };
    PromptPersonaSpec {
        name: PERSONA_DEFAULT.name.trim().to_string(),
        language: normalized_persona_language(&language),
        identity_summary: PERSONA_DEFAULT.identity_summary.trim().to_string(),
    }
}

#[cfg(test)]
fn last_assistant_message(turn: &TurnTraceSourceTurn) -> Option<String> {
    turn.history_messages
        .iter()
        .rev()
        .find(|message| message.is_assistant())
        .and_then(|message| {
            message
                .text_content()
                .map(|content| content.trim().to_string())
        })
        .filter(|message| !message.is_empty())
}

fn single_line(value: &str) -> String {
    value
        .split_whitespace()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_actions_inline(actions: &[EpisodeActionRecord]) -> String {
    if actions.is_empty() {
        return "none".to_string();
    }
    actions
        .iter()
        .map(|action| format!("{}({})", action.kind, single_line(&action.summary)))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn field_value(fields: &[ExampleField], names: &[&str]) -> Option<String> {
    fields
        .iter()
        .find(|field| names.iter().any(|name| field.name == *name))
        .map(|field| field.value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use crate::reasoning::runtime::HistoryMessage;

    use super::*;

    #[test]
    fn parse_prompt_persona_markdown_uses_frontmatter_and_body() {
        let parsed = parse_prompt_persona_markdown(
            r#"---
name: Test Persona
language: en-US
---

Be concise.
Preserve intent.
"#,
        )
        .expect("persona markdown should parse");

        assert_eq!(parsed.name, "Test Persona");
        assert_eq!(parsed.language, "en-US");
        assert_eq!(parsed.identity_summary, "Be concise.\nPreserve intent.");
    }

    #[test]
    fn parse_prompt_persona_markdown_defaults_language() {
        let parsed = parse_prompt_persona_markdown(
            r#"---
name: Test Persona
---

Use the configured locale by default.
"#,
        )
        .expect("persona markdown should parse");

        assert_eq!(parsed.language, "configured-locale");
        assert_eq!(
            parsed.identity_summary,
            "Use the configured locale by default."
        );
    }

    #[test]
    fn parse_prompt_persona_markdown_accepts_crlf_frontmatter() {
        let parsed = parse_prompt_persona_markdown(
            "---\r\nname: Test Persona\r\nlanguage: zh-CN\r\n---\r\n\r\nUse Chinese.\r\n",
        )
        .expect("persona markdown should parse");

        assert_eq!(parsed.name, "Test Persona");
        assert_eq!(parsed.language, "zh-CN");
        assert_eq!(parsed.identity_summary, "Use Chinese.");
    }

    #[test]
    fn default_prompt_persona_spec_uses_generated_default() {
        let parsed =
            parse_prompt_persona_markdown(crate::reasoning::prompts::PERSONA_DEFAULT_SOURCE)
                .expect("generated persona default should parse");
        assert_eq!(PromptPersonaSpec::default(), parsed);
    }

    #[test]
    fn default_prompt_persona_file_is_created_without_overwriting_existing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config").join(PROMPT_PERSONA_FILE_NAME);
        let initial = PromptPersonaSpec {
            name: "Initial Persona".to_string(),
            language: "en-US".to_string(),
            identity_summary: "Initial body.".to_string(),
        };
        write_default_prompt_persona_file_sync(&path, &initial);
        let initial_content = std::fs::read_to_string(&path).expect("initial persona file");
        let parsed_initial = parse_prompt_persona_markdown(&initial_content)
            .expect("written initial persona should parse");
        assert_eq!(parsed_initial, initial);

        let replacement = PromptPersonaSpec {
            name: "Replacement Persona".to_string(),
            language: "zh-CN".to_string(),
            identity_summary: "Replacement body.".to_string(),
        };
        write_default_prompt_persona_file_sync(&path, &replacement);
        let final_content = std::fs::read_to_string(&path).expect("final persona file");
        assert_eq!(final_content, initial_content);
    }

    #[test]
    fn missing_prompt_persona_file_is_created_with_configured_locale_hint() {
        for locale in ["zh-CN", "en-US"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().join("config").join(PROMPT_PERSONA_FILE_NAME);

            let loaded = load_prompt_persona_spec_from_path_sync(&path, Some(locale), true);
            assert_eq!(loaded.language, locale);

            let content = std::fs::read_to_string(&path).expect("written persona file");
            assert!(content.contains("{{name}}"));
            let written =
                parse_prompt_persona_markdown(&content).expect("written persona should parse");
            assert_eq!(written.language, locale);
        }
    }

    #[test]
    fn readonly_prompt_persona_load_does_not_create_missing_file() {
        for locale in ["zh-CN", "en-US"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().join("config").join(PROMPT_PERSONA_FILE_NAME);

            let loaded = load_prompt_persona_spec_from_path_sync(&path, Some(locale), false);
            assert_eq!(loaded.language, locale);
            assert!(!path.exists());
        }
    }

    #[test]
    fn prompt_persona_language_placeholder_resolves_to_configured_locale() {
        let persona = PromptPersonaSpec {
            name: "Test Persona".to_string(),
            language: "configured-locale".to_string(),
            identity_summary: "Body.".to_string(),
        };

        assert_eq!(resolve_prompt_persona_language(&persona, "zh-CN"), "zh-CN");
    }

    #[test]
    fn render_turn_trace_for_judge_includes_actions_and_assistant() {
        let turns = vec![TurnTraceSourceTurn {
            id: "turn-1".to_string(),
            current_doing: "analyze main".to_string(),
            description: "read main.rs".to_string(),
            observation: "needs more inspection".to_string(),
            actions: vec![crate::reasoning::episode::EpisodeActionRecord {
                kind: "assistant_message".to_string(),
                summary: "planning".to_string(),
            }],
            history_messages: vec![HistoryMessage {
                message: crate::reasoning::runtime::AgentMessage::assistant("I will continue."),
                activity_event: None,
                tool_call_activity_events: Vec::new(),
            }],
        }];

        let trace = TurnRolloutRunner::trace_from_turns("span-1", &turns);
        let rendered = render_turn_trace_for_judge(&trace);

        assert!(rendered.contains("turn[1].actions=assistant_message(planning)"));
        assert!(rendered.contains("turn[1].assistant_message=I will continue."));
    }

    #[test]
    fn unique_synthetic_telegram_id_is_positive_and_nonzero() {
        let id = unique_synthetic_telegram_id();
        assert!(id > 0);
    }
}
