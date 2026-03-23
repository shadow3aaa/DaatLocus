mod apply_patch;
mod config;
mod context;
mod core;
mod dashboard;
mod device;
mod emotion;
mod hindsight;
mod memory;
mod obligations;
mod projects;
mod providers;
mod reasoning;
mod runtime_tools;
mod snapshot;
mod system_info;
mod telegram_acl;
mod telegram_device;
mod telegram_transport;
mod terminal_device;
mod terminal_process;
mod tool_ui;
mod work_state;

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    apply_patch::{PatchOperationKind, apply_patch_in_root},
    config::load_config,
    context::Context,
    core::TelegramResolution,
    dashboard::{
        DashboardActivityEvent, DashboardControlCommand, DashboardState, apply_activity_event,
        render_activity_from_messages, run_tui_dashboard,
    },
    device::{DeviceId, DeviceManager},
    emotion::Emotion,
    hindsight::{HindsightClient, HindsightRecallOptions, HindsightReflectOptions},
    memory::Memory,
    obligations::Obligations,
    projects::{ProjectStatus, Projects},
    providers::OpenAIClient,
    reasoning::{
        adapters::swe_train_source::SweTrainSource,
        compiled::{
            COMPILED_DIR_NAME, CompiledPromptStore, load_all_compiled_programs,
            load_compiled_runtime_system_prompt,
        },
        environment::EpisodeObservation,
        episode::{
            EpisodeActionRecord, EpisodeMetric, EpisodeOutcome, EpisodeStatus, EpisodeStep,
            EpisodeTask,
        },
        episode_harness::EpisodeHarness,
        programs::completion_judge::{CompletionJudgeOutput, CompletionJudgeProgram},
        programs::task_understanding::{TaskUnderstandingOutput, TaskUnderstandingProgram},
        prompts::{SYSTEM_PROMPT_KERNEL, build_device_context_prompt},
        render::openai_tools::OpenAIToolRenderer,
        runtime::{
            AgentMessage, AgentTurnRequest, AgentTurnResponse, PromptMemoryContext, PromptMessage,
            PromptRole, execute_program,
        },
        sleep::run_sleep,
        sleep_artifacts::SleepArtifactSuggestedFixKind,
        trace::unread_runtime_trace_count,
    },
    runtime_tools::{
        ToolExecutionResult, build_runtime_tool_specs, execute_agent_tool_call,
        render_tool_call_ui_event, summarize_primary_action_from_tool_call,
    },
    snapshot::Snapshot,
    telegram_acl::TelegramAclHandle,
    telegram_device::TelegramDevice,
    telegram_transport::TelegramTransport,
    terminal_device::TerminalDevice,
    tool_ui::{ToolCallUiEvent, ToolUiEvent, compact_body_lines},
    work_state::WorkState,
};
use chrono::{Local, TimeZone};
use miette::{Result, miette};
use serde_json::json;

const AUTO_SLEEP_IDLE_THRESHOLD: Duration = Duration::from_secs(300);
const AUTO_SLEEP_MIN_INTERVAL: Duration = Duration::from_secs(300);
const FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD: usize = 128;

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
}

struct SleepTaskResult {
    trigger: SleepTrigger,
    result: Result<crate::reasoning::sleep::SleepSummary>,
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();

    if let Some(path) = train_source_inspect_path(&args) {
        match run_train_source_inspect_blocking(path) {
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

    if let Err(err) = runtime.block_on(async_main(args)) {
        eprintln!("{err:?}");
        std::process::exit(1);
    }
}

async fn async_main(args: Vec<String>) -> Result<()> {
    if is_mem_reset_command(&args) {
        run_mem_reset().await?;
        return Ok(());
    }

    if is_prompt_reset_command(&args) {
        run_prompt_reset().await?;
        return Ok(());
    }

    let config = match load_config().await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    if is_sleep_command(&args) {
        let mut context = build_eval_context(config).await;
        match run_sleep(&mut context).await {
            Ok(summary) => {
                print_sleep_summary(&summary);
                context.shutdown().await;
                return Ok(());
            }
            Err(err) => {
                eprintln!("{err:?}");
                context.shutdown().await;
                std::process::exit(1);
            }
        }
    }

    if let Some((path, limit, batch_size)) = train_source_learn_args(&args) {
        match run_train_source_learn(config, path, limit, batch_size).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("{err:?}");
                std::process::exit(1);
            }
        }
    }

    if let Some((path, task_index)) = train_source_rollout_args(&args) {
        match run_train_source_rollout(config, path, task_index).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("{err:?}");
                std::process::exit(1);
            }
        }
    }

    eprintln!("[prompt-compile] loading compiled prompts before dashboard startup...");
    let compiled_prompts = match load_compiled_prompts_only().await {
        Ok(store) => store,
        Err(err) => {
            eprintln!("{err:?}");
            std::process::exit(1);
        }
    };
    if compiled_prompts.is_empty() {
        eprintln!("[prompt-compile] no compiled prompts found; running with baseline prompts");
    } else {
        eprintln!(
            "[prompt-compile] loaded {} compiled prompt suites",
            compiled_prompts.len()
        );
    }

    let memory = Memory::new().await;
    let obligations = Obligations::new().await;
    let projects = Projects::new().await;
    let work_state = WorkState::new().await;
    let emotion = Emotion::new().await;
    let telegram_acl = TelegramAclHandle::load().await;
    let terminal = TerminalDevice::new();
    let telegram = TelegramDevice::new();
    let telegram_handle = telegram.handle();
    bootstrap_telegram_device_from_acl(&telegram_handle, &telegram_acl);
    let devices = DeviceManager::new(
        Some(DeviceId::Terminal),
        vec![Box::new(terminal), Box::new(telegram)],
    )
    .await
    .unwrap();
    let telegram_transport = if config.telegram.enabled && config.telegram.has_real_credentials() {
        Some(tokio::spawn(
            TelegramTransport::new(
                config.telegram.clone(),
                telegram_handle.clone(),
                telegram_acl.clone(),
            )
            .run(),
        ))
    } else {
        None
    };
    let judge_model = config.judge.resolved_model(&config.main_model);
    let client = OpenAIClient::new(&config);
    let judge_client = OpenAIClient::from_model_config(&judge_model);
    let hindsight = HindsightClient::from_config(&config.hindsight)?;
    let hindsight_retain = hindsight.spawn_retain_worker();
    let mut context = Context {
        llm: Box::new(client),
        judge_llm: Box::new(judge_client),
        config,
        hindsight,
        hindsight_retain,
        memory,
        prompt_memory: PromptMemoryContext::default(),
        obligations,
        projects,
        work_state,
        emotion,
        devices,
        telegram: telegram_handle,
        compiled_prompts,
        dashboard_tx: None,
        idle_since: None,
        last_idle_sleep_at: None,
    };
    let device_renders = context.devices.state_renders();

    let (tx, mut rx) = tokio::sync::watch::channel(DashboardState {
        focused_device: context.devices.focused(),
        status_output: render_status_command_output_for_dashboard(&context, &device_renders),
        sleep_status_output: render_sleep_status_output_for_dashboard(
            &context,
            &SleepDashboardStatus::default(),
        ),
        inspect_telegram_output: render_inspect_output_for_dashboard(
            &context,
            &device_renders,
            DeviceId::Telegram,
        ),
        activity_cells: render_activity_for_dashboard(&context),
        live_activity_cells: Vec::new(),
        last_cycle_elapsed_ms: None,
        runtime_status: None,
    });
    context.dashboard_tx = Some(tx.clone());
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    let (dashboard_control_tx, mut dashboard_control_rx) =
        tokio::sync::mpsc::unbounded_channel::<DashboardControlCommand>();
    let (sleep_result_tx, mut sleep_result_rx) =
        tokio::sync::mpsc::unbounded_channel::<SleepTaskResult>();

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

