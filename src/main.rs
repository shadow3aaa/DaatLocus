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

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use chrono::{Local, TimeZone};
use miette::{Result, miette};
use serde::Deserialize;
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
            BENCH_COMPILED_DIR_NAME, COMPILED_DIR_NAME, CompiledPromptStore,
            load_all_compiled_programs,
        },
        environment::EpisodeObservation,
        episode::{EpisodeMetric, EpisodeOutcome, EpisodeStatus, EpisodeStep, EpisodeTask},
        episode_harness::EpisodeHarness,
        eval::run_reasoning_eval,
        optimize::run_reasoning_optimize,
        programs::completion_judge::{CompletionJudgeOutput, CompletionJudgeProgram},
        programs::memory_encoding::{MemoryEncodingOutput, MemoryEncodingProgram},
        programs::task_understanding::{TaskUnderstandingOutput, TaskUnderstandingProgram},
        render::openai_tools::OpenAIToolRenderer,
        runtime::execute_program,
        runtime_policy::RuntimePolicyProgram,
        sleep::run_sleep,
        sleep_artifacts::SleepArtifactSuggestedFixKind,
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
    if is_internal_cli_command(&args) {
        run_internal_cli(&args).await?;
        return Ok(());
    }

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
    initialize_injected_cli_tools(&mut context).await?;

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
    if let Err(err) = initialize_injected_cli_tools(&mut context).await {
        eprintln!("{err:?}");
    }
    context
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
    let tasks = source.into_episode_tasks(64);
    let summary = EpisodeHarness::summarize_tasks(&tasks, 5);
    print_episode_batch_summary(path, &summary);
    Ok(())
}

