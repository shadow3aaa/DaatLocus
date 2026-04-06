use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use miette::{Result, miette};
use ratatui::{
    DefaultTerminal, Frame, TerminalOptions, Viewport,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    try_init_with_options, try_restore,
    widgets::{Gauge, Paragraph},
};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    AgentLoopStepExecution, SpinovaHomeOverride, build_eval_context_with_compiled,
    config::Config,
    context::Context,
    events::TelegramIncomingEvent,
    execute_agent_loop_step,
    pending_work::PendingWork,
    reasoning::{
        episode::EpisodeActionRecord,
        compiled::{
            CompiledPromptStore, CompiledRuntimeSystemPrompt, CompiledRuntimeSystemPromptReport,
            RUNTIME_SYSTEM_PROMPT_COMPILE_KEY, save_compiled_runtime_system_prompt_for_model,
        },
        examples::ExampleField,
        programs::runtime_turn_demo_generator::{
            RuntimeTurnDemoGeneratorOutput, RuntimeTurnDemoGeneratorProgram,
        },
        programs::runtime_turn_prompt_patch_builder::RuntimeTurnPromptPatchBuilderProgram,
        programs::runtime_turn_trace_judge::{
            RuntimeTurnTraceJudgeOutput, RuntimeTurnTraceJudgeProgram,
        },
        render::openai_tools::OpenAIToolRenderer,
        runtime::{PromptMessage, PromptRole},
        runtime::{execute_program_with_ir_report, resolve_program_tuning},
        runtime_review::{RuntimeReviewSpan, RuntimeTurnRecord},
        evaluation_artifacts::{
            EvaluationArtifactRuntimePromptCandidate, EvaluationArtifactRuntimePromptEvolutionReport,
            EvaluationArtifactRuntimePromptEvolutionRound, EvaluationArtifactTurnDemo,
            EvaluationArtifactTurnDemoEvaluation, EvaluationArtifactsStore,
        },
        trace::TraceOrigin,
    },
    spinova_paths::spinova_paths,
};

const MAX_COLD_START_PROMPT_COMPILE_ROUNDS: usize = 8;
const PROMPT_PERSONA_FILE_NAME: &str = "prompt_persona.toml";
const INLINE_TURN_COMPILE_VIEWPORT_HEIGHT: u16 = 5;
const INLINE_TURN_COMPILE_PROGRESS_MAX_WIDTH: u16 = 56;
const COLD_START_ARTIFACT_SCOPE: &str = "cold_start";

static INLINE_TURN_COMPILE_ACTIVE: AtomicBool = AtomicBool::new(false);

fn emit_turn_compile_progress(message: impl AsRef<str>) {
    let message = message.as_ref();
    info!("{message}");
    if !INLINE_TURN_COMPILE_ACTIVE.load(Ordering::Relaxed) {
        println!("{message}");
    }
}

#[derive(Clone, Default)]
struct TurnCompileInlineProgressState {
    phase: String,
    total_rounds: usize,
    current_round: usize,
    total_demos: usize,
    current_demo: usize,
    current_demo_title: String,
    latest_output_preview: String,
}

struct TurnCompileInlineProgress {
    state: Arc<Mutex<TurnCompileInlineProgressState>>,
    running: Arc<AtomicBool>,
    render_thread: Option<JoinHandle<()>>,
}

impl TurnCompileInlineProgress {
    fn try_new() -> Option<Self> {
        let state = Arc::new(Mutex::new(TurnCompileInlineProgressState::default()));
        let running = Arc::new(AtomicBool::new(true));
        let state_for_thread = state.clone();
        let running_for_thread = running.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let render_thread = std::thread::spawn(move || {
            let terminal = match try_init_with_options(TerminalOptions {
                viewport: Viewport::Inline(INLINE_TURN_COMPILE_VIEWPORT_HEIGHT),
            }) {
                Ok(terminal) => {
                    let _ = ready_tx.send(true);
                    terminal
                }
                Err(_) => {
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            INLINE_TURN_COMPILE_ACTIVE.store(true, Ordering::Relaxed);
            run_turn_compile_inline_renderer(terminal, state_for_thread, running_for_thread);
            INLINE_TURN_COMPILE_ACTIVE.store(false, Ordering::Relaxed);
        });

        match ready_rx.recv().ok() {
            Some(true) => Some(Self {
                state,
                running,
                render_thread: Some(render_thread),
            }),
            _ => {
                let _ = render_thread.join();
                None
            }
        }
    }

    fn set_phase(&mut self, phase: impl Into<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.phase = phase.into();
        }
    }

    fn set_total_demos(&mut self, total_demos: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.total_demos = total_demos;
        }
    }

    fn start_round(&mut self, current_round: usize, total_rounds: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.current_round = current_round;
            state.total_rounds = total_rounds;
            state.current_demo = 0;
            state.current_demo_title.clear();
            state.phase = format!("round {current_round}/{total_rounds}");
        }
    }

    fn start_demo(&mut self, current_demo: usize, total_demos: usize, title: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.current_demo = current_demo;
            state.total_demos = total_demos;
            state.current_demo_title = title.to_string();
            state.phase = format!("evaluating demo {current_demo}/{total_demos}");
        }
    }

    fn set_latest_output_preview(&mut self, preview: Option<&str>) {
        if let Ok(mut state) = self.state.lock() {
            state.latest_output_preview = preview
                .map(truncate_for_inline_preview)
                .unwrap_or_else(|| "none".to_string());
        }
    }
}

impl Drop for TurnCompileInlineProgress {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.render_thread.take() {
            let _ = handle.join();
        }
    }
}

fn run_turn_compile_inline_renderer(
    mut terminal: DefaultTerminal,
    state: Arc<Mutex<TurnCompileInlineProgressState>>,
    running: Arc<AtomicBool>,
) {
    while running.load(Ordering::Relaxed) {
        let snapshot = state.lock().map(|state| state.clone()).unwrap_or_default();
        let _ = terminal.draw(|f| render_turn_compile_inline_progress(f, &snapshot));
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = try_restore();
}

fn render_turn_compile_inline_progress(f: &mut Frame, state: &TurnCompileInlineProgressState) {
    let area = f.area();
    if state.phase == "generating demos" {
        let generating = Paragraph::new(Line::from(vec![Span::styled(
            format!("{} generating demos", inline_working_glyph()),
            Style::default().fg(Color::White),
        )]));
        f.render_widget(generating, area);
        return;
    }

    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    let total_rounds = state.total_rounds.max(1);
    let total_demos = state.total_demos.max(1);
    let overall_ratio = if state.current_round == 0 {
        0.0
    } else {
        let completed_rounds = state.current_round.saturating_sub(1) as f64;
        let current_demo_progress = if state.current_demo == 0 {
            0.0
        } else {
            state.current_demo.min(total_demos) as f64 / total_demos as f64
        };
        ((completed_rounds + current_demo_progress) / total_rounds as f64).clamp(0.0, 1.0)
    };
    let title = Paragraph::new(Line::from(format!(
        "Cold-start compile  {}",
        if state.phase.is_empty() {
            "starting"
        } else {
            state.phase.as_str()
        }
    )))
    .style(Style::default().fg(Color::Cyan));
    f.render_widget(title, sections[0]);

    let latest = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{} ", inline_working_glyph()),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!(
                "latest: {}",
                if state.latest_output_preview.is_empty() {
                    "none"
                } else {
                    state.latest_output_preview.as_str()
                }
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    f.render_widget(latest, sections[1]);

    let progress_width = sections[2]
        .width
        .min(INLINE_TURN_COMPILE_PROGRESS_MAX_WIDTH)
        .max(1);
    let progress_row = Layout::horizontal([Constraint::Length(progress_width), Constraint::Min(0)])
        .split(sections[2]);
    let overall = Gauge::default()
        .gauge_style(Style::default().fg(Color::White))
        .ratio(overall_ratio)
        .label("");
    f.render_widget(overall, progress_row[0]);

    let progress_meta = Paragraph::new(
        Line::from(format!(
            "round {}/{}   demo {}/{}",
            state.current_round,
            state.total_rounds.max(1),
            state.current_demo,
            state.total_demos.max(1)
        ))
        .style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(progress_meta, sections[3]);
}

fn inline_working_glyph() -> &'static str {
    let frame = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        / 200) as usize
        % 4;
    ["•", "◦", "▪", "◦"][frame]
}