fn is_mem_reset_command(args: &[String]) -> bool {
    matches!(args, [command] if command == "mem-reset")
}

fn is_prompt_reset_command(args: &[String]) -> bool {
    matches!(args, [command] if command == "prompt-reset")
        || matches!(args, [command] if command == "compile-reset")
}

fn is_sleep_command(args: &[String]) -> bool {
    matches!(args, [command] if command == "sleep")
}

fn train_source_inspect_path(args: &[String]) -> Option<&str> {
    match args {
        [command, subcommand, path] if command == "train-source" && subcommand == "inspect" => {
            Some(path.as_str())
        }
        [command, path] if command == "inspect-train-source" => Some(path.as_str()),
        _ => None,
    }
}

fn train_source_rollout_args(args: &[String]) -> Option<(&str, usize)> {
    match args {
        [command, subcommand, path] if command == "train-source" && subcommand == "rollout" => {
            Some((path.as_str(), 0))
        }
        [command, subcommand, path, index]
            if command == "train-source" && subcommand == "rollout" =>
        {
            let index = index.parse::<usize>().ok()?;
            Some((path.as_str(), index))
        }
        [command, path] if command == "rollout-train-source" => Some((path.as_str(), 0)),
        [command, path, index] if command == "rollout-train-source" => {
            let index = index.parse::<usize>().ok()?;
            Some((path.as_str(), index))
        }
        _ => None,
    }
}

fn train_source_learn_args(args: &[String]) -> Option<(&str, usize, usize)> {
    match args {
        [command, subcommand, path] if command == "train-source" && subcommand == "learn" => {
            Some((path.as_str(), 20, 5))
        }
        [command, subcommand, path, limit]
            if command == "train-source" && subcommand == "learn" =>
        {
            let limit = limit.parse::<usize>().ok()?;
            Some((path.as_str(), limit, 5))
        }
        [command, subcommand, path, limit, batch_size]
            if command == "train-source" && subcommand == "learn" =>
        {
            let limit = limit.parse::<usize>().ok()?;
            let batch_size = batch_size.parse::<usize>().ok()?;
            Some((path.as_str(), limit, batch_size))
        }
        [command, path] if command == "learn-train-source" => Some((path.as_str(), 20, 5)),
        [command, path, limit] if command == "learn-train-source" => {
            let limit = limit.parse::<usize>().ok()?;
            Some((path.as_str(), limit, 5))
        }
        [command, path, limit, batch_size] if command == "learn-train-source" => {
            let limit = limit.parse::<usize>().ok()?;
            let batch_size = batch_size.parse::<usize>().ok()?;
            Some((path.as_str(), limit, batch_size))
        }
        _ => None,
    }
}

async fn run_mem_reset() -> Result<()> {
    let home = get_spinova_home().await;
    let config = load_config()
        .await
        .map_err(|err| miette!("failed to load config for mem-reset: {err}"))?;
    let hindsight = HindsightClient::from_config(&config.hindsight)?;
    hindsight.delete_bank().await?;
    let judge_model = config.judge.resolved_model(&config.main_model);
    let telegram = TelegramDevice::empty();
    let telegram_handle = telegram.handle();
    let devices = DeviceManager::new(None, vec![Box::new(telegram)])
        .await
        .map_err(|err| miette!("failed to construct default devices for mem-reset: {err}"))?;
    let context = Context {
        llm: Box::new(OpenAIClient::new(&config)),
        judge_llm: Box::new(OpenAIClient::from_model_config(&judge_model)),
        config,
        hindsight: hindsight.clone(),
        hindsight_retain: hindsight.spawn_retain_worker(),
        memory: Memory::empty().await,
        prompt_memory: PromptMemoryContext::default(),
        obligations: Obligations::default(),
        projects: Projects::default(),
        work_state: WorkState::default(),
        emotion: Emotion::default(),
        devices,
        telegram: telegram_handle,
        compiled_prompts: CompiledPromptStore::empty(),
        dashboard_tx: None,
        idle_since: None,
        last_idle_sleep_at: None,
    };
    context.shutdown().await;

    let trace_path = home.join("reasoning_traces.jsonl");
    if trace_path.exists() {
        tokio::fs::remove_file(&trace_path)
            .await
            .map_err(|err| miette!("failed to remove {}: {err}", trace_path.display()))?;
    }

    println!(
        "[mem-reset] reset persistent runtime state under {}",
        home.display()
    );
    println!(
        "[mem-reset] cleared via empty context shutdown: l1_memory, projects, obligations, work_state, emotion"
    );
    println!("[mem-reset] cleared: reasoning_traces.jsonl");
    println!("[mem-reset] cleared: hindsight bank");
    println!("[mem-reset] preserved: config.toml, reasoning_compiled/, telegram_acl.json");

    Ok(())
}

async fn run_prompt_reset() -> Result<()> {
    let home = get_spinova_home().await;
    let cleared = clear_prompt_cache_dirs(&home).await?;

    println!(
        "[prompt-reset] cleared prompt compile cache under {}",
        home.display()
    );
    if cleared.is_empty() {
        println!(
            "[prompt-reset] nothing to remove; {} was already absent",
            COMPILED_DIR_NAME
        );
    } else {
        println!("[prompt-reset] cleared: {}", cleared.join(", "));
    }
    println!(
        "[prompt-reset] preserved: config.toml, telegram_acl.json, reasoning_traces.jsonl, runtime memory state"
    );

    Ok(())
}