async fn run_train_source_rollout(config: crate::config::Config, path: &str, task_index: usize) -> Result<()> {
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
    let seeded_task_id = context.tasks.add_task(task.instruction.clone());
    context.tasks.select_working_task(seeded_task_id);

    let outcome = rollout_runtime_policy_episode(&mut context, task, &workspace_dir).await?;
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
                    promoted_l3_entries: sleep_summary.promoted_l3_entries,
                    compiled_prompt_count: current_compiled_count,
                    optimized_suites: 0,
                });
            })
            .await?;
        println!(
            "  batch {} sleep finished: patterns={} demos={} stress={} instructions={} l3={}",
            batch_index + 1,
            sleep_summary.failure_patterns.len(),
            sleep_summary.bootstrap_demos,
            sleep_summary.stress_cases,
            sleep_summary.instruction_hypotheses,
            sleep_summary.promoted_l3_entries
        );
        sync_learning_assets_back_to_shared(&session_learning_home, &shared_learning_home).await?;

        println!("  batch {} optimize starting", batch_index + 1);
        let optimization_results = run_reasoning_optimize(&optimize_context).await?;
        optimize_context.shutdown().await;
        sync_learning_assets_back_to_shared(&session_learning_home, &shared_learning_home).await?;

        let compiled_prompt_count = load_compiled_prompts_only().await?.len();
        session
            .update(|state| {
                state.optimize_runs += 1;
                state.last_compiled_prompt_count = compiled_prompt_count;
                if let Some(last_report) = state.batch_reports.last_mut() {
                    last_report.compiled_prompt_count = compiled_prompt_count;
                    last_report.optimized_suites = optimization_results.len();
                }
            })
            .await?;
        println!(
            "  batch {} optimize finished: compiled_suites={} compiled_prompt_count={}",
            batch_index + 1,
            optimization_results.len(),
            compiled_prompt_count
        );

        println!(
            "  batch update: completed={} active_variant={} sleep_patterns={} demos={} stress={} instructions={} l3={} compiled_suites={}",
            session.snapshot().await.completed_tasks,
            active_variant,
            sleep_summary.failure_patterns.len(),
            sleep_summary.bootstrap_demos,
            sleep_summary.stress_cases,
            sleep_summary.instruction_hypotheses,
            sleep_summary.promoted_l3_entries,
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
    context
        .tasks
        .set_working_task_description(compressed_task);
    context.tasks.set_working_task_guidance(
        task_understanding.key_anchors.clone(),
        task_understanding.investigation_plan.clone(),
    );
    context
        .tasks
        .set_working_task_phase("investigate".to_string());
    let initial_snapshot = Snapshot::new(context).await.to_string();
    let initial_observation = EpisodeObservation {
        summary: format!("task seeded: {}", task.title),
        snapshot_text: initial_snapshot,
        metadata: std::collections::BTreeMap::new(),
    };
    let mut work_phase = "investigate".to_string();

    for index in 0..task.max_steps {
        context.tasks.set_working_task_phase(work_phase.clone());
        let step_execution = execute_runtime_policy_step(context, &renderer).await;
        let output = step_execution.output;
        let effect = output.effect.clone();

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
                .tasks
                .set_working_task_verify_pending_check(completion.next_check.clone());
        } else {
            context.tasks.set_working_task_verify_pending_check(None);
        }
        metadata.insert("work_phase".to_string(), next_work_phase.clone());
        let should_stop = should_abort_after_repeated_wait(
            steps.last(),
            &effect,
            &next_work_phase,
        );
        steps.push(EpisodeStep {
            index,
            module: "runtime_policy".to_string(),
            effect: effect.clone(),
            observation_summary: output.observation,
            snapshot_text: step_execution.snapshot_text,
            metadata,
        });

        work_phase = next_work_phase;
        context.tasks.set_working_task_phase(work_phase.clone());

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

        if context.tasks.is_empty() || context.tasks.working_task().is_none() || should_stop {
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
                "step={} effect={:?} doing={} reason={}",
                step.index,
                step.effect,
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
        task_goal: task
            .task_goal
            .clone()
            .unwrap_or_else(|| task.title.clone()),
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
    execute_program(context.judge_llm.as_ref(), context, &snapshot, renderer, &program).await
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

fn should_abort_after_repeated_wait(
    previous: Option<&EpisodeStep>,
    current_effect: &Effect,
    next_work_phase: &str,
) -> bool {
    if !matches!(current_effect, Effect::Wait | Effect::SilentWait) {
        return false;
    }
    let Some(previous) = previous else {
        return false;
    };
    let repeated_wait = matches!(
        (&previous.effect, current_effect),
        (Effect::Wait, Effect::Wait) | (Effect::SilentWait, Effect::SilentWait)
    );
    if !repeated_wait {
        return false;
    }
    matches!(next_work_phase, "investigate" | "blocked")
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
        if context.tasks.is_empty() || context.tasks.working_task().is_none() {
            EpisodeStatus::Succeeded
        } else if steps.len() >= task.max_steps {
            EpisodeStatus::MaxStepsExceeded
        } else {
            EpisodeStatus::Aborted
        }
    });
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
    let repeated_waits = steps
        .windows(2)
        .filter(|pair| {
            matches!(
                (&pair[0].effect, &pair[1].effect),
                (Effect::Wait, Effect::Wait)
                    | (Effect::SilentWait, Effect::SilentWait)
            )
        })
        .count();
    let repeated_terminal_loops = count_repeated_terminal_loops(steps);
    let repeated_effects = repeated_waits + repeated_terminal_loops;

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
        let Some(signature) = repeated_terminal_loop_signature(&step.effect) else {
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

fn repeated_terminal_loop_signature(effect: &Effect) -> Option<String> {
    let Effect::DeviceAction {
        action: crate::device::DeviceAction::TerminalInput { text },
    } = effect
    else {
        return None;
    };
    let trimmed = text.trim();
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
    prefixes
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix).map(|rest| format!("{prefix}{rest}")))
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

#[derive(serde::Serialize, Clone)]
struct TrainSourceLearnBatchReport {
    completed_tasks: usize,
    active_variant: String,
    sleep_failure_patterns: usize,
    sleep_bootstrap_demos: usize,
    sleep_stress_cases: usize,
    sleep_instruction_hypotheses: usize,
    promoted_l3_entries: usize,
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

async fn prepare_learning_home_root(session_root: &Path) -> Result<PathBuf> {
    let root = session_root.join("learning_home");
    if root.exists() {
        tokio::fs::remove_dir_all(&root)
            .await
            .map_err(|err| miette!("failed to clear learn session home {}: {err}", root.display()))?;
    }
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|err| miette!("failed to create learn session home {}: {err}", root.display()))?;
    Ok(root)
}

async fn sync_learning_assets_to_session(shared_home: &Path, session_home: &Path) -> Result<()> {
    for name in [COMPILED_DIR_NAME, "sleep_artifacts", "l3_memory"] {
        sync_path_replace(&shared_home.join(name), &session_home.join(name)).await?;
    }
    Ok(())
}

async fn sync_learning_assets_back_to_shared(session_home: &Path, shared_home: &Path) -> Result<()> {
    for name in [COMPILED_DIR_NAME, "sleep_artifacts", "l3_memory"] {
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
        tokio::fs::copy(src, dst)
            .await
            .map_err(|err| miette!("failed to copy {} -> {}: {err}", src.display(), dst.display()))?;
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
        tokio::fs::remove_dir_all(&root)
            .await
            .map_err(|err| miette!("failed to clear learn episode root {}: {err}", root.display()))?;
    }
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|err| miette!("failed to create learn episode root {}: {err}", root.display()))?;
    Ok(root)
}

