mod app;
mod apply_patch;
mod browser_app;
mod commands;
mod config;
mod config_wizard;
mod context;
mod context_budget;
mod core;
mod daat_locus_paths;
mod dashboard;
mod events;
mod hindsight;
mod logging;
mod memory;
mod pending_work;
mod plan;
mod providers;
mod reasoning;
mod runtime_context;
mod runtime_tools;
mod sandbox;
mod schema_utils;
mod snapshot;
mod system_info;
mod telegram_acl;
mod telegram_transport;
mod terminal_app;
mod tool_ui;
mod workflow;
mod workspace_app;

use std::{
    collections::HashMap,
    env,
    io::Cursor,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    app::{AppId, AppManager},
    apply_patch::{PatchOperationKind, apply_patch_in_root, summarize_apply_patch_error},
    browser_app::BrowserApp,
    commands::reset::{run_complite_reset, run_memory_reset, run_reset_all, run_state_reset},
    config::load_config,
    context::{ActiveWorkflowRunSession, Context, PendingWorkflowRunFlush, RuntimeTurnPhase},
    context_budget::{
        approx_token_count, estimate_agent_turn_request, is_context_budget_exceeded,
        truncate_text_to_token_budget,
    },
    daat_locus_paths::daat_locus_paths,
    dashboard::render::{
        AUTO_SLEEP_IDLE_THRESHOLD, AUTO_SLEEP_MIN_INTERVAL, FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD,
        SleepDashboardStatus, refresh_sleep_backlogs, render_activity_for_dashboard,
        render_app_status_outputs_for_dashboard, render_dashboard_footer_context,
        render_sleep_status_output_for_dashboard, render_status_command_output_for_dashboard,
        render_system_prompt_output_for_dashboard, render_telegram_status_for_dashboard,
        sync_dashboard_state,
    },
    dashboard::{
        DashboardActivityEvent, DashboardControlCommand, DashboardState,
        activity_cell_from_tool_ui_event, activity_cells_from_tool_call_ui_event,
        apply_activity_event, assistant_activity_cell, run_tui_dashboard,
    },
    events::{EventPayload, EventStatus, EventStore, EventView},
    hindsight::{HindsightClient, HindsightRecallOptions, managed::HindsightManagedServer},
    logging::{
        RuntimeStatusLevel, clear_runtime_status, init_logging, set_runtime_status,
        write_current_turn_messages_dump, write_current_turn_response_dump,
        write_current_turn_response_error_dump,
    },
    memory::{Memory, RuntimeTurnDraft},
    pending_work::{PendingWork, PendingWorkQueue},
    plan::Plan,
    providers::build_llm,
    reasoning::{
        compiled::{
            CompiledPromptStore, load_all_compiled_programs_for_model,
            load_compiled_runtime_system_prompt_for_model,
        },
        episode::EpisodeActionRecord,
        evaluation_artifacts::EvaluationArtifactSuggestedFixKind,
        runtime::{
            AgentMessage, AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult, HistoryMessage,
            PromptMemoryCitation, PromptMemoryContext,
        },
        sleep::run_sleep,
        turn_compile::{TurnCompileEngine, should_run_cold_start_turn_compile},
    },
    runtime_context::{
        MID_TURN_COMPACTION_MAX_RECOVERIES, build_runtime_request_envelope,
        build_runtime_snapshot_text, execute_pre_turn_runtime_compaction,
        maybe_compact_runtime_messages, runtime_request_budget_limits,
    },
    runtime_tools::{
        ToolExecutionResult, build_runtime_tool_specs, execute_agent_tool_call,
        render_tool_call_ui_event, summarize_action_from_tool_call,
    },
    sandbox::RuntimeSandboxPolicy,
    snapshot::Snapshot,
    telegram_acl::TelegramAclHandle,
    telegram_transport::state::TelegramTransportState,
    telegram_transport::{TelegramLiveDraftClient, TelegramTransport},
    terminal_app::TerminalApp,
    tool_ui::{ToolCallUiEvent, ToolUiEvent, compact_body_lines},
    workflow::{WorkflowRunRecord, WorkflowStore, append_workflow_run_records},
    workspace_app::paths::{resolve_runtime_workspace_dir, workspace_apps_dir},
    workspace_app::{
        WorkspaceAppInvalidation, WorkspaceAppRegistry, bootstrap_workspace_apps,
        start_workspace_app_watcher,
    },
};
use chrono::Utc;
use clap::{Parser, Subcommand};
use miette::{Result, miette};
use serde::Deserialize;
use serde_json::json;
use tokio::{sync::mpsc, task::JoinHandle, time::MissedTickBehavior};

const RUNTIME_EVENT_CLAIM_BATCH_SIZE: usize = 1;
const RUNTIME_OVERFLOW_FUSE_THRESHOLD: usize = 3;
const APP_NOTICE_OVERFLOW_SUPPRESSION: Duration = Duration::from_secs(300);

struct RuntimeAppsBootstrap {
    apps: Vec<Box<dyn crate::app::App>>,
    workspace_registry: WorkspaceAppRegistry,
}

fn emit_startup_progress(message: impl AsRef<str>) {
    let message = message.as_ref();
    tracing::info!("{message}");
    println!("{message}");
}

#[derive(Default)]
enum SleepTrigger {
    #[default]
    Manual,
    Idle,
}

struct SleepTaskResult {
    trigger: SleepTrigger,
    result: Result<crate::reasoning::sleep::SleepSummary>,
}

struct TelegramLiveDraftSession {
    join: JoinHandle<()>,
}

impl TelegramLiveDraftSession {
    async fn shutdown(self, context: &mut Context) {
        context.install_live_assistant_progress(None);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.join).await;
    }
}

#[derive(Debug, Parser)]
#[command(name = "daat-locus")]
struct Cli {
    #[command(subcommand)]
    command: Option<DaatLocusCommand>,
}

