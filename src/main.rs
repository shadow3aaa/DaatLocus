mod apply_patch;
mod config;
mod context;
mod context_budget;
mod core;
mod dashboard;
mod device;
mod events;
mod hindsight;
mod logging;
mod memory;
mod pending_work;
mod providers;
mod reasoning;
mod runtime_context;
mod runtime_tools;
mod sandbox;
mod snapshot;
mod spinova_paths;
mod system_info;
mod telegram_acl;
mod telegram_device;
mod telegram_transport;
mod terminal_device;
mod terminal_process;
mod todo_board;
mod tool_ui;
mod work_state;

use std::{
    env,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    apply_patch::{PatchOperationKind, apply_patch_in_root, summarize_apply_patch_error},
    config::load_config,
    context::Context,
    context_budget::{approx_token_count, estimate_agent_turn_request, is_context_budget_exceeded},
    dashboard::{
        DashboardActivityEvent, DashboardControlCommand, DashboardState,
        activity_cell_from_tool_ui_event, activity_cells_from_tool_call_ui_event,
        apply_activity_event, assistant_activity_cell, render_activity_from_messages,
        run_tui_dashboard,
    },
    device::{DeviceId, DeviceManager},
    events::{EventPayload, EventStatus, EventStore, EventView},
    hindsight::{HindsightClient, HindsightRecallOptions},
    logging::{
        RuntimeStatusLevel, clear_runtime_status, init_logging, set_runtime_status,
        write_current_turn_messages_dump, write_current_turn_response_dump,
        write_current_turn_response_error_dump,
    },
    memory::{Memory, RuntimeTurnDraft},
    pending_work::{PendingWork, PendingWorkQueue},
    providers::OpenAIClient,
    reasoning::{
        adapters::swe_train_source::SweTrainSource,
        compiled::{
            COMPILED_DIR_NAME, CompiledPromptStore, load_all_compiled_programs_for_model,
            load_compiled_runtime_system_prompt_for_model,
        },
        environment::EpisodeObservation,
        episode::{
            EpisodeActionRecord, EpisodeMetric, EpisodeOutcome, EpisodeStatus, EpisodeStep,
            EpisodeTask,
        },
        episode_harness::EpisodeHarness,
        programs::completion_judge::{CompletionJudgeOutput, CompletionJudgeProgram},
        programs::task_understanding::{TaskUnderstandingOutput, TaskUnderstandingProgram},
        render::openai_tools::OpenAIToolRenderer,
        runtime::{
            AgentMessage, AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult,
            PromptMemoryContext, PromptMessage, execute_program,
        },
        runtime_review::{
            RuntimeTurnRecord, append_runtime_turn_record, unread_runtime_review_count,
        },
        sleep::run_sleep,
        evaluation_artifacts::EvaluationArtifactSuggestedFixKind,
        trace::unread_runtime_trace_count,
        turn_compile::TurnCompileEngine,
    },
    runtime_context::{
        MID_TURN_COMPACTION_MAX_RECOVERIES, build_runtime_conversation_summary,
        build_runtime_request_envelope, maybe_compact_runtime_messages,
        runtime_request_budget_limits,
    },
    runtime_tools::{
        ToolExecutionResult, build_runtime_tool_specs, execute_agent_tool_call,
        render_tool_call_ui_event, summarize_action_from_tool_call,
    },
    sandbox::RuntimeSandboxPolicy,
    snapshot::Snapshot,
    spinova_paths::{SpinovaPaths, spinova_paths},
    telegram_acl::TelegramAclHandle,
    telegram_device::TelegramDevice,
    telegram_transport::TelegramTransport,
    terminal_device::TerminalDevice,
    todo_board::TodoBoard,
    tool_ui::{ToolCallUiEvent, ToolUiEvent, compact_body_lines},
    work_state::WorkState,
};
use chrono::{Local, TimeZone, Utc};
use clap::{Parser, Subcommand};
use miette::{Result, miette};
use serde_json::json;

const AUTO_SLEEP_IDLE_THRESHOLD: Duration = Duration::from_secs(300);
const AUTO_SLEEP_MIN_INTERVAL: Duration = Duration::from_secs(300);
const FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD: usize = 128;
const RUNTIME_EVENT_CLAIM_BATCH_SIZE: usize = 1;

fn emit_startup_progress(message: impl AsRef<str>) {
    let message = message.as_ref();
    tracing::info!("{message}");
    println!("{message}");
}

enum SleepTrigger {
    Manual,
    Idle,
}

#[derive(Default)]
struct SleepDashboardStatus {
    running: bool,
    current_trigger: Option<&'static str>,
    last_result: Option<String>,
    unread_trace_backlog: usize,
    unread_runtime_review_backlog: usize,
    total_runs: usize,
    total_consumed_trace_events: usize,
    total_consumed_runtime_reviews: usize,
    total_runtime_demos: usize,
    total_turn_demos: usize,
    total_runtime_demo_evaluations: usize,
    total_turn_demo_evaluations: usize,
    total_runtime_demo_passed: usize,
    total_runtime_demo_regressions: usize,
    total_runtime_prompt_candidates: usize,
    total_runtime_prompt_rollbacks: usize,
    total_runtime_prompt_accepts: usize,
}

struct SleepTaskResult {
    trigger: SleepTrigger,
    result: Result<crate::reasoning::sleep::SleepSummary>,
}

#[derive(Debug, Parser)]
#[command(name = "spinova")]
struct Cli {
    #[command(subcommand)]
    command: Option<SpinovaCommand>,
}

#[derive(Debug, Subcommand)]
enum SpinovaCommand {
    Reset {
        #[command(subcommand)]
        target: ResetTarget,
    },
    Sleep,
    TrainSource {
        #[command(subcommand)]
        command: TrainSourceCommand,
    },
    #[command(name = "inspect-train-source", hide = true)]
    LegacyInspectTrainSource {
        path: String,
    },
    #[command(name = "rollout-train-source", hide = true)]
    LegacyRolloutTrainSource {
        path: String,
        task_index: Option<usize>,
    },
    #[command(name = "learn-train-source", hide = true)]
    LegacyLearnTrainSource {
        path: String,
        limit: Option<usize>,
        batch_size: Option<usize>,
    },
}

#[derive(Debug, Subcommand)]
enum ResetTarget {
    #[command(name = "complite", alias = "compile")]
    Complite,
    State,
    Memory,
    All,
}

#[derive(Debug, Subcommand)]
enum TrainSourceCommand {
    Inspect {
        path: String,
    },
    Rollout {
        path: String,
        task_index: Option<usize>,
    },
    Learn {
        path: String,
        limit: Option<usize>,
        batch_size: Option<usize>,
    },
}

fn main() {
    let cli = Cli::parse();

    if let Some(path) = train_source_inspect_path(&cli) {
        match run_train_source_inspect_blocking(&path) {
            Ok(()) => return,
            Err(err) => {
                eprintln!("{err:?}");
                std::process::exit(1);
            }
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    if let Err(err) = runtime.block_on(async_main(cli)) {
        eprintln!("{err:?}");
        std::process::exit(1);
    }
}

async fn async_main(cli: Cli) -> Result<()> {
    match cli.command.as_ref() {
        Some(SpinovaCommand::Reset {
            target: ResetTarget::Complite,
        }) => {
            run_complite_reset().await?;
            return Ok(());
        }
        Some(SpinovaCommand::Reset {
            target: ResetTarget::State,
        }) => {
            run_state_reset().await?;
            return Ok(());
        }
        Some(SpinovaCommand::Reset {
            target: ResetTarget::Memory,
        }) => {
            run_memory_reset().await?;
            return Ok(());
        }
        Some(SpinovaCommand::Reset {
            target: ResetTarget::All,
        }) => {
            run_reset_all().await?;
            return Ok(());
        }
        _ => {}
    }

    init_logging().await;

    let config = match load_config().await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    if matches!(cli.command, Some(SpinovaCommand::Sleep)) {
        let mut context = build_eval_context(config).await;
        match run_sleep(&mut context).await {
            Ok(summary) => {
                print_sleep_summary(&summary);
                context.shutdown().await;
                return Ok(());
            }
            Err(err) => {
                tracing::error!("sleep command failed: {err:?}");
                context.shutdown().await;
                std::process::exit(1);
            }
        }
    }

    if let Some((path, limit, batch_size)) = train_source_learn_args(&cli) {
        match run_train_source_learn_with_dashboard(config, &path, limit, batch_size).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::error!("train-source learn failed: {err:?}");
                std::process::exit(1);
            }
        }
    }

    if let Some((path, task_index)) = train_source_rollout_args(&cli) {
        match run_train_source_rollout_with_dashboard(config, &path, task_index).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::error!("train-source rollout failed: {err:?}");
                std::process::exit(1);
            }
        }
    }

    emit_startup_progress("[prompt-compile] loading compiled prompts before dashboard startup...");
    let mut compiled_prompts = match load_compiled_prompts_only(&config).await {
        Ok(store) => store,
        Err(err) => {
            tracing::error!("failed to load compiled prompts: {err:?}");
            std::process::exit(1);
        }
    };
    if !compiled_prompts.has_runtime_system_prompt() {
        emit_startup_progress(
            "[prompt-compile] runtime system prompt missing; running cold-start turn compile...",
        );
        match TurnCompileEngine::compile_cold_start(config.clone(), compiled_prompts.clone()).await
        {
            Ok(runtime_prompt) => {
                compiled_prompts =
                    compiled_prompts.with_runtime_system_prompt(Some(runtime_prompt));
                emit_startup_progress(
                    "[prompt-compile] cold-start turn compile completed; runtime prompt cached",
                );
            }
            Err(err) => {
                tracing::error!(
                    "cold-start turn compile failed; continuing with baseline runtime prompt: {err:?}"
                );
                println!(
                    "[prompt-compile] cold-start turn compile failed; continuing with baseline runtime prompt"
                );
            }
        }
    }
    if compiled_prompts.is_empty() {
        emit_startup_progress(
            "[prompt-compile] no compiled prompts found; running with baseline prompts",
        );
    } else if compiled_prompts.has_runtime_system_prompt() {
        emit_startup_progress(format!(
            "[prompt-compile] loaded {} compiled prompt suites (including runtime system prompt)",
            compiled_prompts.len()
        ));
    } else {
        emit_startup_progress(format!(
            "[prompt-compile] loaded {} compiled program suites; runtime system prompt still baseline",
            compiled_prompts.len()
        ));
    }

    let memory = Memory::new().await;
    let todo_board = TodoBoard::new().await;
    let work_state = WorkState::new().await;
    let events = EventStore::new().await;
    let pending_work = PendingWorkQueue::new().await;
    let telegram_acl = TelegramAclHandle::load().await;
    let terminal = TerminalDevice::new();
    let telegram = TelegramDevice::new();
    let telegram_handle = telegram.handle();
    bootstrap_telegram_device_from_acl(&telegram_handle, &telegram_acl);
    let devices = DeviceManager::new(Some(DeviceId::Terminal), vec![Box::new(terminal)])
        .await
        .unwrap();
    let judge_model = config.judge.resolved_model(&config.main_model);
    let client = OpenAIClient::new(&config);
    let judge_client = OpenAIClient::from_model_config(&judge_model);
    let hindsight = HindsightClient::connect(&config.hindsight).await?;
    let hindsight_retain = hindsight.spawn_retain_worker();
    let execution_cwd =
        env::current_dir().map_err(|err| miette!("failed to determine execution cwd: {err}"))?;
    let sandbox_policy = sandbox_policy_for_runtime(&execution_cwd).await;
    let mut context = Context {
        llm: Box::new(client),
        judge_llm: Box::new(judge_client),
        config,
        hindsight,
        hindsight_retain,
        memory,
        prompt_memory: PromptMemoryContext::default(),
        todo_board,
        work_state,
        events,
        pending_work,
        devices,
        telegram: telegram_handle,
        compiled_prompts,
        execution_cwd,
        sandbox_policy,
        dashboard_tx: None,
        active_runtime_turn: false,
        active_device_notices: std::collections::HashSet::new(),
        idle_since: None,
        last_idle_sleep_at: None,
        record_runtime_reviews: true,
    };
    let device_renders = context.devices.state_renders();

    let (tx, mut rx) = tokio::sync::watch::channel(DashboardState {
        focused_device: context.devices.focused(),
        status_output: render_status_command_output_for_dashboard(&context, &device_renders),
        sleep_status_output: render_sleep_status_output_for_dashboard(
            &context,
            &SleepDashboardStatus::default(),
        ),
        inspect_telegram_output: render_telegram_status_for_dashboard(&context),
        activity_cells: render_activity_for_dashboard(&context),
        live_activity_cells: Vec::new(),
        last_cycle_elapsed_ms: None,
        runtime_status: None,
        footer_context: render_dashboard_footer_context(&context, None),
        footer_estimated_input_tokens: None,
    });
    context.dashboard_tx = Some(tx.clone());
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    let (dashboard_control_tx, mut dashboard_control_rx) =
        tokio::sync::mpsc::unbounded_channel::<DashboardControlCommand>();
    let (sleep_result_tx, mut sleep_result_rx) =
        tokio::sync::mpsc::unbounded_channel::<SleepTaskResult>();
    let telegram_transport = if context.config.telegram.enabled
        && context.config.telegram.has_real_credentials()
    {
        Some(tokio::spawn(
            TelegramTransport::new(
                context.config.telegram.clone(),
                context.telegram.clone(),
                telegram_acl.clone(),
                context.events.clone(),
                context.pending_work.clone(),
                tx.subscribe(),
                dashboard_control_tx.clone(),
            )
            .run(),
        ))
    } else {
        None
    };

    let agent_handle = tokio::spawn(async move {
        let mut sleep_running = false;
        let mut sleep_status = SleepDashboardStatus::default();
        loop {
            tokio::select! {
                _ = spinova_loop(
                    &mut context,
                    &tx,
                    &sleep_result_tx,
                    &mut sleep_running,
                    &mut sleep_status,
                ) => {}
                Some(command) = dashboard_control_rx.recv() => {
                    handle_dashboard_control_command(
                        &mut context,
                        &tx,
                        &sleep_result_tx,
                        &mut sleep_running,
                        &mut sleep_status,
                        command,
                    ).await;
                }
                Some(result) = sleep_result_rx.recv() => {
                    sleep_running = false;
                    handle_sleep_task_result(&mut context, &tx, &mut sleep_status, result).await;
                }
                _ = &mut shutdown_rx => {
                    context.shutdown().await;
                    break;
                }
            }
        }
    });
    run_tui_dashboard(&mut rx, telegram_acl, dashboard_control_tx)
        .await
        .unwrap();
    if let Some(handle) = telegram_transport {
        handle.abort();
    }
    let _ = shutdown_tx.send(());
    let _ = agent_handle.await;
    Ok(())
}

fn train_source_inspect_path(cli: &Cli) -> Option<String> {
    match cli.command.as_ref()? {
        SpinovaCommand::TrainSource {
            command: TrainSourceCommand::Inspect { path },
        } => Some(path.clone()),
        SpinovaCommand::LegacyInspectTrainSource { path } => Some(path.clone()),
        _ => None,
    }
}

fn train_source_rollout_args(cli: &Cli) -> Option<(String, usize)> {
    match cli.command.as_ref()? {
        SpinovaCommand::TrainSource {
            command: TrainSourceCommand::Rollout { path, task_index },
        } => Some((path.clone(), task_index.unwrap_or(0))),
        SpinovaCommand::LegacyRolloutTrainSource { path, task_index } => {
            Some((path.clone(), task_index.unwrap_or(0)))
        }
        _ => None,
    }
}

fn train_source_learn_args(cli: &Cli) -> Option<(String, usize, usize)> {
    match cli.command.as_ref()? {
        SpinovaCommand::TrainSource {
            command:
                TrainSourceCommand::Learn {
                    path,
                    limit,
                    batch_size,
                },
        } => Some((path.clone(), limit.unwrap_or(20), batch_size.unwrap_or(5))),
        SpinovaCommand::LegacyLearnTrainSource {
            path,
            limit,
            batch_size,
        } => Some((path.clone(), limit.unwrap_or(20), batch_size.unwrap_or(5))),
        _ => None,
    }
}

async fn run_memory_reset() -> Result<()> {
    let home = get_spinova_home().await;
    clear_memory_state(&home).await?;

    println!(
        "[memory-reset] reset memory persistence under {}",
        home.display()
    );
    println!("[memory-reset] cleared: runtime_conversation, hindsight_queue");
    println!("[memory-reset] cleared: reasoning_traces.jsonl, runtime_reviews.jsonl");
    println!("[memory-reset] cleared: hindsight bank");
    println!(
        "[memory-reset] preserved: config/, state/, artifacts/, logs/"
    );

    Ok(())
}