async fn provision_episode_workspace(task: &EpisodeTask, workspace_dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(workspace_dir)
        .await
        .map_err(|err| miette!("failed to create episode workspace {}: {err}", workspace_dir.display()))?;

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
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|err| miette!("failed to create train-source repo cache root {}: {err}", root.display()))?;
    Ok(root)
}

async fn ensure_cached_repo(repo: &str, remote: &str, cache_repo: &Path) -> Result<()> {
    if cache_repo.exists() {
        println!("          repo cache hit for {} at {}", repo, cache_repo.display());
        return Ok(());
    }

    println!("          repo cache miss for {} -> {}", repo, cache_repo.display());
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
        .execute_focused(crate::device::DeviceAction::TerminalInput { text: cd_command })
        .await
        .map_err(|err| miette!("failed to enter episode workspace in terminal: {err}"))?;
    context
        .devices
        .wait_until_settled(Duration::from_millis(300), Duration::from_secs(2))
        .await;
    Ok(())
}

fn is_internal_cli_command(args: &[String]) -> bool {
    matches!(args, [command, subcommand, ..] if command == "internal-cli" && subcommand == "edit")
}

async fn run_internal_cli(args: &[String]) -> Result<()> {
    match args {
        [_, edit, subcommand, rest @ ..] if edit == "edit" && subcommand == "patch" => {
            run_internal_cli_edit_patch(rest).await
        }
        [_, edit, subcommand, rest @ ..] if edit == "edit" && subcommand == "replace" => {
            run_internal_cli_edit_replace(rest).await
        }
        [_, edit, subcommand, rest @ ..] if edit == "edit" && subcommand == "show" => {
            run_internal_cli_edit_show(rest).await
        }
        [_, edit, subcommand, ..] if edit == "edit" => Err(miette!(
            "unknown internal edit subcommand: {subcommand}"
        )),
        _ => Err(miette!("unsupported internal-cli invocation")),
    }
}

async fn run_internal_cli_edit_patch(args: &[String]) -> Result<()> {
    let mut patch_file: Option<PathBuf> = None;
    let mut use_stdin = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--patch-file" => {
                i += 1;
                patch_file = args.get(i).map(PathBuf::from);
            }
            "--stdin" => {
                use_stdin = true;
            }
            flag => return Err(miette!("unknown spin-edit patch flag: {flag}")),
        }
        i += 1;
    }

    if use_stdin && patch_file.is_some() {
        return Err(miette!(
            "spin-edit patch accepts either --stdin or --patch-file, not both"
        ));
    }
    let patch_text = if use_stdin {
        read_stdin_to_string().await?
    } else {
        let patch_file =
            patch_file.ok_or_else(|| miette!("spin-edit patch requires --stdin or --patch-file"))?;
        tokio::fs::read_to_string(&patch_file)
            .await
            .map_err(|err| miette!("failed to read patch file {}: {err}", patch_file.display()))?
    };
    let cwd = env::current_dir().map_err(|err| miette!("failed to read current directory: {err}"))?;
    let summary = apply_spin_edit_patch(&cwd, &patch_text).await?;
    println!("changed_files={}", summary.changed_files);
    println!("added_files={}", summary.added_files);
    println!("deleted_files={}", summary.deleted_files);
    println!("updated_files={}", summary.updated_files);
    for path in summary.paths {
        println!("path={path}");
    }
    Ok(())
}