async fn clear_prompt_cache_dirs(home: &PathBuf) -> Result<Vec<String>> {
    let mut cleared = Vec::new();

    for dir_name in [COMPILED_DIR_NAME] {
        let path = home.join(dir_name);
        if path.exists() {
            tokio::fs::remove_dir_all(&path)
                .await
                .map_err(|err| miette!("failed to remove {}: {err}", path.display()))?;
            cleared.push(dir_name.to_string());
        }
    }

    Ok(cleared)
}

async fn build_eval_context(config: crate::config::Config) -> Context {
    build_eval_context_with_compiled(config, CompiledPromptStore::empty()).await
}

async fn build_eval_context_with_compiled(
    config: crate::config::Config,
    compiled_prompts: CompiledPromptStore,
) -> Context {
    let memory = Memory::new().await;
    let obligations = Obligations::new().await;
    let projects = Projects::new().await;
    let work_state = WorkState::new().await;
    let emotion = Emotion::new().await;
    let terminal = TerminalDevice::new();
    let telegram = TelegramDevice::new();
    let telegram_handle = telegram.handle();
    let devices = DeviceManager::new(
        Some(DeviceId::Terminal),
        vec![Box::new(terminal), Box::new(telegram)],
    )
    .await
    .unwrap();
    let judge_model = config.judge.resolved_model(&config.main_model);
    let client = OpenAIClient::new(&config);
    let judge_client = OpenAIClient::from_model_config(&judge_model);
    let hindsight = HindsightClient::from_config(&config.hindsight)
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
        obligations,
        projects,
        work_state,
        emotion,
        devices,
        telegram: telegram_handle,
        compiled_prompts,
        dashboard_tx: None,
        idle_since: None,
        last_idle_sleep_at: None,
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

async fn load_compiled_prompts_only() -> miette::Result<CompiledPromptStore> {
    let compiled = load_all_compiled_programs().await?;
    let runtime_system_prompt = load_compiled_runtime_system_prompt().await?;
    Ok(CompiledPromptStore::from_entries(compiled).with_runtime_system_prompt(runtime_system_prompt))
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
    let episode_home = episode_root.join("spinova_home");
    let workspace_dir = episode_root.join("workspace");
    let home_override = SpinovaHomeOverride::set(episode_home.clone());
    provision_episode_workspace(&task, &workspace_dir).await?;
    task.workspace_hint = Some(workspace_dir.display().to_string());

    let mut context = build_eval_context(config).await;
    context.devices.focus(DeviceId::Terminal).await?;
    enter_episode_workspace(&mut context, &workspace_dir).await?;
    context
        .work_state
        .set_objective(task.instruction.clone(), None);

    let outcome = rollout_agent_loop_episode(&mut context, task, &workspace_dir).await?;
    print_episode_rollout(&outcome, &episode_home);
    context.shutdown().await;
    drop(home_override);
    Ok(())
}

async fn run_train_source_learn(
    config: crate::config::Config,
    path: &str,
    limit: usize,
    batch_size: usize,
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

    println!(
        "train source learn: path={} total_tasks={} batch_size={} session={} learning_home={} shared_long_term_home={}",
        path,
        tasks.len(),
        batch_size,
        session.session_root().display(),
        session_learning_home.display(),
        shared_learning_home.display()
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
        ) => result,
        _ = tokio::signal::ctrl_c() => {
            session.shutdown(true).await?;
            return Ok(());
        }
    };
    drop(home_override);

    session.shutdown(false).await?;
    run_result
}