async fn clear_memory_state(home: &PathBuf) -> Result<()> {
    let config = load_config()
        .await
        .map_err(|err| miette!("failed to load config for memory-reset: {err}"))?;
    let hindsight = HindsightClient::connect(&config.hindsight).await?;
    hindsight.delete_bank().await?;
    let paths = SpinovaPaths::from_root(home.clone());
    clear_files(
        &[
            paths.state_file("runtime_conversation"),
            paths.state_file("hindsight_queue"),
            paths.journal_file("reasoning_traces.jsonl"),
            paths.journal_file("runtime_reviews.jsonl"),
        ],
    )
    .await?;

    Ok(())
}

async fn run_state_reset() -> Result<()> {
    let home = get_spinova_home().await;
    let cleared = clear_state_files(&home).await?;

    println!(
        "[state-reset] reset runtime state under {}",
        home.display()
    );
    if cleared.is_empty() {
        println!("[state-reset] nothing to remove");
    } else {
        println!("[state-reset] cleared: {}", cleared.join(", "));
    }
    println!(
        "[state-reset] preserved: config/, memory state, artifacts/, logs/"
    );

    Ok(())
}

async fn clear_state_files(home: &PathBuf) -> Result<Vec<String>> {
    let paths = SpinovaPaths::from_root(home.clone());
    let files = [
        "todo_board",
        "work_state",
        "events",
        "pending_work_queue",
        "telegram_transport_state",
    ];
    clear_named_files(paths.state_dir(), &files).await
}

async fn run_complite_reset() -> Result<()> {
    let home = get_spinova_home().await;
    let cleared = clear_compiled_artifacts(&home).await?;

    println!(
        "[complite-reset] cleared compile/evaluation artifacts under {}",
        home.display()
    );
    if cleared.is_empty() {
        println!("[complite-reset] nothing to remove");
    } else {
        println!("[complite-reset] cleared: {}", cleared.join(", "));
    }
    println!(
        "[complite-reset] preserved: config/, state/, memory, logs/"
    );

    Ok(())
}

async fn clear_compiled_artifacts(home: &PathBuf) -> Result<Vec<String>> {
    let mut cleared = Vec::new();
    let paths = SpinovaPaths::from_root(home.clone());

    for dir_name in [COMPILED_DIR_NAME, "evaluations"] {
        let path = paths.artifact_dir(dir_name);
        if path.exists() {
            tokio::fs::remove_dir_all(&path)
                .await
                .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
            cleared.push(dir_name.to_string());
        }
    }

    Ok(cleared)
}

async fn run_reset_all() -> Result<()> {
    let home = get_spinova_home().await;
    let memory_cleared = clear_memory_state(&home).await;
    let state_cleared = clear_state_files(&home).await?;
    let artifact_cleared = clear_compiled_artifacts(&home).await?;
    let log_cleared = clear_log_dirs(&home).await?;
    memory_cleared?;

    println!("[reset] reset all state under {}", home.display());
    if state_cleared.is_empty() {
        println!("[reset] cleared state: none");
    } else {
        println!("[reset] cleared state: {}", state_cleared.join(", "));
    }
    println!(
        "[reset] cleared memory: runtime_conversation, hindsight_queue, reasoning_traces.jsonl, runtime_reviews.jsonl, hindsight bank"
    );
    if artifact_cleared.is_empty() {
        println!("[reset] cleared complite artifacts: none");
    } else {
        println!(
            "[reset] cleared complite artifacts: {}",
            artifact_cleared.join(", ")
        );
    }
    if log_cleared.is_empty() {
        println!("[reset] cleared logs: none");
    } else {
        println!("[reset] cleared logs: {}", log_cleared.join(", "));
    }
    println!("[reset] preserved: config.toml, telegram_acl.json");

    Ok(())
}

async fn clear_log_dirs(home: &PathBuf) -> Result<Vec<String>> {
    let mut cleared = Vec::new();
    let paths = SpinovaPaths::from_root(home.clone());
    let path = paths.logs_dir();
    if path.exists() {
        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
        cleared.push("logs".to_string());
    }
    Ok(cleared)
}

async fn clear_named_files(dir: PathBuf, file_names: &[&str]) -> Result<Vec<String>> {
    let mut cleared = Vec::new();
    for file_name in file_names {
        let path = dir.join(file_name);
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
            cleared.push((*file_name).to_string());
        }
    }
    Ok(cleared)
}

async fn clear_files(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        if path.exists() {
            tokio::fs::remove_file(path)
                .await
                .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
        }
    }
    Ok(())
}

async fn build_eval_context(config: crate::config::Config) -> Context {
    build_eval_context_with_compiled(config, CompiledPromptStore::empty()).await
}

async fn sandbox_policy_for_runtime(execution_cwd: &Path) -> RuntimeSandboxPolicy {
    let spinova_home = get_spinova_home().await;
    let executable_dir = env::current_exe()
        .ok()
        .and_then(|current_exe| current_exe.parent().map(Path::to_path_buf));
    RuntimeSandboxPolicy::protect_spinova_runtime(
        execution_cwd,
        &spinova_home,
        executable_dir.as_deref(),
    )
}

pub(crate) async fn build_eval_context_with_compiled(
    config: crate::config::Config,
    compiled_prompts: CompiledPromptStore,
) -> Context {
    let execution_cwd =
        env::current_dir().unwrap_or_else(|err| panic!("failed to determine execution cwd: {err}"));
    let sandbox_policy = sandbox_policy_for_runtime(&execution_cwd).await;
    let memory = Memory::new().await;
    let todo_board = TodoBoard::new().await;
    let work_state = WorkState::new().await;
    let events = EventStore::new().await;
    let pending_work = PendingWorkQueue::new().await;
    let terminal = TerminalDevice::new();
    let telegram = TelegramDevice::new();
    let telegram_handle = telegram.handle();
    let devices = DeviceManager::new(Some(DeviceId::Terminal), vec![Box::new(terminal)])
        .await
        .unwrap();
    let judge_model = config.judge.resolved_model(&config.main_model);
    let client = OpenAIClient::new(&config);
    let judge_client = OpenAIClient::from_model_config(&judge_model);
    let hindsight = HindsightClient::connect(&config.hindsight)
        .await
        .unwrap_or_else(|err| panic!("failed to construct hindsight client: {err:?}"));
    let hindsight_retain = hindsight.spawn_retain_worker();

    Context {
        llm: Box::new(client),
        judge_llm: Box::new(judge_client),
        config,
        hindsight,
        hindsight_retain,
        memory,
        prompt_memory: PromptMemoryContext::default(),
        todo_board,
        work_state,
        events,
        pending_work,
        devices,
        telegram: telegram_handle,
        compiled_prompts,
        execution_cwd,
        sandbox_policy,
        dashboard_tx: None,
        active_runtime_turn: false,
        active_device_notices: std::collections::HashSet::new(),
        idle_since: None,
        last_idle_sleep_at: None,
        record_runtime_reviews: false,
    }
}

fn bootstrap_telegram_device_from_acl(
    telegram_handle: &crate::telegram_device::TelegramDeviceHandle,
    telegram_acl: &TelegramAclHandle,
) {
    for chat in telegram_acl.approved_chats() {
        telegram_handle.register_known_chat(chat.chat_id.to_string(), chat.title);
    }
}

async fn load_compiled_prompts_only(
    config: &crate::config::Config,
) -> miette::Result<CompiledPromptStore> {
    let compiled = load_all_compiled_programs_for_model(&config.main_model.model_name).await?;
    let runtime_system_prompt =
        load_compiled_runtime_system_prompt_for_model(&config.main_model.model_name).await?;
    Ok(CompiledPromptStore::from_entries(compiled)
        .with_runtime_system_prompt(runtime_system_prompt))
}

fn initial_train_source_dashboard_state(status: String) -> DashboardState {
    DashboardState {
        focused_device: Some(DeviceId::Terminal),
        status_output: "Overview\nTrain-source session starting".to_string(),
        sleep_status_output: "Overview\nSleep controls are unavailable in train-source sessions."
            .to_string(),
        inspect_telegram_output: "Telegram\nNo active Telegram session.".to_string(),
        activity_cells: Vec::new(),
        live_activity_cells: Vec::new(),
        last_cycle_elapsed_ms: None,
        runtime_status: Some(status),
        footer_context: "train-source".to_string(),
        footer_estimated_input_tokens: None,
    }
}

async fn run_train_source_dashboard_session<Fut>(
    initial_status: String,
    runner: impl FnOnce(tokio::sync::watch::Sender<DashboardState>) -> Fut,
) -> Result<()>
where
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let telegram_acl = TelegramAclHandle::load().await;
    let (tx, mut rx) =
        tokio::sync::watch::channel(initial_train_source_dashboard_state(initial_status));
    let (control_tx, mut control_rx) =
        tokio::sync::mpsc::unbounded_channel::<DashboardControlCommand>();
    let control_state_tx = tx.clone();
    let control_task = tokio::spawn(async move {
        while let Some(command) = control_rx.recv().await {
            match command {
                DashboardControlCommand::RunSleep => {
                    set_runtime_status(
                        Some(&control_state_tx),
                        RuntimeStatusLevel::Warn,
                        "sleep command is unavailable during train-source sessions",
                    );
                }
                DashboardControlCommand::ClearConversation => {
                    set_runtime_status(
                        Some(&control_state_tx),
                        RuntimeStatusLevel::Warn,
                        "clear command is unavailable during train-source sessions",
                    );
                }
            }
        }
    });
    let worker = tokio::spawn(runner(tx.clone()));
    let dashboard_result = run_tui_dashboard(&mut rx, telegram_acl, control_tx).await;
    control_task.abort();
    let worker_result = if worker.is_finished() {
        match worker.await {
            Ok(result) => result,
            Err(err) => Err(miette!("train-source worker failed: {err}")),
        }
    } else {
        worker.abort();
        Ok(())
    };
    dashboard_result.map_err(|err| miette!("dashboard failed: {err}"))?;
    worker_result
}

fn train_progress(
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    message: impl Into<String>,
) {
    let message = message.into();
    set_runtime_status(dashboard_tx, RuntimeStatusLevel::Info, message.clone());
    if dashboard_tx.is_none() {
        println!("{message}");
    }
}

fn sync_training_dashboard_state(
    context: &Context,
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) {
    if let Some(tx) = dashboard_tx {
        sync_dashboard_state(context, tx, &SleepDashboardStatus::default(), None);
    }
}

fn run_train_source_inspect_blocking(path: &str) -> Result<()> {
    let source = SweTrainSource::load_blocking(path)?;
    let tasks = source.into_episode_tasks(64);
    let summary = EpisodeHarness::summarize_tasks(&tasks, 5);
    print_episode_batch_summary(path, &summary);
    Ok(())
}

async fn run_train_source_rollout(
    config: crate::config::Config,
    path: &str,
    task_index: usize,
    dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
) -> Result<()> {
    let source = SweTrainSource::load(path).await?;
    let tasks = source.into_episode_tasks(64);
    let Some(mut task) = tasks.get(task_index).cloned() else {
        return Err(miette!(
            "task index {} out of range for training source with {} tasks",
            task_index,
            tasks.len()
        ));
    };

    let episode_root = prepare_isolated_episode_root(&task, "single").await?;
    let episode_home = episode_root.join("h");
    let workspace_dir = episode_root.join("ws");
    let home_override = SpinovaHomeOverride::set(episode_home.clone());
    provision_episode_workspace(&task, &workspace_dir, dashboard_tx.as_ref()).await?;
    task.workspace_hint = Some(workspace_dir.display().to_string());

    let mut context = build_eval_context(config).await;
    context.dashboard_tx = dashboard_tx.clone();
    context.devices.focus(DeviceId::Terminal).await?;
    enter_episode_workspace(&mut context, &workspace_dir).await?;
    context
        .work_state
        .set_objective(task.instruction.clone(), None);
    sync_training_dashboard_state(&context, dashboard_tx.as_ref());
    train_progress(
        dashboard_tx.as_ref(),
        format!("Running train-source rollout task {}", task.id),
    );

    let outcome =
        rollout_agent_loop_episode(&mut context, task, &workspace_dir, dashboard_tx.as_ref())
            .await?;
    print_episode_rollout(&outcome, &episode_home, dashboard_tx.as_ref());
    context.shutdown().await;
    drop(home_override);
    Ok(())
}

async fn run_train_source_rollout_with_dashboard(
    config: crate::config::Config,
    path: &str,
    task_index: usize,
) -> Result<()> {
    let path = path.to_string();
    run_train_source_dashboard_session(
        format!("Preparing train-source rollout {}#{}", path, task_index),
        move |dashboard_tx| async move {
            run_train_source_rollout(config, &path, task_index, Some(dashboard_tx)).await
        },
    )
    .await
}

async fn run_train_source_learn(
    config: crate::config::Config,
    path: &str,
    limit: usize,
    batch_size: usize,
    dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
) -> Result<()> {
    let source = SweTrainSource::load(path).await?;
    let mut tasks = source.into_episode_tasks(64);
    if limit > 0 && tasks.len() > limit {
        tasks.truncate(limit);
    }
    let batch_size = batch_size.max(1);
    let session_root = prepare_learning_session_root(path).await?;
    let shared_learning_home = get_spinova_home().await;
    let session_learning_home = prepare_learning_home_root(&session_root).await?;
    sync_learning_assets_to_session(&shared_learning_home, &session_learning_home).await?;
    let session = TrainSourceLearnSession::new(
        session_root,
        TrainSourceLearnState::new(path.to_string(), tasks.len(), batch_size),
    )
    .await?;

    train_progress(
        dashboard_tx.as_ref(),
        format!(
            "train source learn: path={} total_tasks={} batch_size={} session={}",
            path,
            tasks.len(),
            batch_size,
            session.session_root().display()
        ),
    );

    let home_override = SpinovaHomeOverride::set(session_learning_home.clone());
    let run_result = tokio::select! {
        result = run_train_source_learn_loop(
            config,
            tasks,
            batch_size,
            shared_learning_home,
            session_learning_home,
            session.clone(),
            dashboard_tx.clone(),
        ) => result,
        _ = tokio::signal::ctrl_c() => {
            session.shutdown(true, dashboard_tx.as_ref()).await?;
            return Ok(());
        }
    };
    drop(home_override);

    session.shutdown(false, dashboard_tx.as_ref()).await?;
    run_result
}

async fn run_train_source_learn_with_dashboard(
    config: crate::config::Config,
    path: &str,
    limit: usize,
    batch_size: usize,
) -> Result<()> {
    let path = path.to_string();
    run_train_source_dashboard_session(
        format!("Preparing train-source learn {}", path),
        move |dashboard_tx| async move {
            run_train_source_learn(config, &path, limit, batch_size, Some(dashboard_tx)).await
        },
    )
    .await
}