async fn run_internal_cli_edit_replace(args: &[String]) -> Result<()> {
    let mut file: Option<String> = None;
    let mut old_text: Option<String> = None;
    let mut new_text: Option<String> = None;
    let mut old_file: Option<PathBuf> = None;
    let mut new_file: Option<PathBuf> = None;
    let mut spec_stdin = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                i += 1;
                file = args.get(i).cloned();
            }
            "--old" => {
                i += 1;
                old_text = args.get(i).cloned();
            }
            "--new" => {
                i += 1;
                new_text = args.get(i).cloned();
            }
            "--old-file" => {
                i += 1;
                old_file = args.get(i).map(PathBuf::from);
            }
            "--new-file" => {
                i += 1;
                new_file = args.get(i).map(PathBuf::from);
            }
            "--spec-stdin" => {
                spec_stdin = true;
            }
            flag => {
                return Err(miette!("unknown spin-edit replace flag: {flag}"));
            }
        }
        i += 1;
    }

    #[derive(Deserialize)]
    struct ReplaceSpec {
        file: String,
        old_text: String,
        new_text: String,
    }

    let (file, old_text, new_text) = if spec_stdin {
        if file.is_some() || old_text.is_some() || new_text.is_some() || old_file.is_some() || new_file.is_some() {
            return Err(miette!(
                "spin-edit replace --spec-stdin must not be combined with --file/--old/--new/--old-file/--new-file"
            ));
        }
        let raw = read_stdin_to_string().await?;
        let spec: ReplaceSpec = serde_json::from_str(&raw)
            .map_err(|err| miette!("invalid spin-edit replace JSON on stdin: {err}"))?;
        (spec.file, spec.old_text, spec.new_text)
    } else {
        let file = file.ok_or_else(|| miette!("spin-edit replace requires --file or --spec-stdin"))?;
        let old_text = match (old_text, old_file) {
            (Some(text), None) => text,
            (None, Some(path)) => tokio::fs::read_to_string(&path)
                .await
                .map_err(|err| miette!("failed to read --old-file {}: {err}", path.display()))?,
            (Some(_), Some(_)) => {
                return Err(miette!(
                    "spin-edit replace accepts either --old or --old-file, not both"
                ));
            }
            (None, None) => return Err(miette!("spin-edit replace requires --old or --old-file")),
        };
        let new_text = match (new_text, new_file) {
            (Some(text), None) => text,
            (None, Some(path)) => tokio::fs::read_to_string(&path)
                .await
                .map_err(|err| miette!("failed to read --new-file {}: {err}", path.display()))?,
            (Some(_), Some(_)) => {
                return Err(miette!(
                    "spin-edit replace accepts either --new or --new-file, not both"
                ));
            }
            (None, None) => return Err(miette!("spin-edit replace requires --new or --new-file")),
        };
        (file, old_text, new_text)
    };

    let cwd = env::current_dir().map_err(|err| miette!("failed to read current directory: {err}"))?;
    let path = resolve_relative_path_within_root(&cwd, &file, "spin-edit replace")?;
    let report = execute_precise_file_replace(&path, &old_text, &new_text, "spin-edit replace").await?;
    print_precise_edit_report(&file, &report);
    Ok(())
}

async fn read_stdin_to_string() -> Result<String> {
    use tokio::io::AsyncReadExt;

    let mut stdin = tokio::io::stdin();
    let mut content = String::new();
    stdin
        .read_to_string(&mut content)
        .await
        .map_err(|err| miette!("failed to read stdin: {err}"))?;
    Ok(content)
}

async fn run_internal_cli_edit_show(args: &[String]) -> Result<()> {
    let mut file: Option<String> = None;
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                i += 1;
                file = args.get(i).cloned();
            }
            "--start" => {
                i += 1;
                start = args
                    .get(i)
                    .ok_or_else(|| miette!("missing value for --start"))?
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|err| miette!("invalid --start value: {err}"))?;
            }
            "--end" => {
                i += 1;
                end = args
                    .get(i)
                    .ok_or_else(|| miette!("missing value for --end"))?
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|err| miette!("invalid --end value: {err}"))?;
            }
            flag => return Err(miette!("unknown spin-edit show flag: {flag}")),
        }
        i += 1;
    }

    let file = file.ok_or_else(|| miette!("spin-edit show requires --file"))?;
    let cwd = env::current_dir().map_err(|err| miette!("failed to read current directory: {err}"))?;
    let path = resolve_relative_path_within_root(&cwd, &file, "spin-edit show")?;
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|err| miette!("failed to read {}: {err}", path.display()))?;
    let lines = content.lines().collect::<Vec<_>>();
    let start_line = start.unwrap_or(1).max(1);
    let end_line = end.unwrap_or_else(|| lines.len()).max(start_line);

    for line_no in start_line..=end_line.min(lines.len()) {
        println!("{line_no:>6} {}", lines[line_no - 1]);
    }
    Ok(())
}