#[derive(Debug, Subcommand)]
enum DaatLocusCommand {
    Reset {
        #[command(subcommand)]
        target: ResetTarget,
    },
    Setup {
        #[command(subcommand)]
        target: SetupTarget,
    },
    /// 交互式配置管理（无子命令时进入菜单）
    Config {
        #[command(subcommand)]
        target: Option<ConfigTarget>,
    },
    Sleep,
    Hindsight {
        #[command(subcommand)]
        target: HindsightTarget,
    },
    Inspect {
        #[command(subcommand)]
        target: InspectTarget,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigTarget {
    /// 显示当前配置摘要（secrets 已遮蔽）
    Show,
    /// 交互式添加一个 provider
    #[command(name = "add-provider")]
    AddProvider,
    /// 交互式添加一个 model
    #[command(name = "add-model")]
    AddModel,
    /// 更改 main_model
    #[command(name = "set-main-model")]
    SetMainModel,
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
enum SetupTarget {
    #[command(name = "browser-runtime")]
    BrowserRuntime,
}

#[derive(Debug, Subcommand)]
enum InspectTarget {
    #[command(name = "system-prompt")]
    SystemPrompt,
    Snapshot,
}

#[derive(Debug, Subcommand)]
enum HindsightTarget {
    Config,
    #[command(name = "clear-observations")]
    ClearObservations,
}

fn main() {
    let cli = Cli::parse();

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
    let _log_guard = init_logging().await;

    match cli.command.as_ref() {
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::Complite,
        }) => {
            run_complite_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::State,
        }) => {
            run_state_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::Memory,
        }) => {
            run_memory_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::All,
        }) => {
            run_reset_all().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Setup {
            target: SetupTarget::BrowserRuntime,
        }) => {
            run_browser_runtime_setup().await?;
            return Ok(());
        }
        // Config 子命令：可能在无 config 时运行（add-provider/add-model 除外）
        Some(DaatLocusCommand::Config { target }) => {
            return run_config_command(target.as_ref()).await;
        }
        _ => {}
    }

    // 首次运行：config.toml 不存在时触发交互式 setup
    let config = if !config::config_file_exists().await {
        match config_wizard::run_first_time_setup().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("初始化失败: {e:?}");
                std::process::exit(1);
            }
        }
    } else {
        match load_config().await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!("failed to load config: {e}");
                eprintln!("配置加载失败: {e:?}");
                std::process::exit(1);
            }
        }
    };

    match cli.command.as_ref() {
        Some(DaatLocusCommand::Hindsight { target }) => {
            run_hindsight_command(&config, target).await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Inspect {
            target: InspectTarget::SystemPrompt,
        }) => {
            let context = build_eval_context_for_inspect(config).await;
            println!("{}", render_system_prompt_output_for_dashboard(&context));
            context.shutdown().await;
            return Ok(());
        }
        Some(DaatLocusCommand::Inspect {
            target: InspectTarget::Snapshot,
        }) => {
            let mut context = build_eval_context_for_inspect(config).await;
            let snapshot = Snapshot::new(&mut context).await;
            println!("{}", build_runtime_snapshot_text(&context, &snapshot));
            context.shutdown().await;
            return Ok(());
        }
        _ => {}
    }

    if matches!(cli.command, Some(DaatLocusCommand::Sleep)) {
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

    emit_startup_progress("[prompt-compile] loading compiled prompts before dashboard startup...");
    let mut compiled_prompts = match load_compiled_prompts_only(&config).await {
        Ok(store) => store,
        Err(err) => {
            tracing::error!("failed to load compiled prompts: {err:?}");
            std::process::exit(1);
        }
    };
    if !compiled_prompts.has_runtime_system_prompt() {
        match should_run_cold_start_turn_compile().await {
            Ok(()) => {
                emit_startup_progress(
                    "[prompt-compile] runtime system prompt missing; running cold-start turn compile...",
                );
                match TurnCompileEngine::compile_cold_start(
                    config.clone(),
                    compiled_prompts.clone(),
                )
                .await
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
            Err(err) => {
                tracing::info!(
                    "skipping cold-start turn compile because prompt persona calibration is incomplete: {err:?}"
                );
                emit_startup_progress(format!("[prompt-compile] {err}"));
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
    let plan = Plan::new().await;
    let events = EventStore::new().await;
    let pending_work = PendingWorkQueue::new().await;
    let workflows = WorkflowStore::new().await;
    let telegram_acl = TelegramAclHandle::load().await;
    let telegram = TelegramTransportState::new();
    let telegram_handle = telegram.handle();
    bootstrap_telegram_transport_state_from_acl(&telegram_handle, &telegram_acl);
    let client = build_llm(&config.main_model, &config)?;
    let judge_model_key = config
        .judge
        .model
        .as_deref()
        .unwrap_or(&config.main_model)
        .to_string();
    let judge_client = build_llm(&judge_model_key, &config)?;
    let hindsight = connect_bootstrapped_hindsight(&config).await?;
    let hindsight_retain = hindsight.spawn_retain_worker();
    let execution_cwd = resolve_runtime_workspace_dir()?;
    tokio::fs::create_dir_all(&execution_cwd)
        .await
        .map_err(|err| {
            miette!(
                "failed to create runtime workspace {}: {err}",
                execution_cwd.display()
            )
        })?;
    tokio::fs::create_dir_all(workspace_apps_dir(&execution_cwd))
        .await
        .map_err(|err| {
            miette!(
                "failed to create workspace apps directory {}: {err}",
                workspace_apps_dir(&execution_cwd).display()
            )
        })?;
    let runtime_apps = build_runtime_apps(&execution_cwd);
    let apps = AppManager::new(Some(AppId::terminal()), runtime_apps.apps)
        .await
        .unwrap();
    let sandbox_policy = sandbox_policy_for_runtime().await;
    let mut context = Context {
        llm: client,
        judge_llm: judge_client,
        config,
        hindsight,
        hindsight_retain,
        memory,
        prompt_memory: PromptMemoryContext::default(),
        plan,
        events,
        pending_work,
        workflows,
        bound_workflow_id: None,
        active_workflow_run: None,
        pending_workflow_run_flushes: Vec::new(),
        current_work_origin: None,
        workflow_step_started_bound_id: None,
        apps,
        workspace_apps: runtime_apps.workspace_registry,
        telegram: telegram_handle,
        compiled_prompts,
        execution_cwd,
        sandbox_policy,
        dashboard_tx: None,
        active_runtime_turn: false,
        active_runtime_phase: None,
        runtime_turn_started_at: None,
        active_app_notices: std::collections::HashSet::new(),
        runtime_overflow_failures: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        suppressed_app_notices: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        live_assistant_progress_tx: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        idle_since: None,
        last_idle_sleep_at: None,
    };
    let app_renders = context.apps.state_renders();

    let (tx, mut rx) = tokio::sync::watch::channel(DashboardState {
        focused_app: context.apps.focused(),
        status_output: render_status_command_output_for_dashboard(&context, &app_renders),
        sleep_status_output: render_sleep_status_output_for_dashboard(
            &context,
            &SleepDashboardStatus::default(),
        ),
        inspect_telegram_output: render_telegram_status_for_dashboard(&context),
        system_prompt_output: render_system_prompt_output_for_dashboard(&context),
        app_status_outputs: render_app_status_outputs_for_dashboard(&context),
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
    let (workspace_app_invalidation_tx, mut workspace_app_invalidation_rx) =
        tokio::sync::mpsc::unbounded_channel::<WorkspaceAppInvalidation>();
    let workspace_app_watcher = match start_workspace_app_watcher(
        workspace_apps_dir(&context.execution_cwd),
        workspace_app_invalidation_tx,
    ) {
        Ok(watcher) => {
            tracing::info!(
                backend = watcher.backend_name(),
                root = %workspace_apps_dir(&context.execution_cwd).display(),
                "workspace app watcher started",
            );
            Some(watcher)
        }
        Err(err) => {
            tracing::warn!("failed to start workspace app watcher: {err:?}");
            None
        }
    };
    let telegram_transport =
        if context.config.telegram.enabled && context.config.telegram.has_real_credentials() {
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
                _ = daat_locus_loop(
                    &mut context,
                    &tx,
                    &sleep_result_tx,
                    &mut sleep_running,
                    &mut sleep_status,
                    &mut workspace_app_invalidation_rx,
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
    drop(workspace_app_watcher);
    if let Some(handle) = telegram_transport {
        handle.abort();
    }
    let _ = shutdown_tx.send(());
    let _ = agent_handle.await;
    Ok(())
}

async fn connect_bootstrapped_hindsight(config: &crate::config::Config) -> Result<HindsightClient> {
    let mut hindsight_config = config.hindsight.clone();
    if hindsight_config.managed {
        // In managed mode, force base_url to match managed_port so they're
        // always in sync even if the user configured them independently.
        hindsight_config.base_url = format!("http://127.0.0.1:{}", hindsight_config.managed_port);

        let server = HindsightManagedServer::new(hindsight_config.clone());
        // If the daemon is already running from a previous session, skip startup.
        if server.check_health().await {
            tracing::info!("[hindsight:managed] daemon already running, reusing");
        } else {
            server.start().await?;
        }
    }
    let hindsight = HindsightClient::connect(&hindsight_config).await?;
    hindsight.bootstrap_bank().await?;
    Ok(hindsight)
}

async fn run_config_command(target: Option<&ConfigTarget>) -> Result<()> {
    match target {
        None => config_wizard::run_config_menu().await,
        Some(ConfigTarget::Show) => config_wizard::show_config().await,
        Some(ConfigTarget::AddProvider) => config_wizard::run_add_provider().await,
        Some(ConfigTarget::AddModel) => config_wizard::run_add_model().await,
        Some(ConfigTarget::SetMainModel) => config_wizard::run_set_main_model().await,
    }
}

async fn run_hindsight_command(
    config: &crate::config::Config,
    target: &HindsightTarget,
) -> Result<()> {
    let hindsight = connect_bootstrapped_hindsight(config).await?;
    match target {
        HindsightTarget::Config => {
            let config = hindsight.get_bank_config().await?;
            println!(
                "{}",
                to_pretty_json(&json!({
                    "bank_id": config.bank_id,
                    "config": config.config,
                    "overrides": config.overrides,
                }))?
            );
        }
        HindsightTarget::ClearObservations => {
            let response = hindsight.delete_all_observations().await?;
            println!(
                "{}",
                to_pretty_json(&json!({
                    "success": response.success,
                    "message": response.message,
                    "deleted_count": response.deleted_count.unwrap_or(0),
                }))?
            );
        }
    }
    Ok(())
}

fn to_pretty_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value)
        .map_err(|err| miette!("serialize hindsight output failed: {err}"))
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingManifest {
    channels: std::collections::BTreeMap<String, ChromeForTestingChannel>,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingChannel {
    version: String,
    downloads: ChromeForTestingDownloads,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingDownloads {
    chrome: Vec<ChromeForTestingDownload>,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingDownload {
    platform: String,
    url: String,
}

fn browser_runtime_platform() -> Result<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok("mac-arm64");
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Ok("mac-x64");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok("linux64");
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok("win64");
    }
    #[cfg(all(target_os = "windows", target_arch = "x86"))]
    {
        return Ok("win32");
    }

    #[allow(unreachable_code)]
    Err(miette!("unsupported browser runtime platform"))
}

async fn run_browser_runtime_setup() -> Result<()> {
    const MANIFEST_URL: &str = "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

    let platform = browser_runtime_platform()?;
    let paths = daat_locus_paths().await;
    let runtime_dir = paths.browser_runtime_dir();
    let executable_path = paths.browser_executable_path();

    println!(
        "[setup] downloading browser runtime for platform `{platform}` into {}",
        runtime_dir.display()
    );

    let manifest = reqwest::get(MANIFEST_URL)
        .await
        .map_err(|err| miette!("failed to fetch Chrome for Testing manifest: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("failed to fetch Chrome for Testing manifest: {err}"))?
        .json::<ChromeForTestingManifest>()
        .await
        .map_err(|err| miette!("failed to decode Chrome for Testing manifest: {err}"))?;

    let stable = manifest
        .channels
        .get("Stable")
        .ok_or_else(|| miette!("Chrome for Testing manifest missing Stable channel"))?;
    let download = stable
        .downloads
        .chrome
        .iter()
        .find(|entry| entry.platform == platform)
        .ok_or_else(|| {
            miette!("Chrome for Testing has no chrome download for platform `{platform}`")
        })?;

    println!(
        "[setup] downloading Chrome for Testing {} from {}",
        stable.version, download.url
    );

    let archive_bytes = reqwest::get(&download.url)
        .await
        .map_err(|err| miette!("failed to download browser runtime: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("failed to download browser runtime: {err}"))?
        .bytes()
        .await
        .map_err(|err| miette!("failed to read browser runtime archive: {err}"))?;

    let runtime_dir_for_extract = runtime_dir.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if runtime_dir_for_extract.exists() {
            std::fs::remove_dir_all(&runtime_dir_for_extract).map_err(|err| {
                miette!(
                    "failed to clear existing browser runtime {}: {err}",
                    runtime_dir_for_extract.display()
                )
            })?;
        }
        std::fs::create_dir_all(&runtime_dir_for_extract).map_err(|err| {
            miette!(
                "failed to create browser runtime dir {}: {err}",
                runtime_dir_for_extract.display()
            )
        })?;

        let reader = Cursor::new(archive_bytes.to_vec());
        let mut archive = zip::ZipArchive::new(reader)
            .map_err(|err| miette!("failed to open browser runtime archive: {err}"))?;

        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .map_err(|err| miette!("failed to read browser runtime archive entry: {err}"))?;
            let enclosed = file
                .enclosed_name()
                .ok_or_else(|| miette!("browser runtime archive contained unsafe path"))?
                .to_path_buf();
            let destination = runtime_dir_for_extract.join(enclosed);
            if file.name().ends_with('/') {
                std::fs::create_dir_all(&destination).map_err(|err| {
                    miette!(
                        "failed to create extracted dir {}: {err}",
                        destination.display()
                    )
                })?;
                continue;
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    miette!(
                        "failed to create extracted parent dir {}: {err}",
                        parent.display()
                    )
                })?;
            }
            let mut output = std::fs::File::create(&destination).map_err(|err| {
                miette!(
                    "failed to create extracted file {}: {err}",
                    destination.display()
                )
            })?;
            std::io::copy(&mut file, &mut output).map_err(|err| {
                miette!(
                    "failed to extract browser runtime file {}: {err}",
                    destination.display()
                )
            })?;

            #[cfg(unix)]
            if let Some(mode) = file.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode));
            }
        }

        Ok(())
    })
    .await
    .map_err(|err| miette!("browser runtime extraction task failed: {err}"))??;

    if !executable_path.exists() {
        return Err(miette!(
            "browser runtime installed but executable not found at {}",
            executable_path.display()
        ));
    }

    let version_file = runtime_dir.join("VERSION");
    tokio::fs::write(&version_file, format!("{}\n", stable.version))
        .await
        .map_err(|err| miette!("failed to write browser runtime version file: {err}"))?;

    println!("[setup] browser runtime installed successfully");
    println!("[setup] version: {}", stable.version);
    println!("[setup] executable: {}", executable_path.display());
    Ok(())
}

async fn build_eval_context(config: crate::config::Config) -> Context {
    build_eval_context_with_compiled(config, CompiledPromptStore::empty()).await
}

async fn build_eval_context_for_inspect(config: crate::config::Config) -> Context {
    let compiled_prompts = load_compiled_prompts_only(&config)
        .await
        .unwrap_or_else(|_| CompiledPromptStore::empty());
    build_eval_context_with_compiled(config, compiled_prompts).await
}

async fn sandbox_policy_for_runtime() -> RuntimeSandboxPolicy {
    let daat_locus_home = daat_locus_paths().await.root().to_path_buf();
    RuntimeSandboxPolicy::protect_daat_locus_runtime(&daat_locus_home)
}

fn build_runtime_apps(execution_cwd: &Path) -> RuntimeAppsBootstrap {
    let mut apps: Vec<Box<dyn crate::app::App>> =
        vec![Box::new(BrowserApp::new()), Box::new(TerminalApp::new())];
    let bootstrap = bootstrap_workspace_apps(execution_cwd);
    for error in &bootstrap.errors {
        tracing::warn!("{error}");
    }
    apps.extend(bootstrap.apps);
    RuntimeAppsBootstrap {
        apps,
        workspace_registry: bootstrap.registry,
    }
}

pub(crate) async fn build_eval_context_with_compiled(
    config: crate::config::Config,
    compiled_prompts: CompiledPromptStore,
) -> Context {
    let execution_cwd = resolve_runtime_workspace_dir()
        .unwrap_or_else(|err| panic!("failed to determine execution cwd: {err}"));
    std::fs::create_dir_all(&execution_cwd).unwrap_or_else(|err| {
        panic!(
            "failed to create runtime workspace {}: {err}",
            execution_cwd.display()
        )
    });
    std::fs::create_dir_all(workspace_apps_dir(&execution_cwd)).unwrap_or_else(|err| {
        panic!(
            "failed to create workspace apps directory {}: {err}",
            workspace_apps_dir(&execution_cwd).display()
        )
    });
    let sandbox_policy = sandbox_policy_for_runtime().await;
    let memory = Memory::new().await;
    let plan = Plan::new().await;
    let events = EventStore::new().await;
    let pending_work = PendingWorkQueue::new().await;
    let workflows = WorkflowStore::new().await;
    let telegram = TelegramTransportState::new();
    let telegram_handle = telegram.handle();
    let runtime_apps = build_runtime_apps(&execution_cwd);
    let apps = AppManager::new(Some(AppId::terminal()), runtime_apps.apps)
        .await
        .unwrap();
    let client = build_llm(&config.main_model, &config)
        .unwrap_or_else(|err| panic!("failed to construct main LLM client: {err:?}"));
    let judge_model_key = config
        .judge
        .model
        .as_deref()
        .unwrap_or(&config.main_model)
        .to_string();
    let judge_client = build_llm(&judge_model_key, &config)
        .unwrap_or_else(|err| panic!("failed to construct judge LLM client: {err:?}"));
    let hindsight = connect_bootstrapped_hindsight(&config)
        .await
        .unwrap_or_else(|err| panic!("failed to construct hindsight client: {err:?}"));
    let hindsight_retain = hindsight.spawn_retain_worker();

    Context {
        llm: client,
        judge_llm: judge_client,
        config,
        hindsight,
        hindsight_retain,
        memory,
        prompt_memory: PromptMemoryContext::default(),
        plan,
        events,
        pending_work,
        workflows,
        bound_workflow_id: None,
        active_workflow_run: None,
        pending_workflow_run_flushes: Vec::new(),
        current_work_origin: None,
        workflow_step_started_bound_id: None,
        apps,
        workspace_apps: runtime_apps.workspace_registry,
        telegram: telegram_handle,
        compiled_prompts,
        execution_cwd,
        sandbox_policy,
        dashboard_tx: None,
        active_runtime_turn: false,
        active_runtime_phase: None,
        runtime_turn_started_at: None,
        active_app_notices: std::collections::HashSet::new(),
        runtime_overflow_failures: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        suppressed_app_notices: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        live_assistant_progress_tx: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        idle_since: None,
        last_idle_sleep_at: None,
    }
}

fn bootstrap_telegram_transport_state_from_acl(
    telegram_handle: &crate::telegram_transport::state::TelegramTransportStateHandle,
    telegram_acl: &TelegramAclHandle,
) {
    for chat in telegram_acl.approved_chats() {
        telegram_handle.register_known_chat(chat.chat_id.to_string(), chat.title);
    }
}

async fn load_compiled_prompts_only(
    config: &crate::config::Config,
) -> miette::Result<CompiledPromptStore> {
    let compiled =
        load_all_compiled_programs_for_model(&config.main_model_config().model_id).await?;
    let runtime_system_prompt =
        load_compiled_runtime_system_prompt_for_model(&config.main_model_config().model_id).await?;
    Ok(CompiledPromptStore::from_entries(compiled)
        .with_runtime_system_prompt(runtime_system_prompt))
}

fn print_sleep_summary(summary: &crate::reasoning::sleep::SleepSummary) {
    let prompt = &summary.prompt_improvement;
    let workflow = &summary.workflow_improvement;
    println!(
        "sleep: prompt[traces={} failure_patterns={} reflections={} candidates={} evaluations={} frontier={} lineage={}/{}/{} bootstrap_demos={} stress_cases={} instruction_hypotheses={} runtime_demos={} turn_demos={} additions={} updated={}] workflow[evidence_run_records={} reflections={} patch/merge={}/{} evaluations={} frontier={} lineage={}/{}/{} applied={}/{} rollbacks={} rounds={}]",
        prompt.consumed_trace_events,
        prompt.failure_patterns.len(),
        prompt.prompt_reflections,
        prompt.prompt_candidates,
        prompt.prompt_candidate_evaluations,
        prompt.prompt_frontier_entries,
        prompt.prompt_frontier_root_entries,
        prompt.prompt_frontier_branched_entries,
        prompt.prompt_frontier_max_generation,
        prompt.bootstrap_demos,
        prompt.stress_cases,
        prompt.instruction_hypotheses,
        prompt.runtime_demos,
        prompt.turn_demos,
        prompt.applied_system_additions,
        prompt.compiled_prompt_updated,
        workflow.evidence_run_records,
        workflow.workflow_reflections,
        workflow.patch_candidates,
        workflow.merge_candidates,
        workflow.candidate_evaluations,
        workflow.frontier_entries,
        workflow.frontier_root_entries,
        workflow.frontier_branched_entries,
        workflow.frontier_max_generation,
        workflow.patch_applied,
        workflow.merge_applied,
        workflow.update_rollbacks,
        workflow.optimization_rounds
    );
    for pattern in &prompt.failure_patterns {
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
    let prompt = &summary.prompt_improvement;
    let workflow = &summary.workflow_improvement;
    format!(
        "sleep 完成：prompt traces={}，failure_patterns/reflections/candidates/evaluations/frontier={}/{}/{}/{}/{}，prompt lineage={}/{}/{}，prompt additions={}，workflow evidence/reflections/patch/merge/evaluations/frontier={}/{}/{}/{}/{}/{}，workflow lineage={}/{}/{}，应用 patch/merge={}/{}，回滚 {}",
        prompt.consumed_trace_events,
        prompt.failure_patterns.len(),
        prompt.prompt_reflections,
        prompt.prompt_candidates,
        prompt.prompt_candidate_evaluations,
        prompt.prompt_frontier_entries,
        prompt.prompt_frontier_root_entries,
        prompt.prompt_frontier_branched_entries,
        prompt.prompt_frontier_max_generation,
        prompt.applied_system_additions,
        workflow.evidence_run_records,
        workflow.workflow_reflections,
        workflow.patch_candidates,
        workflow.merge_candidates,
        workflow.candidate_evaluations,
        workflow.frontier_entries,
        workflow.frontier_root_entries,
        workflow.frontier_branched_entries,
        workflow.frontier_max_generation,
        workflow.patch_applied,
        workflow.merge_applied,
        workflow.update_rollbacks,
    )
}

fn drain_workspace_app_invalidations(
    workspace_apps: &mut WorkspaceAppRegistry,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<WorkspaceAppInvalidation>,
) {
    loop {
        match rx.try_recv() {
            Ok(invalidation) => workspace_apps.record_invalidation(invalidation),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            | Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
}

async fn sync_workspace_apps_from_invalidation(context: &mut Context) {
    let report = match context
        .workspace_apps
        .sync_dirty_apps(&mut context.apps)
        .await
    {
        Ok(report) => report,
        Err(err) => {
            tracing::error!("failed to sync workspace apps from invalidation: {err:?}");
            return;
        }
    };

    if report.is_empty() {
        return;
    }

    for removed in &report.removed {
        context.active_app_notices.remove(removed);
    }
    if !report.added.is_empty() {
        tracing::info!(
            apps = report
                .added
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
            "loaded workspace apps from source changes",
        );
    }
    if !report.reloaded.is_empty() {
        tracing::info!(
            apps = report
                .reloaded
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
            "reloaded workspace apps from source changes",
        );
    }
    if !report.removed.is_empty() {
        tracing::info!(
            apps = report
                .removed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
            "unloaded workspace apps removed from source tree",
        );
    }
    for error in report.errors {
        tracing::warn!("{error}");
    }
}

pub(crate) struct DaatLocusHomeOverride {
    previous: Option<String>,
}

impl DaatLocusHomeOverride {
    pub(crate) fn set(path: PathBuf) -> Self {
        let previous = env::var("DAAT_LOCUS_HOME").ok();
        unsafe {
            env::set_var("DAAT_LOCUS_HOME", path);
        }
        Self { previous }
    }
}

impl Drop for DaatLocusHomeOverride {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => unsafe {
                env::set_var("DAAT_LOCUS_HOME", previous);
            },
            None => unsafe {
                env::remove_var("DAAT_LOCUS_HOME");
            },
        }
    }
}

pub(crate) struct AgentLoopStepExecution {
    pub(crate) output: AgentLoopStepOutput,
    pub(crate) history_messages: Vec<HistoryMessage>,
}

pub(crate) struct AgentLoopStepOutput {
    pub(crate) observation: String,
    pub(crate) description: String,
    pub(crate) current_doing: String,
    pub(crate) actions: Vec<EpisodeActionRecord>,
}

const RUNTIME_HISTORY_MIN_MESSAGES: usize = 0;
const RUNTIME_HISTORY_SUMMARY_MAX_TOKENS: usize = 800;
const HINDSIGHT_RECALL_QUERY_MAX_TOKENS: usize = 420;
const RUNTIME_PREFLIGHT_STAGE_TIMEOUT_SECS: u64 = 60;

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

fn detect_runtime_rollback(output: &AgentLoopStepOutput) -> bool {
    let text = format!(
        "{}\n{}\n{}",
        output.description,
        output.observation,
        output
            .actions
            .iter()
            .map(|action| action.summary.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    )
    .to_ascii_lowercase();
    text.contains("rollback") || text.contains("回滚") || text.contains("revert")
}

fn detect_runtime_manual_fix(output: &AgentLoopStepOutput) -> bool {
    output.actions.iter().any(|action| {
        matches!(
            action.kind.as_str(),
            "apply_patch" | "terminal_exec" | "terminal_write_stdin"
        )
    })
}

fn classify_runtime_failure_type(output: &AgentLoopStepOutput) -> Option<String> {
    let text = format!("{}\n{}", output.description, output.observation).to_ascii_lowercase();
    if text.contains("timeout") || text.contains("超时") {
        return Some("timeout".to_string());
    }
    if text.contains("schema") || text.contains("deserialize") || text.contains("json") {
        return Some("schema_drift".to_string());
    }
    if text.contains("permission") || text.contains("forbidden") || text.contains("denied") {
        return Some("permission".to_string());
    }
    if text.contains("tool") && text.contains("failed") {
        return Some("tool_failure".to_string());
    }
    if text.contains("error") || text.contains("失败") {
        return Some("runtime_error".to_string());
    }
    None
}

fn workflow_tool_action_count(output: &AgentLoopStepOutput) -> usize {
    output
        .actions
        .iter()
        .filter(|action| {
            !matches!(
                action.kind.as_str(),
                "assistant_message" | "empty_tool_calls"
            )
        })
        .count()
}

fn workflow_run_summary(output: &AgentLoopStepOutput) -> String {
    format!(
        "{} | {} | {}",
        output.current_doing.trim(),
        output.description.trim(),
        output.observation.trim()
    )
}

fn accumulate_workflow_session_from_output(
    session: &mut ActiveWorkflowRunSession,
    output: &AgentLoopStepOutput,
) {
    session.turn_count = session.turn_count.saturating_add(1);
    session.tool_action_count = session
        .tool_action_count
        .saturating_add(workflow_tool_action_count(output));
    session.manual_fix_detected |= detect_runtime_manual_fix(output);
    session.rollback_detected |= detect_runtime_rollback(output);
    if let Some(failure_type) = classify_runtime_failure_type(output) {
        session.failure_types.insert(failure_type);
    }
    session.final_summary = workflow_run_summary(output);
}

fn workflow_run_record_from_pending_flush(
    flush: PendingWorkflowRunFlush,
    ended_at_ms: i64,
) -> WorkflowRunRecord {
    WorkflowRunRecord {
        run_id: flush.session.run_id,
        workflow_id: flush.session.workflow_id,
        started_at_ms: flush.session.started_at_ms,
        ended_at_ms,
        origin: flush.session.origin,
        outcome: flush.outcome,
        turn_count: flush.session.turn_count,
        tool_action_count: flush.session.tool_action_count,
        manual_fix_detected: flush.session.manual_fix_detected,
        rollback_detected: flush.session.rollback_detected,
        failure_types: flush.session.failure_types.into_iter().collect(),
        final_summary: flush.session.final_summary,
    }
}

async fn record_workflow_run_evidence(context: &mut Context, output: &AgentLoopStepOutput) {
    let target_workflow_id = context
        .workflow_step_started_bound_id
        .clone()
        .or_else(|| context.bound_workflow_id.clone())
        .or_else(|| {
            context
                .pending_workflow_run_flushes
                .last()
                .map(|flush| flush.session.workflow_id.clone())
        });

    if let Some(workflow_id) = target_workflow_id {
        let mut matched_pending = false;
        for flush in context.pending_workflow_run_flushes.iter_mut().rev() {
            if flush.session.workflow_id == workflow_id {
                accumulate_workflow_session_from_output(&mut flush.session, output);
                matched_pending = true;
                break;
            }
        }
        if !matched_pending
            && let Some(session) = context.active_workflow_run.as_mut()
            && session.workflow_id == workflow_id
        {
            accumulate_workflow_session_from_output(session, output);
        }
    }

    if context.pending_workflow_run_flushes.is_empty() {
        return;
    }

    let ended_at_ms = Utc::now().timestamp_millis();
    let records = context
        .pending_workflow_run_flushes
        .drain(..)
        .map(|flush| workflow_run_record_from_pending_flush(flush, ended_at_ms))
        .collect::<Vec<_>>();
    if let Err(err) = append_workflow_run_records(&records).await {
        tracing::error!("failed to append workflow run records at runtime boundary: {err:?}");
    }
}

fn runtime_work_origin(inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    if inputs.is_empty() {
        return None;
    }
    if inputs.len() > 1 {
        return Some("runtime_work:batch".to_string());
    }
    match inputs.first() {
        Some(ClaimedRuntimeInput::Event(event)) => Some(format!("event:{}", event.event_id)),
        Some(ClaimedRuntimeInput::AppNotice { app, reason }) => {
            Some(format!("app_notice:{app}:{}", reason.trim()))
        }
        None => None,
    }
}

fn maybe_start_telegram_live_draft_session(
    context: &mut Context,
    claimed_event_views: &[EventView],
) -> Option<TelegramLiveDraftSession> {
    if claimed_event_views.len() != 1 {
        return None;
    }
    let event = claimed_event_views.first()?;
    let EventPayload::TelegramIncoming(payload) = &event.payload;
    if payload.chat_kind != "private" {
        return None;
    }
    if !context.config.telegram.enabled || !context.config.telegram.has_real_credentials() {
        return None;
    }
    let chat_id = payload.chat_id.parse::<i64>().ok()?;
    let draft_id = Utc::now().timestamp_millis().unsigned_abs().max(1) as i64;
    let client = TelegramLiveDraftClient::new(context.config.telegram.clone());
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    context.install_live_assistant_progress(Some(tx));
    let join = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(900));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut latest_text: Option<String> = None;
        let mut last_sent = String::new();
        let initial_draft_text = format_telegram_live_draft_text("");
        if let Err(err) = client
            .send_message_draft(chat_id, draft_id, &initial_draft_text)
            .await
        {
            tracing::warn!("telegram initial live draft send failed: {err:?}");
        } else {
            last_sent = initial_draft_text;
        }
        loop {
            tokio::select! {
                maybe_text = rx.recv() => {
                    match maybe_text {
                        Some(text) => latest_text = Some(text),
                        None => break,
                    }
                }
                _ = interval.tick() => {
                    if let Some(text) = latest_text.take() {
                        let draft_text = format_telegram_live_draft_text(&text);
                        if draft_text != last_sent {
                            if let Err(err) = client
                                .send_message_draft(chat_id, draft_id, &draft_text)
                                .await
                            {
                                tracing::warn!("telegram live draft update failed: {err:?}");
                            } else {
                                last_sent = draft_text;
                            }
                        }
                    }
                }
            }
        }
        if let Some(text) = latest_text.take() {
            let draft_text = format_telegram_live_draft_text(&text);
            if draft_text != last_sent
                && let Err(err) = client
                    .send_message_draft(chat_id, draft_id, &draft_text)
                    .await
            {
                tracing::warn!("telegram final live draft flush failed: {err:?}");
            }
        }
    });
    Some(TelegramLiveDraftSession { join })
}

fn format_telegram_live_draft_text(content: &str) -> String {
    let trimmed = content.trim();
    let base = if trimmed.is_empty() {
        "Working...".to_string()
    } else {
        format!("Working...\n{trimmed}")
    };
    if base.chars().count() <= 4096 {
        return base;
    }
    let truncated = base.chars().take(4093).collect::<String>();
    format!("{truncated}...")
}

fn enter_runtime_phase(
    context: &mut Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    phase: RuntimeTurnPhase,
) {
    context.set_runtime_phase(Some(phase));
    set_runtime_status(
        tx,
        RuntimeStatusLevel::Info,
        format!("处理中：runtime turn / {}", phase.label()),
    );
}

async fn abort_runtime_turn_before_model(
    context: &mut Context,
    _tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
    live_draft_session: Option<TelegramLiveDraftSession>,
    claimed_input_fingerprint: Option<&str>,
    claimed_event_ids: &[String],
    claimed_app_notices: &[AppId],
    observation: String,
    description: String,
) -> AgentLoopStepExecution {
    context.set_runtime_phase(None);
    if let Some(session) = live_draft_session {
        session.shutdown(context).await;
    } else {
        context.install_live_assistant_progress(None);
    }
    if let Some(fingerprint) = claimed_input_fingerprint {
        context.clear_runtime_overflow_failure(fingerprint);
    }
    let output = AgentLoopStepOutput {
        observation: observation.clone(),
        description,
        current_doing: "等待下一轮工具决策".to_string(),
        actions: vec![EpisodeActionRecord {
            kind: "runtime_preflight_failed".to_string(),
            summary: observation,
        }],
    };
    finalize_claimed_runtime_events(context, claimed_event_ids, &output);
    finalize_claimed_runtime_app_notices(context, claimed_app_notices, &output).await;
    record_workflow_run_evidence(context, &output).await;
    context.current_work_origin = None;
    context.workflow_step_started_bound_id = None;
    AgentLoopStepExecution {
        output,
        history_messages: Vec::new(),
    }
}

pub(crate) async fn execute_agent_loop_step(
    context: &mut Context,
    tx: Option<&tokio::sync::watch::Sender<DashboardState>>,
) -> AgentLoopStepExecution {
    let claimed_inputs = claim_pending_runtime_inputs(context, RUNTIME_EVENT_CLAIM_BATCH_SIZE);
    context.current_work_origin = runtime_work_origin(&claimed_inputs);
    context.workflow_step_started_bound_id = context.bound_workflow_id.clone();
    let claimed_input_fingerprint = claimed_runtime_input_fingerprint(&claimed_inputs);
    let claimed_event_ids = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.event_id.to_string()),
            ClaimedRuntimeInput::AppNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    let claimed_app_notice_entries = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(_) => None,
            ClaimedRuntimeInput::AppNotice { app, reason } => Some((app.clone(), reason.clone())),
        })
        .collect::<Vec<_>>();
    let claimed_app_notices = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(_) => None,
            ClaimedRuntimeInput::AppNotice { app, .. } => Some(app.clone()),
        })
        .collect::<Vec<_>>();

    let preflight_timeout = Duration::from_secs(RUNTIME_PREFLIGHT_STAGE_TIMEOUT_SECS);
    enter_runtime_phase(context, tx, RuntimeTurnPhase::PreflightMemory);
    let preflight_started_at = std::time::Instant::now();
    tracing::info!(
        "runtime preflight stage started: {}",
        RuntimeTurnPhase::PreflightMemory.label()
    );
    let prompt_memory = match tokio::time::timeout(
        preflight_timeout,
        build_hindsight_memory_context(context, &claimed_inputs),
    )
    .await
    {
        Ok(prompt_memory) => {
            tracing::info!(
                elapsed_ms = preflight_started_at.elapsed().as_millis(),
                "runtime preflight stage completed: {}",
                RuntimeTurnPhase::PreflightMemory.label()
            );
            prompt_memory
        }
        Err(_) => {
            let err = miette!(
                "runtime preflight stage `{}` timed out after {}s",
                RuntimeTurnPhase::PreflightMemory.label(),
                preflight_timeout.as_secs()
            );
            set_runtime_status(
                tx,
                RuntimeStatusLevel::Error,
                format!(
                    "runtime turn preflight 超时：{}",
                    RuntimeTurnPhase::PreflightMemory.label()
                ),
            );
            tracing::error!(
                elapsed_ms = preflight_started_at.elapsed().as_millis(),
                timeout_secs = preflight_timeout.as_secs(),
                "runtime preflight stage timed out: {}",
                RuntimeTurnPhase::PreflightMemory.label()
            );
            return abort_runtime_turn_before_model(
                context,
                tx,
                None,
                claimed_input_fingerprint.as_deref(),
                &claimed_event_ids,
                &claimed_app_notices,
                format!("runtime preflight failed: {err}"),
                "构建 hindsight 记忆上下文失败。".to_string(),
            )
            .await;
        }
    };
    context.prompt_memory = prompt_memory;
    let claimed_input_messages = claimed_inputs
        .iter()
        .map(|input| prompt_message_for_claimed_input(context, input))
        .collect::<Vec<_>>();
    let claimed_event_views = claimed_inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.clone()),
            ClaimedRuntimeInput::AppNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    let live_draft_session = maybe_start_telegram_live_draft_session(context, &claimed_event_views);
    enter_runtime_phase(context, tx, RuntimeTurnPhase::PreflightSnapshot);
    let snapshot_started_at = std::time::Instant::now();
    tracing::info!(
        "runtime preflight stage started: {}",
        RuntimeTurnPhase::PreflightSnapshot.label()
    );
    let snapshot = match tokio::time::timeout(
        preflight_timeout,
        Snapshot::new_with_claimed_events(context, &claimed_event_views),
    )
    .await
    {
        Ok(snapshot) => {
            tracing::info!(
                elapsed_ms = snapshot_started_at.elapsed().as_millis(),
                "runtime preflight stage completed: {}",
                RuntimeTurnPhase::PreflightSnapshot.label()
            );
            snapshot
        }
        Err(_) => {
            let err = miette!(
                "runtime preflight stage `{}` timed out after {}s",
                RuntimeTurnPhase::PreflightSnapshot.label(),
                preflight_timeout.as_secs()
            );
            set_runtime_status(
                tx,
                RuntimeStatusLevel::Error,
                format!(
                    "runtime turn preflight 超时：{}",
                    RuntimeTurnPhase::PreflightSnapshot.label()
                ),
            );
            tracing::error!(
                elapsed_ms = snapshot_started_at.elapsed().as_millis(),
                timeout_secs = preflight_timeout.as_secs(),
                "runtime preflight stage timed out: {}",
                RuntimeTurnPhase::PreflightSnapshot.label()
            );
            return abort_runtime_turn_before_model(
                context,
                tx,
                live_draft_session,
                claimed_input_fingerprint.as_deref(),
                &claimed_event_ids,
                &claimed_app_notices,
                format!("runtime preflight failed: {err}"),
                "构建 runtime 快照失败。".to_string(),
            )
            .await;
        }
    };
    let snapshot_text = build_runtime_snapshot_text(context, &snapshot);
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
        enter_runtime_phase(context, tx, RuntimeTurnPhase::PreflightCompaction);
        let compaction_started_at = std::time::Instant::now();
        tracing::info!(
            "runtime preflight stage started: {}",
            RuntimeTurnPhase::PreflightCompaction.label()
        );
        let summary = match tokio::time::timeout(
            preflight_timeout,
            execute_pre_turn_runtime_compaction(context, &plan),
        )
        .await
        {
            Ok(summary) => {
                tracing::info!(
                    elapsed_ms = compaction_started_at.elapsed().as_millis(),
                    "runtime preflight stage completed: {}",
                    RuntimeTurnPhase::PreflightCompaction.label()
                );
                summary
            }
            Err(_) => {
                let err = miette!(
                    "runtime preflight stage `{}` timed out after {}s",
                    RuntimeTurnPhase::PreflightCompaction.label(),
                    preflight_timeout.as_secs()
                );
                set_runtime_status(
                    tx,
                    RuntimeStatusLevel::Error,
                    format!(
                        "runtime turn preflight 超时：{}",
                        RuntimeTurnPhase::PreflightCompaction.label()
                    ),
                );
                tracing::error!(
                    elapsed_ms = compaction_started_at.elapsed().as_millis(),
                    timeout_secs = preflight_timeout.as_secs(),
                    "runtime preflight stage timed out: {}",
                    RuntimeTurnPhase::PreflightCompaction.label()
                );
                return abort_runtime_turn_before_model(
                    context,
                    tx,
                    live_draft_session,
                    claimed_input_fingerprint.as_deref(),
                    &claimed_event_ids,
                    &claimed_app_notices,
                    format!("runtime preflight failed: {err}"),
                    "执行 pre-turn context compaction 失败。".to_string(),
                )
                .await;
            }
        };
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
        enter_runtime_phase(context, tx, RuntimeTurnPhase::ModelRequest);
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
                let is_overflow = is_context_budget_exceeded(&err);
                let overflow_fuse_tripped = if is_overflow {
                    handle_runtime_overflow(
                        context,
                        claimed_input_fingerprint.as_deref(),
                        &claimed_event_ids,
                        &claimed_app_notice_entries,
                        &err.to_string(),
                    )
                } else {
                    false
                };
                if !is_overflow && !overflow_fuse_tripped && !claimed_event_ids.is_empty() {
                    requeue_claimed_runtime_events(context, &claimed_event_ids);
                }
                let observation = format!("agent turn failed: {err}");
                let terminal_action = EpisodeActionRecord {
                    kind: "agent_turn_failed".to_string(),
                    summary: observation.clone(),
                };
                let mut terminal_actions = actions.clone();
                terminal_actions.push(terminal_action.clone());
                runtime_step.push_history_message(HistoryMessage::assistant(observation.clone()));
                if let Some(cell) = assistant_activity_cell(&observation) {
                    append_committed_activity_cells(tx, vec![cell]);
                }
                break 'agent_loop AgentLoopStepOutput {
                    observation: observation.clone(),
                    description: "模型请求失败。".to_string(),
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
                    render_tool_call_ui_event(context, call).unwrap_or_else(|_| {
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
                runtime_step.push_history_message(HistoryMessage::assistant(content));
            }
            runtime_step.push_history_message(HistoryMessage {
                message: AgentMessage::assistant_tool_call_protocol(None, calls.clone()),
                tool_ui_event: None,
                tool_call_ui_events: tool_call_ui_events.clone(),
            });
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
                let action_record =
                    summarize_action_from_tool_call(context, call).unwrap_or_else(|_| {
                        EpisodeActionRecord {
                            kind: "tool_call".to_string(),
                            summary: call.name.clone(),
                        }
                    });
                actions.push(action_record);
                enter_runtime_phase(context, tx, RuntimeTurnPhase::ToolExecution);
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
                runtime_step.push_history_message(HistoryMessage::tool(
                    call.id.clone(),
                    call.name.clone(),
                    result.history_content_with_budget(
                        &call.id,
                        &call.name,
                        context
                            .config
                            .main_model_config()
                            .tool_output_max_tokens
                            .max(1),
                    ),
                    result.ui_event.clone(),
                ));
                append_committed_activity_cells(
                    tx,
                    vec![activity_cell_from_tool_ui_event(result.ui_event.clone())],
                );
                tool_results.push(format!("{} => {}", call.name, result.summary));
                if let Some(reason) = result.turn_boundary_reason.clone() {
                    break 'agent_loop AgentLoopStepOutput {
                        observation: if tool_results.is_empty() {
                            reason.clone()
                        } else {
                            tool_results.join("\n")
                        },
                        description: format!(
                            "某个 tool 改变了后续所需的上下文视图；当前 turn 在该边界后立即结束，并在新 turn 中重新渲染世界状态。原因：{reason}"
                        ),
                        current_doing: "等待下一轮工具决策".to_string(),
                        actions: actions.clone(),
                    };
                }
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
                        current_doing: "等待下一轮工具决策".to_string(),
                        actions: actions.clone(),
                    };
                }
            }
            continue 'agent_loop;
        }

        let content = response_assistant_content.unwrap_or_default();
        if let RuntimeFollowUpDecision::Continue { reason } = runtime_turn_follow_up_decision(
            context,
            response.raw_stream_follow_up,
            &claimed_event_ids,
        ) {
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
        runtime_step.push_history_message(HistoryMessage::assistant(content.clone()));
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
            current_doing,
            actions: actions.clone(),
        };
    };
    runtime_step.set_current_doing(output.current_doing.clone());
    context.set_runtime_phase(None);
    if let Some(session) = live_draft_session {
        session.shutdown(context).await;
    } else {
        context.install_live_assistant_progress(None);
    }
    if let Some(fingerprint) = claimed_input_fingerprint.as_deref() {
        context.clear_runtime_overflow_failure(fingerprint);
    }
    finalize_claimed_runtime_events(context, &claimed_event_ids, &output);
    finalize_claimed_runtime_app_notices(context, &claimed_app_notices, &output).await;
    let history_messages = runtime_step.history_messages().to_vec();
    if !runtime_step.is_history_empty() {
        record_runtime_history_messages(context, runtime_step.into_turn_draft()).await;
    }
    record_workflow_run_evidence(context, &output).await;
    context.current_work_origin = None;
    context.workflow_step_started_bound_id = None;
    AgentLoopStepExecution {
        output,
        history_messages,
    }
}