async fn run_train_source_learn_loop(
    config: crate::config::Config,
    tasks: Vec<EpisodeTask>,
    batch_size: usize,
    shared_learning_home: PathBuf,
    session_learning_home: PathBuf,
    session: TrainSourceLearnSession,
    dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
) -> Result<()> {
    let mut cursor = 0usize;
    let mut batch_index = 0usize;
    while cursor < tasks.len() {
        let batch_end = (cursor + batch_size).min(tasks.len());
        let batch_tasks = &tasks[cursor..batch_end];
        train_progress(
            dashboard_tx.as_ref(),
            format!(
                "batch {} running (tasks {}..{}, count={})",
                batch_index + 1,
                cursor + 1,
                batch_end,
                batch_tasks.len()
            ),
        );
        let compiled_prompts = load_compiled_prompts_only(&config).await?;
        let active_variant = if compiled_prompts.is_empty() {
            "baseline".to_string()
        } else {
            "compiled".to_string()
        };
        train_progress(
            dashboard_tx.as_ref(),
            format!(
                "batch {} active_variant={} (tasks {}..{})",
                batch_index + 1,
                active_variant,
                cursor + 1,
                batch_end
            ),
        );

        for (offset, task) in batch_tasks.iter().enumerate() {
            let absolute_index = cursor + offset;
            let episode_root =
                prepare_learning_episode_root(session.session_root(), task, absolute_index).await?;
            let outcome = execute_train_source_task(
                &config,
                task,
                compiled_prompts.clone(),
                &episode_root,
                true,
                dashboard_tx.clone(),
            )
            .await?;

            session
                .update(|state| {
                    state.completed_tasks += 1;
                    state.last_task_id = Some(outcome.task.id.clone());
                    state.last_task_status = Some(format!("{:?}", outcome.status));
                    state.last_score = Some(outcome.metric.score);
                    state
                        .outcomes
                        .push(TrainSourceLearnOutcomeSummary::from_episode(&outcome));
                })
                .await?;

            let snapshot = session.snapshot().await;
            train_progress(
                dashboard_tx.as_ref(),
                format!(
                    "learn task {}/{} id={} status={:?} score={:.2}",
                    snapshot.completed_tasks,
                    snapshot.total_tasks,
                    outcome.task.id,
                    outcome.status,
                    outcome.metric.score
                ),
            );
        }

        let completed_tasks = session.snapshot().await.completed_tasks;
        train_progress(
            dashboard_tx.as_ref(),
            format!(
                "batch {} sleep starting (completed_tasks={})",
                batch_index + 1,
                completed_tasks
            ),
        );
        let mut optimize_context = build_eval_context_with_compiled(
            config.clone(),
            load_compiled_prompts_only(&config).await?,
        )
        .await;
        optimize_context.dashboard_tx = dashboard_tx.clone();
        let sleep_summary = run_sleep(&mut optimize_context).await?;
        let current_compiled_count = load_compiled_prompts_only(&config).await?.len();
        session
            .update(|state| {
                state.sleep_runs += 1;
                state.batch_reports.push(TrainSourceLearnBatchReport {
                    completed_tasks: state.completed_tasks,
                    active_variant: active_variant.clone(),
                    sleep_failure_patterns: sleep_summary.failure_patterns.len(),
                    sleep_bootstrap_demos: sleep_summary.bootstrap_demos,
                    sleep_stress_cases: sleep_summary.stress_cases,
                    sleep_instruction_hypotheses: sleep_summary.instruction_hypotheses,
                    sleep_runtime_demos: sleep_summary.runtime_demos,
                    sleep_turn_demos: sleep_summary.turn_demos,
                    sleep_runtime_prompt_suggestions: sleep_summary.runtime_prompt_suggestions,
                    sleep_runtime_prompt_candidates: sleep_summary.runtime_prompt_candidates,
                    sleep_runtime_demo_evaluations: sleep_summary.runtime_demo_evaluations,
                    sleep_turn_demo_evaluations: sleep_summary.turn_demo_evaluations,
                    sleep_runtime_demo_passed: sleep_summary.runtime_demo_passed,
                    sleep_runtime_demo_regressions: sleep_summary.runtime_demo_regressions,
                    sleep_runtime_prompt_rolled_back: sleep_summary.runtime_prompt_rolled_back,
                    sleep_runtime_prompt_evolution_rounds: sleep_summary
                        .runtime_prompt_evolution_rounds,
                    sleep_runtime_prompt_accepted: sleep_summary.runtime_prompt_accepted,
                    retained_reflections: sleep_summary.retained_reflections,
                    compiled_prompt_count: current_compiled_count,
                    optimized_suites: 0,
                });
            })
            .await?;
        train_progress(
            dashboard_tx.as_ref(),
            format!(
                "batch {} sleep finished: runtime demos {} turn demos {} evals {}/{} candidates {} rounds {} accepted={}",
                batch_index + 1,
                sleep_summary.runtime_demos,
                sleep_summary.turn_demos,
                sleep_summary.runtime_demo_evaluations,
                sleep_summary.turn_demo_evaluations,
                sleep_summary.runtime_prompt_candidates,
                sleep_summary.runtime_prompt_evolution_rounds,
                sleep_summary.runtime_prompt_accepted
            ),
        );
        sync_learning_assets_back_to_shared(&session_learning_home, &shared_learning_home).await?;

        optimize_context.shutdown().await;
        sync_learning_assets_back_to_shared(&session_learning_home, &shared_learning_home).await?;

        let compiled_prompt_count = load_compiled_prompts_only(&config).await?.len();
        session
            .update(|state| {
                state.last_compiled_prompt_count = compiled_prompt_count;
                if let Some(last_report) = state.batch_reports.last_mut() {
                    last_report.compiled_prompt_count = compiled_prompt_count;
                    last_report.optimized_suites = 0;
                }
            })
            .await?;
        train_progress(
            dashboard_tx.as_ref(),
            format!(
                "batch {} compiled_prompt_count={} completed={}",
                batch_index + 1,
                compiled_prompt_count,
                session.snapshot().await.completed_tasks
            ),
        );

        cursor = batch_end;
        batch_index += 1;
    }

    Ok(())
}

async fn execute_train_source_task(
    config: &crate::config::Config,
    task: &EpisodeTask,
    compiled_prompts: CompiledPromptStore,
    episode_root: &Path,
    use_shared_learning_home: bool,
    dashboard_tx: Option<tokio::sync::watch::Sender<DashboardState>>,
) -> Result<EpisodeOutcome> {
    let episode_home = episode_root.join("h");
    let workspace_dir = episode_root.join("ws");
    let home_override = (!use_shared_learning_home).then(|| SpinovaHomeOverride::set(episode_home));
    train_progress(
        dashboard_tx.as_ref(),
        format!(
            "episode setup: id={} workspace={} home_mode={}",
            task.id,
            workspace_dir.display(),
            if use_shared_learning_home {
                "shared"
            } else {
                "isolated"
            }
        ),
    );
    provision_episode_workspace(task, &workspace_dir, dashboard_tx.as_ref()).await?;

    let mut run_task = task.clone();
    run_task.workspace_hint = Some(workspace_dir.display().to_string());
    let mut context = build_eval_context_with_compiled(config.clone(), compiled_prompts).await;
    context.dashboard_tx = dashboard_tx.clone();
    context.devices.focus(DeviceId::Terminal).await?;
    enter_episode_workspace(&mut context, &workspace_dir).await?;
    context
        .work_state
        .set_objective(run_task.instruction.clone(), None);
    sync_training_dashboard_state(&context, dashboard_tx.as_ref());

    let outcome = rollout_agent_loop_episode(
        &mut context,
        run_task,
        &workspace_dir,
        dashboard_tx.as_ref(),
    )
    .await?;
    save_episode_outcome(episode_root, &outcome).await?;
    context.shutdown().await;
    drop(home_override);
    Ok(outcome)
}

fn print_sleep_summary(summary: &crate::reasoning::sleep::SleepSummary) {
    println!(
        "sleep: consumed {} runtime reviews, {} runtime traces; derived {} failure patterns, {} bootstrap demos, {} stress cases, {} instruction hypotheses, {} runtime demos, {} turn demos, {} runtime prompt suggestions, {} runtime prompt candidates, runtime evals {} / turn evals {} (passed {}, regressed {}, rolled_back {}, rounds {}, accepted {}), retained {} hindsight reflections",
        summary.consumed_runtime_reviews,
        summary.consumed_trace_events,
        summary.failure_patterns.len(),
        summary.bootstrap_demos,
        summary.stress_cases,
        summary.instruction_hypotheses,
        summary.runtime_demos,
        summary.turn_demos,
        summary.runtime_prompt_suggestions,
        summary.runtime_prompt_candidates,
        summary.runtime_demo_evaluations,
        summary.turn_demo_evaluations,
        summary.runtime_demo_passed,
        summary.runtime_demo_regressions,
        summary.runtime_prompt_rolled_back,
        summary.runtime_prompt_evolution_rounds,
        summary.runtime_prompt_accepted,
        summary.retained_reflections
    );
    for pattern in &summary.failure_patterns {
        let kind = match pattern.suggested_fix_kind {
            EvaluationArtifactSuggestedFixKind::Demo => "demo",
            EvaluationArtifactSuggestedFixKind::Instruction => "instruction",
            EvaluationArtifactSuggestedFixKind::StressCase => "stress_case",
        };
        println!(
            "- suite={} pattern_id={} frequency={} severity={} fix={} traces={}",
            pattern.suite,
            pattern.pattern_id,
            pattern.frequency,
            pattern.severity,
            kind,
            pattern.supporting_trace_ids.len()
        );
    }
}

fn summarize_sleep_summary(summary: &crate::reasoning::sleep::SleepSummary) -> String {
    format!(
        "sleep 完成：runtime reviews {}，traces {}，runtime demos {}，turn demos {}，评估 {}/{}（通过 {}，退化 {}），候选 {}，轮次 {}，accepted={}",
        summary.consumed_runtime_reviews,
        summary.consumed_trace_events,
        summary.runtime_demos,
        summary.turn_demos,
        summary.runtime_demo_evaluations,
        summary.turn_demo_evaluations,
        summary.runtime_demo_passed,
        summary.runtime_demo_regressions,
        summary.runtime_prompt_candidates,
        summary.runtime_prompt_evolution_rounds,
        if summary.runtime_prompt_accepted {
            "yes"
        } else {
            "no"
        }
    )
}

fn print_episode_batch_summary(
    path: &str,
    summary: &crate::reasoning::episode_harness::EpisodeBatchSummary,
) {
    println!(
        "train source inspect: path={} total_tasks={} avg_max_steps={:.1}",
        path, summary.total_tasks, summary.avg_max_steps
    );
    if !summary.source_counts.is_empty() {
        println!("sources:");
        for (source, count) in &summary.source_counts {
            println!("- {}: {}", source, count);
        }
    }
    if !summary.tag_counts.is_empty() {
        println!("tags:");
        for (tag, count) in &summary.tag_counts {
            println!("- {}: {}", tag, count);
        }
    }
    if !summary.preview.is_empty() {
        println!("preview:");
        for task in &summary.preview {
            println!(
                "- id={} title={} max_steps={} success_criteria={} validation={} workspace={}",
                task.id,
                task.title,
                task.max_steps,
                task.success_criteria_count,
                task.validation_command_count,
                task.workspace_hint.as_deref().unwrap_or("-")
            );
        }
    }
}

async fn rollout_agent_loop_episode(
    context: &mut Context,
    mut task: EpisodeTask,
    workspace_dir: &Path,
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> Result<EpisodeOutcome> {
    let renderer = OpenAIToolRenderer;
    let mut steps = Vec::new();
    let task_understanding = understand_episode_task(context, &task, &renderer).await?;
    task.task_goal = Some(task_understanding.task_goal.clone());
    task.investigation_plan = task_understanding.investigation_plan.clone();
    task.done_criteria = task_understanding.done_criteria.clone();
    task.key_anchors = task_understanding.key_anchors.clone();
    task.metadata.insert(
        "thread_focus".to_string(),
        task_understanding.thread_focus.clone(),
    );
    let compressed_task = format!(
        "{} | {}",
        task_understanding.thread_focus, task_understanding.task_goal
    );
    context.work_state.set_objective(compressed_task, None);
    context.work_state.set_guidance(
        task_understanding.key_anchors.clone(),
        task_understanding.investigation_plan.clone(),
    );
    context.work_state.set_phase("investigate".to_string());
    let initial_snapshot = Snapshot::new(context).await.to_string();
    let initial_observation = EpisodeObservation {
        summary: format!("task seeded: {}", task.title),
        snapshot_text: initial_snapshot,
        metadata: std::collections::BTreeMap::new(),
    };
    let mut work_phase = "investigate".to_string();
    sync_training_dashboard_state(context, dashboard_tx);

    for index in 0..task.max_steps {
        context.work_state.set_phase(work_phase.clone());
        train_progress(
            dashboard_tx,
            format!(
                "episode step {}/{} [{}]",
                index + 1,
                task.max_steps,
                work_phase
            ),
        );
        let step_execution = execute_agent_loop_step(context, dashboard_tx).await;
        let output = step_execution.output;
        let action = output
            .actions
            .last()
            .cloned()
            .unwrap_or(EpisodeActionRecord {
                kind: "no_action".to_string(),
                summary: String::new(),
            });

        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("description".to_string(), output.description.clone());
        metadata.insert("stop_reason".to_string(), output.stop_reason.clone());
        metadata.insert("current_doing".to_string(), output.current_doing.clone());
        let completion = judge_episode_completion(
            context,
            &task,
            &steps,
            &step_execution.snapshot_text,
            &renderer,
        )
        .await?;
        metadata.insert("completion_state".to_string(), completion.state.clone());
        metadata.insert("completion_reason".to_string(), completion.reason.clone());
        if let Some(next_check) = completion.next_check.clone() {
            metadata.insert("completion_next_check".to_string(), next_check);
        }
        let next_work_phase = normalize_work_phase(&completion.state);
        if next_work_phase == "verify" {
            context
                .work_state
                .set_verify_pending_check(completion.next_check.clone());
        } else {
            context.work_state.set_verify_pending_check(None);
        }
        metadata.insert("work_phase".to_string(), next_work_phase.clone());
        steps.push(EpisodeStep {
            index,
            module: "agent_loop".to_string(),
            action,
            observation_summary: output.observation,
            snapshot_text: step_execution.snapshot_text,
            metadata,
        });
        work_phase = next_work_phase;
        context.work_state.set_phase(work_phase.clone());
        sync_training_dashboard_state(context, dashboard_tx);

        if matches!(work_phase.as_str(), "finish") {
            return finalize_runtime_episode(
                context,
                task,
                workspace_dir,
                initial_observation,
                steps,
                Some(EpisodeStatus::Succeeded),
                dashboard_tx,
            )
            .await;
        }

        if !context.work_state.has_objective() {
            return finalize_runtime_episode(
                context,
                task,
                workspace_dir,
                initial_observation,
                steps,
                None,
                dashboard_tx,
            )
            .await;
        }
    }

    finalize_runtime_episode(
        context,
        task,
        workspace_dir,
        initial_observation,
        steps,
        None,
        dashboard_tx,
    )
    .await
}

async fn understand_episode_task(
    context: &mut Context,
    task: &EpisodeTask,
    renderer: &OpenAIToolRenderer,
) -> Result<TaskUnderstandingOutput> {
    let program = TaskUnderstandingProgram {
        title: task.title.clone(),
        instruction: task.instruction.clone(),
        success_criteria: task.success_criteria.clone(),
        metadata: task
            .metadata
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect(),
    };
    let snapshot = Snapshot::new(context).await;
    execute_program(context.llm.as_ref(), context, &snapshot, renderer, &program).await
}

async fn judge_episode_completion(
    context: &mut Context,
    task: &EpisodeTask,
    steps: &[EpisodeStep],
    snapshot_text: &str,
    renderer: &OpenAIToolRenderer,
) -> Result<CompletionJudgeOutput> {
    let recent_steps = steps
        .iter()
        .rev()
        .take(4)
        .rev()
        .map(|step| {
            format!(
                "step={} action={} ({}) doing={} reason={}",
                step.index,
                step.action.kind,
                step.action.summary,
                step.metadata
                    .get("current_doing")
                    .map(String::as_str)
                    .unwrap_or("-"),
                step.metadata
                    .get("description")
                    .map(String::as_str)
                    .unwrap_or("-")
            )
        })
        .collect::<Vec<_>>();

    let program = CompletionJudgeProgram {
        task_goal: task.task_goal.clone().unwrap_or_else(|| task.title.clone()),
        done_criteria: if task.done_criteria.is_empty() {
            task.success_criteria.clone()
        } else {
            task.done_criteria.clone()
        },
        key_anchors: task.key_anchors.clone(),
        investigation_plan: task.investigation_plan.clone(),
        recent_steps,
        current_terminal: snapshot_text.to_string(),
        validation_summary: if task.validation_commands.is_empty() {
            "none".to_string()
        } else {
            task.validation_commands.join("\n")
        },
    };
    let snapshot = Snapshot::new(context).await;
    execute_program(
        context.judge_llm.as_ref(),
        context,
        &snapshot,
        renderer,
        &program,
    )
    .await
}

fn normalize_work_phase(state: &str) -> String {
    match state.trim().to_ascii_lowercase().as_str() {
        "investigate" | "continue" => "investigate".to_string(),
        "change" | "ready_to_patch" => "change".to_string(),
        "verify" | "ready_to_validate" => "verify".to_string(),
        "finish" | "done" => "finish".to_string(),
        "blocked" => "blocked".to_string(),
        _ => "investigate".to_string(),
    }
}

async fn finalize_runtime_episode(
    context: &mut Context,
    task: EpisodeTask,
    workspace_dir: &Path,
    initial_observation: EpisodeObservation,
    steps: Vec<EpisodeStep>,
    forced_rollout_status: Option<EpisodeStatus>,
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> Result<EpisodeOutcome> {
    let rollout_status = forced_rollout_status.unwrap_or_else(|| {
        if !context.work_state.has_objective() {
            EpisodeStatus::Succeeded
        } else if steps.len() >= task.max_steps {
            EpisodeStatus::MaxStepsExceeded
        } else {
            EpisodeStatus::Aborted
        }
    });
    let validation_results =
        run_validation_commands(&task.validation_commands, workspace_dir, dashboard_tx).await?;
    let status = final_episode_status(rollout_status, &validation_results);
    let final_snapshot = Snapshot::new(context).await.to_string();
    let metric = build_episode_metric(&steps, status, rollout_status, &validation_results);
    let outcome = EpisodeOutcome {
        task,
        environment_name: "agent_loop_rollout".to_string(),
        initial_observation,
        final_observation: EpisodeObservation {
            summary: final_episode_summary(status, &steps, &validation_results),
            snapshot_text: final_snapshot,
            metadata: validation_metadata(&validation_results),
        },
        status,
        steps,
        metric,
    };
    Ok(outcome)
}

fn build_episode_metric(
    steps: &[EpisodeStep],
    status: EpisodeStatus,
    rollout_status: EpisodeStatus,
    validation_results: &[ValidationCommandResult],
) -> EpisodeMetric {
    let repeated_terminal_loops = count_repeated_terminal_loops(steps);
    let repeated_actions = repeated_terminal_loops;

    let success = matches!(status, EpisodeStatus::Succeeded);
    let score = if success {
        (1.0 - (steps.len() as f32 * 0.01) - (repeated_actions as f32 * 0.05)).max(0.0)
    } else {
        0.0
    };
    let mut notes = vec![format!("rollout_status={rollout_status:?}")];
    for result in validation_results {
        notes.push(format!(
            "validation `{}` => {}",
            result.command,
            if result.success { "ok" } else { "failed" }
        ));
    }

    EpisodeMetric {
        success,
        score,
        steps_used: steps.len(),
        repeated_actions,
        stagnation_events: repeated_terminal_loops,
        notes,
    }
}

fn count_repeated_terminal_loops(steps: &[EpisodeStep]) -> usize {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    let mut repeated = 0;
    for step in steps {
        let phase = step.metadata.get("work_phase").map(String::as_str);
        if !matches!(phase, Some("investigate") | Some("verify")) {
            continue;
        }
        let Some(signature) = repeated_terminal_loop_signature(&step.action) else {
            continue;
        };
        let count = seen.entry(signature).or_insert(0);
        *count += 1;
        if *count > 1 {
            repeated += 1;
        }
    }
    repeated
}

fn repeated_terminal_loop_signature(action: &EpisodeActionRecord) -> Option<String> {
    let text = match action.kind.as_str() {
        "terminal_exec" | "terminal_write_stdin" => action.summary.as_str(),
        _ => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("pytest -v elastic/tests/") {
        return Some("pytest:elastic/tests".to_string());
    }
    if trimmed.contains("elastic/tests/pytest.log") {
        if trimmed.starts_with("tail ") {
            return Some("log-tail:elastic/tests/pytest.log".to_string());
        }
        if trimmed.starts_with("grep ") {
            return Some("log-grep:elastic/tests/pytest.log".to_string());
        }
        if trimmed.starts_with("cat ") {
            return Some("log-cat:elastic/tests/pytest.log".to_string());
        }
    }
    let prefixes = [
        "grep -i version ",
        "grep -i opensearch ",
        "grep -i opensearch -r ",
        "grep -i all ",
        "grep '^def ' ",
        "grep 'def stats_for_version' ",
        "head -40 ",
        "head -80 ",
        "head -120 ",
        "ls ",
        "ls -l ",
        "cat ",
        "sed -n ",
    ];
    prefixes.iter().find_map(|prefix| {
        trimmed
            .strip_prefix(prefix)
            .map(|rest| format!("{prefix}{rest}"))
    })
}

fn final_episode_summary(
    status: EpisodeStatus,
    steps: &[EpisodeStep],
    validation_results: &[ValidationCommandResult],
) -> String {
    let last = steps
        .last()
        .map(|step| step.observation_summary.clone())
        .unwrap_or_else(|| "no steps executed".to_string());
    let validation_summary = if validation_results.is_empty() {
        "validation=none".to_string()
    } else {
        format!(
            "validation={}/{}",
            validation_results
                .iter()
                .filter(|result| result.success)
                .count(),
            validation_results.len()
        )
    };
    format!("status={status:?}; {validation_summary}; last_observation={last}")
}

fn print_episode_rollout(
    outcome: &EpisodeOutcome,
    episode_home: &PathBuf,
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) {
    if dashboard_tx.is_some() {
        train_progress(
            dashboard_tx,
            format!(
                "train source rollout complete: id={} steps={} status={:?} score={:.2} home={}",
                outcome.task.id,
                outcome.steps.len(),
                outcome.status,
                outcome.metric.score,
                episode_home.display()
            ),
        );
        return;
    }
    println!(
        "train source rollout: id={} title={} steps={} status={:?} score={:.2} home={}",
        outcome.task.id,
        outcome.task.title,
        outcome.steps.len(),
        outcome.status,
        outcome.metric.score,
        episode_home.display()
    );
    for step in &outcome.steps {
        println!(
            "- step={} module={} action={} ({})\n  observation={}\n  doing={}\n  description={}",
            step.index,
            step.module,
            step.action.kind,
            step.action.summary,
            step.observation_summary,
            step.metadata
                .get("current_doing")
                .map(String::as_str)
                .unwrap_or("-"),
            step.metadata
                .get("description")
                .map(String::as_str)
                .unwrap_or("-")
        );
    }
    println!("final snapshot:");
    println!("{}", outcome.final_observation.snapshot_text);
    if !outcome.metric.notes.is_empty() {
        println!("notes:");
        for note in &outcome.metric.notes {
            println!("- {note}");
        }
    }
}

async fn save_episode_outcome(episode_root: &Path, outcome: &EpisodeOutcome) -> Result<()> {
    let path = episode_root.join("episode_outcome.json");
    let payload = serde_json::to_vec_pretty(outcome).map_err(|err| {
        miette!(
            "failed to serialize episode outcome {}: {err}",
            outcome.task.id
        )
    })?;
    tokio::fs::write(&path, payload)
        .await
        .map_err(|err| miette!("failed to write episode outcome {}: {err}", path.display()))?;
    Ok(())
}

fn print_train_source_learn_summary(
    session_root: &Path,
    state: &TrainSourceLearnState,
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) {
    let succeeded = state
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == "Succeeded")
        .count();
    let failed = state
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == "Failed")
        .count();
    let aborted = state
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == "Aborted")
        .count();
    let max_steps = state
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == "MaxStepsExceeded")
        .count();
    let avg_score = if state.outcomes.is_empty() {
        0.0
    } else {
        state
            .outcomes
            .iter()
            .map(|outcome| outcome.score)
            .sum::<f32>()
            / state.outcomes.len() as f32
    };
    train_progress(
        dashboard_tx,
        format!(
            "train source learn summary: session={} completed={}/{} succeeded={} failed={} aborted={} max_steps={} avg_score={:.2} sleep_runs={} optimize_runs={} compiled_suites={}",
            session_root.display(),
            state.completed_tasks,
            state.total_tasks,
            succeeded,
            failed,
            aborted,
            max_steps,
            avg_score,
            state.sleep_runs,
            state.optimize_runs,
            state.last_compiled_prompt_count
        ),
    );
}