async fn initialize_injected_cli_tools(context: &mut Context) -> Result<()> {
    let bin_dir = ensure_global_cli_tool_dir().await?;
    prepend_cli_tool_dir_to_terminal(context, &bin_dir).await?;
    Ok(())
}

async fn ensure_global_cli_tool_dir() -> Result<PathBuf> {
    let spinova_home = get_spinova_home().await;
    let bin_dir = spinova_home.join("bin");
    tokio::fs::create_dir_all(&bin_dir)
        .await
        .map_err(|err| miette!("failed to create CLI bin dir {}: {err}", bin_dir.display()))?;

    let current_exe =
        env::current_exe().map_err(|err| miette!("failed to resolve current executable: {err}"))?;
    let copied_exe = if cfg!(windows) {
        bin_dir.join("spinova-tool.exe")
    } else {
        bin_dir.join("spinova-tool")
    };
    let needs_copy = match tokio::fs::metadata(&copied_exe).await {
        Ok(meta) => {
            let current_meta = tokio::fs::metadata(&current_exe)
                .await
                .map_err(|err| miette!("failed to stat current executable: {err}"))?;
            meta.len() != current_meta.len()
        }
        Err(_) => true,
    };
    if needs_copy {
        tokio::fs::copy(&current_exe, &copied_exe)
            .await
            .map_err(|err| {
                miette!(
                    "failed to copy {} to {}: {err}",
                    current_exe.display(),
                    copied_exe.display()
                )
            })?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&copied_exe, perms)
            .await
            .map_err(|err| miette!("failed to chmod {}: {err}", copied_exe.display()))?;
    }

    if cfg!(windows) {
        let wrapper = bin_dir.join("spin-edit.cmd");
        let script = format!(
            "@echo off\r\n\"{}\" internal-cli edit %*\r\n",
            copied_exe.display()
        );
        tokio::fs::write(&wrapper, script)
            .await
            .map_err(|err| miette!("failed to write {}: {err}", wrapper.display()))?;
    } else {
        let wrapper = bin_dir.join("spin-edit");
        let script = format!(
            "#!/usr/bin/env sh\nexec \"{}\" internal-cli edit \"$@\"\n",
            copied_exe.display()
        );
        tokio::fs::write(&wrapper, script)
            .await
            .map_err(|err| miette!("failed to write {}: {err}", wrapper.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            tokio::fs::set_permissions(&wrapper, perms)
                .await
                .map_err(|err| miette!("failed to chmod {}: {err}", wrapper.display()))?;
        }
    }

    Ok(bin_dir)
}

async fn prepend_cli_tool_dir_to_terminal(context: &mut Context, bin_dir: &Path) -> Result<()> {
    let command = if cfg!(windows) {
        format!(
            "$env:PATH=\"{};\" + $env:PATH\r",
            bin_dir.display().to_string().replace('"', "`\"")
        )
    } else {
        format!("export PATH=\"{}:$PATH\"\n", bin_dir.display())
    };
    context
        .devices
        .execute_focused(crate::device::DeviceAction::TerminalInput { text: command })
        .await
        .map_err(|err| miette!("failed to inject CLI tool PATH into terminal: {err}"))?;
    context
        .devices
        .wait_until_settled(Duration::from_millis(150), Duration::from_secs(2))
        .await;
    Ok(())
}

struct RuntimePolicyStepExecution {
    output: Output,
    snapshot_text: String,
}