enum ClaimedRuntimeInput {
    Event(EventView),
    AppNotice { app: AppId, reason: String },
}

fn claimed_runtime_input_fingerprint(inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    if inputs.is_empty() {
        return None;
    }

    let mut event_ids = inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.event_id.to_string()),
            ClaimedRuntimeInput::AppNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    event_ids.sort();

    let mut app_notices = inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(_) => None,
            ClaimedRuntimeInput::AppNotice { app, reason } => {
                Some(format!("{app}:{}", reason.trim()))
            }
        })
        .collect::<Vec<_>>();
    app_notices.sort();

    Some(format!(
        "events=[{}]|app_notices=[{}]",
        event_ids.join(","),
        app_notices.join(","),
    ))
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
            PendingWork::AppNotice { app, reason } => {
                let Some(current_reason) = context.apps.notice_reason(&app) else {
                    if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
                        app: app.clone(),
                        reason: String::new(),
                    }) {
                        tracing::error!(
                            "failed to consume stale app notice driver for {app}: {err:?}"
                        );
                    }
                    continue;
                };
                let reason = if current_reason.trim().is_empty() {
                    reason
                } else {
                    current_reason
                };
                claimed_inputs.push(ClaimedRuntimeInput::AppNotice { app, reason });
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

fn handle_runtime_overflow(
    context: &mut Context,
    fingerprint: Option<&str>,
    event_ids: &[String],
    app_notices: &[(AppId, String)],
    error_text: &str,
) -> bool {
    let Some(fingerprint) = fingerprint else {
        if !event_ids.is_empty() {
            requeue_claimed_runtime_events(context, event_ids);
        }
        return false;
    };

    let attempts = context.record_runtime_overflow_failure(fingerprint);
    if attempts < RUNTIME_OVERFLOW_FUSE_THRESHOLD {
        tracing::warn!(
            overflow_attempt = attempts,
            overflow_threshold = RUNTIME_OVERFLOW_FUSE_THRESHOLD,
            claimed_events = event_ids.join(","),
            claimed_app_notices = app_notices
                .iter()
                .map(|(app, _)| app.to_string())
                .collect::<Vec<_>>()
                .join(","),
            "runtime context overflow persisted; requeueing claimed inputs",
        );
        if !event_ids.is_empty() {
            requeue_claimed_runtime_events(context, event_ids);
        }
        return false;
    }

    let failure_note =
        format!("runtime context overflow persisted after {attempts} attempts: {error_text}");
    for event_id in event_ids {
        if let Err(err) =
            context
                .events
                .set_status(event_id, EventStatus::Failed, Some(failure_note.clone()))
        {
            tracing::error!("failed to mark overflowed event {event_id} as failed: {err:?}");
        }
        if let Ok(parsed_event_id) = uuid::Uuid::parse_str(event_id)
            && let Err(err) = context.pending_work.consume(PendingWork::Event {
                event_id: parsed_event_id,
            })
        {
            tracing::error!(
                "failed to consume overflowed event driver {event_id} after fuse trip: {err:?}"
            );
        }
    }

    for (app, reason) in app_notices {
        context.suppress_app_notice(app, reason.clone(), APP_NOTICE_OVERFLOW_SUPPRESSION);
        context.active_app_notices.remove(app);
        if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
            app: app.clone(),
            reason: String::new(),
        }) {
            tracing::error!(
                "failed to consume overflowed app notice driver for {app} after fuse trip: {err:?}"
            );
        }
    }

    context.clear_runtime_overflow_failure(fingerprint);
    tracing::error!(
        overflow_attempts = attempts,
        overflow_threshold = RUNTIME_OVERFLOW_FUSE_THRESHOLD,
        suppression_secs = APP_NOTICE_OVERFLOW_SUPPRESSION.as_secs(),
        claimed_events = event_ids.join(","),
        claimed_app_notices = app_notices
            .iter()
            .map(|(app, _)| app.to_string())
            .collect::<Vec<_>>()
            .join(","),
        "runtime context overflow fuse tripped; claimed inputs were terminated instead of requeued",
    );
    true
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
            action_kind = last_action
                .map(|action| action.kind.as_str())
                .unwrap_or("none"),
            action_summary = last_action
                .map(|action| action.summary.as_str())
                .unwrap_or(""),
            requeued_claimed_events = requeued.len(),
            event_ids = requeued.join(","),
            "requeued claimed runtime events left unresolved at turn end",
        );
    }
}