#[derive(serde::Serialize, Clone)]
struct TrainSourceLearnState {
    path: String,
    total_tasks: usize,
    completed_tasks: usize,
    batch_size: usize,
    sleep_runs: usize,
    optimize_runs: usize,
    last_compiled_prompt_count: usize,
    last_task_id: Option<String>,
    last_task_status: Option<String>,
    last_score: Option<f32>,
    outcomes: Vec<TrainSourceLearnOutcomeSummary>,
    batch_reports: Vec<TrainSourceLearnBatchReport>,
}

impl TrainSourceLearnState {
    fn new(path: String, total_tasks: usize, batch_size: usize) -> Self {
        Self {
            path,
            total_tasks,
            completed_tasks: 0,
            batch_size,
            sleep_runs: 0,
            optimize_runs: 0,
            last_compiled_prompt_count: 0,
            last_task_id: None,
            last_task_status: None,
            last_score: None,
            outcomes: Vec::new(),
            batch_reports: Vec::new(),
        }
    }
}

#[derive(serde::Serialize, Clone)]
struct TrainSourceLearnOutcomeSummary {
    task_id: String,
    status: String,
    score: f32,
    steps_used: usize,
    repeated_actions: usize,
}

impl TrainSourceLearnOutcomeSummary {
    fn from_episode(outcome: &EpisodeOutcome) -> Self {
        Self {
            task_id: outcome.task.id.clone(),
            status: format!("{:?}", outcome.status),
            score: outcome.metric.score,
            steps_used: outcome.metric.steps_used,
            repeated_actions: outcome.metric.repeated_actions,
        }
    }
}

#[derive(serde::Serialize, Clone)]
struct TrainSourceLearnBatchReport {
    completed_tasks: usize,
    active_variant: String,
    sleep_failure_patterns: usize,
    sleep_bootstrap_demos: usize,
    sleep_stress_cases: usize,
    sleep_instruction_hypotheses: usize,
    sleep_runtime_demos: usize,
    sleep_turn_demos: usize,
    sleep_runtime_prompt_suggestions: usize,
    sleep_runtime_prompt_candidates: usize,
    sleep_runtime_demo_evaluations: usize,
    sleep_turn_demo_evaluations: usize,
    sleep_runtime_demo_passed: usize,
    sleep_runtime_demo_regressions: usize,
    sleep_runtime_prompt_rolled_back: bool,
    sleep_runtime_prompt_evolution_rounds: usize,
    sleep_runtime_prompt_accepted: bool,
    retained_reflections: usize,
    compiled_prompt_count: usize,
    optimized_suites: usize,
}

#[derive(Clone)]
struct TrainSourceLearnSession {
    session_root: PathBuf,
    state: Arc<tokio::sync::Mutex<TrainSourceLearnState>>,
}

impl TrainSourceLearnSession {
    async fn new(session_root: PathBuf, state: TrainSourceLearnState) -> Result<Self> {
        let session = Self {
            session_root,
            state: Arc::new(tokio::sync::Mutex::new(state)),
        };
        session.save().await?;
        Ok(session)
    }

    fn session_root(&self) -> &Path {
        &self.session_root
    }

    async fn snapshot(&self) -> TrainSourceLearnState {
        self.state.lock().await.clone()
    }

    async fn update<F>(&self, mutate: F) -> Result<()>
    where
        F: FnOnce(&mut TrainSourceLearnState),
    {
        let payload = {
            let mut state = self.state.lock().await;
            mutate(&mut state);
            serde_json::to_vec_pretty(&*state)
                .map_err(|err| miette!("failed to serialize learn state: {err}"))?
        };
        let path = self.session_root.join("learn_state.json");
        tokio::fs::write(&path, payload)
            .await
            .map_err(|err| miette!("failed to write learn state {}: {err}", path.display()))?;
        Ok(())
    }

    async fn save(&self) -> Result<()> {
        let payload = {
            let state = self.state.lock().await;
            serde_json::to_vec_pretty(&*state)
                .map_err(|err| miette!("failed to serialize learn state: {err}"))?
        };
        let path = self.session_root.join("learn_state.json");
        tokio::fs::write(&path, payload)
            .await
            .map_err(|err| miette!("failed to write learn state {}: {err}", path.display()))?;
        Ok(())
    }

    async fn shutdown(
        &self,
        interrupted: bool,
        dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    ) -> Result<()> {
        self.save().await?;
        let state = self.snapshot().await;
        if interrupted {
            train_progress(
                dashboard_tx,
                format!(
                    "train source learn interrupted: session state saved to {}",
                    self.session_root.display()
                ),
            );
        }
        print_train_source_learn_summary(&self.session_root, &state, dashboard_tx);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ValidationCommandResult {
    command: String,
    success: bool,
    summary: String,
}

async fn run_validation_commands(
    commands: &[String],
    workspace_dir: &Path,
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> Result<Vec<ValidationCommandResult>> {
    let mut results = Vec::new();
    for command in commands {
        train_progress(dashboard_tx, format!("validation command: {}", command));
        let output = run_shell_line_capture(command, workspace_dir).await?;
        results.push(ValidationCommandResult {
            command: command.clone(),
            success: output.status.success(),
            summary: summarize_command_output(&output),
        });
    }
    Ok(results)
}

fn summarize_command_output(output: &std::process::Output) -> String {
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            "ok".to_string()
        } else {
            truncate_from_left(&stdout, 120)
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            format!("status {}", output.status)
        } else {
            truncate_from_left(&stderr, 120)
        }
    }
}

fn final_episode_status(
    rollout_status: EpisodeStatus,
    validation_results: &[ValidationCommandResult],
) -> EpisodeStatus {
    if validation_results.is_empty() {
        return rollout_status;
    }
    if validation_results.iter().all(|result| result.success) {
        EpisodeStatus::Succeeded
    } else {
        EpisodeStatus::Failed
    }
}

fn validation_metadata(
    validation_results: &[ValidationCommandResult],
) -> std::collections::BTreeMap<String, String> {
    let mut metadata = std::collections::BTreeMap::new();
    for (index, result) in validation_results.iter().enumerate() {
        metadata.insert(
            format!("validation_{index}"),
            format!(
                "{} => {} ({})",
                result.command,
                if result.success { "ok" } else { "failed" },
                result.summary
            ),
        );
    }
    metadata
}

pub(crate) struct SpinovaHomeOverride {
    previous: Option<String>,
}

impl SpinovaHomeOverride {
    pub(crate) fn set(path: PathBuf) -> Self {
        let previous = env::var("SPINOVA_HOME").ok();
        unsafe {
            env::set_var("SPINOVA_HOME", path);
        }
        Self { previous }
    }
}

impl Drop for SpinovaHomeOverride {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => unsafe {
                env::set_var("SPINOVA_HOME", previous);
            },
            None => unsafe {
                env::remove_var("SPINOVA_HOME");
            },
        }
    }
}

async fn prepare_isolated_episode_root(task: &EpisodeTask, variant_name: &str) -> Result<PathBuf> {
    let task_slug = slugify(&task.id);
    let short_task = task_slug.chars().take(16).collect::<String>();
    let variant_slug = slugify(variant_name);
    let short_variant = variant_slug.chars().take(8).collect::<String>();
    let path = env::current_dir()
        .map_err(|err| miette!("failed to get current dir for episode home: {err}"))?
        .join("tmp")
        .join("ep")
        .join(short_task)
        .join(short_variant);

    if path.exists() {
        tokio::fs::remove_dir_all(&path).await.map_err(|err| {
            miette!(
                "failed to clear isolated episode root {}: {err}",
                path.display()
            )
        })?;
    }
    tokio::fs::create_dir_all(&path).await.map_err(|err| {
        miette!(
            "failed to create isolated episode root {}: {err}",
            path.display()
        )
    })?;
    Ok(path)
}

async fn prepare_learning_session_root(path: &str) -> Result<PathBuf> {
    let session_id = format!(
        "{}-{}",
        Local::now().format("%Y%m%d-%H%M%S"),
        slugify(path).chars().take(8).collect::<String>()
    );
    let root = env::current_dir()
        .map_err(|err| miette!("failed to get current dir for learn session: {err}"))?
        .join("tmp")
        .join("tsl")
        .join(session_id);
    tokio::fs::create_dir_all(&root).await.map_err(|err| {
        miette!(
            "failed to create learn session root {}: {err}",
            root.display()
        )
    })?;
    Ok(root)
}

async fn prepare_learning_home_root(session_root: &Path) -> Result<PathBuf> {
    let root = session_root.join("h");
    if root.exists() {
        tokio::fs::remove_dir_all(&root).await.map_err(|err| {
            miette!(
                "failed to clear learn session home {}: {err}",
                root.display()
            )
        })?;
    }
    tokio::fs::create_dir_all(&root).await.map_err(|err| {
        miette!(
            "failed to create learn session home {}: {err}",
            root.display()
        )
    })?;
    Ok(root)
}

async fn sync_learning_assets_to_session(shared_home: &Path, session_home: &Path) -> Result<()> {
    let shared = SpinovaPaths::from_root(shared_home.to_path_buf());
    let session = SpinovaPaths::from_root(session_home.to_path_buf());
    sync_path_replace(
        &shared.artifact_dir(COMPILED_DIR_NAME),
        &session.artifact_dir(COMPILED_DIR_NAME),
    )
    .await?;
    sync_path_replace(
        &shared.artifact_dir("evaluations"),
        &session.artifact_dir("evaluations"),
    )
    .await?;
    Ok(())
}

async fn sync_learning_assets_back_to_shared(
    session_home: &Path,
    shared_home: &Path,
) -> Result<()> {
    let shared = SpinovaPaths::from_root(shared_home.to_path_buf());
    let session = SpinovaPaths::from_root(session_home.to_path_buf());
    sync_path_replace(
        &session.artifact_dir(COMPILED_DIR_NAME),
        &shared.artifact_dir(COMPILED_DIR_NAME),
    )
    .await?;
    sync_path_replace(
        &session.artifact_dir("evaluations"),
        &shared.artifact_dir("evaluations"),
    )
    .await?;
    Ok(())
}