async fn execute_runtime_policy_step(
    context: &mut Context,
    renderer: &OpenAIToolRenderer,
) -> RuntimePolicyStepExecution {
    let snapshot = Snapshot::new(context).await;
    let snapshot_text = snapshot.to_string();
    let runtime_policy = RuntimePolicyProgram;
    let work_phase = context
        .tasks
        .working_task_phase()
        .unwrap_or("investigate")
        .to_string();
    let outcome = runtime_policy
        .run_once(context, &snapshot, renderer, &work_phase)
        .await;
    let output = outcome.output;
    if should_record_effect(&output.effect) {
        let mut evidence = collect_memory_evidence(&output.effect);
        evidence.extend(collect_contextual_memory_evidence(context, &output.effect));
        let memory_entry = encode_memory_entry(
            context,
            &snapshot,
            renderer,
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
    let effect = output.effect.clone();
    apply_effect(context, effect).await;
    if outcome.touched_working_task {
        context.tasks.touch_working_task();
    }
    RuntimePolicyStepExecution {
        output,
        snapshot_text,
    }
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
    let renderer = OpenAIToolRenderer;
    let _step = execute_runtime_policy_step(context, &renderer).await;
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

fn summarize_inline_text(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let compact = text.replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
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

fn resolve_relative_path_within_root(
    root: &Path,
    relative_path: &str,
    caller: &str,
) -> miette::Result<PathBuf> {
    let candidate = Path::new(relative_path);
    if candidate.is_absolute() {
        return Err(miette!(
            "{caller} requires a workspace-relative path, got absolute path: {}",
            candidate.display(),
        ));
    }
    if candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(miette!(
            "{caller} path must not escape the workspace: {}",
            candidate.display(),
        ));
    }
    Ok(root.join(candidate))
}

struct PreciseEditReport {
    matches_before: usize,
    changed: bool,
    before_excerpt: String,
    after_excerpt: String,
}

fn build_excerpt(text: &str) -> String {
    summarize_inline_text(text)
}

fn print_precise_edit_report(relative_path: &str, report: &PreciseEditReport) {
    println!("changed={}", if report.changed { 1 } else { 0 });
    println!("matches_before={}", report.matches_before);
    println!("path={relative_path}");
    println!("before_excerpt={}", report.before_excerpt);
    println!("after_excerpt={}", report.after_excerpt);
}

async fn execute_precise_file_replace(
    path: &Path,
    old_text: &str,
    new_text: &str,
    caller: &str,
) -> miette::Result<PreciseEditReport> {
    let existing = tokio::fs::read_to_string(path)
        .await
        .map_err(|err| miette!("failed to read {} for {caller}: {err}", path.display()))?;
    let matches = existing.match_indices(old_text).count();
    if matches != 1 {
        return Err(miette!(
            "{caller} expected exactly 1 match in {}, found {}",
            path.display(),
            matches
        ));
    }
    let updated = existing.replacen(old_text, new_text, 1);
    tokio::fs::write(path, &updated)
        .await
        .map_err(|err| miette!("failed to write {} for {caller}: {err}", path.display()))?;
    Ok(PreciseEditReport {
        matches_before: matches,
        changed: true,
        before_excerpt: build_excerpt(old_text),
        after_excerpt: build_excerpt(new_text),
    })
}

#[derive(Default)]
struct SpinEditPatchSummary {
    changed_files: usize,
    added_files: usize,
    deleted_files: usize,
    updated_files: usize,
    paths: Vec<String>,
}

enum PatchOp {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        hunks: Vec<PatchHunk>,
    },
}

#[derive(Default)]
struct PatchHunk {
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

fn parse_spin_edit_patch(patch_text: &str) -> miette::Result<Vec<PatchOp>> {
    let lines = patch_text.lines().collect::<Vec<_>>();
    if lines.first().copied() != Some("*** Begin Patch") {
        return Err(miette!("spin-edit patch must start with `*** Begin Patch`"));
    }
    if lines.last().copied() != Some("*** End Patch") {
        return Err(miette!("spin-edit patch must end with `*** End Patch`"));
    }

    let mut ops = Vec::new();
    let mut i = 1;
    while i + 1 < lines.len() {
        let line = lines[i];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            i += 1;
            let mut added = Vec::new();
            while i < lines.len() && !lines[i].starts_with("*** ") {
                let raw = lines[i];
                let Some(content) = raw.strip_prefix('+') else {
                    return Err(miette!(
                        "add file lines must start with `+`, got `{}`",
                        raw
                    ));
                };
                added.push(content.to_string());
                i += 1;
            }
            ops.push(PatchOp::Add {
                path: path.to_string(),
                lines: added,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            ops.push(PatchOp::Delete {
                path: path.to_string(),
            });
            i += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            i += 1;
            let mut hunks = Vec::new();
            let mut current = PatchHunk::default();
            let mut saw_change_line = false;
            while i < lines.len() && !lines[i].starts_with("*** ") {
                let raw = lines[i];
                if raw == "@@" || raw.starts_with("@@ ") {
                    if saw_change_line {
                        hunks.push(current);
                        current = PatchHunk::default();
                        saw_change_line = false;
                    }
                    i += 1;
                    continue;
                }
                let (prefix, body) = raw.split_at(1);
                match prefix {
                    " " => {
                        current.old_lines.push(body.to_string());
                        current.new_lines.push(body.to_string());
                        saw_change_line = true;
                    }
                    "-" => {
                        current.old_lines.push(body.to_string());
                        saw_change_line = true;
                    }
                    "+" => {
                        current.new_lines.push(body.to_string());
                        saw_change_line = true;
                    }
                    _ => {
                        return Err(miette!(
                            "update file hunk lines must start with space/+/- or @@, got `{}`",
                            raw
                        ));
                    }
                }
                i += 1;
            }
            if saw_change_line {
                hunks.push(current);
            }
            if hunks.is_empty() {
                return Err(miette!("update file `{path}` contains no hunks"));
            }
            ops.push(PatchOp::Update {
                path: path.to_string(),
                hunks,
            });
            continue;
        }

        return Err(miette!("unknown patch directive: {line}"));
    }

    Ok(ops)
}

fn find_unique_hunk_start(
    haystack: &[String],
    needle: &[String],
    offset: usize,
) -> miette::Result<usize> {
    if needle.is_empty() {
        return Ok(offset.min(haystack.len()));
    }
    let mut matches = Vec::new();
    for start in offset..=haystack.len().saturating_sub(needle.len()) {
        if haystack[start..start + needle.len()] == *needle {
            matches.push(start);
        }
    }
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    if matches.is_empty() {
        for start in 0..=haystack.len().saturating_sub(needle.len()) {
            if haystack[start..start + needle.len()] == *needle {
                matches.push(start);
            }
        }
        if matches.len() == 1 {
            return Ok(matches[0]);
        }
    }
    match matches.len() {
        0 => Err(miette!("patch hunk old text not found uniquely in target file")),
        n => Err(miette!(
            "patch hunk old text matched {} locations in target file; provide more context",
            n
        )),
    }
}

async fn apply_spin_edit_patch(root: &Path, patch_text: &str) -> miette::Result<SpinEditPatchSummary> {
    let ops = parse_spin_edit_patch(patch_text)?;
    let mut summary = SpinEditPatchSummary::default();

    for op in ops {
        match op {
            PatchOp::Add { path, lines } => {
                let file_path = resolve_relative_path_within_root(root, &path, "spin-edit patch add")?;
                if tokio::fs::try_exists(&file_path)
                    .await
                    .map_err(|err| miette!("failed to stat {}: {err}", file_path.display()))?
                {
                    return Err(miette!("spin-edit patch cannot add existing file {}", path));
                }
                if let Some(parent) = file_path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|err| miette!("failed to create {}: {err}", parent.display()))?;
                }
                let mut content = lines.join("\n");
                if !content.is_empty() {
                    content.push('\n');
                }
                tokio::fs::write(&file_path, content)
                    .await
                    .map_err(|err| miette!("failed to write {}: {err}", file_path.display()))?;
                summary.changed_files += 1;
                summary.added_files += 1;
                summary.paths.push(path);
            }
            PatchOp::Delete { path } => {
                let file_path =
                    resolve_relative_path_within_root(root, &path, "spin-edit patch delete")?;
                tokio::fs::remove_file(&file_path)
                    .await
                    .map_err(|err| miette!("failed to delete {}: {err}", file_path.display()))?;
                summary.changed_files += 1;
                summary.deleted_files += 1;
                summary.paths.push(path);
            }
            PatchOp::Update { path, hunks } => {
                let file_path =
                    resolve_relative_path_within_root(root, &path, "spin-edit patch update")?;
                let original = tokio::fs::read_to_string(&file_path)
                    .await
                    .map_err(|err| miette!("failed to read {}: {err}", file_path.display()))?;
                let mut lines = original.lines().map(ToString::to_string).collect::<Vec<_>>();
                let mut offset = 0usize;
                for hunk in hunks {
                    let start = find_unique_hunk_start(&lines, &hunk.old_lines, offset)?;
                    let end = start + hunk.old_lines.len();
                    lines.splice(start..end, hunk.new_lines.clone());
                    offset = start + hunk.new_lines.len();
                }
                let mut updated = lines.join("\n");
                if original.ends_with('\n') || !updated.is_empty() {
                    updated.push('\n');
                }
                tokio::fs::write(&file_path, updated)
                    .await
                    .map_err(|err| miette!("failed to write {}: {err}", file_path.display()))?;
                summary.changed_files += 1;
                summary.updated_files += 1;
                summary.paths.push(path);
            }
        }
    }

    Ok(summary)
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
