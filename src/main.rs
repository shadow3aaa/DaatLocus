mod config;
mod context;
mod core;
mod dashboard;
mod device;
mod embeding;
mod emotion;
mod memory;
mod obligations;
mod projects;
mod providers;
mod pty;
mod reasoning;
mod snapshot;
mod system_info;
mod tasks;
mod telegram_acl;
mod telegram_device;
mod telegram_transport;
mod terminal_device;

use std::{env, path::{Path, PathBuf}, time::Duration};

use chrono::{Local, TimeZone};
use miette::{Result, miette};
use uuid::Uuid;

use crate::{
    config::load_config,
    context::Context,
    core::{Effect, Output, TelegramResolution},
    dashboard::{DashboardState, DashboardTaskEntry, run_tui_dashboard},
    device::{DeviceId, DeviceManager},
    emotion::Emotion,
    memory::Memory,
    obligations::{ObligationSource, ObligationStatus, Obligations},
    projects::{ProjectOrigin, Projects},
    providers::OpenAIClient,
    reasoning::{
        adapters::swe_train_source::SweTrainSource,
        bench::{
            eval::{
                run_bench_eval_continuity, run_bench_eval_interactive_cli, run_bench_eval_memory,
                run_bench_eval_memory_encoding, run_bench_eval_terminal_completion,
            },
            optimize::{
                run_bench_optimize_continuity, run_bench_optimize_interactive_cli,
                run_bench_optimize_memory, run_bench_optimize_memory_encoding,
                run_bench_optimize_terminal_completion,
            },
        },
        compiled::{
            BENCH_COMPILED_DIR_NAME, COMPILED_DIR_NAME, CompiledProgram, CompiledPromptStore,
            StoredPromptTuningConfig,
            load_all_compiled_programs,
        },
        environment::EpisodeObservation,
        episode::{EpisodeMetric, EpisodeOutcome, EpisodeStatus, EpisodeStep, EpisodeTask},
        episode_harness::EpisodeHarness,
        eval::run_reasoning_eval,
        optimizer::PromptTuningConfig,
        optimize::run_reasoning_optimize,
        program::Program,
        programs::action_phase_common::ActionPhaseProgramSpec,
        programs::attend_notifications::AttendNotificationsProgram,
        programs::memory_encoding::{MemoryEncodingOutput, MemoryEncodingProgram},
        programs::execute_task::ExecuteTaskProgram,
        programs::explore_new_tasks::ExploreNewTasksProgram,
        programs::plan_from_project::PlanFromProjectProgram,
        programs::terminal_next_step::TerminalNextStepProgram,
        render::openai_tools::OpenAIToolRenderer,
        runtime::execute_program,
        runtime_policy::RuntimePolicyProgram,
        sleep::run_sleep,
        sleep_artifacts::SleepArtifactSuggestedFixKind,
        teleprompter::{build_bootstrap_demo_candidates, build_teleprompter_candidates},
    },
    snapshot::Snapshot,
    tasks::Tasks,
    telegram_acl::TelegramAclHandle,
    telegram_device::TelegramDevice,
    telegram_transport::TelegramTransport,
    terminal_device::TerminalDevice,
};

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

    if is_sleep_optimize_command(&args) {
        run_sleep_optimize(config).await?;
        return Ok(());
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

    if let Some((path, limit)) = train_source_optimize_args(&args) {
        match run_train_source_optimize(config, path, limit).await {
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

    if is_reasoning_eval_command(&args) {
        let context = build_eval_context(config).await;
        match run_reasoning_eval(&context).await {
            Ok(results) => {
                print_reasoning_eval_results(&results);
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

    if is_reasoning_optimize_command(&args) {
        let context = build_eval_context(config).await;
        match run_reasoning_optimize(&context).await {
            Ok(results) => {
                print_reasoning_optimization_results(&results);
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

    if is_bench_eval_memory_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_eval_memory(&context).await {
            Ok(results) => {
                print_bench_eval_results("memory", &results);
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

    if is_bench_eval_memory_encoding_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_eval_memory_encoding(&context).await {
            Ok(results) => {
                print_bench_eval_results("memory-encoding", &results);
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

    if is_bench_eval_continuity_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_eval_continuity(&context).await {
            Ok(results) => {
                print_bench_eval_results("continuity", &results);
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

    if is_bench_eval_terminal_completion_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_eval_terminal_completion(&context).await {
            Ok(results) => {
                print_bench_eval_results("terminal-completion", &results);
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

    if is_bench_eval_interactive_cli_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_eval_interactive_cli(&context).await {
            Ok(results) => {
                print_bench_eval_results("interactive-cli", &results);
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

    if is_bench_optimize_memory_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_optimize_memory(&context).await {
            Ok(results) => {
                print_bench_optimization_results("memory", &results);
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

    if is_bench_optimize_memory_encoding_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_optimize_memory_encoding(&context).await {
            Ok(results) => {
                print_bench_optimization_results("memory-encoding", &results);
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

    if is_bench_optimize_continuity_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_optimize_continuity(&context).await {
            Ok(results) => {
                print_bench_optimization_results("continuity", &results);
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

    if is_bench_optimize_terminal_completion_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_optimize_terminal_completion(&context).await {
            Ok(results) => {
                print_bench_optimization_results("terminal-completion", &results);
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

    if is_bench_optimize_interactive_cli_command(&args) {
        let context = build_eval_context(config).await;
        match run_bench_optimize_interactive_cli(&context).await {
            Ok(results) => {
                print_bench_optimization_results("interactive-cli", &results);
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
    let tasks = Tasks::new().await;
    let emotion = Emotion::new().await;
    let telegram_acl = TelegramAclHandle::load().await;
    let terminal = TerminalDevice::new();
    let telegram = TelegramDevice::new();
    let telegram_handle = telegram.handle();
    bootstrap_telegram_device_from_acl(&telegram_handle, &telegram_acl);
    let terminal_parser = terminal.parser();
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
    let mut context = Context {
        llm: Box::new(client),
        judge_llm: Box::new(judge_client),
        config,
        memory,
        obligations,
        projects,
        tasks,
        emotion,
        devices,
        telegram: telegram_handle,
        compiled_prompts,
    };

    let (tx, mut rx) = tokio::sync::watch::channel(DashboardState {
        pty_parser: terminal_parser,
        focused_device: context.devices.focused(),
        focused_title: context
            .devices
            .focused_render()
            .as_ref()
            .map(|view| view.title.clone()),
        focused_content: context
            .devices
            .focused_render()
            .as_ref()
            .map(|view| view.content.clone()),
        obligations: render_obligations_for_dashboard(&context),
        projects: render_projects_for_dashboard(&context),
        tasks: context
            .tasks
            .tasks()
            .map(|(id, task)| (id, render_task_for_dashboard(task, &context)))
            .collect(),
        working_task: context.tasks.working_task(),
        latest_trail: context.memory.trail().last().cloned(),
        last_cycle_elapsed_ms: None,
    });
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

    let agent_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = spinova_loop(&mut context, &tx) => {}
                _ = &mut shutdown_rx => {
                    context.shutdown().await;
                    break;
                }
            }
        }
    });
    run_tui_dashboard(&mut rx, telegram_acl).await.unwrap();
    if let Some(handle) = telegram_transport {
        handle.abort();
    }
    let _ = shutdown_tx.send(());
    let _ = agent_handle.await;
    Ok(())
}

fn is_reasoning_eval_command(args: &[String]) -> bool {
    matches!(args, [command, target] if command == "eval" && target == "reasoning")
        || matches!(args, [command] if command == "eval-reasoning")
}

fn is_reasoning_optimize_command(args: &[String]) -> bool {
    matches!(args, [command, target] if command == "optimize" && target == "reasoning")
        || matches!(args, [command] if command == "optimize-reasoning")
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

fn is_sleep_optimize_command(args: &[String]) -> bool {
    matches!(args, [command] if command == "sleep-optimize")
}

fn train_source_inspect_path(args: &[String]) -> Option<&str> {
    match args {
        [command, subcommand, path]
            if command == "train-source" && subcommand == "inspect" =>
        {
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

fn train_source_optimize_args(args: &[String]) -> Option<(&str, usize)> {
    match args {
        [command, subcommand, path] if command == "train-source" && subcommand == "optimize" => {
            Some((path.as_str(), 5))
        }
        [command, subcommand, path, limit]
            if command == "train-source" && subcommand == "optimize" =>
        {
            let limit = limit.parse::<usize>().ok()?;
            Some((path.as_str(), limit))
        }
        [command, path] if command == "optimize-train-source" => Some((path.as_str(), 5)),
        [command, path, limit] if command == "optimize-train-source" => {
            let limit = limit.parse::<usize>().ok()?;
            Some((path.as_str(), limit))
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

fn is_bench_eval_memory_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "eval" && category == "bench" && target == "memory")
        || matches!(args, [command] if command == "eval-bench-memory")
}

fn is_bench_eval_memory_encoding_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "eval" && category == "bench" && target == "memory-encoding")
        || matches!(args, [command] if command == "eval-bench-memory-encoding")
}

fn is_bench_eval_continuity_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "eval" && category == "bench" && target == "continuity")
        || matches!(args, [command] if command == "eval-bench-continuity")
}

fn is_bench_optimize_memory_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "optimize" && category == "bench" && target == "memory")
        || matches!(args, [command] if command == "optimize-bench-memory")
}

fn is_bench_optimize_memory_encoding_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "optimize" && category == "bench" && target == "memory-encoding")
        || matches!(args, [command] if command == "optimize-bench-memory-encoding")
}

fn is_bench_optimize_continuity_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "optimize" && category == "bench" && target == "continuity")
        || matches!(args, [command] if command == "optimize-bench-continuity")
}

fn is_bench_eval_terminal_completion_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "eval" && category == "bench" && target == "terminal-completion")
        || matches!(args, [command] if command == "eval-bench-terminal-completion")
}

fn is_bench_eval_interactive_cli_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "eval" && category == "bench" && target == "interactive-cli")
        || matches!(args, [command] if command == "eval-bench-interactive-cli")
}

fn is_bench_optimize_terminal_completion_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "optimize" && category == "bench" && target == "terminal-completion")
        || matches!(args, [command] if command == "optimize-bench-terminal-completion")
}

fn is_bench_optimize_interactive_cli_command(args: &[String]) -> bool {
    matches!(args, [command, category, target] if command == "optimize" && category == "bench" && target == "interactive-cli")
        || matches!(args, [command] if command == "optimize-bench-interactive-cli")
}

async fn run_mem_reset() -> Result<()> {
    let home = get_spinova_home().await;
    let config = crate::config::Config::default();
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
        memory: Memory::empty().await,
        obligations: Obligations::default(),
        projects: Projects::default(),
        tasks: Tasks::default(),
        emotion: Emotion::default(),
        devices,
        telegram: telegram_handle,
        compiled_prompts: CompiledPromptStore::empty(),
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
        "[mem-reset] cleared via empty context shutdown: l1_memory, l2_memory.lancedb, tasks, projects, obligations, emotion"
    );
    println!("[mem-reset] cleared: reasoning_traces.jsonl");
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
            "[prompt-reset] nothing to remove; {} and {} were already absent",
            COMPILED_DIR_NAME, BENCH_COMPILED_DIR_NAME
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

    for dir_name in [COMPILED_DIR_NAME, BENCH_COMPILED_DIR_NAME] {
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
    let tasks = Tasks::new().await;
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

    Context {
        llm: Box::new(client),
        judge_llm: Box::new(judge_client),
        config,
        memory,
        obligations,
        projects,
        tasks,
        emotion,
        devices,
        telegram: telegram_handle,
        compiled_prompts,
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
    Ok(CompiledPromptStore::from_entries(compiled))
}

async fn run_sleep_optimize(config: crate::config::Config) -> Result<()> {
    let mut context = build_eval_context(config.clone()).await;
    let summary = run_sleep(&mut context).await?;
    print_sleep_summary(&summary);
    let results = run_reasoning_optimize(&context).await;
    context.shutdown().await;

    let results = results?;
    print_reasoning_optimization_results(&results);
    Ok(())
}

fn run_train_source_inspect_blocking(path: &str) -> Result<()> {
    let source = SweTrainSource::load_blocking(path)?;
    let tasks = source.into_episode_tasks(32);
    let summary = EpisodeHarness::summarize_tasks(&tasks, 5);
    print_episode_batch_summary(path, &summary);
    Ok(())
}

async fn run_train_source_rollout(config: crate::config::Config, path: &str, task_index: usize) -> Result<()> {
    let source = SweTrainSource::load(path).await?;
    let tasks = source.into_episode_tasks(32);
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
    let seeded_task_id = context.tasks.add_task(task.instruction.clone());
    context.tasks.select_working_task(seeded_task_id);

    let outcome = rollout_runtime_policy_episode(&mut context, task, &workspace_dir).await?;
    print_episode_rollout(&outcome, &episode_home);
    context.shutdown().await;
    drop(home_override);
    Ok(())
}

async fn run_train_source_optimize(
    config: crate::config::Config,
    path: &str,
    limit: usize,
) -> Result<()> {
    let source = SweTrainSource::load(path).await?;
    let mut tasks = source.into_episode_tasks(32);
    if limit > 0 && tasks.len() > limit {
        tasks.truncate(limit);
    }

    let variants = build_train_source_policy_variants().await?;

    let mut summaries = Vec::new();
    for variant in variants {
        let outcomes = run_train_source_variant(&config, &tasks, &variant).await?;
        summaries.push(TrainSourceVariantSummary {
            variant_name: variant.name,
            outcomes,
        });
    }

    print_train_source_optimization_summary(path, &summaries);
    Ok(())
}

async fn run_train_source_learn(
    config: crate::config::Config,
    path: &str,
    limit: usize,
    batch_size: usize,
) -> Result<()> {
    let source = SweTrainSource::load(path).await?;
    let mut tasks = source.into_episode_tasks(32);
    if limit > 0 && tasks.len() > limit {
        tasks.truncate(limit);
    }
    let batch_size = batch_size.max(1);
    let session_root = prepare_learning_session_root(path).await?;
    let shared_home = session_root.join("spinova_home");
    let home_override = SpinovaHomeOverride::set(shared_home.clone());
    let mut state = TrainSourceLearnState::new(path.to_string(), tasks.len(), batch_size);
    save_train_source_learn_state(&session_root, &state).await?;

    println!(
        "train source learn: path={} total_tasks={} batch_size={} session={}",
        path,
        tasks.len(),
        batch_size,
        session_root.display()
    );

    let mut cursor = 0usize;
    let mut batch_index = 0usize;
    while cursor < tasks.len() {
        let batch_end = (cursor + batch_size).min(tasks.len());
        let batch_tasks = &tasks[cursor..batch_end];
        println!(
            "  batch {} preview starting (tasks {}..{}, count={})",
            batch_index + 1,
            cursor + 1,
            batch_end,
            batch_tasks.len()
        );
        let selection =
            select_train_source_variant_for_batch(&config, batch_tasks, &session_root, batch_index)
                .await?;
        println!(
            "  batch {} selected_variant={} (tasks {}..{})",
            batch_index + 1,
            selection.variant.variant_name(),
            cursor + 1,
            batch_end
        );

        for (offset, task) in batch_tasks.iter().enumerate() {
            let absolute_index = cursor + offset;
            let episode_root =
                prepare_learning_episode_root(&session_root, task, absolute_index).await?;
            let outcome = execute_train_source_task(
                &config,
                task,
                selection.variant.compiled_prompts.clone(),
                &episode_root,
            )
            .await?;

            state.completed_tasks += 1;
            state.last_task_id = Some(outcome.task.id.clone());
            state.last_task_status = Some(format!("{:?}", outcome.status));
            state.last_score = Some(outcome.metric.score);
            state.outcomes.push(TrainSourceLearnOutcomeSummary::from_episode(&outcome));
            save_train_source_learn_state(&session_root, &state).await?;

            println!(
                "- learn task {}/{} id={} status={:?} score={:.2}",
                state.completed_tasks,
                state.total_tasks,
                outcome.task.id,
                outcome.status,
                outcome.metric.score
            );
        }

        let mut optimize_context =
            build_eval_context_with_compiled(config.clone(), load_compiled_prompts_only().await?)
                .await;
        let sleep_summary = run_sleep(&mut optimize_context).await?;
        let optimization_results = run_reasoning_optimize(&optimize_context).await?;
        optimize_context.shutdown().await;

        state.sleep_runs += 1;
        state.optimize_runs += 1;
        state.last_compiled_prompt_count = load_compiled_prompts_only().await?.len();
        state.batch_reports.push(TrainSourceLearnBatchReport {
            completed_tasks: state.completed_tasks,
            selected_variant: selection.variant.variant_name().to_string(),
            selection_scores: selection
                .all_summaries
                .iter()
                .map(TrainSourceLearnVariantScore::from_summary)
                .collect(),
            sleep_failure_patterns: sleep_summary.failure_patterns.len(),
            sleep_bootstrap_demos: sleep_summary.bootstrap_demos,
            sleep_stress_cases: sleep_summary.stress_cases,
            sleep_instruction_hypotheses: sleep_summary.instruction_hypotheses,
            promoted_l3_entries: sleep_summary.promoted_l3_entries,
            compiled_prompt_count: state.last_compiled_prompt_count,
            optimized_suites: optimization_results.len(),
        });
        save_train_source_learn_state(&session_root, &state).await?;

        println!(
            "  batch update: completed={} selected_variant={} sleep_patterns={} demos={} stress={} instructions={} l3={} compiled_suites={}",
            state.completed_tasks,
            selection.variant.variant_name(),
            sleep_summary.failure_patterns.len(),
            sleep_summary.bootstrap_demos,
            sleep_summary.stress_cases,
            sleep_summary.instruction_hypotheses,
            sleep_summary.promoted_l3_entries,
            state.last_compiled_prompt_count
        );

        cursor = batch_end;
        batch_index += 1;
    }

    print_train_source_learn_summary(&session_root, &state);
    drop(home_override);
    Ok(())
}

async fn build_train_source_policy_variants() -> Result<Vec<TrainSourcePolicyVariant>> {
    let compiled_entries = load_all_compiled_programs().await?;
    let mut variants = vec![TrainSourcePolicyVariant {
        name: "baseline".to_string(),
        compiled_prompts: CompiledPromptStore::empty(),
    }];

    if compiled_entries.is_empty() {
        return Ok(variants);
    }

    variants.push(TrainSourcePolicyVariant {
        name: "compiled".to_string(),
        compiled_prompts: CompiledPromptStore::from_entries(compiled_entries.clone()),
    });

    let execute_only = filter_compiled_entries(
        &compiled_entries,
        &["action_phase.execute_task"],
    );
    if !execute_only.is_empty() {
        variants.push(TrainSourcePolicyVariant {
            name: "execute-only".to_string(),
            compiled_prompts: CompiledPromptStore::from_entries(execute_only),
        });
    }

    let task_chain = filter_compiled_entries(
        &compiled_entries,
        &[
            "action_phase.execute_task",
            "action_phase.plan_from_project",
            "action_phase.explore_new_tasks",
        ],
    );
    if !task_chain.is_empty() {
        variants.push(TrainSourcePolicyVariant {
            name: "task-chain".to_string(),
            compiled_prompts: CompiledPromptStore::from_entries(task_chain),
        });
    }

    variants.extend(build_execute_task_candidate_variants(&compiled_entries));
    variants.extend(build_terminal_next_step_candidate_variants(&compiled_entries));
    variants.extend(build_attend_notifications_candidate_variants(&compiled_entries));
    variants.extend(build_plan_from_project_candidate_variants(&compiled_entries));
    variants.extend(build_explore_new_tasks_candidate_variants(&compiled_entries));

    Ok(variants)
}

fn filter_compiled_entries(
    entries: &[crate::reasoning::compiled::CompiledProgram],
    allowed_suites: &[&str],
) -> Vec<crate::reasoning::compiled::CompiledProgram> {
    entries
        .iter()
        .filter(|entry| allowed_suites.iter().any(|suite| *suite == entry.suite))
        .cloned()
        .collect()
}

fn build_execute_task_candidate_variants(
    compiled_entries: &[CompiledProgram],
) -> Vec<TrainSourcePolicyVariant> {
    let base_entries = {
        let task_chain = filter_compiled_entries(
            compiled_entries,
            &[
                "action_phase.execute_task",
                "action_phase.plan_from_project",
                "action_phase.explore_new_tasks",
            ],
        );
        if task_chain.is_empty() {
            compiled_entries.to_vec()
        } else {
            task_chain
        }
    };

    let program = ExecuteTaskProgram;
    let base = program.default_tuning();
    let mut candidates = vec![
        (
            "candidate.execute_task.minimal_examples",
            PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples: base.examples.iter().take(1).cloned().collect(),
            },
        ),
        (
            "candidate.execute_task.phase_bias",
            PromptTuningConfig {
                extra_instructions: vec![
                    "执行阶段优先推进当前已存在的下一步动作，不要绕回探索。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        ),
    ];

    let teleprompt = build_teleprompter_candidates(
        &base,
        "teleprompt_instruction",
        &["执行阶段优先按照训练边界行动：先选中已有动作、保持正确设备前景、误入交互式认证时先中断。"],
    );
    for candidate in teleprompt {
        candidates.push((
            &*Box::leak(
                format!("candidate.execute_task.{}", candidate.name)
                    .into_boxed_str(),
            ),
            candidate.config,
        ));
    }

    let bootstrap = build_bootstrap_demo_candidates(
        &base,
        "bootstrap_train_demos",
        "bootstrap_train_combo",
        &["执行阶段优先按照训练边界行动：先选中已有动作、保持正确设备前景、误入交互式认证时先中断。"],
        crate::reasoning::datasets::action_phase::all_bootstrap_examples_by_suite(
            "action_phase.execute_task",
        ),
    );
    for candidate in bootstrap {
        candidates.push((
            &*Box::leak(
                format!("candidate.execute_task.{}", candidate.name)
                    .into_boxed_str(),
            ),
            candidate.config,
        ));
    }

    candidates
        .into_iter()
        .map(|(variant_name, tuning)| TrainSourcePolicyVariant {
            name: variant_name.to_string(),
            compiled_prompts: compiled_store_with_suite_override(
                &base_entries,
                "action_phase.execute_task",
                variant_name,
                tuning,
            ),
        })
        .collect()
}

fn build_plan_from_project_candidate_variants(
    compiled_entries: &[CompiledProgram],
) -> Vec<TrainSourcePolicyVariant> {
    build_action_phase_program_candidate_variants(
        compiled_entries,
        PlanFromProjectProgram,
        "candidate.plan_from_project",
        Some("项目规划阶段应优先补出挂到该项目上的下一步动作，而不是转去探索别的方向。"),
        &["项目规划阶段优先按照训练边界行动：为 Active 项目补出 project-scoped 的具体下一步动作，而不是偏离项目。"],
    )
}

fn build_attend_notifications_candidate_variants(
    compiled_entries: &[CompiledProgram],
) -> Vec<TrainSourcePolicyVariant> {
    build_action_phase_program_candidate_variants(
        compiled_entries,
        AttendNotificationsProgram,
        "candidate.attend_notifications",
        Some("提醒处理阶段只要 Telegram 在后台有待处理消息，就应先切到 Telegram，而不是继续终端工作。"),
        &["提醒处理阶段优先按照训练边界行动：先处理 Telegram 与 Pending 义务，再考虑其他设备或探索。"],
    )
}

fn build_explore_new_tasks_candidate_variants(
    compiled_entries: &[CompiledProgram],
) -> Vec<TrainSourcePolicyVariant> {
    build_action_phase_program_candidate_variants(
        compiled_entries,
        ExploreNewTasksProgram,
        "candidate.explore_new_tasks",
        Some("探索阶段在完全空闲且没有前景设备时，应先切到 Terminal 获取可操作环境。"),
        &["探索阶段优先按照训练边界行动：无前景设备时先 FocusTerminal，完全空闲时用 SilentWait。"],
    )
}

fn build_action_phase_program_candidate_variants<P: ActionPhaseProgramSpec + Copy>(
    compiled_entries: &[CompiledProgram],
    program: P,
    variant_prefix: &str,
    phase_bias_instruction: Option<&str>,
    teleprompt_instructions: &[&str],
) -> Vec<TrainSourcePolicyVariant> {
    let base_entries = {
        let task_chain = filter_compiled_entries(
            compiled_entries,
            &[
                "action_phase.execute_task",
                "action_phase.plan_from_project",
                "action_phase.explore_new_tasks",
            ],
        );
        if task_chain.is_empty() {
            compiled_entries.to_vec()
        } else {
            task_chain
        }
    };

    let base = program.default_tuning();
    let mut candidates = vec![(
        format!("{variant_prefix}.minimal_examples"),
        PromptTuningConfig {
            extra_instructions: base.extra_instructions.clone(),
            examples: base.examples.iter().take(1).cloned().collect(),
        },
    )];

    if let Some(instruction) = phase_bias_instruction {
        candidates.push((
            format!("{variant_prefix}.phase_bias"),
            PromptTuningConfig {
                extra_instructions: vec![instruction.to_string()],
                examples: base.examples.clone(),
            },
        ));
    }

    for candidate in build_teleprompter_candidates(
        &base,
        "teleprompt_instruction",
        teleprompt_instructions,
    ) {
        candidates.push((
            format!("{variant_prefix}.{}", candidate.name),
            candidate.config,
        ));
    }

    for candidate in build_bootstrap_demo_candidates(
        &base,
        "bootstrap_train_demos",
        "bootstrap_train_combo",
        teleprompt_instructions,
        crate::reasoning::datasets::action_phase::all_bootstrap_examples_by_suite(
            program.suite_name(),
        ),
    ) {
        candidates.push((
            format!("{variant_prefix}.{}", candidate.name),
            candidate.config,
        ));
    }

    candidates
        .into_iter()
        .map(|(variant_name, tuning)| TrainSourcePolicyVariant {
            name: variant_name.clone(),
            compiled_prompts: compiled_store_with_suite_override(
                &base_entries,
                program.suite_name(),
                &variant_name,
                tuning,
            ),
        })
        .collect()
}

fn build_terminal_next_step_candidate_variants(
    compiled_entries: &[CompiledProgram],
) -> Vec<TrainSourcePolicyVariant> {
    let base_entries = if compiled_entries.is_empty() {
        Vec::new()
    } else {
        compiled_entries.to_vec()
    };

    let program = TerminalNextStepProgram;
    let base = program.default_tuning();
    let mut candidates = vec![
        (
            "candidate.terminal_next_step.minimal_examples",
            PromptTuningConfig {
                extra_instructions: base.extra_instructions.clone(),
                examples: base.examples.iter().take(1).cloned().collect(),
            },
        ),
        (
            "candidate.terminal_next_step.prompt_return_bias",
            PromptTuningConfig {
                extra_instructions: vec![
                    "一旦终端底部已经回到 shell prompt，应把上一条命令视为结束；如果只是窗口不够高，优先换查看策略，不要重跑同一命令。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        ),
        (
            "candidate.terminal_next_step.interactive_prompt_bias",
            PromptTuningConfig {
                extra_instructions: vec![
                    "如果看到 REPL 提示符、问答式登录向导、密码提示或 (END) 这类交互/分页信号，不要当作普通静态输出；应优先退出、中断或给出明确安全输入。".to_string(),
                ],
                examples: base.examples.clone(),
            },
        ),
    ];

    let teleprompt = build_teleprompter_candidates(
        &base,
        "teleprompt_instruction",
        &[
            "优先正确理解 PTY：prompt 返回说明命令已结束，交互式提示与分页器不等于 still running，流式输出才应 Wait。",
        ],
    );
    for candidate in teleprompt {
        candidates.push((
            &*Box::leak(
                format!("candidate.terminal_next_step.{}", candidate.name).into_boxed_str(),
            ),
            candidate.config,
        ));
    }

    candidates
        .into_iter()
        .map(|(variant_name, tuning)| TrainSourcePolicyVariant {
            name: variant_name.to_string(),
            compiled_prompts: compiled_store_with_suite_override(
                &base_entries,
                "terminal_next_step",
                variant_name,
                tuning,
            ),
        })
        .collect()
}

fn compiled_store_with_suite_override(
    base_entries: &[CompiledProgram],
    suite: &str,
    candidate_name: &str,
    tuning: PromptTuningConfig<Output>,
) -> CompiledPromptStore {
    let mut entries = base_entries.to_vec();
    entries.retain(|entry| entry.suite != suite);
    entries.push(CompiledProgram {
        suite: suite.to_string(),
        compile_key: format!("learn-preview-{candidate_name}"),
        best_candidate: candidate_name.to_string(),
        score: 0,
        total_cases: 0,
        tuning: StoredPromptTuningConfig::from_typed(&tuning),
        report: None,
    });
    CompiledPromptStore::from_entries(entries)
}

async fn select_train_source_variant_for_batch(
    config: &crate::config::Config,
    tasks: &[EpisodeTask],
    session_root: &Path,
    batch_index: usize,
) -> Result<SelectedTrainSourceVariant> {
    let variants = build_train_source_policy_variants().await?;
    let mut all_summaries = Vec::new();

    for variant in variants {
        println!(
            "    preview variant={} batch={} tasks={}",
            variant.variant_name(),
            batch_index + 1,
            tasks.len()
        );
        let mut outcomes = Vec::new();
        for (task_index, task) in tasks.iter().enumerate() {
            println!(
                "      preview task {}/{} id={}",
                task_index + 1,
                tasks.len(),
                task.id
            );
            let preview_root = prepare_learning_selection_episode_root(
                session_root,
                batch_index,
                variant.variant_name(),
                task,
                task_index,
            )
            .await?;
            let preview_outcome = execute_train_source_task(
                config,
                task,
                variant.compiled_prompts.clone(),
                &preview_root,
            )
            .await?;
            outcomes.push(preview_outcome);
        }
        all_summaries.push(TrainSourceVariantSummary {
            variant_name: variant.variant_name().to_string(),
            outcomes,
        });
    }

    let selected_name = all_summaries
        .iter()
        .max_by(|left, right| compare_variant_summaries(left, right))
        .map(|summary| summary.variant_name.clone())
        .ok_or_else(|| miette!("no policy variants available for batch selection"))?;

    let selected_variant = build_train_source_policy_variants()
        .await?
        .into_iter()
        .find(|variant| variant.variant_name() == selected_name)
        .ok_or_else(|| miette!("selected variant `{selected_name}` missing after rebuild"))?;

    Ok(SelectedTrainSourceVariant {
        variant: selected_variant,
        all_summaries,
    })
}

async fn run_train_source_variant(
    config: &crate::config::Config,
    tasks: &[EpisodeTask],
    variant: &TrainSourcePolicyVariant,
) -> Result<Vec<EpisodeOutcome>> {
    let mut outcomes = Vec::new();
    for task in tasks {
        let episode_root = prepare_isolated_episode_root(task, &variant.name).await?;
        outcomes.push(
            execute_train_source_task(
                config,
                task,
                variant.compiled_prompts.clone(),
                &episode_root,
            )
            .await?,
        );
    }
    Ok(outcomes)
}

async fn execute_train_source_task(
    config: &crate::config::Config,
    task: &EpisodeTask,
    compiled_prompts: CompiledPromptStore,
    episode_root: &Path,
) -> Result<EpisodeOutcome> {
    let episode_home = episode_root.join("spinova_home");
    let workspace_dir = episode_root.join("workspace");
    let home_override = SpinovaHomeOverride::set(episode_home);
    println!(
        "        episode setup: id={} workspace={}",
        task.id,
        workspace_dir.display()
    );
    provision_episode_workspace(task, &workspace_dir).await?;

    let mut run_task = task.clone();
    run_task.workspace_hint = Some(workspace_dir.display().to_string());
    let mut context = build_eval_context_with_compiled(config.clone(), compiled_prompts).await;
    context.devices.focus(DeviceId::Terminal).await?;
    enter_episode_workspace(&mut context, &workspace_dir).await?;
    let seeded_task_id = context.tasks.add_task(run_task.instruction.clone());
    context.tasks.select_working_task(seeded_task_id);

    let outcome = rollout_runtime_policy_episode(&mut context, run_task, &workspace_dir).await?;
    save_episode_outcome(episode_root, &outcome).await?;
    context.shutdown().await;
    drop(home_override);
    Ok(outcome)
}

fn print_reasoning_eval_results(results: &[crate::reasoning::eval::EvalCaseResult]) {
    let passed = results.iter().filter(|result| result.passed).count();
    let failed = results.len().saturating_sub(passed);
    println!(
        "reasoning eval: total={} passed={} failed={}",
        results.len(),
        passed,
        failed
    );
    for result in results {
        let status = if result.passed { "PASS" } else { "FAIL" };
        println!(
            "[{}] {} / {} - {}",
            status, result.suite, result.case_name, result.detail
        );
    }
}

fn print_reasoning_optimization_results(
    results: &[crate::reasoning::optimizer::OptimizationResult],
) {
    println!("reasoning optimize:");
    for result in results {
        println!(
            "- suite={} best_candidate={} score={}/{}",
            result.suite, result.best_candidate, result.score, result.total_cases
        );
    }
}

fn print_bench_eval_results(
    benchmark_name: &str,
    results: &[crate::reasoning::eval::EvalCaseResult],
) {
    let passed = results.iter().filter(|result| result.passed).count();
    let failed = results.len().saturating_sub(passed);
    println!(
        "bench eval ({}): total={} passed={} failed={}",
        benchmark_name,
        results.len(),
        passed,
        failed
    );
    for result in results {
        let status = if result.passed { "PASS" } else { "FAIL" };
        println!(
            "[{}] {} / {} - {}",
            status, result.suite, result.case_name, result.detail
        );
    }
}

fn print_bench_optimization_results(
    benchmark_name: &str,
    results: &[crate::reasoning::optimizer::OptimizationResult],
) {
    println!("bench optimize ({}):", benchmark_name);
    for result in results {
        println!(
            "- suite={} best_candidate={} score={}/{}",
            result.suite, result.best_candidate, result.score, result.total_cases
        );
    }
}

fn print_sleep_summary(summary: &crate::reasoning::sleep::SleepSummary) {
    println!(
        "sleep: derived {} failure patterns, {} bootstrap demos, {} stress cases, {} instruction hypotheses, promoted {} l3 entries (failure={}, success={})",
        summary.failure_patterns.len(),
        summary.bootstrap_demos,
        summary.stress_cases,
        summary.instruction_hypotheses,
        summary.promoted_l3_entries,
        summary.promoted_failure_l3_entries,
        summary.promoted_success_l3_entries
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

async fn rollout_runtime_policy_episode(
    context: &mut Context,
    task: EpisodeTask,
    workspace_dir: &Path,
) -> Result<EpisodeOutcome> {
    let renderer = OpenAIToolRenderer;
    let runtime_policy = RuntimePolicyProgram;
    let mut steps = Vec::new();
    let initial_snapshot = Snapshot::new(context).await.to_string();
    let initial_observation = EpisodeObservation {
        summary: format!("task seeded: {}", task.title),
        snapshot_text: initial_snapshot,
        metadata: std::collections::BTreeMap::new(),
    };

    for index in 0..task.max_steps {
        let snapshot = Snapshot::new(context).await;
        let outcome = runtime_policy.run_once(context, &snapshot, &renderer).await;
        let output = outcome.output;
        let effect = output.effect.clone();
        let should_stop = matches!(effect, Effect::Wait | Effect::SilentWait)
            && steps.last().is_some_and(|previous: &EpisodeStep| {
                matches!(
                    (&previous.effect, &effect),
                    (Effect::Wait, Effect::Wait)
                        | (Effect::SilentWait, Effect::SilentWait)
                )
            });

        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("description".to_string(), output.description.clone());
        metadata.insert("current_doing".to_string(), output.current_doing.clone());
        steps.push(EpisodeStep {
            index,
            module: "runtime_policy".to_string(),
            effect: effect.clone(),
            observation_summary: output.observation,
            snapshot_text: snapshot.to_string(),
            metadata,
        });

        apply_effect(context, effect).await;
        if outcome.touched_working_task {
            context.tasks.touch_working_task();
        }

        if context.tasks.is_empty() || context.tasks.working_task().is_none() || should_stop {
            return finalize_runtime_episode(context, task, workspace_dir, initial_observation, steps).await;
        }
    }

    finalize_runtime_episode(context, task, workspace_dir, initial_observation, steps).await
}

async fn finalize_runtime_episode(
    context: &mut Context,
    task: EpisodeTask,
    workspace_dir: &Path,
    initial_observation: EpisodeObservation,
    steps: Vec<EpisodeStep>,
) -> Result<EpisodeOutcome> {
    let rollout_status = if context.tasks.is_empty() || context.tasks.working_task().is_none() {
        EpisodeStatus::Succeeded
    } else if steps.len() >= task.max_steps {
        EpisodeStatus::MaxStepsExceeded
    } else {
        EpisodeStatus::Aborted
    };
    let validation_results = run_validation_commands(&task.validation_commands, workspace_dir).await?;
    let status = final_episode_status(rollout_status, &validation_results);
    let final_snapshot = Snapshot::new(context).await.to_string();
    let metric = build_episode_metric(&steps, status, rollout_status, &validation_results);
    Ok(EpisodeOutcome {
        task,
        environment_name: "runtime_policy_rollout".to_string(),
        initial_observation,
        final_observation: EpisodeObservation {
            summary: final_episode_summary(status, &steps, &validation_results),
            snapshot_text: final_snapshot,
            metadata: validation_metadata(&validation_results),
        },
        status,
        steps,
        metric,
    })
}

fn build_episode_metric(
    steps: &[EpisodeStep],
    status: EpisodeStatus,
    rollout_status: EpisodeStatus,
    validation_results: &[ValidationCommandResult],
) -> EpisodeMetric {
    let repeated_effects = steps
        .windows(2)
        .filter(|pair| {
            matches!(
                (&pair[0].effect, &pair[1].effect),
                (Effect::Wait, Effect::Wait)
                    | (Effect::SilentWait, Effect::SilentWait)
            )
        })
        .count();

    let success = matches!(status, EpisodeStatus::Succeeded);
    let score = if success {
        (1.0 - (steps.len() as f32 * 0.01) - (repeated_effects as f32 * 0.05)).max(0.0)
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
        repeated_effects,
        stagnation_events: repeated_effects,
        notes,
    }
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
            validation_results.iter().filter(|result| result.success).count(),
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
            "- step={} module={} effect={:?}\n  observation={}\n  doing={}\n  description={}",
            step.index,
            step.module,
            step.effect,
            step.observation_summary,
            step.metadata.get("current_doing").map(String::as_str).unwrap_or("-"),
            step.metadata.get("description").map(String::as_str).unwrap_or("-")
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
    let payload = serde_json::to_vec_pretty(outcome)
        .map_err(|err| miette!("failed to serialize episode outcome {}: {err}", outcome.task.id))?;
    tokio::fs::write(&path, payload)
        .await
        .map_err(|err| miette!("failed to write episode outcome {}: {err}", path.display()))?;
    Ok(())
}

async fn save_train_source_learn_state(
    session_root: &Path,
    state: &TrainSourceLearnState,
) -> Result<()> {
    let path = session_root.join("learn_state.json");
    let payload = serde_json::to_vec_pretty(state)
        .map_err(|err| miette!("failed to serialize learn state: {err}"))?;
    tokio::fs::write(&path, payload)
        .await
        .map_err(|err| miette!("failed to write learn state {}: {err}", path.display()))?;
    Ok(())
}

fn print_train_source_optimization_summary(path: &str, summaries: &[TrainSourceVariantSummary]) {
    println!(
        "train source optimize: path={} variants={}",
        path,
        summaries.len()
    );
    for summary in summaries {
        let total = summary.outcomes.len();
        let succeeded = summary
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == EpisodeStatus::Succeeded)
            .count();
        let failed = summary
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == EpisodeStatus::Failed)
            .count();
        let aborted = summary
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == EpisodeStatus::Aborted)
            .count();
        let max_steps = summary
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == EpisodeStatus::MaxStepsExceeded)
            .count();
        let avg_score = if total == 0 {
            0.0
        } else {
            summary
                .outcomes
                .iter()
                .map(|outcome| outcome.metric.score)
                .sum::<f32>()
                / total as f32
        };
        println!(
            "- variant={} total={} succeeded={} failed={} aborted={} max_steps={} avg_score={:.2}",
            summary.variant_name,
            total,
            succeeded,
            failed,
            aborted,
            max_steps,
            avg_score
        );
        for outcome in &summary.outcomes {
            println!(
                "  id={} status={:?} score={:.2} steps={} repeated_effects={}",
                outcome.task.id,
                outcome.status,
                outcome.metric.score,
                outcome.metric.steps_used,
                outcome.metric.repeated_effects
            );
        }
    }

    if let Some(best) = summaries.iter().max_by(|left, right| {
        compare_variant_summaries(left, right)
    }) {
        println!("selected_variant={}", best.variant_name);
    }
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
        state.outcomes.iter().map(|outcome| outcome.score).sum::<f32>() / state.outcomes.len() as f32
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

fn compare_variant_summaries(
    left: &TrainSourceVariantSummary,
    right: &TrainSourceVariantSummary,
) -> std::cmp::Ordering {
    let left_successes = left
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == EpisodeStatus::Succeeded)
        .count();
    let right_successes = right
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == EpisodeStatus::Succeeded)
        .count();
    left_successes
        .cmp(&right_successes)
        .then_with(|| {
            let left_avg = if left.outcomes.is_empty() {
                0.0
            } else {
                left.outcomes
                    .iter()
                    .map(|outcome| outcome.metric.score)
                    .sum::<f32>()
                    / left.outcomes.len() as f32
            };
            let right_avg = if right.outcomes.is_empty() {
                0.0
            } else {
                right.outcomes
                    .iter()
                    .map(|outcome| outcome.metric.score)
                    .sum::<f32>()
                    / right.outcomes.len() as f32
            };
            left_avg
                .partial_cmp(&right_avg)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

#[derive(Clone)]
struct TrainSourcePolicyVariant {
    name: String,
    compiled_prompts: CompiledPromptStore,
}

impl TrainSourcePolicyVariant {
    fn variant_name(&self) -> &str {
        &self.name
    }
}

struct SelectedTrainSourceVariant {
    variant: TrainSourcePolicyVariant,
    all_summaries: Vec<TrainSourceVariantSummary>,
}

#[derive(Clone)]
struct TrainSourceVariantSummary {
    variant_name: String,
    outcomes: Vec<EpisodeOutcome>,
}

#[derive(serde::Serialize)]
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

#[derive(serde::Serialize)]
struct TrainSourceLearnOutcomeSummary {
    task_id: String,
    status: String,
    score: f32,
    steps_used: usize,
    repeated_effects: usize,
}

impl TrainSourceLearnOutcomeSummary {
    fn from_episode(outcome: &EpisodeOutcome) -> Self {
        Self {
            task_id: outcome.task.id.clone(),
            status: format!("{:?}", outcome.status),
            score: outcome.metric.score,
            steps_used: outcome.metric.steps_used,
            repeated_effects: outcome.metric.repeated_effects,
        }
    }
}

#[derive(serde::Serialize)]
struct TrainSourceLearnBatchReport {
    completed_tasks: usize,
    selected_variant: String,
    selection_scores: Vec<TrainSourceLearnVariantScore>,
    sleep_failure_patterns: usize,
    sleep_bootstrap_demos: usize,
    sleep_stress_cases: usize,
    sleep_instruction_hypotheses: usize,
    promoted_l3_entries: usize,
    compiled_prompt_count: usize,
    optimized_suites: usize,
}

#[derive(serde::Serialize)]
struct TrainSourceLearnVariantScore {
    variant_name: String,
    succeeded: usize,
    failed: usize,
    avg_score: f32,
}

impl TrainSourceLearnVariantScore {
    fn from_summary(summary: &TrainSourceVariantSummary) -> Self {
        let succeeded = summary
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == EpisodeStatus::Succeeded)
            .count();
        let failed = summary
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == EpisodeStatus::Failed)
            .count();
        let avg_score = if summary.outcomes.is_empty() {
            0.0
        } else {
            summary
                .outcomes
                .iter()
                .map(|outcome| outcome.metric.score)
                .sum::<f32>()
                / summary.outcomes.len() as f32
        };
        Self {
            variant_name: summary.variant_name.clone(),
            succeeded,
            failed,
            avg_score,
        }
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
        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|err| miette!("failed to clear isolated episode root {}: {err}", path.display()))?;
    }
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(|err| miette!("failed to create isolated episode root {}: {err}", path.display()))?;
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
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|err| miette!("failed to create learn session root {}: {err}", root.display()))?;
    Ok(root)
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
        tokio::fs::remove_dir_all(&root)
            .await
            .map_err(|err| miette!("failed to clear learn episode root {}: {err}", root.display()))?;
    }
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|err| miette!("failed to create learn episode root {}: {err}", root.display()))?;
    Ok(root)
}

async fn prepare_learning_selection_episode_root(
    session_root: &Path,
    batch_index: usize,
    variant_name: &str,
    task: &EpisodeTask,
    task_index: usize,
) -> Result<PathBuf> {
    let root = session_root
        .join("selection")
        .join(format!("batch-{:04}", batch_index))
        .join(slugify(variant_name))
        .join(format!("{:04}-{}", task_index, slugify(&task.id)));
    if root.exists() {
        tokio::fs::remove_dir_all(&root)
            .await
            .map_err(|err| miette!("failed to clear selection episode root {}: {err}", root.display()))?;
    }
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|err| miette!("failed to create selection episode root {}: {err}", root.display()))?;
    Ok(root)
}

async fn provision_episode_workspace(task: &EpisodeTask, workspace_dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(workspace_dir)
        .await
        .map_err(|err| miette!("failed to create episode workspace {}: {err}", workspace_dir.display()))?;

    if let Some(repo) = task.metadata.get("repo") {
        let remote = infer_repo_remote(repo);
        println!("          clone repo={} from {}", repo, remote);
        run_host_command(
            &["git", "clone", remote.as_str(), workspace_dir.to_string_lossy().as_ref()],
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
        .execute_focused(crate::device::DeviceAction::TerminalInput { text: cd_command })
        .await
        .map_err(|err| miette!("failed to enter episode workspace in terminal: {err}"))?;
    context
        .devices
        .wait_until_settled(Duration::from_millis(300), Duration::from_secs(2))
        .await;
    Ok(())
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

async fn spinova_loop(context: &mut Context, tx: &tokio::sync::watch::Sender<DashboardState>) {
    let cycle_started_at = std::time::Instant::now();
    context
        .devices
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    let snapshot = Snapshot::new(context).await;
    let renderer = OpenAIToolRenderer;
    let runtime_policy = RuntimePolicyProgram;
    let outcome = runtime_policy.run_once(context, &snapshot, &renderer).await;
    let output = outcome.output;
    if should_record_effect(&output.effect) {
        let mut evidence = collect_memory_evidence(&output.effect);
        evidence.extend(collect_contextual_memory_evidence(context, &output.effect));
        let memory_entry = encode_memory_entry(
            context,
            &snapshot,
            &renderer,
            &output.current_doing,
            &output.observation,
            &output.description,
            &evidence,
        )
        .await
        .unwrap_or_else(|_| fallback_memory_encoding(&output, &evidence));
        context
            .memory
            .record_encoded(
                memory_entry.thread_focus,
                memory_entry.event_summary,
                memory_entry.anchors,
                memory_entry.thread_effect,
            )
            .await;
    }
    apply_effect(context, output.effect).await;
    if outcome.touched_working_task {
        context.tasks.touch_working_task();
    }
    sync_dashboard_state(context, tx, Some(cycle_started_at.elapsed().as_millis()));
}

fn should_record_effect(effect: &Effect) -> bool {
    !matches!(effect, Effect::SilentWait)
}

fn collect_memory_evidence(effect: &Effect) -> Vec<String> {
    match effect {
        Effect::TaskAdd {
            description,
            project_id,
        } => {
            let mut evidence = vec![format!("新增任务：{description}")];
            if let Some(project_id) = project_id {
                evidence.push(format!("关联项目引用：{project_id}"));
            }
            evidence
        }
        Effect::TaskDelete { task_id } => vec![format!("删除任务引用：{task_id}")],
        Effect::TaskSelect { task_id } => vec![format!("选中任务引用：{task_id}")],
        Effect::ResolveTelegramChat {
            chat_id,
            resolution,
        } => {
            let mut evidence = vec![format!("处理 Telegram 会话：{chat_id}")];
            match resolution {
                TelegramResolution::ReplyOnly { reply }
                | TelegramResolution::AskClarification { reply }
                | TelegramResolution::Decline { reply } => {
                    evidence.push(format!("回复内容：{reply}"));
                }
                TelegramResolution::AcceptAsProject {
                    reply,
                    project_title,
                    success_criteria,
                    first_next_action,
                } => {
                    evidence.push(format!("项目标题：{project_title}"));
                    evidence.push(format!("成功标准：{success_criteria}"));
                    if let Some(reply) = reply {
                        evidence.push(format!("回复内容：{reply}"));
                    }
                    if let Some(first_next_action) = first_next_action {
                        evidence.push(format!("首个下一步动作：{first_next_action}"));
                    }
                }
                TelegramResolution::NoReplyNeeded => {}
            }
            evidence
        }
        Effect::ObligationSatisfy { obligation_id } => {
            vec![format!("完成义务引用：{obligation_id}")]
        }
        Effect::CommitToProject {
            obligation_id,
            title,
            success_criteria,
            initial_next_action,
            acknowledgment,
        } => {
            let mut evidence = vec![
                format!("承诺义务引用：{obligation_id}"),
                format!("项目标题：{title}"),
                format!("成功标准：{success_criteria}"),
            ];
            if let Some(initial_next_action) = initial_next_action {
                evidence.push(format!("初始下一步动作：{initial_next_action}"));
            }
            if let Some(acknowledgment) = acknowledgment {
                evidence.push(format!("承诺回复：{acknowledgment}"));
            }
            evidence
        }
        Effect::ProjectComplete {
            project_id,
            summary,
        } => {
            vec![
                format!("完成项目引用：{project_id}"),
                format!("完成总结：{summary}"),
            ]
        }
        Effect::FocusDevice { device } => vec![format!("切换前景设备：{device}")],
        Effect::PutAwayDevice => vec!["收起当前前景设备".to_string()],
        Effect::DeviceAction { action } => match action {
            crate::device::DeviceAction::TerminalInput { text } => {
                vec![format!("终端实际输入：{}", text.trim_end())]
            }
            crate::device::DeviceAction::TelegramSelectChat { chat_id } => {
                vec![format!("打开 Telegram 会话：{chat_id}")]
            }
            crate::device::DeviceAction::TelegramSendMessage { text } => {
                vec![format!("实际发送消息：{text}")]
            }
        },
        Effect::Wait => vec!["本轮选择等待".to_string()],
        Effect::SilentWait => Vec::new(),
    }
}

fn collect_contextual_memory_evidence(context: &Context, effect: &Effect) -> Vec<String> {
    let include_selected_chat = matches!(
        effect,
        Effect::ResolveTelegramChat { .. }
            | Effect::FocusDevice {
                device: DeviceId::Telegram
            }
            | Effect::DeviceAction {
                action: crate::device::DeviceAction::TelegramSelectChat { .. }
            }
            | Effect::DeviceAction {
                action: crate::device::DeviceAction::TelegramSendMessage { .. }
            }
            | Effect::Wait
            | Effect::SilentWait
            | Effect::TaskAdd { .. }
            | Effect::TaskSelect { .. }
    );

    if include_selected_chat {
        context.telegram.selected_chat_memory_evidence()
    } else {
        Vec::new()
    }
}

async fn encode_memory_entry(
    context: &Context,
    snapshot: &Snapshot,
    renderer: &OpenAIToolRenderer,
    thread_focus: &str,
    observation: &str,
    action_description: &str,
    evidence: &[String],
) -> miette::Result<MemoryEncodingOutput> {
    let program = MemoryEncodingProgram {
        thread_focus: thread_focus.to_string(),
        observation: observation.to_string(),
        action_description: action_description.to_string(),
        evidence: if evidence.is_empty() {
            "无额外证据".to_string()
        } else {
            evidence.join("\n")
        },
    };
    execute_program(context.llm.as_ref(), context, snapshot, renderer, &program).await
}

fn fallback_memory_encoding(
    output: &crate::core::Output,
    evidence: &[String],
) -> MemoryEncodingOutput {
    MemoryEncodingOutput {
        thread_focus: output.current_doing.clone(),
        event_summary: format!(
            "观察与结论：{}\n采取动作：{}",
            output.observation.trim(),
            output.description.trim()
        ),
        anchors: evidence.to_vec(),
        thread_effect: infer_memory_thread_effect(&output.observation, &output.description),
    }
}

fn infer_memory_thread_effect(observation: &str, action_description: &str) -> String {
    let text = format!("{observation}\n{action_description}");
    if ["完成", "已完成", "结束", "成功标准已达到"]
        .iter()
        .any(|needle| text.contains(needle))
    {
        "completed".to_string()
    } else if [
        "失败", "404", "无法", "无效", "受阻", "报错", "中断", "卡住",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        "blocked".to_string()
    } else if ["补充说明", "澄清", "确认", "请确认", "请求提供"]
        .iter()
        .any(|needle| text.contains(needle))
    {
        "clarified".to_string()
    } else if ["切换", "转到", "改为", "聚焦到"]
        .iter()
        .any(|needle| text.contains(needle))
    {
        "switched".to_string()
    } else {
        "continue".to_string()
    }
}

async fn apply_effect(context: &mut Context, effect: Effect) {
    match effect {
        Effect::TaskAdd {
            description,
            project_id,
        } => {
            let project_id = project_id
                .as_deref()
                .map(|project_id| resolve_project_reference(context, project_id))
                .transpose();
            match project_id {
                Ok(project_id) => {
                    context.tasks.add_task_with_project(description, project_id);
                }
                Err(err) => eprintln!("{err:?}"),
            }
        }
        Effect::TaskDelete { task_id } => match resolve_task_reference(context, &task_id) {
            Ok(id) => {
                context.tasks.delete_task(id);
            }
            Err(err) => eprintln!("{err:?}"),
        },
        Effect::TaskSelect { task_id } => match resolve_task_reference(context, &task_id) {
            Ok(id) => {
                context.tasks.select_working_task(id);
            }
            Err(err) => eprintln!("{err:?}"),
        },
        Effect::ResolveTelegramChat {
            chat_id,
            resolution,
        } => {
            if let Err(err) = execute_resolve_telegram_chat(context, &chat_id, resolution).await {
                eprintln!("{err:?}");
            }
        }
        Effect::ObligationSatisfy { obligation_id } => {
            match resolve_obligation_reference(context, &obligation_id) {
                Ok(id) => {
                    context
                        .obligations
                        .set_status(id, ObligationStatus::Satisfied);
                }
                Err(err) => eprintln!("{err:?}"),
            }
        }
        Effect::CommitToProject {
            obligation_id,
            title,
            success_criteria,
            initial_next_action,
            acknowledgment,
        } => {
            if let Err(err) = execute_commit_to_project(
                context,
                &obligation_id,
                title,
                success_criteria,
                initial_next_action,
                acknowledgment,
            )
            .await
            {
                eprintln!("{err:?}");
            }
        }
        Effect::ProjectComplete {
            project_id,
            summary,
        } => {
            if let Err(err) = execute_project_complete(context, &project_id, summary) {
                eprintln!("{err:?}");
            }
        }
        Effect::FocusDevice { device } => {
            if let Err(err) = context.devices.focus(device).await {
                eprintln!("{err:?}");
            }
        }
        Effect::PutAwayDevice => {
            if let Err(err) = context.devices.put_away().await {
                eprintln!("{err:?}");
            }
        }
        Effect::DeviceAction { action } => {
            if let Err(err) = context.devices.execute_focused(action).await {
                eprintln!("{err:?}");
            }
        }
        Effect::Wait => {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        Effect::SilentWait => {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
}

fn sync_dashboard_state(
    context: &Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    last_cycle_elapsed_ms: Option<u128>,
) {
    tx.send_modify(|state| {
        let focused_render = context.devices.focused_render();
        state.focused_device = context.devices.focused();
        state.focused_title = focused_render.as_ref().map(|view| view.title.clone());
        state.focused_content = focused_render.as_ref().map(|view| view.content.clone());
        state.obligations = render_obligations_for_dashboard(context);
        state.projects = render_projects_for_dashboard(context);
        state.tasks = context
            .tasks
            .tasks()
            .map(|(id, task)| (id, render_task_for_dashboard(task, context)))
            .collect();
        state.working_task = context.tasks.working_task();
        state.latest_trail = context.memory.trail().last().cloned();
        state.last_cycle_elapsed_ms = last_cycle_elapsed_ms;
    });
}

fn render_obligations_for_dashboard(context: &Context) -> Vec<String> {
    let mut obligations = context.obligations.active_obligations().collect::<Vec<_>>();
    obligations.sort_by_key(|(id, _)| id.to_string());
    obligations
        .into_iter()
        .map(|(_, obligation)| {
            format!(
                "[{} / {} / reply={}] {}",
                obligation.status,
                obligation.urgency,
                if obligation.requires_reply {
                    "yes"
                } else {
                    "no"
                },
                obligation.summary
            )
        })
        .collect()
}

fn render_projects_for_dashboard(context: &Context) -> Vec<String> {
    let mut projects = context.projects.projects().collect::<Vec<_>>();
    projects.sort_by_key(|(id, _)| id.to_string());
    projects
        .into_iter()
        .map(|(_, project)| {
            format!(
                "[{} / {}] {}",
                project.status, project.origin, project.title
            )
        })
        .collect()
}

fn render_task_for_dashboard(task: &crate::tasks::Task, context: &Context) -> DashboardTaskEntry {
    let description_tail = truncate_from_left(&task.description, 42);
    let last_touched = format_last_touched(task.last_touched_at_ms);

    let display = match task.project_id {
        Some(project_id) => {
            let project_title = context
                .projects
                .projects()
                .find(|(id, _)| *id == project_id)
                .map(|(_, project)| project.title.clone())
                .unwrap_or_else(|| project_id.to_string());
            format!(
                "{description_tail}\n  上次处理: {last_touched} | 项目: {}",
                truncate_from_left(&project_title, 18)
            )
        }
        None => format!("{description_tail}\n  上次处理: {last_touched}"),
    };

    DashboardTaskEntry {
        display,
        last_touched_at_ms: task.last_touched_at_ms,
    }
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

fn normalize_reference(reference: &str) -> String {
    reference.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve_reference(
    kind: &str,
    reference: &str,
    candidates: Vec<(Uuid, String)>,
) -> miette::Result<Uuid> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(miette!("{kind} reference is empty"));
    }

    if let Ok(id) = Uuid::parse_str(reference) {
        if candidates
            .iter()
            .any(|(candidate_id, _)| *candidate_id == id)
        {
            return Ok(id);
        }
        return Err(miette!("unknown {kind} id: {reference}"));
    }

    let normalized_reference = normalize_reference(reference);
    let exact_matches = candidates
        .iter()
        .filter_map(|(id, label)| {
            (normalize_reference(label) == normalized_reference).then_some(*id)
        })
        .collect::<Vec<_>>();
    if exact_matches.len() == 1 {
        return Ok(exact_matches[0]);
    }
    if exact_matches.len() > 1 {
        return Err(miette!(
            "ambiguous {kind} reference `{reference}`: matched {} items by exact description/title",
            exact_matches.len()
        ));
    }

    let fuzzy_matches = candidates
        .iter()
        .filter_map(|(id, label)| {
            let normalized_label = normalize_reference(label);
            (normalized_label.contains(&normalized_reference)
                || normalized_reference.contains(&normalized_label))
            .then_some(*id)
        })
        .collect::<Vec<_>>();
    if fuzzy_matches.len() == 1 {
        return Ok(fuzzy_matches[0]);
    }
    if fuzzy_matches.len() > 1 {
        return Err(miette!(
            "ambiguous {kind} reference `{reference}`: matched {} items fuzzily",
            fuzzy_matches.len()
        ));
    }

    Err(miette!(
        "invalid {kind} reference `{reference}`: expected a UUID from the snapshot, or a unique matching title/summary"
    ))
}

fn resolve_string_reference(
    kind: &str,
    reference: &str,
    candidates: Vec<(String, String)>,
) -> miette::Result<String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(miette!("{kind} reference is empty"));
    }

    if let Some((id, _)) = candidates
        .iter()
        .find(|(candidate_id, _)| candidate_id == reference)
    {
        return Ok(id.clone());
    }

    let normalized_reference = normalize_reference(reference);
    let exact_matches = candidates
        .iter()
        .filter_map(|(id, label)| {
            (normalize_reference(label) == normalized_reference).then_some(id.clone())
        })
        .collect::<Vec<_>>();
    if exact_matches.len() == 1 {
        return Ok(exact_matches[0].clone());
    }
    if exact_matches.len() > 1 {
        return Err(miette!(
            "ambiguous {kind} reference `{reference}`: matched {} items by exact description/title",
            exact_matches.len()
        ));
    }

    let fuzzy_matches = candidates
        .iter()
        .filter_map(|(id, label)| {
            let normalized_label = normalize_reference(label);
            (normalized_label.contains(&normalized_reference)
                || normalized_reference.contains(&normalized_label))
            .then_some(id.clone())
        })
        .collect::<Vec<_>>();
    if fuzzy_matches.len() == 1 {
        return Ok(fuzzy_matches[0].clone());
    }
    if fuzzy_matches.len() > 1 {
        return Err(miette!(
            "ambiguous {kind} reference `{reference}`: matched {} items fuzzily",
            fuzzy_matches.len()
        ));
    }

    Err(miette!(
        "invalid {kind} reference `{reference}`: expected a chat id from the device view, or a unique matching title"
    ))
}

fn resolve_task_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_reference(
        "task",
        reference,
        context
            .tasks
            .tasks()
            .map(|(id, task)| (id, task.description.clone()))
            .collect(),
    )
}

fn resolve_obligation_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_reference(
        "obligation",
        reference,
        context
            .obligations
            .obligations()
            .map(|(id, obligation)| (id, obligation.summary.clone()))
            .collect(),
    )
}

fn resolve_project_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_reference(
        "project",
        reference,
        context
            .projects
            .projects()
            .map(|(id, project)| (id, project.title.clone()))
            .collect(),
    )
}

fn resolve_telegram_chat_reference(context: &Context, reference: &str) -> miette::Result<String> {
    resolve_string_reference("telegram chat", reference, context.telegram.chat_refs())
}

fn trim_optional_field(value: Option<String>) -> Option<String> {
    value.and_then(trim_required_field)
}

fn trim_required_field(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn require_field(value: String, field_name: &str) -> miette::Result<String> {
    trim_required_field(value).ok_or_else(|| miette!("missing required field: {field_name}"))
}

async fn send_telegram_message(
    context: &mut Context,
    chat_id: &str,
    text: String,
) -> miette::Result<()> {
    context.devices.focus(DeviceId::Telegram).await?;
    context
        .devices
        .execute_focused(crate::device::DeviceAction::TelegramSelectChat {
            chat_id: chat_id.to_string(),
        })
        .await?;
    context
        .devices
        .execute_focused(crate::device::DeviceAction::TelegramSendMessage { text })
        .await?;
    Ok(())
}

async fn execute_resolve_telegram_chat(
    context: &mut Context,
    chat_reference: &str,
    resolution: TelegramResolution,
) -> miette::Result<()> {
    let chat_id = resolve_telegram_chat_reference(context, chat_reference)?;

    match resolution {
        TelegramResolution::ReplyOnly { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::AskClarification { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::Decline { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::NoReplyNeeded => {
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::AcceptAsProject {
            reply,
            project_title,
            success_criteria,
            first_next_action,
        } => {
            let project_title = require_field(project_title, "project_title")?;
            let success_criteria = require_field(success_criteria, "success_criteria")?;
            let project_id = context.projects.add(
                project_title,
                ProjectOrigin::Telegram,
                success_criteria,
                Some(crate::projects::ReportTarget {
                    device: DeviceId::Telegram,
                    target: chat_id.clone(),
                }),
            );

            if let Some(next_action) = trim_optional_field(first_next_action) {
                let task_id = context
                    .tasks
                    .add_task_with_project(next_action, Some(project_id));
                context.tasks.select_working_task(task_id);
            }

            if let Some(reply) = trim_optional_field(reply) {
                send_telegram_message(context, &chat_id, reply).await?;
                context.telegram.resolve_chat(&chat_id, Some(false))?;
            } else {
                context.telegram.resolve_chat(&chat_id, None)?;
            }
        }
    }

    Ok(())
}

async fn execute_commit_to_project(
    context: &mut Context,
    obligation_id: &str,
    title: String,
    success_criteria: String,
    initial_next_action: Option<String>,
    acknowledgment: Option<String>,
) -> miette::Result<()> {
    let obligation_id = resolve_obligation_reference(context, obligation_id)?;
    let Some(obligation) = context.obligations.get(obligation_id).cloned() else {
        return Err(miette!("unknown obligation: {obligation_id}"));
    };

    let project_id = context.projects.add(
        title,
        project_origin_from(obligation.source),
        success_criteria,
        obligation.reply_target.clone(),
    );
    context.obligations.link_project(obligation_id, project_id);

    if let Some(next_action) = initial_next_action.map(|s| s.trim().to_string()) {
        if !next_action.is_empty() {
            let task_id = context
                .tasks
                .add_task_with_project(next_action, Some(project_id));
            context.tasks.select_working_task(task_id);
        }
    }

    if let Some(ack) = acknowledgment.map(|s| s.trim().to_string()) {
        if !ack.is_empty() {
            enqueue_obligation_acknowledgment(context, obligation_id, &obligation, ack).await?;
            return Ok(());
        }
    }

    if obligation.requires_reply {
        context
            .obligations
            .set_status(obligation_id, ObligationStatus::Seen);
    } else {
        context
            .obligations
            .set_status(obligation_id, ObligationStatus::Satisfied);
    }
    Ok(())
}

async fn enqueue_obligation_acknowledgment(
    context: &mut Context,
    obligation_id: Uuid,
    obligation: &crate::obligations::Obligation,
    acknowledgment: String,
) -> miette::Result<()> {
    let Some(target) = obligation.reply_target.clone() else {
        return Err(miette!("obligation {obligation_id} has no reply target"));
    };

    match target.device {
        DeviceId::Telegram => {
            context.devices.focus(DeviceId::Telegram).await?;
            context
                .devices
                .execute_focused(crate::device::DeviceAction::TelegramSelectChat {
                    chat_id: target.target,
                })
                .await?;
            context
                .devices
                .execute_focused(crate::device::DeviceAction::TelegramSendMessage {
                    text: acknowledgment,
                })
                .await?;
            context
                .obligations
                .set_status(obligation_id, ObligationStatus::Seen);
            Ok(())
        }
        DeviceId::Terminal => Err(miette!(
            "terminal obligations do not support external acknowledgment"
        )),
    }
}

fn project_origin_from(source: ObligationSource) -> ProjectOrigin {
    match source {
        ObligationSource::Telegram => ProjectOrigin::Telegram,
        ObligationSource::Terminal => ProjectOrigin::Terminal,
        ObligationSource::System => ProjectOrigin::System,
    }
}

fn execute_project_complete(
    context: &mut Context,
    project_id: &str,
    summary: String,
) -> miette::Result<()> {
    let project_id = resolve_project_reference(context, project_id)?;
    let Some(project) = context.projects.get(project_id).cloned() else {
        return Err(miette!("unknown project: {project_id}"));
    };

    context
        .projects
        .set_status(project_id, crate::projects::ProjectStatus::Completed);
    context.tasks.delete_tasks_for_project(project_id);

    if let Some(target) = project.report_back_to {
        context.obligations.add(
            match target.device {
                DeviceId::Telegram => ObligationSource::Telegram,
                DeviceId::Terminal => ObligationSource::Terminal,
            },
            format!(
                "把项目《{}》的结果回复给对方：{}",
                project.title,
                summary.trim()
            ),
            true,
            crate::obligations::Urgency::High,
            Some(project_id),
            Some(target),
        );
    }

    Ok(())
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