async fn sync_path_replace(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    if dst.exists() {
        let metadata = tokio::fs::metadata(dst)
            .await
            .map_err(|err| miette!("failed to stat {}: {err}", dst.display()))?;
        if metadata.is_dir() {
            tokio::fs::remove_dir_all(dst)
                .await
                .map_err(|err| miette!("failed to clear {}: {err}", dst.display()))?;
        } else {
            tokio::fs::remove_file(dst)
                .await
                .map_err(|err| miette!("failed to clear {}: {err}", dst.display()))?;
        }
    }
    copy_path_recursive(src, dst).await
}

async fn copy_path_recursive(src: &Path, dst: &Path) -> Result<()> {
    let metadata = tokio::fs::metadata(src)
        .await
        .map_err(|err| miette!("failed to stat {}: {err}", src.display()))?;
    if metadata.is_dir() {
        tokio::fs::create_dir_all(dst)
            .await
            .map_err(|err| miette!("failed to create {}: {err}", dst.display()))?;
        let mut entries = tokio::fs::read_dir(src)
            .await
            .map_err(|err| miette!("failed to read dir {}: {err}", src.display()))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|err| miette!("failed to iterate dir {}: {err}", src.display()))?
        {
            let child_src = entry.path();
            let child_dst = dst.join(entry.file_name());
            Box::pin(copy_path_recursive(&child_src, &child_dst)).await?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| miette!("failed to create parent {}: {err}", parent.display()))?;
        }
        tokio::fs::copy(src, dst).await.map_err(|err| {
            miette!(
                "failed to copy {} -> {}: {err}",
                src.display(),
                dst.display()
            )
        })?;
    }
    Ok(())
}

async fn prepare_learning_episode_root(
    session_root: &Path,
    task: &EpisodeTask,
    index: usize,
) -> Result<PathBuf> {
    let task_slug = slugify(&task.id);
    let short_task = task_slug.chars().take(12).collect::<String>();
    let root = session_root
        .join("e")
        .join(format!("{:04}-{}", index, short_task));
    if root.exists() {
        tokio::fs::remove_dir_all(&root).await.map_err(|err| {
            miette!(
                "failed to clear learn episode root {}: {err}",
                root.display()
            )
        })?;
    }
    tokio::fs::create_dir_all(&root).await.map_err(|err| {
        miette!(
            "failed to create learn episode root {}: {err}",
            root.display()
        )
    })?;
    Ok(root)
}