fn truncate_for_inline_preview(value: &str) -> String {
    let compact = single_line(value);
    let mut chars = compact.chars();
    let prefix = chars.by_ref().take(50).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn format_elapsed(duration: Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TurnCompileMode {
    ColdStart,
    SleepReplay,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PromptPersonaSpec {
    pub compile_key: String,
    pub name: String,
    #[serde(default = "default_prompt_persona_language")]
    pub language: String,
    pub identity_summary: String,
    pub channel_contract: String,
    #[serde(default)]
    pub behavior_rules: Vec<String>,
    #[serde(default)]
    pub terminal_answer_rules: Vec<String>,
    #[serde(default)]
    pub tool_use_rules: Vec<String>,
    #[serde(default)]
    pub anti_patterns: Vec<String>,
}

fn default_prompt_persona_language() -> String {
    "zh-CN".to_string()
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

pub struct TurnRolloutRunner;

struct IsolatedEvalContext {
    context: Context,
    home_override: SpinovaHomeOverride,
    home_path: PathBuf,
}

impl IsolatedEvalContext {
    async fn new(config: Config, compiled_prompts: CompiledPromptStore) -> Result<Self> {
        let home_path = std::env::temp_dir().join(format!(
            "spinova-turn-compile-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&home_path).await.map_err(|err| {
            miette!(
                "failed to create isolated turn-compile home '{}': {err}",
                home_path.display()
            )
        })?;
        let home_override = SpinovaHomeOverride::set(home_path.clone());
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

impl TurnRolloutRunner {
    pub fn trace_from_span(span: &RuntimeReviewSpan) -> TurnTraceArtifact {
        let steps = span
            .turns
            .iter()
            .map(turn_trace_step_from_runtime_turn)
            .collect::<Vec<_>>();
        let final_assistant_message = span
            .last_turn()
            .history_messages
            .iter()
            .rev()
            .find(|message| {
                matches!(
                    message.role,
                    crate::reasoning::runtime::PromptRole::Assistant
                )
            })
            .map(|message| message.content.clone())
            .filter(|message| !message.trim().is_empty());
        let final_reply_message = last_finish_and_send_reply_message(&span.last_turn().history_messages);
        TurnTraceArtifact {
            span_id: span.id.clone(),
            turn_count: span.turns.len(),
            steps,
            final_assistant_message,
            final_reply_message,
        }
    }
}

pub struct TurnCompileEngine;

impl TurnCompileEngine {
    pub async fn evaluate_from_review_spans(
        context: &mut Context,
        turn_demos: &[EvaluationArtifactTurnDemo],
        runtime_review_spans: &[RuntimeReviewSpan],
        current_system_prompt: String,
        previous_system_prompt: String,
    ) -> Result<Vec<EvaluationArtifactTurnDemoEvaluation>> {
        info!(
            "[turn-compile:{}] evaluating {} turn demos from runtime review spans",
            compile_mode_label(TurnCompileMode::SleepReplay),
            turn_demos.len()
        );
        evaluate_turn_demos_from_review_spans(
            context,
            turn_demos,
            runtime_review_spans,
            current_system_prompt,
            previous_system_prompt,
        )
        .await
    }

    async fn evaluate_cold_start(
        config: Config,
        compiled_prompts: CompiledPromptStore,
        turn_demos: &[EvaluationArtifactTurnDemo],
        current_system_prompt: String,
        previous_system_prompt: String,
        mut progress_ui: Option<&mut TurnCompileInlineProgress>,
    ) -> Result<Vec<EvaluationArtifactTurnDemoEvaluation>> {
        if turn_demos.is_empty() {
            return Ok(Vec::new());
        }
        emit_turn_compile_progress(format!(
            "[turn-compile:{}] evaluating {} cold-start turn demos",
            compile_mode_label(TurnCompileMode::ColdStart),
            turn_demos.len()
        ));

        let renderer = OpenAIToolRenderer;
        let program = RuntimeTurnTraceJudgeProgram;
        let mut evaluations = Vec::with_capacity(turn_demos.len());

        for (index, demo) in turn_demos.iter().cloned().enumerate() {
            let demo_number = index + 1;
            if let Some(ui) = progress_ui.as_deref_mut() {
                ui.start_demo(demo_number, turn_demos.len(), &demo.title);
                ui.set_phase(format!("rollout demo {demo_number}/{}", turn_demos.len()));
            }
            emit_turn_compile_progress(format!(
                "[turn-compile:{}] demo {}/{} rollout start: {}",
                compile_mode_label(TurnCompileMode::ColdStart),
                demo_number,
                turn_demos.len(),
                demo.title
            ));
            let rollout_started = Instant::now();
            let mut isolated_context =
                IsolatedEvalContext::new(config.clone(), compiled_prompts.clone()).await?;
            let tuning = resolve_program_tuning(&mut isolated_context.context, &program).await;
            let trace = run_cold_start_turn_demo(
                &mut isolated_context.context,
                &TurnCompileSpec::from_demo(&demo),
                progress_ui.as_deref_mut(),
            )
            .await?;
            emit_turn_compile_progress(format!(
                "[turn-compile:{}] demo {}/{} rollout finished in {}",
                compile_mode_label(TurnCompileMode::ColdStart),
                demo_number,
                turn_demos.len(),
                format_elapsed(rollout_started.elapsed())
            ));
            if let Some(ui) = progress_ui.as_deref_mut() {
                let latest_output = preview_text_from_trace(&trace);
                ui.set_latest_output_preview(latest_output);
                ui.set_phase(format!("judging demo {demo_number}/{}", turn_demos.len()));
            }
            let judge_focus = if demo.judge_focus.is_empty() {
                String::from("none")
            } else {
                demo.judge_focus.join("\n")
            };
            let rendered_trace = render_turn_trace_for_judge(&trace);
            emit_turn_compile_progress(format!(
                "[turn-compile:{}] demo {}/{} judge start: {}",
                compile_mode_label(TurnCompileMode::ColdStart),
                demo_number,
                turn_demos.len(),
                demo.title
            ));
            let judge_started = Instant::now();
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
            emit_turn_compile_progress(format!(
                "[turn-compile:{}] demo {}/{} judge finished in {} (passed={} regression={})",
                compile_mode_label(TurnCompileMode::ColdStart),
                demo_number,
                turn_demos.len(),
                format_elapsed(judge_started.elapsed()),
                output.output.passed,
                output.output.regression_detected
            ));
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

    pub async fn compile_cold_start(
        config: Config,
        compiled_prompts: CompiledPromptStore,
    ) -> Result<CompiledRuntimeSystemPrompt> {
        let artifacts = EvaluationArtifactsStore::open_scoped(Some(COLD_START_ARTIFACT_SCOPE)).await?;
        let mut progress_ui = TurnCompileInlineProgress::try_new();
        let persona_spec = load_or_seed_prompt_persona_spec().await?;
        if let Some(ui) = progress_ui.as_mut() {
            ui.set_phase("loading persona");
        }
        emit_turn_compile_progress(format!(
            "[turn-compile:{}] loaded persona '{}' from ~/.spinova/config/{}",
            compile_mode_label(TurnCompileMode::ColdStart),
            persona_spec.name,
            PROMPT_PERSONA_FILE_NAME
        ));
        if let Some(ui) = progress_ui.as_mut() {
            ui.set_phase("generating demos");
        }
        let turn_demos = generate_turn_demos_from_persona_spec(
            config.clone(),
            compiled_prompts.clone(),
            &persona_spec,
        )
        .await?;
        if turn_demos.is_empty() {
            return Err(miette!(
                "prompt persona spec '{}' produced zero turn demos",
                persona_spec.name
            ));
        }
        emit_turn_compile_progress(format!(
            "[turn-compile:{}] generated {} demos from persona spec",
            compile_mode_label(TurnCompileMode::ColdStart),
            turn_demos.len()
        ));
        if let Some(ui) = progress_ui.as_mut() {
            ui.set_total_demos(turn_demos.len());
            ui.set_phase(format!("generated {} demos", turn_demos.len()));
        }
        let _ = artifacts.replace_turn_demos(&turn_demos).await?;

        let mut current_prompt =
            current_runtime_system_prompt_artifact_from_store(&compiled_prompts);
        let mut best_prompt = current_prompt.clone();
        let mut best_passed = 0usize;
        let mut best_evaluations = Vec::new();
        let mut previous_system_prompt = String::from("none");
        let mut all_candidates = Vec::new();
        let mut round_history = Vec::new();
        let mut latest_regressions = 0usize;

        for round in 0..MAX_COLD_START_PROMPT_COMPILE_ROUNDS {
            emit_turn_compile_progress(format!(
                "[turn-compile:{}] round {}/{}",
                compile_mode_label(TurnCompileMode::ColdStart),
                round + 1,
                MAX_COLD_START_PROMPT_COMPILE_ROUNDS
            ));
            if let Some(ui) = progress_ui.as_mut() {
                ui.start_round(round + 1, MAX_COLD_START_PROMPT_COMPILE_ROUNDS);
            }
            let current_store =
                compiled_prompts_with_runtime_prompt(&compiled_prompts, current_prompt.clone());
            let current_system_prompt = runtime_system_prompt_text(&current_store);
            let evaluations = Self::evaluate_cold_start(
                config.clone(),
                current_store,
                &turn_demos,
                current_system_prompt.clone(),
                previous_system_prompt.clone(),
                progress_ui.as_mut(),
            )
            .await?;
            let _ = artifacts
                .replace_turn_demo_evaluations(&evaluations)
                .await?;
            let passed = evaluations.iter().filter(|item| item.passed).count();
            let regressions = evaluations
                .iter()
                .filter(|item| item.regression_detected)
                .count();
            latest_regressions = regressions;
            if regressions == 0 && passed >= best_passed {
                best_passed = passed;
                best_prompt = current_prompt.clone();
                best_evaluations = evaluations.clone();
            }

            round_history.push(EvaluationArtifactRuntimePromptEvolutionRound {
                round: round + 1,
                candidate: current_prompt.best_candidate.clone(),
                passed,
                total_demos: turn_demos.len(),
                regressions,
                rolled_back: false,
                accepted: regressions == 0 && passed == turn_demos.len(),
                suggestion_titles: evaluations
                    .iter()
                    .filter(|item| !item.passed)
                    .map(|item| format!("turn suggestion {}", item.demo_title))
                    .collect(),
                candidate_titles: Vec::new(),
            });

            if regressions == 0 && passed == turn_demos.len() {
                let mut accepted = current_prompt.clone();
                accepted.report = Some(build_compiled_runtime_system_prompt_report(
                    passed,
                    turn_demos.len(),
                    &turn_evaluation_summary_lines(&evaluations),
                ));
                save_compiled_runtime_system_prompt_for_model(
                    &config.main_model.model_name,
                    &accepted,
                )
                .await?;
                let _ = artifacts
                    .replace_runtime_prompt_evolution_reports(&[
                        build_runtime_prompt_evolution_report(
                            turn_demos.len(),
                            &accepted,
                            &round_history,
                            true,
                            false,
                            latest_regressions,
                            best_passed,
                        ),
                    ])
                    .await?;
                emit_turn_compile_progress(format!(
                    "[turn-compile:{}] accepted prompt after round {} ({}/{})",
                    compile_mode_label(TurnCompileMode::ColdStart),
                    round + 1,
                    passed,
                    turn_demos.len()
                ));
                if let Some(ui) = progress_ui.as_mut() {
                    ui.set_phase(format!("accepted {passed}/{}", turn_demos.len()));
                }
                return Ok(accepted);
            }

            let Some(candidate) = generate_turn_prompt_candidate(
                config.clone(),
                compiled_prompts_with_runtime_prompt(&compiled_prompts, current_prompt.clone()),
                &evaluations,
            )
            .await?
            else {
                break;
            };
            if let Some(ui) = progress_ui.as_mut() {
                ui.set_phase(format!(
                    "patching {}",
                    truncate_for_inline_preview(&candidate.title)
                ));
            }
            emit_turn_compile_progress(format!(
                "[turn-compile:{}] patch builder selected candidate start: {}",
                compile_mode_label(TurnCompileMode::ColdStart),
                candidate.title
            ));
            all_candidates.push(candidate.clone());
            let _ = artifacts
                .replace_runtime_prompt_candidates(&all_candidates)
                .await?;
            if let Some(last_round) = round_history.last_mut() {
                last_round.candidate_titles = vec![candidate.title.clone()];
            }
            emit_turn_compile_progress(format!(
                "[turn-compile:{}] patch builder selected candidate: {}",
                compile_mode_label(TurnCompileMode::ColdStart),
                candidate.title
            ));

            previous_system_prompt = current_system_prompt;
            current_prompt = apply_runtime_prompt_candidate_shared(&current_prompt, &candidate);
        }

        let mut selected = best_prompt;
        selected.report = Some(build_compiled_runtime_system_prompt_report(
            best_passed,
            turn_demos.len(),
            &turn_evaluation_summary_lines(&best_evaluations),
        ));
        save_compiled_runtime_system_prompt_for_model(&config.main_model.model_name, &selected)
            .await?;
        let _ = artifacts
            .replace_runtime_prompt_evolution_reports(&[build_runtime_prompt_evolution_report(
                turn_demos.len(),
                &selected,
                &round_history,
                best_passed == turn_demos.len() && latest_regressions == 0,
                false,
                latest_regressions,
                best_passed,
            )])
            .await?;
        emit_turn_compile_progress(format!(
            "[turn-compile:{}] selected best available prompt ({}/{})",
            compile_mode_label(TurnCompileMode::ColdStart),
            best_passed,
            turn_demos.len()
        ));
        if let Some(ui) = progress_ui.as_mut() {
            ui.set_phase(format!("selected best {best_passed}/{}", turn_demos.len()));
        }
        Ok(selected)
    }
}

async fn prompt_persona_path() -> PathBuf {
    spinova_paths().await.config_file(PROMPT_PERSONA_FILE_NAME)
}

async fn load_or_seed_prompt_persona_spec() -> Result<PromptPersonaSpec> {
    let path = prompt_persona_path().await;
    if !path.exists() {
        let seeded = PromptPersonaSpec::default();
        write_prompt_persona_spec(&path, &seeded).await?;
        return Ok(seeded);
    }

    let content = fs::read_to_string(&path).await.map_err(|error| {
        miette!(
            "failed to read prompt persona spec '{}': {error}",
            path.display()
        )
    })?;
    let parsed: PromptPersonaSpec = toml::from_str(&content).map_err(|error| {
        miette!(
            "failed to parse prompt persona spec '{}': {error}",
            path.display()
        )
    })?;
    let canonical = toml::to_string_pretty(&parsed)
        .map_err(|error| miette!("failed to render prompt persona spec: {error}"))?;
    if canonical.trim() != content.trim() {
        fs::write(&path, canonical).await.map_err(|error| {
            miette!(
                "failed to normalize prompt persona spec '{}': {error}",
                path.display()
            )
        })?;
    }
    Ok(parsed)
}

async fn write_prompt_persona_spec(path: &PathBuf, spec: &PromptPersonaSpec) -> Result<()> {
    let content = toml::to_string_pretty(spec)
        .map_err(|error| miette!("failed to render default prompt persona spec: {error}"))?;
    fs::write(path, content).await.map_err(|error| {
        miette!(
            "failed to write prompt persona spec '{}': {error}",
            path.display()
        )
    })?;
    Ok(())
}

async fn generate_turn_demos_from_persona_spec(
    config: Config,
    compiled_prompts: CompiledPromptStore,
    spec: &PromptPersonaSpec,
) -> Result<Vec<EvaluationArtifactTurnDemo>> {
    let workspace_facts = collect_turn_demo_workspace_facts().await?;
    let mut isolated_context = IsolatedEvalContext::new(config, compiled_prompts).await?;
    let renderer = OpenAIToolRenderer;
    let program = RuntimeTurnDemoGeneratorProgram;
    let tuning = resolve_program_tuning(&mut isolated_context.context, &program).await;
    let output = execute_program_with_ir_report(
        isolated_context.context.judge_llm.as_ref(),
        &isolated_context.context,
        &renderer,
        &program,
        program.dataset_ir(
            format!(
                "{}\n\n{}",
                crate::reasoning::prompts::SYSTEM_PROMPT_KERNEL,
                crate::reasoning::prompts::TOOL_ACTION_PROMPT
            ),
            render_persona_spec_for_generator(spec),
            workspace_facts,
        ),
        &tuning,
        TraceOrigin::Sleep,
    )
    .await?;
    isolated_context.shutdown().await;
    demos_from_generator_output(spec, &output.output)
}

fn demos_from_generator_output(
    spec: &PromptPersonaSpec,
    output: &RuntimeTurnDemoGeneratorOutput,
) -> Result<Vec<EvaluationArtifactTurnDemo>> {
    let usable_generated_demos = output
        .rule_demo_groups
        .iter()
        .flat_map(|group| {
            group.demos.iter().filter_map(move |demo| {
                if demo.title.trim().is_empty()
                    || demo.scenario_summary.trim().is_empty()
                    || demo.incoming_text.trim().is_empty()
                    || demo.expected_behavior.trim().is_empty()
                {
                    None
                } else {
                    Some((group.terminal_answer_rule.as_str(), demo))
                }
            })
        })
        .collect::<Vec<_>>();

    validate_generated_demo_coverage(spec, &output.rule_demo_groups, &usable_generated_demos)?;

    let demos = usable_generated_demos
        .iter()
        .map(|(terminal_rule, demo)| normalize_generated_turn_demo(spec, terminal_rule, demo))
        .collect::<Vec<_>>();

    if demos.is_empty() {
        return Err(miette!(
            "runtime_turn_demo_generator produced zero usable demos"
        ));
    }
    Ok(demos)
}

fn normalize_generated_turn_demo(
    spec: &PromptPersonaSpec,
    terminal_answer_rule: &str,
    demo: &crate::reasoning::programs::runtime_turn_demo_generator::GeneratedTurnDemo,
) -> EvaluationArtifactTurnDemo {
    let (requires_fresh_world_state, must_use_tools) =
        normalized_demo_requirements(demo.requires_fresh_world_state, demo.must_use_tools);
    let mut judge_focus = demo
        .judge_focus
        .iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if !judge_focus
        .iter()
        .any(|line| line.contains("终止时 assistant 文本必须可直接交付用户"))
    {
        judge_focus.push("终止时 assistant 文本必须可直接交付用户".to_string());
    }
    if requires_fresh_world_state
        && !judge_focus
            .iter()
            .any(|line| line.contains("答案依赖当前世界状态"))
    {
        judge_focus.push("答案依赖当前世界状态，不能凭空作答".to_string());
    }
    if must_use_tools
        && !judge_focus
            .iter()
            .any(|line| line.contains("必须先查再答") || line.contains("必须先用工具"))
    {
        judge_focus.push("必须先查再答，不能跳过工具调用".to_string());
    }

    EvaluationArtifactTurnDemo {
        compile_key: spec.compile_key.clone(),
        title: demo.title.trim().to_string(),
        scenario_summary: demo.scenario_summary.trim().to_string(),
        initial_inputs: vec![
            ExampleField {
                name: "incoming_text".to_string(),
                value: demo.incoming_text.trim().to_string(),
            },
            ExampleField {
                name: "chat_title".to_string(),
                value: spec.name.clone(),
            },
        ],
        expected_behavior: demo.expected_behavior.trim().to_string(),
        judge_focus,
        coverage_axes: demo
            .coverage_axes
            .iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        persona_anchors: demo
            .persona_anchors
            .iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        covered_terminal_answer_rules: vec![terminal_answer_rule.trim().to_string()],
        must_use_tools,
        must_not_final_answer_patterns: demo
            .must_not_final_answer_patterns
            .iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        must_end_with_terminal_answer: demo.must_end_with_terminal_answer,
        source_trace_ids: Vec::new(),
        confidence: demo.confidence.clamp(0.0, 1.0) as f32,
    }
}

fn normalized_demo_requirements(
    requires_fresh_world_state: bool,
    must_use_tools: bool,
) -> (bool, bool) {
    (requires_fresh_world_state || must_use_tools, must_use_tools)
}

fn validate_generated_demo_coverage(
    spec: &PromptPersonaSpec,
    groups: &[crate::reasoning::programs::runtime_turn_demo_generator::GeneratedTurnDemoGroup],
    demos: &[(
        &str,
        &crate::reasoning::programs::runtime_turn_demo_generator::GeneratedTurnDemo,
    )],
) -> Result<()> {
    if demos.len() < 2 {
        return Err(miette!(
            "runtime_turn_demo_generator produced {} demos; expected multiple demos covering more than one angle",
            demos.len()
        ));
    }

    let required_terminal_rules = spec
        .terminal_answer_rules
        .iter()
        .map(|rule| normalize_rule_text(rule))
        .filter(|rule| !rule.is_empty())
        .collect::<Vec<_>>();
    if !required_terminal_rules.is_empty() && groups.len() != required_terminal_rules.len() {
        return Err(miette!(
            "runtime_turn_demo_generator produced {} rule_demo_groups, expected exactly terminal_answer_rules count {}",
            groups.len(),
            required_terminal_rules.len()
        ));
    }

    let mut seen_titles = std::collections::HashSet::new();
    let mut seen_axes = std::collections::HashSet::new();
    let mut seen_anchors = std::collections::HashSet::new();
    let mut seen_terminal_rules = std::collections::HashSet::new();
    let required_terminal_rule_set = required_terminal_rules
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut usable_demo_counts_by_rule = std::collections::HashMap::new();
    for (terminal_rule, _demo) in demos {
        *usable_demo_counts_by_rule
            .entry(normalize_rule_text(terminal_rule))
            .or_insert(0usize) += 1;
    }
    for group in groups {
        let normalized_group_rule = normalize_rule_text(&group.terminal_answer_rule);
        if normalized_group_rule.is_empty() {
            return Err(miette!(
                "runtime_turn_demo_generator produced a rule_demo_group without terminal_answer_rule"
            ));
        }
        if !required_terminal_rule_set.contains(&normalized_group_rule) {
            return Err(miette!(
                "runtime_turn_demo_generator produced unknown terminal_answer_rule '{}'",
                normalized_group_rule
            ));
        }
        if !seen_terminal_rules.insert(normalized_group_rule.clone()) {
            return Err(miette!(
                "runtime_turn_demo_generator produced duplicate rule_demo_group for terminal rule '{}'",
                normalized_group_rule
            ));
        }
        if usable_demo_counts_by_rule
            .get(&normalized_group_rule)
            .copied()
            .unwrap_or_default()
            == 0
        {
            return Err(miette!(
                "runtime_turn_demo_generator produced rule_demo_group for terminal rule '{}' but no usable demos in that group",
                normalized_group_rule
            ));
        }
    }

    for (_terminal_rule, demo) in demos {
        let normalized_title = demo.title.trim().to_string();
        if !seen_titles.insert(normalized_title.clone()) {
            return Err(miette!(
                "runtime_turn_demo_generator produced duplicate demo title '{}'",
                normalized_title
            ));
        }
        for axis in &demo.coverage_axes {
            let normalized = axis.trim().to_ascii_lowercase();
            if !normalized.is_empty() {
                seen_axes.insert(normalized);
            }
        }
        for anchor in &demo.persona_anchors {
            let normalized = anchor.trim().to_ascii_lowercase();
            if !normalized.is_empty() {
                seen_anchors.insert(normalized);
            }
        }
    }

    if seen_axes.len() < 2 {
        return Err(miette!(
            "runtime_turn_demo_generator produced demos that are too narrow; expected coverage across multiple risk axes"
        ));
    }

    if seen_anchors.len() < 2 {
        return Err(miette!(
            "runtime_turn_demo_generator did not anchor demos to enough persona directions"
        ));
    }

    if !required_terminal_rule_set.is_empty() {
        let missing_rules = required_terminal_rules
            .iter()
            .filter(|rule| !seen_terminal_rules.contains(*rule))
            .cloned()
            .collect::<Vec<_>>();
        if !missing_rules.is_empty() {
            return Err(miette!(
                "runtime_turn_demo_generator did not cover all terminal_answer_rules; missing: {}",
                missing_rules.join(" | ")
            ));
        }
    }

    Ok(())
}

fn normalize_rule_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn render_persona_spec_for_generator(spec: &PromptPersonaSpec) -> String {
    let mut sections = vec![
        format!("name:\n- {}", spec.name.trim()),
        format!("language:\n- {}", spec.language.trim()),
        format!("identity_summary:\n- {}", spec.identity_summary.trim()),
        format!("channel_contract:\n- {}", spec.channel_contract.trim()),
    ];
    if !spec.behavior_rules.is_empty() {
        sections.push(format!(
            "behavior_rules:\n{}",
            render_rule_list(&spec.behavior_rules)
        ));
    }
    if !spec.terminal_answer_rules.is_empty() {
        sections.push(format!(
            "terminal_answer_rules:\n{}",
            render_rule_list(&spec.terminal_answer_rules)
        ));
    }
    if !spec.tool_use_rules.is_empty() {
        sections.push(format!(
            "tool_use_rules:\n{}",
            render_rule_list(&spec.tool_use_rules)
        ));
    }
    if !spec.anti_patterns.is_empty() {
        sections.push(format!(
            "anti_patterns:\n{}",
            render_rule_list(&spec.anti_patterns)
        ));
    }
    sections.join("\n")
}

async fn collect_turn_demo_workspace_facts() -> Result<String> {
    let cwd = crate::resolve_runtime_workspace_dir()?;
    fs::create_dir_all(&cwd).await.map_err(|error| {
        miette!(
            "failed to create runtime workspace for turn demos '{}': {error}",
            cwd.display()
        )
    })?;
    let top_level_entries = read_dir_entry_names(&cwd).await?;
    let src_dir = cwd.join("src");
    let src_entries = if src_dir.exists() {
        read_dir_entry_names(&src_dir).await?
    } else {
        Vec::new()
    };

    let mut sections = vec![
        format!("cwd: {}", cwd.display()),
        format!(
            "top_level_entries: {}",
            if top_level_entries.is_empty() {
                "none".to_string()
            } else {
                top_level_entries.join(", ")
            }
        ),
        format!(
            "src_entries: {}",
            if src_entries.is_empty() {
                "none".to_string()
            } else {
                src_entries.join(", ")
            }
        ),
        "known_runtime_facts: Terminal is the only interactive app; Telegram is an event transport, not a app.".to_string(),
        "known_runtime_facts: Fresh incoming messages arrive as events and are judged semantically; do not invent hidden inbox navigation state.".to_string(),
        "known_runtime_facts: If a demo depends on todos, events, app health, or repository facts, it should ask about current visible state rather than fabricate specific unseen records.".to_string(),
        "known_runtime_facts: Runtime snapshot already includes concise TodoBoard summary, event list, and app structural state; read-only questions about those visible summaries do not inherently require tools.".to_string(),
        "known_runtime_facts: Repository file existence, file contents, directory structure, and any fact not already rendered in runtime snapshot still require tools.".to_string(),
    ];

    if !top_level_entries.iter().any(|entry| entry.ends_with(".py")) {
        sections.push(
            "absent_file_types: no top-level Python entrypoints are visible in workspace facts."
                .to_string(),
        );
    }

    Ok(sections.join("\n"))
}

async fn read_dir_entry_names(path: &PathBuf) -> Result<Vec<String>> {
    let mut entries = fs::read_dir(path).await.map_err(|error| {
        miette!(
            "failed to read directory '{}' while collecting turn demo facts: {error}",
            path.display()
        )
    })?;
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        miette!(
            "failed to read directory entry from '{}' while collecting turn demo facts: {error}",
            path.display()
        )
    })? {
        let name = entry.file_name();
        let name = name.to_string_lossy().trim().to_string();
        if !name.is_empty() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn render_rule_list(items: &[String]) -> String {
    items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn turn_prompt_suggestions_from_evaluations(
    evaluations: &[EvaluationArtifactTurnDemoEvaluation],
) -> Vec<String> {
    evaluations
        .iter()
        .filter(|item| !item.passed)
        .map(|item| format!("turn suggestion {}", item.demo_title))
        .collect()
}

pub fn turn_evaluation_stats(evaluations: &[EvaluationArtifactTurnDemoEvaluation]) -> (usize, usize) {
    let passed = evaluations.iter().filter(|item| item.passed).count();
    let regressions = evaluations
        .iter()
        .filter(|item| item.regression_detected)
        .count();
    (passed, regressions)
}

pub fn is_acceptable_turn_round(passed: usize, total: usize, has_regression: bool) -> bool {
    !has_regression && passed == total
}

pub async fn generate_turn_prompt_candidates(
    context: &mut Context,
    evaluations: &[EvaluationArtifactTurnDemoEvaluation],
    sleep_hypotheses: String,
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
    let program = RuntimeTurnPromptPatchBuilderProgram;
    let tuning = resolve_program_tuning(context, &program).await;
    let current_system_prompt = runtime_system_prompt_text(&context.compiled_prompts);
    let output = execute_program_with_ir_report(
        context.judge_llm.as_ref(),
        context,
        &renderer,
        &program,
        program.dataset_ir(
            current_system_prompt,
            render_failed_turn_demos(&failed),
            render_turn_judge_feedback(&failed),
            sleep_hypotheses,
        ),
        &tuning,
        TraceOrigin::Sleep,
    )
    .await?;

    let Some(candidate) = turn_prompt_candidate_from_output(&output.output, &failed) else {
        return Ok(Vec::new());
    };
    Ok(vec![candidate])
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

pub async fn evaluate_turn_demos_from_review_spans(
    context: &mut Context,
    turn_demos: &[EvaluationArtifactTurnDemo],
    runtime_review_spans: &[RuntimeReviewSpan],
    current_system_prompt: String,
    previous_system_prompt: String,
) -> Result<Vec<EvaluationArtifactTurnDemoEvaluation>> {
    if turn_demos.is_empty() {
        return Ok(Vec::new());
    }

    let span_by_id = runtime_review_spans
        .iter()
        .map(|span| (span.id.as_str(), span))
        .collect::<HashMap<_, _>>();
    let renderer = OpenAIToolRenderer;
    let program = RuntimeTurnTraceJudgeProgram;
    let tuning = resolve_program_tuning(context, &program).await;
    let mut evaluations = Vec::with_capacity(turn_demos.len());

    for demo in turn_demos.iter().cloned() {
        let Some(span) = demo
            .source_trace_ids
            .iter()
            .find_map(|trace_id| span_by_id.get(trace_id.as_str()).copied())
        else {
            warn!(
                "turn demo '{}' skipped: no matching runtime review span found in source_trace_ids",
                demo.title
            );
            continue;
        };

        let judge_focus = if demo.judge_focus.is_empty() {
            String::from("none")
        } else {
            demo.judge_focus.join("\n")
        };
        let trace = TurnRolloutRunner::trace_from_span(span);
        let turn_trace = render_turn_trace_for_judge(&trace);
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
                turn_trace.clone(),
            ),
            &tuning,
            TraceOrigin::Sleep,
        )
        .await?;

        evaluations.push(turn_demo_evaluation_from_output(
            &demo,
            &trace,
            &turn_trace,
            &output.output,
        ));
    }

    Ok(evaluations)
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

fn turn_trace_step_from_runtime_turn(turn: &RuntimeTurnRecord) -> TurnTraceStep {
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

async fn run_cold_start_turn_demo(
    context: &mut Context,
    spec: &TurnCompileSpec,
    progress_ui: Option<&mut TurnCompileInlineProgress>,
) -> Result<TurnTraceArtifact> {
    let synthetic_update_id = unique_synthetic_telegram_id();
    let incoming_text = field_value(
        &spec.initial_inputs,
        &["incoming_text", "message", "user_message"],
    )
    .unwrap_or_else(|| spec.scenario_summary.clone());
    let chat_id = field_value(&spec.initial_inputs, &["chat_id"])
        .and_then(|value| value.parse::<i64>().ok().map(|_| value))
        .unwrap_or_else(|| synthetic_update_id.to_string());
    let chat_title = field_value(&spec.initial_inputs, &["chat_title"])
        .unwrap_or_else(|| "Turn Compile Demo".to_string());
    let sender = field_value(&spec.initial_inputs, &["sender", "user_name"])
        .unwrap_or_else(|| "demo-user".to_string());

    context
        .telegram
        .register_known_chat(chat_id.clone(), chat_title.clone());

    let event_id = context
        .events
        .register_telegram_incoming(TelegramIncomingEvent {
            chat_id,
            chat_title,
            sender,
            incoming_text,
            telegram_update_id: synthetic_update_id,
            telegram_message_id: Some(synthetic_update_id),
            telegram_message_date: None,
        })?;
    context
        .pending_work
        .enqueue(PendingWork::Event { event_id })?;
    let execution = execute_agent_loop_step(context, None).await;
    if let Some(ui) = progress_ui {
        let latest_output = preview_text_from_execution(&execution);
        ui.set_latest_output_preview(latest_output.as_deref());
    }

    Ok(TurnTraceArtifact {
        span_id: format!("cold-start-demo:{}", spec.title),
        turn_count: 1,
        steps: vec![TurnTraceStep {
            turn_id: format!("cold-start-turn:{event_id}"),
            current_doing: execution.output.current_doing.clone(),
            description: execution.output.description.clone(),
            observation: execution.output.observation.clone(),
            actions: execution.output.actions.clone(),
            assistant_message: execution
                .history_messages
                .iter()
                .rev()
                .find(|message| matches!(message.role, PromptRole::Assistant))
                .map(|message| message.content.clone())
                .filter(|message| !message.trim().is_empty()),
            reply_message: last_finish_and_send_reply_message(&execution.history_messages),
        }],
        final_assistant_message: execution
            .history_messages
            .iter()
            .rev()
            .find(|message| matches!(message.role, PromptRole::Assistant))
            .map(|message| message.content.clone())
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

async fn generate_turn_prompt_candidate(
    config: Config,
    compiled_prompts: CompiledPromptStore,
    evaluations: &[EvaluationArtifactTurnDemoEvaluation],
) -> Result<Option<EvaluationArtifactRuntimePromptCandidate>> {
    let failed = evaluations
        .iter()
        .filter(|item| !item.passed)
        .cloned()
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return Ok(None);
    }

    let mut isolated_context = IsolatedEvalContext::new(config, compiled_prompts).await?;
    let renderer = OpenAIToolRenderer;
    let program = RuntimeTurnPromptPatchBuilderProgram;
    let tuning = resolve_program_tuning(&mut isolated_context.context, &program).await;
    let current_system_prompt = runtime_system_prompt_text(&isolated_context.context.compiled_prompts);
    emit_turn_compile_progress(format!(
        "[turn-compile:{}] patch builder start for {} failed demos",
        compile_mode_label(TurnCompileMode::ColdStart),
        failed.len()
    ));
    let patch_started = Instant::now();
    let output = execute_program_with_ir_report(
        isolated_context.context.judge_llm.as_ref(),
        &isolated_context.context,
        &renderer,
        &program,
        program.dataset_ir(
            current_system_prompt,
            render_failed_turn_demos(&failed),
            render_turn_judge_feedback(&failed),
            "none".to_string(),
        ),
        &tuning,
        TraceOrigin::Sleep,
    )
    .await?;
    emit_turn_compile_progress(format!(
        "[turn-compile:{}] patch builder finished in {}",
        compile_mode_label(TurnCompileMode::ColdStart),
        format_elapsed(patch_started.elapsed())
    ));
    isolated_context.shutdown().await;

    Ok(turn_prompt_candidate_from_output(&output.output, &failed))
}

fn render_failed_turn_demos(evaluations: &[EvaluationArtifactTurnDemoEvaluation]) -> String {
    evaluations
        .iter()
        .map(|item| {
            let judge_focus = if item.judge_focus.is_empty() {
                "none".to_string()
            } else {
                item.judge_focus.join(" | ")
            };
            let bad_patterns = if item.must_not_final_answer_patterns.is_empty() {
                "none".to_string()
            } else {
                item.must_not_final_answer_patterns.join(" | ")
            };
            format!(
                "- title={}\n  incoming_text={}\n  expected_behavior={}\n  must_use_tools={}\n  must_not_final_answer_patterns={}\n  judge_focus={}\n  reason={}\n  trace_summary={}\n  final_assistant_message={}\n  final_reply_message={}\n  actions_rendered={}\n  trace=\n{}",
                item.demo_title,
                single_line(&item.incoming_text),
                single_line(&item.expected_behavior),
                item.must_use_tools,
                bad_patterns,
                judge_focus,
                item.reason.trim(),
                single_line(&item.trace_summary),
                single_line(&item.final_assistant_message),
                single_line(&item.final_reply_message),
                single_line(&item.actions_rendered),
                item.trace_rendered
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn prompt_message_finish_and_send_reply_message(message: &PromptMessage) -> Option<String> {
    if !matches!(message.role, PromptRole::Tool)
        || !message.content.contains("\nname=finish_and_send\n")
    {
        return None;
    }
    let payload = message.content.split_once("payload=\n")?.1;
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    value
        .get("reply_message")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn last_finish_and_send_reply_message(history_messages: &[PromptMessage]) -> Option<String> {
    history_messages
        .iter()
        .rev()
        .find_map(prompt_message_finish_and_send_reply_message)
}

fn render_turn_judge_feedback(evaluations: &[EvaluationArtifactTurnDemoEvaluation]) -> String {
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

fn turn_prompt_candidate_from_output(
    output: &crate::reasoning::programs::runtime_turn_prompt_patch_builder::RuntimeTurnPromptPatchBuilderOutput,
    evaluations: &[EvaluationArtifactTurnDemoEvaluation],
) -> Option<EvaluationArtifactRuntimePromptCandidate> {
    if output.prompt_patches.is_empty() {
        return None;
    }
    Some(EvaluationArtifactRuntimePromptCandidate {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        title: if output.title.trim().is_empty() {
            "turn cold-start candidate".to_string()
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
        source_hypotheses: Vec::new(),
    })
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
        best_candidate: "cold_start_baseline".to_string(),
        system_additions: compiled_prompts.runtime_system_additions().to_vec(),
        selected_demo_titles: Vec::new(),
        report: None,
    }
}

pub fn runtime_system_prompt_text(compiled_prompts: &CompiledPromptStore) -> String {
    let mut lines = vec![
        crate::reasoning::prompts::SYSTEM_PROMPT_KERNEL.to_string(),
        crate::reasoning::prompts::TOOL_ACTION_PROMPT.to_string(),
    ];
    lines.extend(
        compiled_prompts
            .runtime_system_additions()
            .iter()
            .filter(|line| !line.trim().is_empty())
            .cloned(),
    );
    lines.join("\n\n")
}

pub fn choose_best_non_regressing_prompt_shared(
    best_prompt: &CompiledRuntimeSystemPrompt,
    best_passed: usize,
    current_prompt: &CompiledRuntimeSystemPrompt,
    current_passed: usize,
    has_regression: bool,
) -> (CompiledRuntimeSystemPrompt, usize) {
    if !has_regression && current_passed >= best_passed {
        (current_prompt.clone(), current_passed)
    } else {
        (best_prompt.clone(), best_passed)
    }
}

pub fn build_compiled_runtime_system_prompt_report(
    score: usize,
    total_cases: usize,
    summary_lines: &[String],
) -> CompiledRuntimeSystemPromptReport {
    let judge_summary = if summary_lines.is_empty() {
        None
    } else {
        Some(summary_lines.join("\n"))
    };
    CompiledRuntimeSystemPromptReport {
        score,
        total_cases,
        judge_summary,
    }
}

pub fn build_runtime_prompt_evolution_report(
    total_demos: usize,
    selected_prompt: &CompiledRuntimeSystemPrompt,
    round_history: &[EvaluationArtifactRuntimePromptEvolutionRound],
    accepted: bool,
    rolled_back: bool,
    regressions: usize,
    passed: usize,
) -> EvaluationArtifactRuntimePromptEvolutionReport {
    EvaluationArtifactRuntimePromptEvolutionReport {
        compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
        rounds: round_history.len(),
        accepted,
        rolled_back,
        passed,
        total_demos,
        regressions,
        selected_candidate: selected_prompt.best_candidate.clone(),
        selected_demo_titles: selected_prompt.selected_demo_titles.clone(),
        final_system_additions: selected_prompt.system_additions.clone(),
        round_history: round_history.to_vec(),
    }
}

pub fn turn_evaluation_summary_lines(
    evaluations: &[EvaluationArtifactTurnDemoEvaluation],
) -> Vec<String> {
    evaluations
        .iter()
        .map(|item| {
            format!(
                "- {}: passed={} regression={} reason={}",
                item.demo_title,
                item.passed,
                item.regression_detected,
                single_line(&item.reason)
            )
        })
        .collect()
}

impl Default for PromptPersonaSpec {
    fn default() -> Self {
        Self {
            compile_key: RUNTIME_SYSTEM_PROMPT_COMPILE_KEY.to_string(),
            name: "Spinova".to_string(),
            language: default_prompt_persona_language(),
            identity_summary: "Spinova 是一个冷静、机敏、结果导向的猫娘执行型智能体。它默认使用中文交流，回答要自然带一点猫娘口吻，并在合适位置带“喵”，但不能因此牺牲信息密度与可执行性。".to_string(),
            channel_contract: "当一轮自然结束时，最后 assistant 文本会直接发送给外部用户；因此停止就意味着交付。除非用户主动闲聊，否则默认使用简洁中文直接作答，保持猫娘口吻并适度带“喵”，但不要展示内部过程。".to_string(),
            behavior_rules: vec![
                "先判断问题属于直接答复、查证、执行还是决策，再行动。".to_string(),
                "信息已在当前上下文或 runtime snapshot 中可得时，直接给结论，不要为了显得谨慎而绕路。".to_string(),
                "当用户要求你自己决定时，要给出明确选择和理由，不要把决策再推回给用户。".to_string(),
                "语气保持冷静、具体、短句、少套话；默认使用中文猫娘口吻。".to_string(),
                "在直接回复用户时，默认应自然带“喵”；但不要每句都机械重复。".to_string(),
            ],
            terminal_answer_rules: vec![
                "最终回复必须像一条可以直接发送给用户的消息，不暴露内部流程。".to_string(),
                "先给结论，再补必要依据；避免长铺垫。".to_string(),
                "最终回复应总结已确认事实，而不是描述接下来会做什么。".to_string(),
                "不要以“接下来我会”“后续将”“稍后继续”这类计划文本收尾。".to_string(),
                "如果关键事实不足，明确指出缺口和下一步查证动作；不要含糊其辞。".to_string(),
                "只要不是明显不适合卖萌的高风险场景，最终回复应保留轻微猫娘风格，并出现“喵”。".to_string(),
            ],
            tool_use_rules: vec![
                "代码库文件、目录、内容，以及 runtime snapshot 未直接给出的事实，必须先查证。".to_string(),
                "TodoBoard 摘要、事件列表、应用结构状态等 runtime snapshot 已直接可见的摘要，可以直接据此回答。".to_string(),
                "工具调用要服务于结论，不要为了展示过程而调用。".to_string(),
                "不要在证据不足时提前停止。".to_string(),
            ],
            anti_patterns: vec![
                "完全丢失猫娘口吻，像普通客服或普通助手".to_string(),
                "把“喵”机械堆在每一句后面".to_string(),
                "客服式热情寒暄或空泛安抚".to_string(),
                "阶段性计划伪装成最终回复".to_string(),
                "浅查一下就交差".to_string(),
                "为了显得谨慎而机械调用工具".to_string(),
                "把决策责任推回给用户".to_string(),
            ],
        }
    }
}

fn compile_mode_label(mode: TurnCompileMode) -> &'static str {
    match mode {
        TurnCompileMode::ColdStart => "cold-start",
        TurnCompileMode::SleepReplay => "sleep-replay",
    }
}

fn last_assistant_message(turn: &RuntimeTurnRecord) -> Option<String> {
    turn.history_messages
        .iter()
        .rev()
        .find(|message| {
            matches!(
                message.role,
                crate::reasoning::runtime::PromptRole::Assistant
            )
        })
        .map(|message| message.content.trim().to_string())
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

fn preview_summary_from_action(action: &EpisodeActionRecord) -> Option<&str> {
    if action.kind == "finish_and_send" {
        return None;
    }
    let summary = action.summary.trim();
    (!summary.is_empty()).then_some(summary)
}

fn preview_text_from_trace(trace: &TurnTraceArtifact) -> Option<&str> {
    trace
        .steps
        .iter()
        .rev()
        .find_map(|step| step.assistant_message.as_deref())
        .or_else(|| trace.steps.iter().rev().find_map(|step| step.reply_message.as_deref()))
        .or_else(|| trace.steps.iter().rev().find_map(|step| step.actions.last().and_then(preview_summary_from_action)))
}

fn preview_text_from_execution(execution: &AgentLoopStepExecution) -> Option<String> {
    execution
        .history_messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, PromptRole::Assistant))
        .map(|message| message.content.trim().to_string())
        .filter(|text| !text.is_empty())
        .or_else(|| {
            execution
                .history_messages
                .iter()
                .rev()
                .find_map(prompt_message_finish_and_send_reply_message)
        })
        .or_else(|| {
            execution
                .output
                .actions
                .last()
                .and_then(preview_summary_from_action)
                .map(ToOwned::to_owned)
        })
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
    use crate::reasoning::{
        runtime::{PromptMessage, PromptRole},
        runtime_review::RuntimeReviewSpan,
    };

    use super::*;

    #[test]
    fn render_turn_trace_for_judge_includes_actions_and_assistant() {
        let span = RuntimeReviewSpan {
            id: "span-1".to_string(),
            turns: vec![RuntimeTurnRecord {
                id: "turn-1".to_string(),
                recorded_at_ms: 1,
                current_doing: "analyze main".to_string(),
                description: "read main.rs".to_string(),
                observation: "needs more inspection".to_string(),
                actions: vec![crate::reasoning::episode::EpisodeActionRecord {
                    kind: "assistant_message".to_string(),
                    summary: "planning".to_string(),
                }],
                before_snapshot_text: String::new(),
                after_snapshot_text: String::new(),
                history_messages: vec![PromptMessage {
                    role: PromptRole::Assistant,
                    content: "I will continue.".to_string(),
                    tool_ui_event: None,
                    tool_call_ui_events: Vec::new(),
                }],
                metadata: std::collections::BTreeMap::new(),
            }],
        };

        let trace = TurnRolloutRunner::trace_from_span(&span);
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