async fn run_train_source_learn_loop(
    config: crate::config::Config,
    tasks: Vec<EpisodeTask>,
    batch_size: usize,
    shared_learning_home: PathBuf,
    session_learning_home: PathBuf,
    session: TrainSourceLearnSession,
) -> Result<()> {
    let mut cursor = 0usize;
    let mut batch_index = 0usize;
    while cursor < tasks.len() {
        let batch_end = (cursor + batch_size).min(tasks.len());
        let batch_tasks = &tasks[cursor..batch_end];
        println!(
            "  batch {} running (tasks {}..{}, count={})",
            batch_index + 1,
            cursor + 1,
            batch_end,
            batch_tasks.len()
        );
        let compiled_prompts = load_compiled_prompts_only().await?;
        let active_variant = if compiled_prompts.is_empty() {
            "baseline".to_string()
        } else {
            "compiled".to_string()
        };
        println!(
            "  batch {} active_variant={} (tasks {}..{})",
            batch_index + 1,
            active_variant,
            cursor + 1,
            batch_end
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
            println!(
                "- learn task {}/{} id={} status={:?} score={:.2}",
                snapshot.completed_tasks,
                snapshot.total_tasks,
                outcome.task.id,
                outcome.status,
                outcome.metric.score
            );
        }

        let completed_tasks = session.snapshot().await.completed_tasks;
        println!(
            "  batch {} sleep starting (completed_tasks={})",
            batch_index + 1,
            completed_tasks
        );
        let mut optimize_context =
            build_eval_context_with_compiled(config.clone(), load_compiled_prompts_only().await?)
                .await;
        let sleep_summary = run_sleep(&mut optimize_context).await?;
        let current_compiled_count = load_compiled_prompts_only().await?.len();
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
                    sleep_runtime_prompt_suggestions: sleep_summary.runtime_prompt_suggestions,
                    sleep_runtime_prompt_candidates: sleep_summary.runtime_prompt_candidates,
                    sleep_runtime_demo_evaluations: sleep_summary.runtime_demo_evaluations,
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
        println!(
            "  batch {} sleep finished: patterns={} demos={} stress={} instructions={} runtime_demos={} runtime_prompt_suggestions={} runtime_prompt_candidates={} runtime_demo_evals={} runtime_demo_passed={} runtime_demo_regressions={} rolled_back={} rounds={} accepted={} reflections={}",
            batch_index + 1,
            sleep_summary.failure_patterns.len(),
            sleep_summary.bootstrap_demos,
            sleep_summary.stress_cases,
            sleep_summary.instruction_hypotheses,
            sleep_summary.runtime_demos,
            sleep_summary.runtime_prompt_suggestions,
            sleep_summary.runtime_prompt_candidates,
            sleep_summary.runtime_demo_evaluations,
            sleep_summary.runtime_demo_passed,
            sleep_summary.runtime_demo_regressions,
            sleep_summary.runtime_prompt_rolled_back,
            sleep_summary.runtime_prompt_evolution_rounds,
            sleep_summary.runtime_prompt_accepted,
            sleep_summary.retained_reflections
        );
        sync_learning_assets_back_to_shared(&session_learning_home, &shared_learning_home).await?;

        optimize_context.shutdown().await;
        sync_learning_assets_back_to_shared(&session_learning_home, &shared_learning_home).await?;

        let compiled_prompt_count = load_compiled_prompts_only().await?.len();
        session
            .update(|state| {
                state.last_compiled_prompt_count = compiled_prompt_count;
                if let Some(last_report) = state.batch_reports.last_mut() {
                    last_report.compiled_prompt_count = compiled_prompt_count;
                    last_report.optimized_suites = 0;
                }
            })
            .await?;
        println!(
            "  batch {} compiled_prompt_count={}",
            batch_index + 1,
            compiled_prompt_count
        );

        println!(
            "  batch update: completed={} active_variant={} sleep_patterns={} demos={} stress={} instructions={} runtime_demos={} runtime_prompt_suggestions={} runtime_prompt_candidates={} runtime_demo_evals={} runtime_demo_passed={} runtime_demo_regressions={} rolled_back={} rounds={} accepted={} reflections={} compiled_suites={}",
            session.snapshot().await.completed_tasks,
            active_variant,
            sleep_summary.failure_patterns.len(),
            sleep_summary.bootstrap_demos,
            sleep_summary.stress_cases,
            sleep_summary.instruction_hypotheses,
            sleep_summary.runtime_demos,
            sleep_summary.runtime_prompt_suggestions,
            sleep_summary.runtime_prompt_candidates,
            sleep_summary.runtime_demo_evaluations,
            sleep_summary.runtime_demo_passed,
            sleep_summary.runtime_demo_regressions,
            sleep_summary.runtime_prompt_rolled_back,
            sleep_summary.runtime_prompt_evolution_rounds,
            sleep_summary.runtime_prompt_accepted,
            sleep_summary.retained_reflections,
            compiled_prompt_count
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
) -> Result<EpisodeOutcome> {
    let episode_home = episode_root.join("spinova_home");
    let workspace_dir = episode_root.join("workspace");
    let home_override = (!use_shared_learning_home).then(|| SpinovaHomeOverride::set(episode_home));
    println!(
        "        episode setup: id={} workspace={} home_mode={}",
        task.id,
        workspace_dir.display(),
        if use_shared_learning_home {
            "shared"
        } else {
            "isolated"
        }
    );
    provision_episode_workspace(task, &workspace_dir).await?;

    let mut run_task = task.clone();
    run_task.workspace_hint = Some(workspace_dir.display().to_string());
    let mut context = build_eval_context_with_compiled(config.clone(), compiled_prompts).await;
    context.devices.focus(DeviceId::Terminal).await?;
    enter_episode_workspace(&mut context, &workspace_dir).await?;
    context
        .work_state
        .set_objective(run_task.instruction.clone(), None);

    let outcome = rollout_agent_loop_episode(&mut context, run_task, &workspace_dir).await?;
    save_episode_outcome(episode_root, &outcome).await?;
    context.shutdown().await;
    drop(home_override);
    Ok(outcome)
}

fn print_sleep_summary(summary: &crate::reasoning::sleep::SleepSummary) {
    println!(
        "sleep: derived {} failure patterns, {} bootstrap demos, {} stress cases, {} instruction hypotheses, {} runtime demos, {} runtime prompt suggestions, {} runtime prompt candidates, {} runtime demo evaluations (passed {}, regressed {}, rolled_back {}, rounds {}, accepted {}), retained {} hindsight reflections",
        summary.failure_patterns.len(),
        summary.bootstrap_demos,
        summary.stress_cases,
        summary.instruction_hypotheses,
        summary.runtime_demos,
        summary.runtime_prompt_suggestions,
        summary.runtime_prompt_candidates,
        summary.runtime_demo_evaluations,
        summary.runtime_demo_passed,
        summary.runtime_demo_regressions,
        summary.runtime_prompt_rolled_back,
        summary.runtime_prompt_evolution_rounds,
        summary.runtime_prompt_accepted,
        summary.retained_reflections
    );
    for pattern in &summary.failure_patterns {
        let kind = match pattern.suggested_fix_kind {
            SleepArtifactSuggestedFixKind::Demo => "demo",
            SleepArtifactSuggestedFixKind::Instruction => "instruction",
            SleepArtifactSuggestedFixKind::StressCase => "stress_case",
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
        "sleep 完成：runtime demos {}，评估 {}（通过 {}，退化 {}），候选 {}，轮次 {}，accepted={}",
        summary.runtime_demos,
        summary.runtime_demo_evaluations,
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

    for index in 0..task.max_steps {
        context.work_state.set_phase(work_phase.clone());
        let step_execution = execute_agent_loop_step(context, None).await;
        let output = step_execution.output;
        let action = output.primary_action.clone();

        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("description".to_string(), output.description.clone());
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

        if matches!(work_phase.as_str(), "finish") {
            return finalize_runtime_episode(
                context,
                task,
                workspace_dir,
                initial_observation,
                steps,
                Some(EpisodeStatus::Succeeded),
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
        run_validation_commands(&task.validation_commands, workspace_dir).await?;
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

fn print_episode_rollout(outcome: &EpisodeOutcome, episode_home: &PathBuf) {
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

fn print_train_source_learn_summary(session_root: &Path, state: &TrainSourceLearnState) {
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
    println!(
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
    sleep_runtime_prompt_suggestions: usize,
    sleep_runtime_prompt_candidates: usize,
    sleep_runtime_demo_evaluations: usize,
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

    async fn shutdown(&self, interrupted: bool) -> Result<()> {
        self.save().await?;
        let state = self.snapshot().await;
        if interrupted {
            println!(
                "train source learn interrupted: session state saved to {}",
                self.session_root.display()
            );
        }
        print_train_source_learn_summary(&self.session_root, &state);
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
) -> Result<Vec<ValidationCommandResult>> {
    let mut results = Vec::new();
    for command in commands {
        println!("        validation command: {}", command);
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

struct SpinovaHomeOverride {
    previous: Option<String>,
}

impl SpinovaHomeOverride {
    fn set(path: PathBuf) -> Self {
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
    let path = env::current_dir()
        .map_err(|err| miette!("failed to get current dir for episode home: {err}"))?
        .join("tmp")
        .join("episode_envs")
        .join(slugify(&task.id))
        .join(slugify(variant_name));

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
        slugify(path).chars().take(24).collect::<String>()
    );
    let root = env::current_dir()
        .map_err(|err| miette!("failed to get current dir for learn session: {err}"))?
        .join("tmp")
        .join("train_source_learn")
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
    let root = session_root.join("learning_home");
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
    for name in [COMPILED_DIR_NAME, "sleep_artifacts"] {
        sync_path_replace(&shared_home.join(name), &session_home.join(name)).await?;
    }
    Ok(())
}

async fn sync_learning_assets_back_to_shared(
    session_home: &Path,
    shared_home: &Path,
) -> Result<()> {
    for name in [COMPILED_DIR_NAME, "sleep_artifacts"] {
        sync_path_replace(&session_home.join(name), &shared_home.join(name)).await?;
    }
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
    let root = session_root
        .join("episodes")
        .join(format!("{:04}-{}", index, slugify(&task.id)));
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

async fn provision_episode_workspace(task: &EpisodeTask, workspace_dir: &Path) -> Result<()> {
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
        ensure_cached_repo(repo, &remote, &cache_repo).await?;
        println!(
            "          clone repo={} from local cache {}",
            repo,
            cache_repo.display()
        );
        run_host_command(
            &[
                "git",
                "clone",
                "--shared",
                cache_repo.to_string_lossy().as_ref(),
                workspace_dir.to_string_lossy().as_ref(),
            ],
            None,
        )
        .await?;

        if let Some(base_commit) = task.metadata.get("base_commit") {
            println!("          checkout base_commit={}", base_commit);
            run_host_command(
                &["git", "checkout", base_commit.as_str()],
                Some(workspace_dir),
            )
            .await?;
        }
    }

    for command in &task.setup_commands {
        println!("          setup command: {}", command);
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

async fn ensure_cached_repo(repo: &str, remote: &str, cache_repo: &Path) -> Result<()> {
    if cache_repo.exists() {
        println!(
            "          repo cache hit for {} at {}",
            repo,
            cache_repo.display()
        );
        return Ok(());
    }

    println!(
        "          repo cache miss for {} -> {}",
        repo,
        cache_repo.display()
    );
    run_host_command(
        &[
            "git",
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
    let cd_command = if cfg!(windows) {
        format!("Set-Location \"{}\"\r", workspace_dir.display())
    } else {
        format!("cd \"{}\"\n", workspace_dir.display())
    };
    context
        .devices
        .send_terminal_input(cd_command)
        .await
        .map_err(|err| miette!("failed to enter episode workspace in terminal: {err}"))?;
    context
        .devices
        .wait_until_settled(Duration::from_millis(300), Duration::from_secs(2))
        .await;
    Ok(())
}

struct AgentLoopStepExecution {
    output: AgentLoopStepOutput,
    snapshot_text: String,
}

struct AgentLoopStepOutput {
    observation: String,
    description: String,
    current_doing: String,
    primary_action: EpisodeActionRecord,
}

fn prompt_message_to_agent_message(message: PromptMessage) -> AgentMessage {
    match message.role {
        PromptRole::System => AgentMessage::system(message.content),
        PromptRole::User => AgentMessage::user(message.content),
        PromptRole::Assistant => AgentMessage::assistant(message.content),
        PromptRole::Tool => {
            AgentMessage::tool("historical-tool", "historical_tool", message.content)
        }
    }
}

fn build_runtime_agent_messages(context: &Context, snapshot_text: &str) -> Vec<AgentMessage> {
    let mut messages = vec![
        AgentMessage::system(SYSTEM_PROMPT_KERNEL),
        AgentMessage::system(crate::reasoning::prompts::TOOL_ACTION_PROMPT),
    ];
    messages.extend(
        context
            .compiled_prompts
            .runtime_system_additions()
            .iter()
            .filter(|line| !line.trim().is_empty())
            .cloned()
            .map(AgentMessage::system),
    );
    messages.push(AgentMessage::system(build_device_context_prompt(context)));
    if !context.prompt_memory.recalled_memories.is_empty() {
        messages.push(AgentMessage::system(format!(
            "相关长期记忆：\n{}",
            context.prompt_memory.recalled_memories.join("\n")
        )));
    }
    if let Some(reflection) = &context.prompt_memory.reflected_strategy {
        messages.push(AgentMessage::system(format!(
            "相关长期反思：\n{reflection}"
        )));
    }
    messages.extend(
        context
            .memory
            .prompt_messages()
            .into_iter()
            .map(prompt_message_to_agent_message),
    );
    messages.push(AgentMessage::user(render_world_snapshot_fragment(snapshot_text)));
    messages
}

fn render_world_snapshot_fragment(snapshot_text: &str) -> String {
    format!("<world_snapshot>\n{snapshot_text}\n</world_snapshot>")
}

async fn record_runtime_history_messages(
    context: &mut Context,
    current_doing: String,
    messages: Vec<PromptMessage>,
    retain_text: String,
) {
    let retain_plan = context
        .memory
        .record_agent_turn(current_doing, messages, retain_text)
        .await;
    for job in retain_plan.jobs {
        if let Err(err) = context.hindsight_retain.enqueue(job) {
            eprintln!("{err:?}");
            context.memory.mark_pending_retained();
            return;
        }
    }
    if retain_plan.must_flush_before_continue {
        if let Err(err) = context.hindsight_retain.flush().await {
            eprintln!("{err:?}");
        }
        context.memory.mark_pending_retained();
    }
}

async fn execute_agent_loop_step(
    context: &mut Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> AgentLoopStepExecution {
    context.prompt_memory = build_hindsight_memory_context(context).await;
    let snapshot = Snapshot::new(context).await;
    let snapshot_text = snapshot.to_string();
    let mut messages = build_runtime_agent_messages(context, &snapshot_text);
    let mut history_messages = Vec::new();
    let mut tool_results = Vec::new();
    let mut primary_action = None;
    let mut telegram_tool_nudges = 0usize;
    let mut progress_tool_nudges = 0usize;

    let output = 'agent_loop: loop {
        let request = AgentTurnRequest {
            messages: messages.clone(),
            tools: build_runtime_tool_specs(context),
        };
        let response = run_agent_turn_with_retry(context, request, tx).await;
        match response {
            AgentTurnResponse::ToolCalls { content, calls } if !calls.is_empty() => {
                let assistant_text = content.unwrap_or_else(|| {
                    format!(
                        "tool_calls={}",
                        calls
                            .iter()
                            .map(|call| call.name.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                });
                let tool_call_ui_events = calls
                    .iter()
                    .map(|call| {
                        render_tool_call_ui_event(call).unwrap_or_else(|_| {
                            ToolCallUiEvent::error(
                                call.name.clone(),
                                vec![call.arguments.to_string()],
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                let (agent_message, history_message) =
                    AgentMessage::assistant_tool_calls_with_history(
                        if assistant_text.trim().is_empty() {
                            None
                        } else {
                            Some(assistant_text.clone())
                        },
                        calls.clone(),
                        tool_call_ui_events.clone(),
                    );
                messages.push(agent_message);
                let suppress_assistant_history = assistant_text.starts_with("tool_calls=")
                    || tool_call_ui_events.iter().all(|event| {
                        matches!(
                            event,
                            ToolCallUiEvent::Terminal(event)
                                if matches!(event.action, crate::tool_ui::TerminalUiAction::Poll)
                        )
                    });
                history_messages.push(if suppress_assistant_history {
                    PromptMessage {
                        content: String::new(),
                        ..history_message
                    }
                } else {
                    history_message
                });
                if primary_action.is_none() {
                    primary_action = Some(
                        summarize_primary_action_from_tool_call(&calls[0]).unwrap_or_else(|_| {
                            EpisodeActionRecord {
                                kind: "tool_call".to_string(),
                                summary: calls[0].name.clone(),
                            }
                        }),
                    );
                }
                for (call, call_ui_event) in calls.iter().zip(tool_call_ui_events.iter()) {
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
                            ToolExecutionResult::new(
                                format!("{} failed", call.name),
                                json!({
                                    "error": error_text,
                                }),
                                ToolUiEvent::error(
                                    format!("{} failed", call.name),
                                    compact_body_lines(&error_text, 12),
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
                    messages.push(AgentMessage::tool(
                        call.id.clone(),
                        call.name.clone(),
                        result.model_content(),
                    ));
                    history_messages.push(PromptMessage::tool_with_ui(
                        result.history_content(&call.id, &call.name),
                        result.ui_event.clone(),
                    ));
                    tool_results.push(format!("{} => {}", call.name, result.summary));
                }
            }
            AgentTurnResponse::Assistant { content } => {
                if telegram_requires_tool_action(context) && telegram_tool_nudges < 2 {
                    telegram_tool_nudges += 1;
                    messages.push(AgentMessage::system(
                        "Telegram 仍有待处理信号。不要只描述“继续等待”；请立即使用 Telegram 相关 tool 推进。先读取会话，再根据最新 incoming 决定 resolve 或 send。outgoing 是你自己已经发出的消息，不是新的外部输入。".to_string(),
                    ));
                    continue 'agent_loop;
                }
                if tool_results.is_empty()
                    && runtime_turn_requires_tool_progress(context)
                    && progress_tool_nudges < 2
                {
                    progress_tool_nudges += 1;
                    messages.push(AgentMessage::system(
                        "当前仍有待处理的世界状态需要推进。不要把 world snapshot 当成用户对话，也不要只返回 assistant 说明；请直接调用能够改变世界状态的 tool。".to_string(),
                    ));
                    continue 'agent_loop;
                }
                let current_doing = content
                    .lines()
                    .next()
                    .filter(|line| !line.trim().is_empty())
                    .unwrap_or("等待下一轮工具决策")
                    .to_string();
                history_messages.push(PromptMessage::assistant(content.clone()));
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
                    current_doing,
                    primary_action: primary_action.clone().unwrap_or(EpisodeActionRecord {
                        kind: "assistant_message".to_string(),
                        summary: "assistant-only turn without tool call".to_string(),
                    }),
                };
            }
            AgentTurnResponse::ToolCalls { .. } => {
                if telegram_requires_tool_action(context) && telegram_tool_nudges < 2 {
                    telegram_tool_nudges += 1;
                    messages.push(AgentMessage::system(
                        "Telegram 仍有待处理信号。不要返回空 tool call；请立即使用 Telegram 相关 tool 推进。".to_string(),
                    ));
                    continue 'agent_loop;
                }
                if tool_results.is_empty()
                    && runtime_turn_requires_tool_progress(context)
                    && progress_tool_nudges < 2
                {
                    progress_tool_nudges += 1;
                    messages.push(AgentMessage::system(
                        "当前仍有待处理的世界状态需要推进。空 tool call 列表不会改变世界；请直接调用能够推进状态的 tool。".to_string(),
                    ));
                    continue 'agent_loop;
                }
                let observation = "模型返回了空 tool call 列表。".to_string();
                history_messages.push(PromptMessage::assistant(observation.clone()));
                break 'agent_loop AgentLoopStepOutput {
                    observation,
                    description: "没有可执行动作。".to_string(),
                    current_doing: "等待下一轮工具决策".to_string(),
                    primary_action: primary_action.clone().unwrap_or(EpisodeActionRecord {
                        kind: "empty_tool_calls".to_string(),
                        summary: "empty tool call list".to_string(),
                    }),
                };
            }
        }
    };
    if !history_messages.is_empty() {
        let retain_text = if tool_results.is_empty() {
            format!(
                "runtime agent turn\nassistant/tool history:\n{}",
                history_messages
                    .iter()
                    .map(|message| format!(
                        "{}:\n{}",
                        match message.role {
                            PromptRole::System => "system",
                            PromptRole::User => "user",
                            PromptRole::Assistant => "assistant",
                            PromptRole::Tool => "tool",
                        },
                        message.content
                    ))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            )
        } else {
            format!(
                "runtime agent turn\nassistant/tool history:\n{}\n\ntool_results:\n{}",
                history_messages
                    .iter()
                    .map(|message| format!(
                        "{}:\n{}",
                        match message.role {
                            PromptRole::System => "system",
                            PromptRole::User => "user",
                            PromptRole::Assistant => "assistant",
                            PromptRole::Tool => "tool",
                        },
                        message.content
                    ))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                tool_results.join("\n")
            )
        };
        record_runtime_history_messages(
            context,
            output.current_doing.clone(),
            history_messages,
            retain_text,
        )
        .await;
    }
    if !matches!(
        output.primary_action.kind.as_str(),
        "assistant_message" | "empty_tool_calls"
    ) {
        context.work_state.touch();
    }
    AgentLoopStepExecution {
        output,
        snapshot_text,
    }
}

fn telegram_requires_tool_action(context: &Context) -> bool {
    context
        .devices
        .state_renders()
        .into_iter()
        .any(|(device_id, render)| {
            matches!(device_id, DeviceId::Telegram)
                && matches!(render.attention, crate::device::AttentionLevel::Notice)
        })
}

fn runtime_turn_requires_tool_progress(context: &Context) -> bool {
    !runtime_trigger_reasons(context).is_empty()
}

async fn run_agent_turn_with_retry(
    context: &Context,
    request: AgentTurnRequest,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> AgentTurnResponse {
    let mut attempt = 1usize;
    loop {
        match context.llm.run_agent_turn(context, request.clone()).await {
            Ok(response) => {
                if let Some(tx) = tx {
                    tx.send_modify(|state| state.runtime_status = None);
                }
                return response;
            }
            Err(err) => {
                let capped_shift = (attempt.saturating_sub(1)).min(6) as u32;
                let backoff_ms = 300u64.saturating_mul(1u64 << capped_shift).min(30_000);
                let summary = format!(
                    "请求失败，重试 #{attempt}，等待 {:.1}s",
                    backoff_ms as f64 / 1000.0
                );
                if let Some(tx) = tx {
                    tx.send_modify(|state| state.runtime_status = Some(summary));
                }
                eprintln!("run_agent_turn retry #{attempt} after {backoff_ms}ms:\n{err}");
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
    let task_description = context.work_state.objective().map(str::to_string);
    let recent_messages = context
        .memory
        .trail()
        .into_iter()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let terminal_summary = summarize_terminal_for_hindsight(context);
    let thread_focus = context.memory.current_thread_focus();
    let mut query_sections = vec![format!("阶段：{phase}")];
    if let Some(task_description) = task_description
        && !task_description.trim().is_empty()
    {
        query_sections.insert(0, format!("目标：{task_description}"));
    }
    if let Some(thread_focus) = thread_focus
        && !thread_focus.trim().is_empty()
    {
        query_sections.push(format!("主线：{thread_focus}"));
    }
    if let Some(terminal_summary) = terminal_summary
        && !terminal_summary.trim().is_empty()
    {
        query_sections.push(format!("终端摘要：\n{terminal_summary}"));
    }
    if !recent_messages.is_empty() {
        query_sections.push(format!("近期消息：\n{}", recent_messages.join("\n")));
    }
    let query = query_sections.join("\n");

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
    let reflect = hindsight
        .reflect(
            &query,
            HindsightReflectOptions {
                budget: None,
                context: Some(format!("当前阶段：{phase}")),
                max_tokens: Some(500),
                include_facts: false,
                ..Default::default()
            },
        )
        .await;

    let recalled_memories = recall
        .ok()
        .map(|response| {
            response
                .results
                .into_iter()
                .take(5)
                .map(|item| item.text)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let reflected_strategy = reflect.ok().map(|response| response.text);

    PromptMemoryContext {
        recalled_memories,
        reflected_strategy,
    }
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
    refresh_sleep_trace_backlog(sleep_status).await;
    let forced_sleep_status =
        maybe_start_forced_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await;
    let trigger_reasons = runtime_trigger_reasons(context);
    if trigger_reasons.is_empty() {
        if context.idle_since.is_none() {
            context.idle_since = Some(std::time::Instant::now());
        }
        if let Some(status) =
            maybe_start_idle_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await
        {
            tx.send_modify(|state| state.runtime_status = Some(status));
        } else if let Some(status) = forced_sleep_status {
            tx.send_modify(|state| state.runtime_status = Some(status));
        } else {
            tx.send_modify(|state| {
                state.runtime_status =
                    Some("空闲中：没有待处理的工作、义务、项目或设备信号".to_string())
            });
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
    tx.send_modify(|state| {
        let mut status = format!("处理中：{}", trigger_reasons.join(" | "));
        if let Some(forced_sleep_status) = forced_sleep_status.as_deref() {
            status.push_str(" | ");
            status.push_str(forced_sleep_status);
        }
        state.runtime_status = Some(status)
    });
    context
        .devices
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    let _step = execute_agent_loop_step(context, Some(tx)).await;
    refresh_sleep_trace_backlog(sleep_status).await;
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
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
    let backlog = sleep_status.unread_trace_backlog;
    if backlog < FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD {
        return None;
    }
    start_background_sleep(
        context,
        tx,
        sleep_result_tx,
        sleep_running,
        sleep_status,
        SleepTrigger::Idle,
        "trace backlog 过高：已启动后台 sleep",
    )
    .await;
    Some(format!("trace backlog={}：后台 sleep 已启动", backlog))
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
                tx.send_modify(|state| state.runtime_status = Some("sleep 已在后台运行".to_string()));
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
    tx.send_modify(|state| state.runtime_status = Some(status.to_string()));
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
            if let Ok(store) = load_compiled_prompts_only().await {
                context.compiled_prompts = store;
            }
            let prefix = match result.trigger {
                SleepTrigger::Manual => "sleep 完成",
                SleepTrigger::Idle => "后台 sleep 完成",
            };
            sleep_status.last_result = Some(format!("{prefix}：{}", summarize_sleep_summary(&summary)));
            tx.send_modify(|state| {
                state.runtime_status = Some(format!("{prefix}：{}", summarize_sleep_summary(&summary)))
            });
        }
        Err(err) => {
            let prefix = match result.trigger {
                SleepTrigger::Manual => "sleep 失败",
                SleepTrigger::Idle => "后台 sleep 失败",
            };
            sleep_status.last_result = Some(format!("{prefix}：{err}"));
            tx.send_modify(|state| state.runtime_status = Some(format!("{prefix}：{err}")));
        }
    }
    refresh_sleep_trace_backlog(sleep_status).await;
    sync_dashboard_state(context, tx, sleep_status, None);
}

fn runtime_trigger_reasons(context: &Context) -> Vec<String> {
    let mut reasons = Vec::new();

    if context.work_state.has_objective() {
        reasons.push("存在当前工作目标".to_string());
    }

    let active_obligation_count = context.obligations.active_obligations().count();
    if active_obligation_count > 0 {
        reasons.push(format!("存在 {active_obligation_count} 条待处理义务"));
    }

    let active_project_count = context
        .projects
        .projects()
        .filter(|(_, project)| {
            matches!(
                project.status,
                ProjectStatus::Active | ProjectStatus::Blocked
            )
        })
        .count();
    if active_project_count > 0 {
        reasons.push(format!("存在 {active_project_count} 个进行中项目"));
    }

    for (device_id, render) in context.devices.state_renders() {
        if !matches!(render.attention, crate::device::AttentionLevel::Notice) {
            continue;
        }
        reasons.push(format!("{device_id} 有待处理信号"));
    }

    reasons
}

async fn execute_apply_patch_tool(patch_text: &str) -> miette::Result<ToolExecutionResult> {
    let cwd =
        env::current_dir().map_err(|err| miette!("failed to read current directory: {err}"))?;
    let summary = apply_patch_in_root(&cwd, patch_text).await?;
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
        state.inspect_telegram_output =
            render_inspect_output_for_dashboard(context, &device_renders, DeviceId::Telegram);
        state.activity_cells = render_activity_for_dashboard(context);
        state.last_cycle_elapsed_ms = last_cycle_elapsed_ms;
    });
}

async fn refresh_sleep_trace_backlog(sleep_status: &mut SleepDashboardStatus) {
    if let Ok(backlog) = unread_runtime_trace_count().await {
        sleep_status.unread_trace_backlog = backlog;
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
    _renders: &[(DeviceId, crate::device::DeviceStateRender)],
) -> String {
    let mut sections = Vec::new();

    let focused = context
        .devices
        .focused()
        .map(|device| device.to_string())
        .unwrap_or_else(|| "none".to_string());
    let active_projects = context.projects.projects().count();
    let active_obligations = context.obligations.active_obligations().count();
    sections.push(format!(
        "Overview\nFocused device: {focused}\nProjects: {active_projects}\nObligations: {active_obligations}"
    ));

    sections.push(format!(
        "Work\n{}",
        render_work_state_for_dashboard(context).unwrap_or_else(|| "No active work.".to_string())
    ));

    let project_lines = render_status_project_lines(context);
    sections.push(format!("Projects\n{}", project_lines.join("\n")));

    let obligation_lines = render_status_obligation_lines(context);
    sections.push(format!("Obligations\n{}", obligation_lines.join("\n")));

    sections.join("\n\n")
}

fn render_status_project_lines(context: &Context) -> Vec<String> {
    let mut projects = context.projects.projects().collect::<Vec<_>>();
    projects.sort_by_key(|(id, _)| id.to_string());
    if projects.is_empty() {
        return vec!["No active projects.".to_string()];
    }
    projects
        .into_iter()
        .take(6)
        .map(|(_, project)| {
            format!(
                "• {}  [{} / {}]",
                project.title, project.status, project.origin
            )
        })
        .collect()
}

fn render_status_obligation_lines(context: &Context) -> Vec<String> {
    let mut obligations = context.obligations.active_obligations().collect::<Vec<_>>();
    obligations.sort_by_key(|(id, _)| id.to_string());
    if obligations.is_empty() {
        return vec!["No active obligations.".to_string()];
    }
    obligations
        .into_iter()
        .take(6)
        .map(|(_, obligation)| {
            format!(
                "• {}  [{} / {}{}]",
                obligation.summary,
                obligation.status,
                obligation.urgency,
                if obligation.requires_reply {
                    " / reply"
                } else {
                    ""
                }
            )
        })
        .collect()
}

fn render_inspect_output_for_dashboard(
    context: &Context,
    renders: &[(DeviceId, crate::device::DeviceStateRender)],
    target: DeviceId,
) -> String {
    match target {
        DeviceId::Terminal => "unknown inspect target: Terminal".to_string(),
        DeviceId::Telegram => render_telegram_status_for_dashboard(context, renders),
    }
}

fn render_telegram_status_for_dashboard(
    context: &Context,
    renders: &[(DeviceId, crate::device::DeviceStateRender)],
) -> String {
    let focused = renders
        .iter()
        .find(|(device_id, _)| *device_id == DeviceId::Telegram)
        .map(|(_, render)| render.is_focused)
        .unwrap_or(false);
    let chats = context.telegram.chat_summaries_view();
    let selected = context.telegram.selected_chat_view(8);

    let mut lines = vec![format!(
        "Telegram\nState: {}",
        if focused { "focused" } else { "background" }
    )];

    if chats.is_empty() {
        lines.push(String::new());
        lines.push("No chats.".to_string());
        return lines.join("\n");
    }

    lines.push(String::new());
    lines.push("Chats".to_string());
    lines.extend(chats.iter().take(8).map(|chat| {
        let mut flags = Vec::new();
        if chat.unread > 0 {
            flags.push(format!("{} unread", chat.unread));
        }
        if chat.pending_resolution {
            flags.push("needs resolution".to_string());
        }
        if chat.needs_reply {
            flags.push("needs reply".to_string());
        }
        let suffix = if flags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", flags.join(", "))
        };
        format!("• {} ({}){}", chat.title, chat.chat_id, suffix)
    }));

    if let Some(chat) = selected {
        lines.push(String::new());
        lines.push(format!("Selected: {} ({})", chat.title, chat.chat_id));
        let mut flags = Vec::new();
        if chat.unread > 0 {
            flags.push(format!("{} unread", chat.unread));
        }
        if chat.pending_resolution {
            flags.push("needs resolution".to_string());
        }
        if chat.needs_reply {
            flags.push("needs reply".to_string());
        }
        if !flags.is_empty() {
            lines.push(format!("Status: {}", flags.join(", ")));
        }
        lines.push(String::new());
        lines.push("Recent messages".to_string());
        if chat.messages.is_empty() {
            lines.push("• No messages".to_string());
        } else {
            lines.extend(chat.messages.into_iter().map(|message| {
                format!(
                    "• [{}|{}] {}: {}",
                    message.direction, message.delivery, message.sender, message.text
                )
            }));
        }
    }

    lines.join("\n")
}

fn render_activity_for_dashboard(context: &Context) -> Vec<crate::dashboard::ActivityCell> {
    render_activity_from_messages(context.memory.prompt_messages())
}

fn render_work_state_for_dashboard(context: &Context) -> Option<String> {
    let objective = context.work_state.objective()?;
    let objective = truncate_from_left(objective, 56);
    let last_touched = format_last_touched(context.work_state.last_touched_at_ms);
    let mut lines = vec![objective, format!("上次处理: {last_touched}")];
    if let Some(project_id) = context.work_state.project_id {
        let project_title = context
            .projects
            .projects()
            .find(|(id, _)| *id == project_id)
            .map(|(_, project)| project.title.clone())
            .unwrap_or_else(|| project_id.to_string());
        lines.push(format!("项目: {}", truncate_from_left(&project_title, 24)));
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
    let path = env::var("SPINOVA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::home_dir().unwrap().join(".spinova"));
    if !path.exists() {
        tokio::fs::create_dir_all(&path).await.unwrap();
    }
    path
}