async fn provision_episode_workspace(
    task: &EpisodeTask,
    workspace_dir: &Path,
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> Result<()> {
    tokio::fs::create_dir_all(workspace_dir)
        .await
        .map_err(|err| {
            miette!(
                "failed to create episode workspace {}: {err}",
                workspace_dir.display()
            )
        })?;

    if let Some(repo) = task.metadata.get("repo") {
        let remote = infer_repo_remote(repo);
        let cache_root = prepare_train_source_repo_cache_root().await?;
        let cache_repo = cache_root.join(format!("{}.git", slugify(repo)));
        ensure_cached_repo(repo, &remote, &cache_repo, dashboard_tx).await?;
        train_progress(
            dashboard_tx,
            format!(
                "clone repo={} from local cache {}",
                repo,
                cache_repo.display()
            ),
        );
        run_host_command(
            &[
                "git",
                "-c",
                "core.longpaths=true",
                "clone",
                "--shared",
                cache_repo.to_string_lossy().as_ref(),
                workspace_dir.to_string_lossy().as_ref(),
            ],
            None,
        )
        .await?;

        if let Some(base_commit) = task.metadata.get("base_commit") {
            train_progress(
                dashboard_tx,
                format!("checkout base_commit={}", base_commit),
            );
            run_host_command(
                &[
                    "git",
                    "-c",
                    "core.longpaths=true",
                    "checkout",
                    base_commit.as_str(),
                ],
                Some(workspace_dir),
            )
            .await?;
        }
    }

    for command in &task.setup_commands {
        train_progress(dashboard_tx, format!("setup command: {}", command));
        run_shell_line(command, workspace_dir).await?;
    }

    Ok(())
}

fn infer_repo_remote(repo: &str) -> String {
    if repo.starts_with("http://") || repo.starts_with("https://") || repo.ends_with(".git") {
        repo.to_string()
    } else {
        format!("https://github.com/{repo}.git")
    }
}

async fn prepare_train_source_repo_cache_root() -> Result<PathBuf> {
    let root = env::current_dir()
        .map_err(|err| miette!("failed to get current dir for train-source repo cache: {err}"))?
        .join("tmp")
        .join("train_source_repo_cache");
    tokio::fs::create_dir_all(&root).await.map_err(|err| {
        miette!(
            "failed to create train-source repo cache root {}: {err}",
            root.display()
        )
    })?;
    Ok(root)
}

async fn ensure_cached_repo(
    repo: &str,
    remote: &str,
    cache_repo: &Path,
    dashboard_tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> Result<()> {
    if cache_repo.exists() {
        train_progress(
            dashboard_tx,
            format!("repo cache hit for {} at {}", repo, cache_repo.display()),
        );
        return Ok(());
    }

    train_progress(
        dashboard_tx,
        format!("repo cache miss for {} -> {}", repo, cache_repo.display()),
    );
    run_host_command(
        &[
            "git",
            "-c",
            "core.longpaths=true",
            "clone",
            "--mirror",
            remote,
            cache_repo.to_string_lossy().as_ref(),
        ],
        None,
    )
    .await
}

async fn run_host_command(args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let mut command = tokio::process::Command::new(args[0]);
    command.args(&args[1..]);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .output()
        .await
        .map_err(|err| miette!("failed to run host command {:?}: {err}", args))?;
    if !output.status.success() {
        return Err(miette!(
            "host command {:?} failed with status {}: {}",
            args,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

async fn run_shell_line(command: &str, cwd: &Path) -> Result<()> {
    let output = run_shell_line_capture(command, cwd).await?;

    if !output.status.success() {
        return Err(miette!(
            "setup command `{}` failed with status {}: {}",
            command,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

async fn run_shell_line_capture(command: &str, cwd: &Path) -> Result<std::process::Output> {
    let output = if cfg!(windows) {
        tokio::process::Command::new("powershell")
            .arg("-NoLogo")
            .arg("-NoProfile")
            .arg("-Command")
            .arg(command)
            .current_dir(cwd)
            .output()
            .await
    } else {
        tokio::process::Command::new("bash")
            .arg("-lc")
            .arg(command)
            .current_dir(cwd)
            .output()
            .await
    }
    .map_err(|err| miette!("failed to run setup command `{command}`: {err}"))?;
    Ok(output)
}

async fn enter_episode_workspace(context: &mut Context, workspace_dir: &Path) -> Result<()> {
    context.execution_cwd = workspace_dir.to_path_buf();
    Ok(())
}

pub(crate) struct AgentLoopStepExecution {
    pub(crate) output: AgentLoopStepOutput,
    pub(crate) snapshot_text: String,
    pub(crate) history_messages: Vec<PromptMessage>,
}

pub(crate) struct AgentLoopStepOutput {
    pub(crate) observation: String,
    pub(crate) description: String,
    pub(crate) stop_reason: String,
    pub(crate) current_doing: String,
    pub(crate) actions: Vec<EpisodeActionRecord>,
}

const RUNTIME_HISTORY_MIN_MESSAGES: usize = 4;
const RUNTIME_HISTORY_SUMMARY_MAX_TOKENS: usize = 800;
const HINDSIGHT_RECALL_QUERY_MAX_TOKENS: usize = 420;
const HINDSIGHT_RECENT_MESSAGES_MAX_TOKENS: usize = 160;
const HINDSIGHT_RECENT_MESSAGES_MIN_ENTRIES: usize = 2;
const CLAIMED_EVENT_FINISHED_STOP_REASON: &str = "claimed_event_finished";

fn select_recent_trail_lines_for_hindsight(context: &Context) -> Vec<String> {
    select_recent_items_by_token_budget(
        context.memory.trail(),
        HINDSIGHT_RECENT_MESSAGES_MAX_TOKENS,
        HINDSIGHT_RECENT_MESSAGES_MIN_ENTRIES,
        |line| approx_token_count(line),
    )
}

fn select_recent_items_by_token_budget<T, F>(
    items: Vec<T>,
    max_tokens: usize,
    min_items: usize,
    mut token_cost: F,
) -> Vec<T>
where
    F: FnMut(&T) -> usize,
{
    let mut selected = Vec::new();
    let mut total_tokens = 0usize;
    for item in items.into_iter().rev() {
        let cost = token_cost(&item);
        let can_fit = total_tokens.saturating_add(cost) <= max_tokens;
        if selected.len() < min_items || can_fit {
            total_tokens = total_tokens.saturating_add(cost);
            selected.push(item);
        } else {
            break;
        }
    }
    selected.reverse();
    selected
}

async fn record_runtime_history_messages(context: &mut Context, draft: RuntimeTurnDraft) {
    let retain_plan = context.memory.commit_runtime_turn(draft).await;
    for job in retain_plan.jobs {
        if let Err(err) = context.hindsight_retain.enqueue(job) {
            tracing::error!("failed to enqueue hindsight retain job: {err:?}");
            return;
        }
    }
    if retain_plan.must_flush_before_continue {
        if let Err(err) = context.hindsight_retain.flush().await {
            tracing::error!("failed to flush hindsight retain queue: {err:?}");
        } else {
            context.memory.mark_queued_retained();
        }
    }
}

async fn record_runtime_review_turn(
    context: &mut Context,
    pre_step_snapshot_text: &str,
    history_messages: &[PromptMessage],
    output: &AgentLoopStepOutput,
) {
    if !context.record_runtime_reviews || history_messages.is_empty() {
        return;
    }
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert("origin".to_string(), "runtime_agent_loop".to_string());
    if let Some(action) = output.actions.last() {
        metadata.insert("action_kind".to_string(), action.kind.clone());
        metadata.insert("action_summary".to_string(), action.summary.clone());
    }
    metadata.insert("stop_reason".to_string(), output.stop_reason.clone());
    if let Some(objective) = context.work_state.objective() {
        metadata.insert("objective".to_string(), objective.to_string());
    }
    if let Some(phase) = context.work_state.work_phase() {
        metadata.insert("work_phase".to_string(), phase.to_string());
    }
    if let Some(item_id) = context.work_state.item_id {
        metadata.insert("item_id".to_string(), item_id.to_string());
    }
    metadata.insert(
        "execution_cwd".to_string(),
        context.execution_cwd.display().to_string(),
    );
    if let Some(info) = context.llm.token_usage_info() {
        metadata.insert(
            "main_model_total_tokens".to_string(),
            info.total_token_usage.total_tokens.to_string(),
        );
        metadata.insert(
            "main_model_last_tokens".to_string(),
            info.last_token_usage.total_tokens.to_string(),
        );
    }
    if let Some(info) = context.judge_llm.token_usage_info() {
        metadata.insert(
            "judge_model_total_tokens".to_string(),
            info.total_token_usage.total_tokens.to_string(),
        );
        metadata.insert(
            "judge_model_last_tokens".to_string(),
            info.last_token_usage.total_tokens.to_string(),
        );
    }

    let turn = RuntimeTurnRecord {
        id: format!("runtime-turn:{}", uuid::Uuid::new_v4()),
        recorded_at_ms: Utc::now().timestamp_millis(),
        current_doing: output.current_doing.clone(),
        description: output.description.clone(),
        observation: output.observation.clone(),
        actions: output.actions.clone(),
        before_snapshot_text: pre_step_snapshot_text.to_string(),
        after_snapshot_text: Snapshot::new(context).await.to_runtime_text(),
        history_messages: history_messages.to_vec(),
        metadata,
    };
    append_runtime_turn_record(&turn).await;
}

pub(crate) async fn execute_agent_loop_step(
    context: &mut Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> AgentLoopStepExecution {
    context.prompt_memory = build_hindsight_memory_context(context).await;
    let claimed_inputs = claim_pending_runtime_inputs(context, RUNTIME_EVENT_CLAIM_BATCH_SIZE);
    let claimed_event_ids = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.event_id.to_string()),
            ClaimedRuntimeInput::DeviceNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    let claimed_device_notices = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(_) => None,
            ClaimedRuntimeInput::DeviceNotice { device, .. } => Some(*device),
        })
        .collect::<Vec<_>>();
    let claimed_input_messages = claimed_inputs
        .iter()
        .map(|input| prompt_message_for_claimed_input(context, input))
        .collect::<Vec<_>>();
    let claimed_event_views = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.clone()),
            ClaimedRuntimeInput::DeviceNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    let snapshot = Snapshot::new_with_claimed_events(context, &claimed_event_views).await;
    let snapshot_text = snapshot.to_runtime_text();
    let request_envelope = build_runtime_request_envelope(context, &snapshot_text);
    let initial_tools = build_runtime_tool_specs(context);
    let runtime_conversation_budget = request_envelope
        .conversation_budget_tokens(&initial_tools, runtime_request_budget_limits(context));
    let runtime_conversation_summary_budget =
        RUNTIME_HISTORY_SUMMARY_MAX_TOKENS.min(runtime_conversation_budget);
    if let Some(plan) = context.memory.plan_runtime_conversation_compaction(
        runtime_conversation_budget,
        RUNTIME_HISTORY_MIN_MESSAGES,
        runtime_conversation_summary_budget,
    ) {
        let summary = build_runtime_conversation_summary(context, &plan).await;
        let _ = context
            .memory
            .apply_runtime_conversation_compaction(plan, summary);
    }
    let mut conversation_slice = context.memory.runtime_conversation_slice(
        runtime_conversation_budget,
        RUNTIME_HISTORY_MIN_MESSAGES,
        runtime_conversation_summary_budget,
    );
    conversation_slice.extend(claimed_input_messages.iter().cloned());
    let mut runtime_step = context
        .memory
        .begin_runtime_step_from_parts(request_envelope, conversation_slice);
    let mut tool_results = Vec::new();
    let mut actions = Vec::new();
    let mut budget_recoveries = 0usize;

    let output = 'agent_loop: loop {
        let tools = build_runtime_tool_specs(context);
        if maybe_compact_runtime_messages(context, &mut runtime_step, &tools, false).await {
            set_runtime_status(tx, RuntimeStatusLevel::Info, "Compacting runtime context");
        }
        let request = AgentTurnRequest {
            messages: runtime_step.clone_agent_messages(),
            tools: tools.clone(),
        };
        let response = match run_agent_turn_with_retry(context, request, tx).await {
            Ok(response) => response,
            Err(err) => {
                if is_context_budget_exceeded(&err)
                    && budget_recoveries < MID_TURN_COMPACTION_MAX_RECOVERIES
                    && maybe_compact_runtime_messages(context, &mut runtime_step, &tools, true)
                        .await
                {
                    budget_recoveries += 1;
                    set_runtime_status(
                        tx,
                        RuntimeStatusLevel::Warn,
                        format!(
                            "Recovering from context overflow ({budget_recoveries}/{})",
                            MID_TURN_COMPACTION_MAX_RECOVERIES
                        ),
                    );
                    continue 'agent_loop;
                }
                if !claimed_event_ids.is_empty() {
                    requeue_claimed_runtime_events(context, &claimed_event_ids);
                }
                let observation = format!("agent turn failed: {err}");
                let terminal_action = EpisodeActionRecord {
                    kind: "agent_turn_failed".to_string(),
                    summary: observation.clone(),
                };
                let mut terminal_actions = actions.clone();
                terminal_actions.push(terminal_action.clone());
                runtime_step.push_history_message(PromptMessage::assistant(observation.clone()));
                if let Some(cell) = assistant_activity_cell(&observation) {
                    append_committed_activity_cells(tx, vec![cell]);
                }
                break 'agent_loop AgentLoopStepOutput {
                    observation: observation.clone(),
                    description: "模型请求失败。".to_string(),
                    stop_reason: "agent_turn_failed".to_string(),
                    current_doing: "等待下一轮工具决策".to_string(),
                    actions: terminal_actions,
                };
            }
        };
        let mut response_assistant_messages = Vec::new();
        let mut response_tool_calls = Vec::new();
        for item in response.items {
            match item {
                AgentTurnItem::AssistantMessage { content } => {
                    if !content.trim().is_empty() {
                        response_assistant_messages.push(content);
                    }
                }
                AgentTurnItem::ToolCall { call } => response_tool_calls.push(call),
            }
        }
        let response_assistant_content = response
            .last_assistant_message
            .clone()
            .or_else(|| response_assistant_messages.last().cloned());

        if !response_tool_calls.is_empty() {
            let calls = response_tool_calls;
            let assistant_text = if response_assistant_messages.is_empty() {
                None
            } else {
                Some(response_assistant_messages.join("\n\n"))
            };
            let tool_call_ui_events = calls
                .iter()
                .map(|call| {
                    render_tool_call_ui_event(call).unwrap_or_else(|_| {
                        if call.name == "apply_patch" {
                            ToolCallUiEvent::error(
                                "apply_patch".to_string(),
                                vec!["invalid patch syntax".to_string()],
                            )
                        } else {
                            ToolCallUiEvent::error(
                                call.name.clone(),
                                vec![call.arguments.to_string()],
                            )
                        }
                    })
                })
                .collect::<Vec<_>>();
            runtime_step.push_agent_message(AgentMessage::assistant_tool_call_protocol(
                assistant_text.clone(),
                calls.clone(),
            ));
            if let Some(content) = assistant_text.clone()
                && !content.trim().is_empty()
            {
                runtime_step.push_history_message(PromptMessage::assistant(content));
            }
            runtime_step.push_history_message(PromptMessage::assistant_with_tool_calls(
                String::new(),
                tool_call_ui_events.clone(),
            ));
            let mut committed_cells = Vec::new();
            if let Some(content) = assistant_text.clone()
                && let Some(cell) = assistant_activity_cell(&content)
            {
                committed_cells.push(cell);
            }
            committed_cells.extend(
                tool_call_ui_events
                    .iter()
                    .cloned()
                    .filter(|event| match event {
                        event if is_apply_patch_tool_call_event(event) => false,
                        ToolCallUiEvent::Exec(_) => false,
                        ToolCallUiEvent::Terminal(terminal_event) => !matches!(
                            terminal_event.action,
                            crate::tool_ui::TerminalUiAction::Execute
                                | crate::tool_ui::TerminalUiAction::Continue
                        ),
                        _ => true,
                    })
                    .flat_map(activity_cells_from_tool_call_ui_event),
            );
            append_committed_activity_cells(tx, committed_cells);
            for (call, call_ui_event) in calls.iter().zip(tool_call_ui_events.iter()) {
                let action_record = summarize_action_from_tool_call(call).unwrap_or_else(|_| {
                        EpisodeActionRecord {
                            kind: "tool_call".to_string(),
                            summary: call.name.clone(),
                        }
                    });
                actions.push(action_record);
                if let Some(tx) = tx {
                    match call_ui_event.clone() {
                        ToolCallUiEvent::Exec(event) => {
                            tx.send_modify(|state| {
                                apply_activity_event(
                                    state,
                                    DashboardActivityEvent::ExecBegin {
                                        key: call.id.clone(),
                                        title: event.title,
                                        call_lines: event.body_lines,
                                    },
                                );
                            });
                        }
                        ToolCallUiEvent::Terminal(event)
                            if matches!(
                                event.action,
                                crate::tool_ui::TerminalUiAction::Execute
                                    | crate::tool_ui::TerminalUiAction::Continue
                            ) =>
                        {
                            tx.send_modify(|state| {
                                apply_activity_event(
                                    state,
                                    DashboardActivityEvent::ExecBegin {
                                        key: call.id.clone(),
                                        title: event.title,
                                        call_lines: event.body_lines,
                                    },
                                );
                            });
                        }
                        _ => {}
                    }
                }
                let result = match execute_agent_tool_call(context, call).await {
                    Ok(result) => result,
                    Err(err) => {
                        let error_text = err.to_string();
                        let ui_error_text = if call.name == "apply_patch" {
                            summarize_apply_patch_error(&error_text)
                        } else {
                            error_text.clone()
                        };
                        ToolExecutionResult::new(
                            format!("{} failed", call.name),
                            json!({
                                "error": error_text,
                            }),
                            ToolUiEvent::error(
                                format!("{} failed", call.name),
                                compact_body_lines(&ui_error_text, 6),
                            ),
                        )
                    }
                };
                if let Some(tx) = tx {
                    tx.send_modify(|state| {
                        apply_activity_event(
                            state,
                            DashboardActivityEvent::ExecEnd {
                                key: call.id.clone(),
                            },
                        );
                    });
                }
                runtime_step.push_agent_message(AgentMessage::tool(
                    call.id.clone(),
                    call.name.clone(),
                    result.model_content(),
                ));
                runtime_step.push_history_message(PromptMessage::tool_with_ui(
                    result.history_content(&call.id, &call.name),
                    result.ui_event.clone(),
                ));
                append_committed_activity_cells(
                    tx,
                    vec![activity_cell_from_tool_ui_event(result.ui_event.clone())],
                );
                tool_results.push(format!("{} => {}", call.name, result.summary));
                if claimed_events_are_terminal(context, &claimed_event_ids) {
                    if actions.is_empty() {
                        actions.push(EpisodeActionRecord {
                            kind: "claimed_events_completed".to_string(),
                            summary: "claimed events reached terminal state".to_string(),
                        });
                    }
                    break 'agent_loop AgentLoopStepOutput {
                        observation: if tool_results.is_empty() {
                            "claimed events reached terminal state".to_string()
                        } else {
                            tool_results.join("\n")
                        },
                        description: "本轮领取的事件已完成或已交接，turn 在相关 tool 后立即终止。"
                            .to_string(),
                        stop_reason: CLAIMED_EVENT_FINISHED_STOP_REASON.to_string(),
                        current_doing: "等待下一轮工具决策".to_string(),
                        actions: actions.clone(),
                    };
                }
            }
            continue 'agent_loop;
        }

        let content = response_assistant_content.unwrap_or_default();
        if let RuntimeFollowUpDecision::Continue { reason } =
            runtime_turn_follow_up_decision(
                context,
                response.raw_stream_follow_up,
                &claimed_event_ids,
            )
        {
            runtime_step.push_agent_message(AgentMessage::system(reason.message().to_string()));
            continue 'agent_loop;
        }
        let current_doing = content
            .lines()
            .next()
            .filter(|line| !line.trim().is_empty())
            .unwrap_or("等待下一轮工具决策")
            .to_string();
        let assistant_action = EpisodeActionRecord {
            kind: "assistant_message".to_string(),
            summary: current_doing.clone(),
        };
        actions.push(assistant_action.clone());
        runtime_step.set_current_doing(current_doing.clone());
        runtime_step.push_history_message(PromptMessage::assistant(content.clone()));
        if let Some(cell) = assistant_activity_cell(&content) {
            append_committed_activity_cells(tx, vec![cell]);
        }
        break 'agent_loop AgentLoopStepOutput {
            observation: if tool_results.is_empty() {
                content.clone()
            } else {
                tool_results.join("\n")
            },
            description: if tool_results.is_empty() {
                "模型返回了 assistant 文本，但没有调用 tool。".to_string()
            } else {
                content
            },
            stop_reason: "assistant_message".to_string(),
            current_doing,
            actions: actions.clone(),
        };
    };
    runtime_step.set_current_doing(output.current_doing.clone());
    finalize_claimed_runtime_events(context, &claimed_event_ids, &output);
    finalize_claimed_runtime_device_notices(context, &claimed_device_notices, &output);
    let history_messages = runtime_step.history_messages().to_vec();
    if !runtime_step.is_history_empty() {
        record_runtime_history_messages(context, runtime_step.into_turn_draft()).await;
    }
    if output
        .actions
        .last()
        .map(|action| !matches!(action.kind.as_str(), "assistant_message" | "empty_tool_calls"))
        .unwrap_or(false)
    {
        context.work_state.touch();
    }
    record_runtime_review_turn(context, &snapshot_text, &history_messages, &output).await;
    AgentLoopStepExecution {
        output,
        snapshot_text,
        history_messages,
    }
}

enum ClaimedRuntimeInput {
    Event(EventView),
    DeviceNotice { device: DeviceId, reason: String },
}

fn claim_pending_runtime_inputs(context: &Context, max_events: usize) -> Vec<ClaimedRuntimeInput> {
    let queued_work = match context.pending_work.claim_batch(max_events) {
        Ok(items) => items,
        Err(err) => {
            tracing::error!("failed to claim pending runtime work batch: {err:?}");
            return Vec::new();
        }
    };

    let mut claimed_inputs = Vec::new();
    for work in queued_work {
        match work {
            PendingWork::Event { event_id } => {
                match context.events.claim_event_if_pending(event_id) {
                    Ok(Some(event)) => claimed_inputs.push(ClaimedRuntimeInput::Event(event)),
                    Ok(None) => {
                        if let Err(err) = context
                            .pending_work
                            .consume(PendingWork::Event { event_id })
                        {
                            tracing::error!(
                                "failed to consume stale runtime event driver {event_id}: {err:?}"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            "failed to claim pending runtime event {event_id}: {err:?}"
                        );
                    }
                }
            }
            PendingWork::DeviceNotice { device, reason } => {
                let render = context
                    .devices
                    .state_renders()
                    .into_iter()
                    .find(|(device_id, _)| *device_id == device)
                    .map(|(_, render)| render);
                let Some(render) = render else {
                    if let Err(err) = context.pending_work.consume(PendingWork::DeviceNotice {
                        device,
                        reason: String::new(),
                    }) {
                        tracing::error!(
                            "failed to consume missing device notice driver for {device}: {err:?}"
                        );
                    }
                    continue;
                };
                if !matches!(render.attention, crate::device::AttentionLevel::Notice) {
                    if let Err(err) = context.pending_work.consume(PendingWork::DeviceNotice {
                        device,
                        reason: String::new(),
                    }) {
                        tracing::error!(
                            "failed to consume stale device notice driver for {device}: {err:?}"
                        );
                    }
                    continue;
                }
                claimed_inputs.push(ClaimedRuntimeInput::DeviceNotice { device, reason });
            }
        }
    }
    claimed_inputs
}

fn requeue_claimed_runtime_events(context: &Context, event_ids: &[String]) {
    for event_id in event_ids {
        match context.events.requeue_if_claimed(event_id) {
            Ok(true) => {
                if let Ok(event_id) = uuid::Uuid::parse_str(event_id)
                    && let Err(err) = context
                        .pending_work
                        .requeue_front(PendingWork::Event { event_id })
                {
                    tracing::error!(
                        "failed to requeue pending runtime work for event {event_id}: {err:?}"
                    );
                }
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!("failed to requeue claimed runtime event {event_id}: {err:?}");
            }
        }
    }
}

fn finalize_claimed_runtime_events(
    context: &Context,
    event_ids: &[String],
    output: &AgentLoopStepOutput,
) {
    if event_ids.is_empty() {
        return;
    }

    let mut requeued = Vec::new();
    for event_id in event_ids {
        match context.events.requeue_if_claimed(event_id) {
            Ok(true) => {
                if let Ok(parsed_event_id) = uuid::Uuid::parse_str(event_id)
                    && let Err(err) = context.pending_work.requeue_front(PendingWork::Event {
                        event_id: parsed_event_id,
                    })
                {
                    tracing::error!(
                        "failed to requeue pending runtime work for event {event_id}: {err:?}"
                    );
                }
                requeued.push(event_id.clone());
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!("failed to finalize claimed runtime event {event_id}: {err:?}");
            }
        }
    }

    if !requeued.is_empty() {
        let last_action = output.actions.last();
        tracing::info!(
            action_kind = last_action.map(|action| action.kind.as_str()).unwrap_or("none"),
            action_summary = last_action
                .map(|action| action.summary.as_str())
                .unwrap_or(""),
            requeued_claimed_events = requeued.len(),
            event_ids = requeued.join(","),
            "requeued claimed runtime events left unresolved at turn end",
        );
    }
}

fn finalize_claimed_runtime_device_notices(
    context: &Context,
    devices: &[DeviceId],
    output: &AgentLoopStepOutput,
) {
    if devices.is_empty() {
        return;
    }

    let renders = context.devices.state_renders();
    let mut released = Vec::new();
    for device in devices {
        let still_noticed = renders
            .iter()
            .find(|(device_id, _)| device_id == device)
            .map(|(_, render)| matches!(render.attention, crate::device::AttentionLevel::Notice))
            .unwrap_or(false);
        let work = PendingWork::DeviceNotice {
            device: *device,
            reason: String::new(),
        };
        if still_noticed {
            match context.pending_work.release_claimed(work) {
                Ok(true) => released.push(device.to_string()),
                Ok(false) => {}
                Err(err) => {
                    tracing::error!(
                        "failed to release claimed device notice driver for {device}: {err:?}"
                    );
                }
            }
        } else if let Err(err) = context.pending_work.consume(work) {
            tracing::error!("failed to consume device notice driver for {device}: {err:?}");
        }
    }

    if !released.is_empty() {
        let last_action = output.actions.last();
        tracing::info!(
            action_kind = last_action.map(|action| action.kind.as_str()).unwrap_or("none"),
            action_summary = last_action
                .map(|action| action.summary.as_str())
                .unwrap_or(""),
            reactivated_device_notice_drivers = released.len(),
            devices = released.join(","),
            "released claimed runtime device notice drivers back into frontier at turn end",
        );
    }
}

fn claimed_events_are_terminal(context: &Context, event_ids: &[String]) -> bool {
    if event_ids.is_empty() {
        return false;
    }

    let statuses = event_ids
        .iter()
        .map(|event_id| context.events.view(event_id).map(|event| event.status))
        .collect::<Result<Vec<_>, _>>()
        .ok();
    statuses
        .as_deref()
        .map(claimed_event_statuses_are_terminal)
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaimedEventStatusSummary {
    has_claimed: bool,
    all_terminal: bool,
}

fn summarize_claimed_event_statuses(statuses: &[EventStatus]) -> ClaimedEventStatusSummary {
    if statuses.is_empty() {
        return ClaimedEventStatusSummary {
            has_claimed: false,
            all_terminal: false,
        };
    }

    let mut all_terminal = true;
    let mut has_claimed = false;

    for status in statuses {
        match status {
            EventStatus::Claimed => {
                has_claimed = true;
                return ClaimedEventStatusSummary {
                    has_claimed,
                    all_terminal: false,
                };
            }
            EventStatus::AwaitingDelivery
            | EventStatus::Resolved
            | EventStatus::Dismissed
            | EventStatus::Failed => {}
            _ => {
                all_terminal = false;
                return ClaimedEventStatusSummary {
                    has_claimed,
                    all_terminal,
                };
            }
        }
    }

    ClaimedEventStatusSummary {
        has_claimed,
        all_terminal,
    }
}

fn claimed_event_statuses_are_terminal(statuses: &[EventStatus]) -> bool {
    summarize_claimed_event_statuses(statuses).all_terminal
}

fn prompt_message_for_claimed_input(
    _context: &Context,
    input: &ClaimedRuntimeInput,
) -> PromptMessage {
    match input {
        ClaimedRuntimeInput::Event(event) => match &event.payload {
            EventPayload::TelegramIncoming(payload) => PromptMessage::user(format!(
                "<world_event source=\"telegram\" event_id=\"{}\" status=\"{}\">\nfrom: {}\nchat_title: {}\nchat_id: {}\nincoming_text: {}\n</world_event>",
                event.event_id,
                event.status,
                payload.sender,
                payload.chat_title,
                payload.chat_id,
                payload.incoming_text.trim(),
            )),
        },
        ClaimedRuntimeInput::DeviceNotice { device, reason } => PromptMessage::user(format!(
            "<device_notice device=\"{}\">\nreason: {}\n</device_notice>",
            device, reason,
        )),
    }
}

enum RuntimeFollowUpDecision {
    Continue { reason: RuntimeFollowUpReason },
    AllowFinish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeFollowUpReason {
    RawStreamRequestedFollowUp,
    ClaimedEventNeedsExplicitResolution,
}

struct RuntimeTurnFollowUpState<'a> {
    raw_stream_requested_follow_up: bool,
    claimed_statuses: &'a [EventStatus],
}

impl RuntimeFollowUpReason {
    fn message(self) -> &'static str {
        match self {
            Self::RawStreamRequestedFollowUp => {
                "本次采样仍标记为 needs_follow_up；请继续推进当前 turn。"
            }
            Self::ClaimedEventNeedsExplicitResolution => {
                "当前 turn 已领取事件。不要只输出文本回复来结束；请继续调用工具，并在准备好最终答复时显式调用 `finish_and_send` 提交 reply_message。"
            }
        }
    }
}

fn runtime_turn_follow_up_decision(
    context: &Context,
    raw_stream_follow_up: bool,
    claimed_event_ids: &[String],
) -> RuntimeFollowUpDecision {
    let claimed_statuses = claimed_event_ids
        .iter()
        .filter_map(|event_id| context.events.view(event_id).ok().map(|event| event.status))
        .collect::<Vec<_>>();

    let state = RuntimeTurnFollowUpState {
        raw_stream_requested_follow_up: raw_stream_follow_up,
        claimed_statuses: &claimed_statuses,
    };

    runtime_turn_follow_up_decision_from_state(&state)
}

fn runtime_turn_follow_up_decision_from_state(
    state: &RuntimeTurnFollowUpState<'_>,
) -> RuntimeFollowUpDecision {
    if state.raw_stream_requested_follow_up {
        return RuntimeFollowUpDecision::Continue {
            reason: RuntimeFollowUpReason::RawStreamRequestedFollowUp,
        };
    }

    if summarize_claimed_event_statuses(state.claimed_statuses).has_claimed {
        return RuntimeFollowUpDecision::Continue {
            reason: RuntimeFollowUpReason::ClaimedEventNeedsExplicitResolution,
        };
    }

    RuntimeFollowUpDecision::AllowFinish
}
fn is_apply_patch_tool_call_event(event: &ToolCallUiEvent) -> bool {
    match event {
        ToolCallUiEvent::Patch(_) => true,
        ToolCallUiEvent::Error(data) => data.title == "apply_patch",
        _ => false,
    }
}

fn append_committed_activity_cells(
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    cells: Vec<crate::dashboard::ActivityCell>,
) {
    if cells.is_empty() {
        return;
    }
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            apply_activity_event(
                state,
                DashboardActivityEvent::AppendCommittedCells { cells },
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimed_terminal_status_depends_only_on_statuses() {
        assert!(claimed_event_statuses_are_terminal(&[EventStatus::AwaitingDelivery]));
        assert!(claimed_event_statuses_are_terminal(&[EventStatus::Resolved]));
        assert!(claimed_event_statuses_are_terminal(&[EventStatus::Dismissed]));
        assert!(claimed_event_statuses_are_terminal(&[EventStatus::Failed]));
        assert!(!claimed_event_statuses_are_terminal(&[EventStatus::Claimed]));
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::AwaitingDelivery,
            EventStatus::Resolved,
        ]));
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::Resolved,
            EventStatus::Dismissed,
        ]));
        assert!(!claimed_event_statuses_are_terminal(&[
            EventStatus::AwaitingDelivery,
            EventStatus::Claimed,
        ]));
        assert!(!claimed_event_statuses_are_terminal(&[]));
    }

    #[test]
    fn claimed_status_summary_tracks_claimed_and_terminal_reason() {
        assert_eq!(
            summarize_claimed_event_statuses(&[EventStatus::Claimed]),
            ClaimedEventStatusSummary {
                has_claimed: true,
                all_terminal: false,
            }
        );
        assert_eq!(
            summarize_claimed_event_statuses(&[
                EventStatus::AwaitingDelivery,
                EventStatus::Resolved,
            ]),
            ClaimedEventStatusSummary {
                has_claimed: false,
                all_terminal: true,
            }
        );
        assert_eq!(
            summarize_claimed_event_statuses(&[EventStatus::Resolved, EventStatus::Failed,]),
            ClaimedEventStatusSummary {
                has_claimed: false,
                all_terminal: true,
            }
        );
        assert_eq!(
            summarize_claimed_event_statuses(&[EventStatus::Resolved, EventStatus::Claimed,]),
            ClaimedEventStatusSummary {
                has_claimed: true,
                all_terminal: false,
            }
        );
    }

    #[test]
    fn runtime_turn_follow_up_decision_state_machine_prefers_runtime_gate() {
        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: true,
            claimed_statuses: &[],
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::Continue { .. }
        ));

        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: false,
            claimed_statuses: &[EventStatus::Claimed],
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::Continue { .. }
        ));

        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: false,
            claimed_statuses: &[EventStatus::Resolved],
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::AllowFinish
        ));

        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: false,
            claimed_statuses: &[EventStatus::Resolved],
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::AllowFinish
        ));
    }

    #[test]
    fn follow_up_reason_messages_are_structured() {
        assert_eq!(
            RuntimeFollowUpReason::RawStreamRequestedFollowUp.message(),
            "本次采样仍标记为 needs_follow_up；请继续推进当前 turn。"
        );
    }
}