async fn finalize_claimed_runtime_app_notices(
    context: &mut Context,
    apps: &[AppId],
    output: &AgentLoopStepOutput,
) {
    if apps.is_empty() {
        return;
    }

    let mut released = Vec::new();
    for app in apps {
        if let Err(err) = context.apps.refresh_notice_for(app).await {
            tracing::error!("failed to refresh app notice for {app}: {err:?}");
        }
        let still_noticed = context.apps.notice_reason(app).is_some();
        let work = PendingWork::AppNotice {
            app: app.clone(),
            reason: String::new(),
        };
        if still_noticed {
            match context.pending_work.release_claimed(work) {
                Ok(true) => released.push(app.to_string()),
                Ok(false) => {}
                Err(err) => {
                    tracing::error!(
                        "failed to release claimed app notice driver for {app}: {err:?}"
                    );
                }
            }
        } else if let Err(err) = context.pending_work.consume(work) {
            tracing::error!("failed to consume app notice driver for {app}: {err:?}");
        }
    }

    if !released.is_empty() {
        let last_action = output.actions.last();
        tracing::info!(
            action_kind = last_action
                .map(|action| action.kind.as_str())
                .unwrap_or("none"),
            action_summary = last_action
                .map(|action| action.summary.as_str())
                .unwrap_or(""),
            reactivated_app_notice_drivers = released.len(),
            apps = released.join(","),
            "released claimed runtime app notice drivers back into frontier at turn end",
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
) -> HistoryMessage {
    match input {
        ClaimedRuntimeInput::Event(event) => match &event.payload {
            EventPayload::TelegramIncoming(payload) => HistoryMessage::user(format!(
                "<world_event source=\"telegram\" event_id=\"{}\" status=\"{}\">\nfrom: {}\nchat_title: {}\nchat_id: {}\nincoming_text: {}\n</world_event>",
                event.event_id,
                event.status,
                payload.sender,
                payload.chat_title,
                payload.chat_id,
                payload.incoming_text.trim(),
            )),
        },
        ClaimedRuntimeInput::AppNotice { app, reason } => HistoryMessage::user(format!(
            "<app_notice app=\"{}\">\nreason: {}\n</app_notice>",
            app, reason,
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
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::AwaitingDelivery
        ]));
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::Resolved
        ]));
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::Dismissed
        ]));
        assert!(claimed_event_statuses_are_terminal(&[EventStatus::Failed]));
        assert!(!claimed_event_statuses_are_terminal(&[
            EventStatus::Claimed
        ]));
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
    fn claimed_runtime_input_fingerprint_is_stable_and_sorted() {
        let event_a = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let event_b = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let inputs = vec![
            ClaimedRuntimeInput::AppNotice {
                app: AppId::terminal(),
                reason: "busy".to_string(),
            },
            ClaimedRuntimeInput::Event(EventView {
                event_id: event_a,
                source: crate::events::EventSource::Telegram,
                status: EventStatus::Pending,
                arrived_at_ms: 0,
                payload: EventPayload::TelegramIncoming(crate::events::TelegramIncomingEvent {
                    chat_id: "1".to_string(),
                    chat_kind: "private".to_string(),
                    chat_title: "chat".to_string(),
                    sender: "alice".to_string(),
                    incoming_text: "hello".to_string(),
                    telegram_update_id: 1,
                    telegram_message_id: None,
                    telegram_message_date: None,
                }),
                last_error: None,
            }),
            ClaimedRuntimeInput::Event(EventView {
                event_id: event_b,
                source: crate::events::EventSource::Telegram,
                status: EventStatus::Pending,
                arrived_at_ms: 0,
                payload: EventPayload::TelegramIncoming(crate::events::TelegramIncomingEvent {
                    chat_id: "2".to_string(),
                    chat_kind: "private".to_string(),
                    chat_title: "chat".to_string(),
                    sender: "bob".to_string(),
                    incoming_text: "world".to_string(),
                    telegram_update_id: 2,
                    telegram_message_id: None,
                    telegram_message_date: None,
                }),
                last_error: None,
            }),
        ];

        assert_eq!(
            claimed_runtime_input_fingerprint(&inputs).as_deref(),
            Some(
                "events=[00000000-0000-0000-0000-000000000001,00000000-0000-0000-0000-000000000002]|app_notices=[Terminal:busy]"
            )
        );
    }

    #[test]
    fn claimed_runtime_input_fingerprint_is_none_for_empty_batch() {
        assert_eq!(claimed_runtime_input_fingerprint(&[]), None);
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
    let request_timeout =
        Duration::from_secs(context.config.main_model_config().request_timeout_secs());
    let model_name = context
        .llm
        .model_name()
        .unwrap_or_else(|| context.config.main_model_config().model_id.clone());
    let mut attempt = 1usize;
    loop {
        set_runtime_status(tx, RuntimeStatusLevel::Debug, "Working");
        let turn_result = tokio::time::timeout(
            request_timeout,
            context.llm.run_agent_turn(context, request.clone()),
        )
        .await;
        match turn_result {
            Err(_) => {
                let err = miette!(
                    "agent turn timed out after {}s (model={}, messages={}, tools={}, estimated_input_tokens={estimated_input_tokens})",
                    request_timeout.as_secs(),
                    model_name,
                    request.messages.len(),
                    request.tools.len(),
                );
                let will_retry = true;
                write_current_turn_response_error_dump(&err.to_string(), attempt, will_retry).await;
                let capped_shift = (attempt.saturating_sub(1)).min(6) as u32;
                let backoff_ms = 300u64.saturating_mul(1u64 << capped_shift).min(30_000);
                let summary = format!(
                    "模型请求超时，重试 #{attempt}，等待 {:.1}s",
                    backoff_ms as f64 / 1000.0
                );
                set_runtime_status(tx, RuntimeStatusLevel::Warn, summary);
                tracing::warn!(
                    "run_agent_turn timed out after {}s; retry #{attempt} in {backoff_ms}ms (model={}, messages={}, tools={}, estimated_input_tokens={estimated_input_tokens})",
                    request_timeout.as_secs(),
                    model_name,
                    request.messages.len(),
                    request.tools.len(),
                );
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                attempt += 1;
            }
            Ok(Ok(response)) => {
                write_current_turn_response_dump(&response, attempt).await;
                clear_runtime_status(tx);
                return Ok(response);
            }
            Ok(Err(err)) => {
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

async fn build_hindsight_memory_context(
    context: &mut Context,
    claimed_inputs: &[ClaimedRuntimeInput],
) -> PromptMemoryContext {
    let hindsight = context.hindsight.clone();
    let current_input = current_turn_input_for_hindsight(claimed_inputs);

    let query = build_hindsight_recall_query(
        current_input.as_deref(),
        select_recent_runtime_conversation_for_hindsight(
            &context.memory.runtime_conversation_messages(),
            current_input.as_deref(),
        ),
    );

    let observations = hindsight
        .recall(
            &query,
            HindsightRecallOptions {
                types: vec!["observation".to_string()],
                max_tokens: 1200,
                budget: Some("mid".to_string()),
                include_chunks: false,
                max_chunk_tokens: 0,
                include_source_facts: false,
                max_source_facts_tokens: 0,
                ..Default::default()
            },
        )
        .await;
    let observations = match observations {
        Ok(response) => response
            .results
            .into_iter()
            .take(4)
            .map(Into::into)
            .collect::<Vec<_>>(),
        Err(err) => {
            tracing::warn!("hindsight observation recall failed: {err:?}");
            Vec::new()
        }
    };

    let raw_memories = hindsight
        .recall(
            &query,
            HindsightRecallOptions {
                types: vec!["world".to_string(), "experience".to_string()],
                max_tokens: 1400,
                budget: Some("mid".to_string()),
                include_chunks: false,
                max_chunk_tokens: 0,
                include_source_facts: true,
                max_source_facts_tokens: 1200,
                ..Default::default()
            },
        )
        .await;
    let raw_memories = match raw_memories {
        Ok(response) => response
            .results
            .into_iter()
            .take(4)
            .map(Into::into)
            .collect::<Vec<_>>(),
        Err(err) => {
            tracing::warn!("hindsight raw memory recall failed: {err:?}");
            Vec::new()
        }
    };

    let citations = build_prompt_memory_citations(&observations, &raw_memories);
    tracing::debug!(
        "hindsight memory context observations={} raw_memories={} citations={}",
        observations.len(),
        raw_memories.len(),
        citations.len()
    );

    PromptMemoryContext {
        observations,
        raw_memories,
        citations,
    }
}

fn build_prompt_memory_citations(
    observations: &[crate::reasoning::runtime::PromptMemoryFact],
    raw_memories: &[crate::reasoning::runtime::PromptMemoryFact],
) -> Vec<PromptMemoryCitation> {
    let mut citations = Vec::new();
    citations.extend(observations.iter().map(|memory| PromptMemoryCitation {
        kind: "observation".to_string(),
        id: memory.id.clone(),
        summary: summarize_hindsight_query_value(&memory.text, 96),
    }));
    citations.extend(raw_memories.iter().map(|memory| {
        PromptMemoryCitation {
            kind: memory
                .memory_type
                .clone()
                .unwrap_or_else(|| "memory".to_string()),
            id: memory.id.clone(),
            summary: summarize_hindsight_query_value(&memory.text, 96),
        }
    }));
    citations
}

fn build_hindsight_recall_query(
    current_input: Option<&str>,
    recent_messages: Vec<String>,
) -> String {
    let mut lines = vec!["问题：召回最相关的历史经验，帮助继续推进当前任务。".to_string()];
    if !recent_messages.is_empty() {
        lines.push("前文:".to_string());
        lines.extend(
            recent_messages
                .into_iter()
                .map(|line| format!("- {}", summarize_hindsight_query_value(&line, 120))),
        );
    }
    if let Some(current_input) = current_input.filter(|value| !value.trim().is_empty()) {
        lines.push("当前输入:".to_string());
        lines.push(summarize_hindsight_query_value(current_input, 240));
    }

    let mut query = lines.join("\n");
    if approx_token_count(&query) > HINDSIGHT_RECALL_QUERY_MAX_TOKENS {
        query = truncate_hindsight_query_preserving_latest_input(
            &query,
            current_input.unwrap_or_default(),
            HINDSIGHT_RECALL_QUERY_MAX_TOKENS,
        );
    }
    query
}

fn current_turn_input_for_hindsight(claimed_inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    claimed_inputs.first().and_then(|input| match input {
        ClaimedRuntimeInput::Event(event) => match &event.payload {
            EventPayload::TelegramIncoming(payload) => {
                let text = payload.incoming_text.trim();
                (!text.is_empty()).then(|| text.to_string())
            }
        },
        ClaimedRuntimeInput::AppNotice { app, reason } => {
            let reason = reason.trim();
            (!reason.is_empty()).then(|| format!("app notice from {app}: {reason}"))
        }
    })
}

fn select_recent_runtime_conversation_for_hindsight(
    messages: &[HistoryMessage],
    latest_input: Option<&str>,
) -> Vec<String> {
    let contextual_messages = slice_recent_runtime_conversation_turns(messages, 1);
    contextual_messages
        .into_iter()
        .filter_map(|message| format_runtime_message_for_hindsight(message, latest_input))
        .collect()
}

fn slice_recent_runtime_conversation_turns(
    messages: &[HistoryMessage],
    turns: usize,
) -> &[HistoryMessage] {
    if messages.is_empty() || turns == 0 {
        return &[];
    }

    let mut user_turns_seen = 0usize;
    let mut start_index = None;
    for (index, message) in messages.iter().enumerate().rev() {
        if message.is_user() {
            user_turns_seen += 1;
            if user_turns_seen >= turns {
                start_index = Some(index);
                break;
            }
        }
    }

    match start_index {
        Some(index) => &messages[index..],
        None => messages,
    }
}

fn format_runtime_message_for_hindsight(
    message: &HistoryMessage,
    latest_input: Option<&str>,
) -> Option<String> {
    let content = message.text_content()?.trim();
    if content.is_empty()
        || is_runtime_summary_message_for_hindsight(content)
        || content.starts_with("assistant tool-call protocol:")
    {
        return None;
    }

    let role = match &message.message {
        AgentMessage::User { .. } => "user",
        AgentMessage::Assistant { .. } => "assistant",
        AgentMessage::AssistantToolCallProtocol { .. } => "assistant",
        AgentMessage::System { .. } | AgentMessage::Tool { .. } => return None,
    };

    if role == "user" && latest_input.is_some_and(|latest| latest.trim() == content) {
        return None;
    }

    Some(format!("{role}: {content}"))
}

fn is_runtime_summary_message_for_hindsight(content: &str) -> bool {
    content.starts_with("Earlier runtime history summary:")
        || content.starts_with("Earlier tool/context progress summary:")
}

fn truncate_hindsight_query_preserving_latest_input(
    query: &str,
    latest_input: &str,
    max_tokens: usize,
) -> String {
    let latest_input = latest_input.trim();
    if latest_input.is_empty() {
        return truncate_text_to_token_budget(query, max_tokens);
    }

    let latest_only =
        format!("问题：召回最相关的历史经验，帮助继续推进当前任务。\n当前输入:\n{latest_input}");
    if approx_token_count(&latest_only) > max_tokens {
        return truncate_text_to_token_budget(&latest_only, max_tokens);
    }

    let marker = "\n当前输入:\n";
    let Some(marker_index) = query.find(marker) else {
        return truncate_text_to_token_budget(query, max_tokens);
    };
    let suffix = &query[marker_index..];
    if approx_token_count(suffix) >= max_tokens {
        return truncate_text_to_token_budget(&latest_only, max_tokens);
    }

    let prefix = &query[..marker_index];
    let prefix_lines = prefix.lines().collect::<Vec<_>>();
    let mut kept_prefix_lines = Vec::new();
    for line in prefix_lines.into_iter().rev() {
        kept_prefix_lines.insert(0, line);
        let candidate = format!("{}\n{}", kept_prefix_lines.join("\n"), suffix.trim_start());
        if approx_token_count(&candidate) > max_tokens {
            kept_prefix_lines.remove(0);
            break;
        }
    }
    let mut result = if kept_prefix_lines.is_empty() {
        latest_only
    } else {
        format!("{}\n{}", kept_prefix_lines.join("\n"), suffix.trim_start())
    };
    if approx_token_count(&result) > max_tokens {
        result = truncate_text_to_token_budget(&result, max_tokens);
    }
    result
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

async fn daat_locus_loop(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepDashboardStatus,
    workspace_app_invalidation_rx: &mut tokio::sync::mpsc::UnboundedReceiver<
        WorkspaceAppInvalidation,
    >,
) {
    let cycle_started_at = std::time::Instant::now();
    drain_workspace_app_invalidations(&mut context.workspace_apps, workspace_app_invalidation_rx);
    sync_workspace_apps_from_invalidation(context).await;
    if let Err(err) = context.apps.refresh_all_notices().await {
        tracing::error!("failed to refresh app notices: {err:?}");
    }
    refresh_sleep_backlogs(sleep_status).await;
    let forced_sleep_status =
        maybe_start_forced_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await;
    enqueue_app_notice_work(context);
    sync_driver_frontier_from_sources(context);
    if context.active_runtime_turn {
        // 检测 select! 取消导致的 stale flag：若 turn 已运行超过 request_timeout + 120s
        // 但 active_runtime_turn 仍为 true，说明 daat_locus_loop 被 tokio::select! 取消时
        // 未能执行 active_runtime_turn = false，需主动重置。
        let stale_threshold = Duration::from_secs(
            context
                .config
                .main_model_config()
                .request_timeout_secs()
                .saturating_add(120),
        );
        let is_stale = context
            .runtime_turn_started_at
            .map(|started| started.elapsed() > stale_threshold)
            .unwrap_or(false);
        if is_stale {
            tracing::warn!(
                elapsed_secs = context
                    .runtime_turn_started_at
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(0),
                threshold_secs = stale_threshold.as_secs(),
                "stale active_runtime_turn detected (likely cancelled by tokio::select!); resetting"
            );
            context.active_runtime_turn = false;
            context.set_runtime_phase(None);
            context.runtime_turn_started_at = None;
            // fall through to normal processing
        } else {
            let phase = context
                .active_runtime_phase
                .map(|phase| phase.label())
                .unwrap_or("running");
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                format!("处理中：runtime turn 正在运行 / {phase}"),
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
        .apps
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    context.active_runtime_turn = true;
    context.runtime_turn_started_at = Some(std::time::Instant::now());
    context.set_runtime_phase(Some(RuntimeTurnPhase::PreflightMemory));
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
    let _ = execute_agent_loop_step(context, Some(tx)).await;
    context.active_runtime_turn = false;
    context.runtime_turn_started_at = None;
    context.set_runtime_phase(None);
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

fn enqueue_app_notice_work(context: &mut Context) {
    for app_id in context.apps.app_ids() {
        if let Some(reason) = context.apps.notice_reason(&app_id) {
            if context.is_app_notice_suppressed(&app_id, &reason) {
                context.active_app_notices.remove(&app_id);
                if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
                    app: app_id.clone(),
                    reason: String::new(),
                }) {
                    tracing::error!(
                        "failed to remove suppressed app notice work for {app_id}: {err:?}"
                    );
                }
                continue;
            }
            if context.active_app_notices.insert(app_id.clone()) {
                if let Err(err) = context.pending_work.enqueue(PendingWork::AppNotice {
                    app: app_id.clone(),
                    reason,
                }) {
                    tracing::error!("failed to enqueue app notice work for {app_id}: {err:?}");
                }
            }
        } else {
            context.active_app_notices.remove(&app_id);
            context.clear_app_notice_suppression(&app_id);
        }
    }
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
    if trace_backlog < FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD {
        return None;
    }
    let status = format!("backlog 过高（traces={}）：已启动后台 sleep", trace_backlog);
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
        "backlog 过高（traces={}）：后台 sleep 已启动",
        trace_backlog
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
            let _ = context.plan.clear();
            for job in retain_plan.jobs {
                if let Err(err) = context.hindsight_retain.enqueue(job) {
                    tracing::error!("failed to enqueue hindsight retain job during clear: {err:?}");
                }
            }
            if retain_plan.must_flush_before_continue || context.memory.retain_backlog_count() > 0 {
                match context.hindsight_retain.flush().await {
                    Ok(()) => context.memory.mark_queued_retained(),
                    Err(err) => {
                        tracing::error!(
                            "failed to flush hindsight retain queue during clear: {err:?}"
                        );
                    }
                }
            }
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                "已将当前会话转入 hindsight，并清空当前会话消息历史与当前 plan",
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
            let prompt = &summary.prompt_improvement;
            let workflow = &summary.workflow_improvement;
            sleep_status.total_runs += 1;
            sleep_status.total_prompt_consumed_trace_events += prompt.consumed_trace_events;
            sleep_status.total_failure_patterns += prompt.failure_patterns.len();
            sleep_status.total_prompt_reflections += prompt.prompt_reflections;
            sleep_status.total_prompt_candidates += prompt.prompt_candidates;
            sleep_status.total_prompt_candidate_evaluations += prompt.prompt_candidate_evaluations;
            sleep_status.total_prompt_frontier_entries += prompt.prompt_frontier_entries;
            sleep_status.latest_prompt_frontier_root_entries = prompt.prompt_frontier_root_entries;
            sleep_status.latest_prompt_frontier_branched_entries =
                prompt.prompt_frontier_branched_entries;
            sleep_status.latest_prompt_frontier_max_generation =
                prompt.prompt_frontier_max_generation;
            sleep_status.total_bootstrap_demos += prompt.bootstrap_demos;
            sleep_status.total_stress_cases += prompt.stress_cases;
            sleep_status.total_instruction_hypotheses += prompt.instruction_hypotheses;
            sleep_status.total_runtime_demos += prompt.runtime_demos;
            sleep_status.total_turn_demos += prompt.turn_demos;
            sleep_status.total_prompt_system_additions += prompt.applied_system_additions;
            sleep_status.total_compiled_prompt_updates +=
                usize::from(prompt.compiled_prompt_updated);
            sleep_status.total_workflow_evidence_run_records += workflow.evidence_run_records;
            sleep_status.total_workflow_reflections += workflow.workflow_reflections;
            sleep_status.total_workflow_patch_candidates += workflow.patch_candidates;
            sleep_status.total_workflow_merge_candidates += workflow.merge_candidates;
            sleep_status.total_workflow_candidate_evaluations += workflow.candidate_evaluations;
            sleep_status.total_workflow_frontier_entries += workflow.frontier_entries;
            sleep_status.latest_workflow_frontier_root_entries = workflow.frontier_root_entries;
            sleep_status.latest_workflow_frontier_branched_entries =
                workflow.frontier_branched_entries;
            sleep_status.latest_workflow_frontier_max_generation = workflow.frontier_max_generation;
            sleep_status.total_workflow_patch_applied += workflow.patch_applied;
            sleep_status.total_workflow_merge_applied += workflow.merge_applied;
            sleep_status.total_workflow_update_rollbacks += workflow.update_rollbacks;
            sleep_status.total_workflow_optimization_rounds += workflow.optimization_rounds;
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
    let summary =
        apply_patch_in_root(&context.execution_cwd, &context.sandbox_policy, patch_text).await?;
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