async fn run_agent_turn_with_retry(
    context: &Context,
    request: AgentTurnRequest,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> Result<AgentTurnStreamResult> {
    let budget = estimate_agent_turn_request(
        &request.messages,
        &request.tools,
        runtime_request_budget_limits(context),
    );
    let estimated_input_tokens = budget.total_input_tokens;
    write_current_turn_messages_dump(&request, &budget, context.llm.model_name().as_deref()).await;
    if let Some(tx) = tx {
        tx.send_modify(|state| {
            state.footer_estimated_input_tokens = Some(estimated_input_tokens);
            state.footer_context =
                render_dashboard_footer_context(context, state.footer_estimated_input_tokens);
        });
    }
    let mut attempt = 1usize;
    loop {
        set_runtime_status(tx, RuntimeStatusLevel::Debug, "Working");
        match context.llm.run_agent_turn(context, request.clone()).await {
            Ok(response) => {
                write_current_turn_response_dump(&response, attempt).await;
                clear_runtime_status(tx);
                return Ok(response);
            }
            Err(err) => {
                let will_retry = !is_context_budget_exceeded(&err);
                write_current_turn_response_error_dump(&err.to_string(), attempt, will_retry).await;
                if is_context_budget_exceeded(&err) {
                    clear_runtime_status(tx);
                    return Err(err);
                }
                let capped_shift = (attempt.saturating_sub(1)).min(6) as u32;
                let backoff_ms = 300u64.saturating_mul(1u64 << capped_shift).min(30_000);
                let summary = format!(
                    "请求失败，重试 #{attempt}，等待 {:.1}s",
                    backoff_ms as f64 / 1000.0
                );
                set_runtime_status(tx, RuntimeStatusLevel::Warn, summary);
                tracing::warn!("run_agent_turn retry #{attempt} after {backoff_ms}ms: {err}");
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                attempt += 1;
            }
        }
    }
}

async fn build_hindsight_memory_context(context: &mut Context) -> PromptMemoryContext {
    let hindsight = context.hindsight.clone();

    let phase = context
        .work_state
        .work_phase()
        .unwrap_or("investigate")
        .to_string();
    let query = build_hindsight_recall_query(
        context.work_state.objective(),
        &phase,
        context.memory.current_thread_focus().as_deref(),
        summarize_terminal_for_hindsight(context),
        select_recent_trail_lines_for_hindsight(context),
    );

    let recall = hindsight
        .recall(
            &query,
            HindsightRecallOptions {
                max_tokens: 1800,
                budget: None,
                include_chunks: false,
                max_chunk_tokens: 0,
                include_source_facts: true,
                max_source_facts_tokens: 1600,
                ..Default::default()
            },
        )
        .await;
    let recalled_memories = match recall {
        Ok(response) => {
            let memories = response
                .results
                .into_iter()
                .take(5)
                .map(|item| item.text)
                .collect::<Vec<_>>();
            tracing::debug!(
                "hindsight recall returned {} memory item(s) for phase={phase}",
                memories.len()
            );
            memories
        }
        Err(err) => {
            tracing::warn!("hindsight recall failed: {err:?}");
            Vec::new()
        }
    };

    PromptMemoryContext { recalled_memories }
}

fn build_hindsight_recall_query(
    objective: Option<&str>,
    phase: &str,
    thread_focus: Option<&str>,
    terminal_summary: Option<String>,
    recent_messages: Vec<String>,
) -> String {
    let mut lines = vec![
        "问题：召回最相关的历史经验，帮助继续推进当前目标。".to_string(),
        format!("阶段: {}", summarize_hindsight_query_value(phase, 48)),
    ];
    if let Some(objective) = objective.filter(|value| !value.trim().is_empty()) {
        lines.push(format!(
            "目标: {}",
            summarize_hindsight_query_value(objective, 120)
        ));
    }
    if let Some(thread_focus) = thread_focus.filter(|value| !value.trim().is_empty()) {
        lines.push(format!(
            "主线: {}",
            summarize_hindsight_query_value(thread_focus, 120)
        ));
    }

    let mut sections = Vec::new();
    if !recent_messages.is_empty() {
        sections.push((
            "近期".to_string(),
            recent_messages
                .into_iter()
                .map(|line| format!("- {}", summarize_hindsight_query_value(&line, 120)))
                .collect::<Vec<_>>(),
        ));
    }
    if let Some(terminal_summary) = terminal_summary.filter(|value| !value.trim().is_empty()) {
        sections.push((
            "终端".to_string(),
            terminal_summary
                .lines()
                .filter(|line| !line.trim().is_empty())
                .take(3)
                .map(|line| format!("- {}", summarize_hindsight_query_value(line, 120)))
                .collect::<Vec<_>>(),
        ));
    }

    let mut query = lines.join("\n");
    for (title, section_lines) in sections {
        if section_lines.is_empty() {
            continue;
        }
        let candidate = format!("{query}\n{title}:\n{}", section_lines.join("\n"));
        if approx_token_count(&candidate) > HINDSIGHT_RECALL_QUERY_MAX_TOKENS {
            continue;
        }
        query = candidate;
    }
    query
}

fn summarize_hindsight_query_value(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = compact.chars().count();
    if char_count <= max_chars {
        return compact;
    }
    let head = compact.chars().take(max_chars).collect::<String>();
    format!("{head}...")
}

fn summarize_terminal_for_hindsight(context: &Context) -> Option<String> {
    if context.devices.focused() != Some(crate::device::DeviceId::Terminal) {
        return None;
    }
    let (_, render) = context
        .devices
        .state_renders()
        .into_iter()
        .find(|(_, render)| render.is_focused)?;
    Some(render.lines.join("\n"))
}

fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.' | ':' | ' ') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

async fn spinova_loop(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepDashboardStatus,
) {
    let cycle_started_at = std::time::Instant::now();
    refresh_sleep_backlogs(sleep_status).await;
    let forced_sleep_status =
        maybe_start_forced_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await;
    enqueue_device_notice_work(context);
    sync_driver_frontier_from_sources(context);
    if context.active_runtime_turn {
        set_runtime_status(
            Some(tx),
            RuntimeStatusLevel::Info,
            "处理中：runtime turn 正在运行",
        );
        sync_dashboard_state(
            context,
            tx,
            sleep_status,
            Some(cycle_started_at.elapsed().as_millis()),
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
        return;
    }
    if context.memory.should_block_new_turns_on_retain_backlog() {
        let retain_backlog = context.memory.retain_backlog_count();
        set_runtime_status(
            Some(tx),
            RuntimeStatusLevel::Info,
            format!(
                "处理中：等待 hindsight retain 队列回落（{} turn）",
                retain_backlog
            ),
        );
        sync_dashboard_state(
            context,
            tx,
            sleep_status,
            Some(cycle_started_at.elapsed().as_millis()),
        );
        match context.hindsight_retain.flush().await {
            Ok(()) => context.memory.mark_queued_retained(),
            Err(err) => {
                tracing::error!("failed to flush hindsight retain queue before new turn: {err:?}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        return;
    }
    let pending_work_count = context.pending_work.pending_count();
    if pending_work_count == 0 {
        if context.idle_since.is_none() {
            context.idle_since = Some(std::time::Instant::now());
        }
        if let Some(status) =
            maybe_start_idle_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await
        {
            set_runtime_status(Some(tx), RuntimeStatusLevel::Info, status);
        } else if let Some(status) = forced_sleep_status {
            set_runtime_status(Some(tx), RuntimeStatusLevel::Info, status);
        } else {
            clear_runtime_status(Some(tx));
        }
        sync_dashboard_state(
            context,
            tx,
            sleep_status,
            Some(cycle_started_at.elapsed().as_millis()),
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        return;
    }
    context.idle_since = None;
    let mut status = format!("处理中：{} 个 pending work", pending_work_count);
    if let Some(forced_sleep_status) = forced_sleep_status.as_deref() {
        status.push_str(" | ");
        status.push_str(forced_sleep_status);
    }
    set_runtime_status(Some(tx), RuntimeStatusLevel::Info, status);
    context
        .devices
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    context.active_runtime_turn = true;
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
    execute_agent_loop_step(context, Some(tx)).await;
    context.active_runtime_turn = false;
    refresh_sleep_backlogs(sleep_status).await;
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
}

fn sync_driver_frontier_from_sources(context: &Context) {
    for (event_id, status) in context.events.driver_event_statuses() {
        let work = PendingWork::Event { event_id };
        if matches!(status, crate::events::EventStatus::Pending) {
            if let Err(err) = context.pending_work.enqueue(work) {
                tracing::error!("failed to sync pending event driver {event_id}: {err:?}");
            }
        } else if let Err(err) = context.pending_work.consume(work) {
            tracing::error!("failed to remove stale event driver {event_id}: {err:?}");
        }
    }
}

fn enqueue_device_notice_work(context: &mut Context) {
    let renders = context.devices.state_renders();
    for (device_id, render) in renders {
        let noticed = matches!(render.attention, crate::device::AttentionLevel::Notice);
        if noticed {
            if context.active_device_notices.insert(device_id) {
                let reason = summarize_device_notice_reason(device_id, &render);
                if let Err(err) = context.pending_work.enqueue(PendingWork::DeviceNotice {
                    device: device_id,
                    reason,
                }) {
                    tracing::error!(
                        "failed to enqueue device notice work for {device_id}: {err:?}"
                    );
                }
            }
        } else {
            context.active_device_notices.remove(&device_id);
        }
    }
}

fn summarize_device_notice_reason(
    device_id: DeviceId,
    render: &crate::device::DeviceStateRender,
) -> String {
    match device_id {
        DeviceId::Terminal => {
            let unread_sessions = numeric_field(&render.lines, "sessions_with_unread_output");
            if unread_sessions > 0 {
                format!("{unread_sessions} terminal session(s) have unread output")
            } else {
                "terminal requires attention".to_string()
            }
        }
    }
}

fn numeric_field(lines: &[String], key: &str) -> usize {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

async fn maybe_start_forced_sleep(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepDashboardStatus,
) -> Option<String> {
    if *sleep_running {
        return None;
    }
    let trace_backlog = sleep_status.unread_trace_backlog;
    let runtime_review_backlog = sleep_status.unread_runtime_review_backlog;
    if trace_backlog < FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD
        && runtime_review_backlog < FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD
    {
        return None;
    }
    let status = format!(
        "backlog 过高（traces={} runtime_reviews={}）：已启动后台 sleep",
        trace_backlog, runtime_review_backlog
    );
    start_background_sleep(
        context,
        tx,
        sleep_result_tx,
        sleep_running,
        sleep_status,
        SleepTrigger::Idle,
        &status,
    )
    .await;
    Some(format!(
        "backlog 过高（traces={} runtime_reviews={}）：后台 sleep 已启动",
        trace_backlog, runtime_review_backlog
    ))
}

async fn handle_dashboard_control_command(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepDashboardStatus,
    command: DashboardControlCommand,
) {
    match command {
        DashboardControlCommand::RunSleep => {
            if *sleep_running {
                set_runtime_status(Some(tx), RuntimeStatusLevel::Info, "sleep 已在后台运行");
                sync_dashboard_state(context, tx, sleep_status, None);
                return;
            }
            start_background_sleep(
                context,
                tx,
                sleep_result_tx,
                sleep_running,
                sleep_status,
                SleepTrigger::Manual,
                "正在后台执行 sleep",
            )
            .await;
        }
        DashboardControlCommand::ClearConversation => {
            let retain_plan = context.memory.clear_runtime_conversation().await;
            for job in retain_plan.jobs {
                if let Err(err) = context.hindsight_retain.enqueue(job) {
                    tracing::error!("failed to enqueue hindsight retain job during clear: {err:?}");
                }
            }
            if retain_plan.must_flush_before_continue || context.memory.retain_backlog_count() > 0 {
                match context.hindsight_retain.flush().await {
                    Ok(()) => context.memory.mark_queued_retained(),
                    Err(err) => {
                        tracing::error!("failed to flush hindsight retain queue during clear: {err:?}");
                    }
                }
            }
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                "已将当前会话转入 hindsight，并清空当前会话消息历史",
            );
            sync_dashboard_state(context, tx, sleep_status, None);
        }
    }
}

async fn maybe_start_idle_sleep(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepDashboardStatus,
) -> Option<String> {
    let Some(idle_since) = context.idle_since else {
        return None;
    };
    if idle_since.elapsed() < AUTO_SLEEP_IDLE_THRESHOLD {
        return None;
    }
    if context
        .last_idle_sleep_at
        .is_some_and(|last| last.elapsed() < AUTO_SLEEP_MIN_INTERVAL)
    {
        return None;
    }
    if *sleep_running {
        return Some("空闲中：后台 sleep 正在运行".to_string());
    }
    context.last_idle_sleep_at = Some(std::time::Instant::now());
    start_background_sleep(
        context,
        tx,
        sleep_result_tx,
        sleep_running,
        sleep_status,
        SleepTrigger::Idle,
        "空闲中：已启动后台 sleep",
    )
    .await;
    Some("空闲中：已启动后台 sleep".to_string())
}

async fn start_background_sleep(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepDashboardStatus,
    trigger: SleepTrigger,
    status: &str,
) {
    *sleep_running = true;
    sleep_status.running = true;
    sleep_status.current_trigger = Some(match trigger {
        SleepTrigger::Manual => "manual",
        SleepTrigger::Idle => "automatic",
    });
    set_runtime_status(Some(tx), RuntimeStatusLevel::Info, status.to_string());
    sync_dashboard_state(context, tx, sleep_status, None);
    let config = context.config.clone();
    let compiled_prompts = context.compiled_prompts.clone();
    let sleep_result_tx = sleep_result_tx.clone();
    tokio::spawn(async move {
        let mut sleep_context = build_eval_context_with_compiled(config, compiled_prompts).await;
        let result = run_sleep(&mut sleep_context).await;
        sleep_context.shutdown().await;
        let _ = sleep_result_tx.send(SleepTaskResult { trigger, result });
    });
}

async fn handle_sleep_task_result(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_status: &mut SleepDashboardStatus,
    result: SleepTaskResult,
) {
    sleep_status.running = false;
    sleep_status.current_trigger = None;
    match result.result {
        Ok(summary) => {
            if let Ok(store) = load_compiled_prompts_only(&context.config).await {
                context.compiled_prompts = store;
            }
            let prefix = match result.trigger {
                SleepTrigger::Manual => "sleep 完成",
                SleepTrigger::Idle => "后台 sleep 完成",
            };
            sleep_status.total_runs += 1;
            sleep_status.total_consumed_trace_events += summary.consumed_trace_events;
            sleep_status.total_consumed_runtime_reviews += summary.consumed_runtime_reviews;
            sleep_status.total_runtime_demos += summary.runtime_demos;
            sleep_status.total_turn_demos += summary.turn_demos;
            sleep_status.total_runtime_demo_evaluations += summary.runtime_demo_evaluations;
            sleep_status.total_turn_demo_evaluations += summary.turn_demo_evaluations;
            sleep_status.total_runtime_demo_passed += summary.runtime_demo_passed;
            sleep_status.total_runtime_demo_regressions += summary.runtime_demo_regressions;
            sleep_status.total_runtime_prompt_candidates += summary.runtime_prompt_candidates;
            if summary.runtime_prompt_rolled_back {
                sleep_status.total_runtime_prompt_rollbacks += 1;
            }
            if summary.runtime_prompt_accepted {
                sleep_status.total_runtime_prompt_accepts += 1;
            }
            let summary_text = summarize_sleep_summary(&summary);
            sleep_status.last_result = Some(summary_text.clone());
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                format!("{prefix}：{summary_text}"),
            );
        }
        Err(err) => {
            let prefix = match result.trigger {
                SleepTrigger::Manual => "sleep 失败",
                SleepTrigger::Idle => "后台 sleep 失败",
            };
            sleep_status.last_result = Some(err.to_string());
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Error,
                format!("{prefix}：{err}"),
            );
        }
    }
    refresh_sleep_backlogs(sleep_status).await;
    sync_dashboard_state(context, tx, sleep_status, None);
}

async fn execute_apply_patch_tool(
    context: &Context,
    patch_text: &str,
) -> miette::Result<ToolExecutionResult> {
    let summary = apply_patch_in_root(&context.execution_cwd, &context.sandbox_policy, patch_text)
        .await?;
    Ok(ToolExecutionResult::new(
        format!("patched {} file(s)", summary.changed_files),
        json!({
            "changed_files": summary.changed_files,
            "added_files": summary.added_files,
            "deleted_files": summary.deleted_files,
            "updated_files": summary.updated_files,
            "added_lines": summary.added_lines,
            "removed_lines": summary.removed_lines,
            "files": summary.files.iter().map(|file| {
                json!({
                    "path": file.path,
                    "operation": match file.operation {
                        PatchOperationKind::Add => "add",
                        PatchOperationKind::Delete => "delete",
                        PatchOperationKind::Update => "update",
                    },
                    "added_lines": file.added_lines,
                    "removed_lines": file.removed_lines,
                })
            }).collect::<Vec<_>>(),
        }),
        ToolUiEvent::patch(
            format!("patched {} file(s)", summary.changed_files),
            format!(
                "{} file(s) changed (+{} -{})",
                summary.changed_files, summary.added_lines, summary.removed_lines
            ),
            summary
                .files
                .iter()
                .cloned()
                .map(|file| crate::tool_ui::PatchFileUiData {
                    path: file.path,
                    operation: match file.operation {
                        PatchOperationKind::Add => "add".to_string(),
                        PatchOperationKind::Delete => "delete".to_string(),
                        PatchOperationKind::Update => "update".to_string(),
                    },
                    added_lines: file.added_lines,
                    removed_lines: file.removed_lines,
                })
                .collect(),
        ),
    ))
}

fn sync_dashboard_state(
    context: &Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_status: &SleepDashboardStatus,
    last_cycle_elapsed_ms: Option<u128>,
) {
    tx.send_modify(|state| {
        let device_renders = context.devices.state_renders();
        state.focused_device = context.devices.focused();
        state.status_output = render_status_command_output_for_dashboard(context, &device_renders);
        state.sleep_status_output = render_sleep_status_output_for_dashboard(context, sleep_status);
        state.inspect_telegram_output = render_telegram_status_for_dashboard(context);
        if state.activity_cells.is_empty() {
            state.activity_cells = render_activity_for_dashboard(context);
        }
        state.last_cycle_elapsed_ms = last_cycle_elapsed_ms;
        state.footer_context =
            render_dashboard_footer_context(context, state.footer_estimated_input_tokens);
    });
}

fn render_dashboard_footer_context(
    context: &Context,
    estimated_input_tokens: Option<usize>,
) -> String {
    let model = context
        .llm
        .model_name()
        .unwrap_or_else(|| context.config.main_model.model_name.clone());
    let focused_device = context
        .devices
        .focused()
        .map(|device| device.to_string())
        .unwrap_or_else(|| "none".to_string());
    let effective_window = context
        .config
        .main_model
        .effective_context_window_tokens()
        .max(1);
    let Some(info) = context.llm.token_usage_info() else {
        return format!(
            "{}",
            render_footer_context_with_usage(
                &model,
                estimated_input_tokens,
                effective_window,
                &focused_device
            )
        );
    };
    let used = usize::try_from(info.last_token_usage.input_tokens.max(0)).unwrap_or(0);
    let footer_usage = if used > 0 {
        Some((used, false))
    } else {
        estimated_input_tokens.map(|value| (value, true))
    };
    match footer_usage {
        Some((used, estimated)) => format!(
            "{model} · {}{}/{} used · {}",
            if estimated { "~" } else { "" },
            format_compact_tokens(used),
            format_compact_tokens(effective_window),
            focused_device
        ),
        None => format!(
            "{model} · {} window · {}",
            format_compact_tokens(effective_window),
            focused_device
        ),
    }
}

fn render_footer_context_with_usage(
    model: &str,
    estimated_input_tokens: Option<usize>,
    effective_window: usize,
    focused_device: &str,
) -> String {
    match estimated_input_tokens {
        Some(used) => format!(
            "{model} · ~{}/{} used · {}",
            format_compact_tokens(used),
            format_compact_tokens(effective_window),
            focused_device
        ),
        None => format!(
            "{model} · {} window · {}",
            format_compact_tokens(effective_window),
            focused_device
        ),
    }
}

fn format_compact_tokens(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        let major = tokens / 1_000_000;
        let minor = (tokens % 1_000_000) / 100_000;
        if minor == 0 {
            format!("{major}m")
        } else {
            format!("{major}.{minor}m")
        }
    } else if tokens >= 1_000 {
        let major = tokens / 1_000;
        let minor = (tokens % 1_000) / 100;
        if minor == 0 {
            format!("{major}k")
        } else {
            format!("{major}.{minor}k")
        }
    } else {
        tokens.to_string()
    }
}

async fn refresh_sleep_backlogs(sleep_status: &mut SleepDashboardStatus) {
    if let Ok(backlog) = unread_runtime_trace_count().await {
        sleep_status.unread_trace_backlog = backlog;
    }
    if let Ok(backlog) = unread_runtime_review_count().await {
        sleep_status.unread_runtime_review_backlog = backlog;
    }
}

fn render_sleep_status_output_for_dashboard(
    context: &Context,
    sleep_status: &SleepDashboardStatus,
) -> String {
    let mut sections = Vec::new();
    let state = if sleep_status.running {
        "running"
    } else {
        "idle"
    };
    let mut overview_lines = vec![format!("State: {state}")];
    if let Some(trigger) = sleep_status.current_trigger {
        overview_lines.push(format!("Trigger: {trigger}"));
    }
    if let Some(last_result) = sleep_status.last_result.as_deref() {
        overview_lines.push(format!("Last result: {last_result}"));
    }
    sections.push(format!("Overview\n{}", overview_lines.join("\n")));

    let totals_lines = vec![
        format!("• Total runs: {}", sleep_status.total_runs),
        format!(
            "• Total consumed trace events: {}",
            sleep_status.total_consumed_trace_events
        ),
        format!(
            "• Total consumed runtime reviews: {}",
            sleep_status.total_consumed_runtime_reviews
        ),
        format!(
            "• Total runtime demos: {}",
            sleep_status.total_runtime_demos
        ),
        format!("• Total turn demos: {}", sleep_status.total_turn_demos),
        format!(
            "• Total runtime demo evaluations: {}",
            sleep_status.total_runtime_demo_evaluations
        ),
        format!(
            "• Total turn demo evaluations: {}",
            sleep_status.total_turn_demo_evaluations
        ),
        format!(
            "• Total runtime demo passes: {}",
            sleep_status.total_runtime_demo_passed
        ),
        format!(
            "• Total runtime demo regressions: {}",
            sleep_status.total_runtime_demo_regressions
        ),
        format!(
            "• Total prompt candidates: {}",
            sleep_status.total_runtime_prompt_candidates
        ),
        format!(
            "• Total prompt accepts: {}",
            sleep_status.total_runtime_prompt_accepts
        ),
        format!(
            "• Total prompt rollbacks: {}",
            sleep_status.total_runtime_prompt_rollbacks
        ),
    ];
    sections.push(format!("Totals\n{}", totals_lines.join("\n")));

    let mut trigger_lines = vec![
        format!(
            "• Force backlog threshold: {} traces",
            FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD
        ),
        format!(
            "• Current trace backlog: {}",
            sleep_status.unread_trace_backlog
        ),
        format!(
            "• Current runtime review backlog: {}",
            sleep_status.unread_runtime_review_backlog
        ),
        format!(
            "• Auto sleep after idle: {}",
            format_duration(AUTO_SLEEP_IDLE_THRESHOLD)
        ),
        format!(
            "• Minimum idle sleep interval: {}",
            format_duration(AUTO_SLEEP_MIN_INTERVAL)
        ),
    ];
    match context.idle_since {
        Some(idle_since) => trigger_lines.push(format!(
            "• Currently idle for {}",
            format_duration(idle_since.elapsed())
        )),
        None => trigger_lines.push("• Currently not idle".to_string()),
    }
    if let Some(last_idle_sleep_at) = context.last_idle_sleep_at {
        trigger_lines.push(format!(
            "• Last idle sleep: {} ago",
            format_duration(last_idle_sleep_at.elapsed())
        ));
    }
    sections.push(format!("Triggers\n{}", trigger_lines.join("\n")));

    sections.join("\n\n")
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else if seconds >= 60 {
        let minutes = seconds / 60;
        let rem = seconds % 60;
        if rem == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {rem}s")
        }
    } else {
        format!("{seconds}s")
    }
}

fn render_status_command_output_for_dashboard(
    context: &Context,
    _: &[(DeviceId, crate::device::DeviceStateRender)],
) -> String {
    let mut sections = Vec::new();

    let focused = context
        .devices
        .focused()
        .map(|device| device.to_string())
        .unwrap_or_else(|| "none".to_string());
    let active_todos = context.todo_board.active_items().count();
    let active_events = context.pending_work.pending_count();
    let runtime_turn = if context.active_runtime_turn {
        "running"
    } else {
        "idle"
    };
    sections.push(format!(
        "Overview\nRuntime turn: {runtime_turn}\nFocused device: {focused}\nTodos: {active_todos}\nEvents: {active_events}"
    ));

    sections.push(format!(
        "Work\n{}",
        render_work_state_for_dashboard(context).unwrap_or_else(|| "No active work.".to_string())
    ));

    let usage_lines = render_status_usage_lines(context);
    sections.push(format!("Model usage\n{}", usage_lines.join("\n")));

    let todo_lines = render_status_todo_lines(context);
    sections.push(format!("Todos\n{}", todo_lines.join("\n")));

    sections.join("\n\n")
}

fn render_status_usage_lines(context: &Context) -> Vec<String> {
    let mut lines = Vec::new();
    for (label, llm) in [("main", &context.llm), ("judge", &context.judge_llm)] {
        let Some(info) = llm.token_usage_info() else {
            continue;
        };
        if info.total_token_usage.is_zero() {
            continue;
        }
        let model = llm.model_name().unwrap_or_else(|| "<unknown>".to_string());
        let context_window = info
            .model_context_window
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        lines.push(format!(
            "• {label}  model={model} total={} input={} output={} cached={} reasoning={} window={context_window}",
            info.total_token_usage.total_tokens,
            info.total_token_usage.input_tokens,
            info.total_token_usage.output_tokens,
            info.total_token_usage.cached_input_tokens,
            info.total_token_usage.reasoning_output_tokens,
        ));
        lines.push(format!(
            "  last={} input={} output={}",
            info.last_token_usage.total_tokens,
            info.last_token_usage.input_tokens,
            info.last_token_usage.output_tokens,
        ));
    }
    if lines.is_empty() {
        vec!["No token usage recorded yet.".to_string()]
    } else {
        lines
    }
}

fn render_status_todo_lines(context: &Context) -> Vec<String> {
    let mut items = context.todo_board.active_items().collect::<Vec<_>>();
    items.sort_by_key(|(id, _)| id.to_string());
    if items.is_empty() {
        return vec!["No active todos.".to_string()];
    }
    items
        .into_iter()
        .take(6)
        .map(|(_, item)| format!("• {}  [{} / {}]", item.title, item.status, item.origin))
        .collect()
}

fn render_telegram_status_for_dashboard(context: &Context) -> String {
    let chats = context.telegram.chat_summaries_view();
    let queued_outbound = chats
        .iter()
        .map(|chat| chat.pending_outbound_count)
        .sum::<usize>();

    let mut lines = vec![
        "Telegram".to_string(),
        "Role: transport / adapter".to_string(),
        format!("Known chats: {}", chats.len()),
        format!("Queued outbound: {queued_outbound}"),
    ];

    if chats.is_empty() {
        lines.push(String::new());
        lines.push("No chats.".to_string());
        return lines.join("\n");
    }

    lines.push(String::new());
    lines.push("Chats".to_string());
    lines.extend(chats.iter().take(8).map(|chat| {
        let mut flags = Vec::new();
        if chat.pending_outbound_count > 0 {
            flags.push(format!("{} queued", chat.pending_outbound_count));
        }
        let suffix = if flags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", flags.join(", "))
        };
        format!("• {} ({}){}", chat.title, chat.chat_id, suffix)
    }));

    lines.join("\n")
}

fn render_activity_for_dashboard(context: &Context) -> Vec<crate::dashboard::ActivityCell> {
    render_activity_from_messages(context.memory.runtime_conversation_messages())
}

fn render_work_state_for_dashboard(context: &Context) -> Option<String> {
    let objective = context.work_state.objective()?;
    let objective = truncate_from_left(objective, 56);
    let last_touched = format_last_touched(context.work_state.last_touched_at_ms);
    let mut lines = vec![objective, format!("上次处理: {last_touched}")];
    if let Some(item_id) = context.work_state.item_id {
        let item_title = context
            .todo_board
            .items()
            .find(|(id, _)| *id == item_id)
            .map(|(_, item)| item.title.clone())
            .unwrap_or_else(|| item_id.to_string());
        lines.push(format!("Todo: {}", truncate_from_left(&item_title, 24)));
    }
    if let Some(phase) = context.work_state.work_phase() {
        lines.push(format!("阶段: {phase}"));
    }
    Some(lines.join("\n"))
}

fn format_last_touched(last_touched_at_ms: Option<i64>) -> String {
    let Some(timestamp_ms) = last_touched_at_ms else {
        return "未处理".to_string();
    };

    let Some(datetime) = Local.timestamp_millis_opt(timestamp_ms).single() else {
        return "时间无效".to_string();
    };

    let now = Local::now();
    if now.date_naive() == datetime.date_naive() {
        datetime.format("%H:%M:%S").to_string()
    } else {
        datetime.format("%m-%d %H:%M").to_string()
    }
}

fn truncate_from_left(text: &str, max_chars: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return text.to_string();
    }

    let tail = chars[chars.len().saturating_sub(max_chars - 1)..]
        .iter()
        .collect::<String>();
    format!("…{tail}")
}

pub async fn get_spinova_home() -> PathBuf {
    spinova_paths().await.root().to_path_buf()
}
