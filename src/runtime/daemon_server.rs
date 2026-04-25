use std::{collections::HashMap, time::Duration};

use crate::{
    app::{AppId, AppManager},
    context::Context,
    daemon::{
        DaemonControlCommand as RuntimeDaemonControlCommand, DaemonLifecycleHandle,
        DaemonLifecycleState, DaemonLock, start_server,
    },
    dashboard::render::{
        SleepDashboardStatus, render_activity_for_dashboard,
        render_app_status_outputs_for_dashboard, render_dashboard_footer_context,
        render_sleep_status_output_for_dashboard, render_status_command_output_for_dashboard,
        render_system_prompt_output_for_dashboard, render_telegram_status_for_dashboard,
    },
    dashboard::{DashboardControlCommand, DashboardState},
    events::EventStore,
    hindsight::{env::hindsight_llm_env_vars, managed::HindsightManagedServer},
    memory::Memory,
    pending_work::PendingWorkQueue,
    plan::Plan,
    providers::build_llm,
    reasoning::runtime::PromptMemoryContext,
    runtime_context::build_runtime_snapshot_text,
    snapshot::Snapshot,
    telegram_acl::TelegramAclHandle,
    telegram_transport::TelegramTransport,
    telegram_transport::state::TelegramTransportState,
    workflow::WorkflowStore,
    workspace_app::paths::{resolve_runtime_workspace_dir, workspace_apps_dir},
    workspace_app::{WorkspaceAppInvalidation, start_workspace_app_watcher},
};
use miette::{Result, miette};

use crate::browser_install::maybe_setup_browser_runtime;
use crate::runtime::bootstrap::{
    bootstrap_telegram_transport_state_from_acl, build_runtime_apps,
    connect_bootstrapped_hindsight, emit_startup_progress, load_compiled_prompts_only,
    sandbox_policy_for_runtime,
};
use crate::runtime::runtime_loop::{
    SleepTaskResult, daat_locus_loop, handle_dashboard_control_command, handle_sleep_task_result,
};

pub(crate) async fn run_daemon_serve(config: crate::config::Config) -> Result<()> {
    let mut lock = DaemonLock::acquire().await?;
    let daemon_token_registry = crate::daemon::load_or_create_daemon_token_registry().await?;
    let daemon_lifecycle = DaemonLifecycleHandle::new(DaemonLifecycleState::Initializing);
    let mut startup_failure_guard = DaemonStartupFailureGuard::new(daemon_lifecycle.clone());

    // Load telegram_acl first, create all channels, and start the HTTP server
    // immediately on the fixed local port so wait_for_daemon_ready can return
    // before expensive initialization starts.
    let telegram_acl = TelegramAclHandle::load().await;
    let events = EventStore::new().await;
    let pending_work = PendingWorkQueue::new().await;
    let (tx, _rx) = tokio::sync::watch::channel(DashboardState {
        runtime_status: Some("Daemon initializing".to_string()),
        footer_context: "Daemon is initializing; runtime commands are disabled until ready."
            .to_string(),
        ..DashboardState::default()
    });
    let (dashboard_control_tx, mut dashboard_control_rx) =
        tokio::sync::mpsc::unbounded_channel::<DashboardControlCommand>();
    let (sleep_result_tx, mut sleep_result_rx) =
        tokio::sync::mpsc::unbounded_channel::<SleepTaskResult>();
    let (workspace_app_invalidation_tx, mut workspace_app_invalidation_rx) =
        tokio::sync::mpsc::unbounded_channel::<WorkspaceAppInvalidation>();
    let (daemon_control_tx, mut daemon_control_rx) =
        tokio::sync::mpsc::unbounded_channel::<RuntimeDaemonControlCommand>();
    let (server_shutdown_tx, server_shutdown_rx) = tokio::sync::oneshot::channel();

    let daemon_server = start_server(
        config.daemon.port,
        daemon_token_registry,
        daemon_lifecycle.clone(),
        tx.subscribe(),
        telegram_acl.clone(),
        events.clone(),
        pending_work.clone(),
        dashboard_control_tx.clone(),
        daemon_control_tx.clone(),
        server_shutdown_rx,
    )
    .await?;
    emit_startup_progress(format!(
        "[daemon] listening on http://{}:{}",
        "127.0.0.1", daemon_server.port
    ));

    // Register signal handling before expensive initialization so Ctrl+C /
    // SIGTERM still works during cold-start compile. During startup we exit
    // directly; after entering the main loop this task is aborted and signal
    // handling is owned by the main select!. The DaemonLock file may remain, but
    // acquire() removes it on the next startup via stale PID detection.
    #[cfg(unix)]
    let mut early_sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|err| miette!("failed to install early SIGTERM handler: {err}"))?;
    let early_shutdown_handle = tokio::spawn(async move {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                match result {
                    Ok(()) => tracing::info!("daemon received SIGINT during startup, exiting"),
                    Err(err) => {
                        tracing::warn!("ctrl_c listener failed during startup: {err}");
                        return;
                    }
                }
            }
            _ = {
                #[cfg(unix)] { early_sigterm.recv() }
                #[cfg(not(unix))] { std::future::pending::<Option<()>>() }
            } => {
                tracing::info!("daemon received SIGTERM during startup, exiting");
            }
        }
        std::process::exit(0);
    });

    // Auto-install the browser runtime when it is missing.
    maybe_setup_browser_runtime().await;

    // Run expensive initialization after the server is already listening.
    let hindsight = connect_bootstrapped_hindsight(&config, true).await?;
    let hindsight_retain = hindsight.spawn_retain_worker();

    emit_startup_progress("[prompt-compile] loading compiled prompts before daemon startup...");
    let compiled_prompts = match load_compiled_prompts_only(&config).await {
        Ok(store) => store,
        Err(err) => {
            tracing::error!("failed to load compiled prompts: {err:?}");
            return Err(err);
        }
    };

    let memory = Memory::new().await;
    let plan = Plan::new().await;
    let workflows = WorkflowStore::new().await;
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
    let sandbox_policy = sandbox_policy_for_runtime(&config).await;
    let runtime_apps = build_runtime_apps(&execution_cwd, &sandbox_policy);
    let apps = AppManager::new(Some(AppId::terminal()), runtime_apps.apps).await?;
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
        telegram_acl: telegram_acl.clone(),
        compiled_prompts,
        execution_cwd,
        sandbox_policy,
        dashboard_tx: Some(tx.clone()),
        active_runtime_turn: false,
        active_runtime_phase: None,
        runtime_turn_started_at: None,
        active_app_notices: std::collections::HashSet::new(),
        runtime_overflow_failures: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        suppressed_app_notices: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        live_progress_tx: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        claimed_event_ids: Vec::new(),
        idle_since: None,
        last_idle_sleep_at: None,
    };

    // Replace the placeholder dashboard state with real state after context is built.
    let startup_snapshot = Snapshot::new(&mut context).await;
    let startup_snapshot_output = build_runtime_snapshot_text(&context, &startup_snapshot);
    let app_renders = context.apps.state_renders();
    tx.send_modify(|state| {
        *state = DashboardState {
            focused_app: context.apps.focused(),
            status_output: render_status_command_output_for_dashboard(&context, &app_renders),
            sleep_status_output: render_sleep_status_output_for_dashboard(
                &context,
                &SleepDashboardStatus::default(),
            ),
            inspect_telegram_output: render_telegram_status_for_dashboard(&context),
            system_prompt_output: render_system_prompt_output_for_dashboard(&context),
            snapshot_output: startup_snapshot_output,
            app_status_outputs: render_app_status_outputs_for_dashboard(&context),
            pending_access_requests: context.telegram_acl.pending_requests(),
            activity_cells: render_activity_for_dashboard(&context),
            live_activity_cells: Vec::new(),
            last_cycle_elapsed_ms: None,
            runtime_status: None,
            footer_context: render_dashboard_footer_context(&context, None),
            footer_estimated_input_tokens: None,
        };
    });

    let workspace_app_watcher = match start_workspace_app_watcher(
        workspace_apps_dir(&context.execution_cwd),
        workspace_app_invalidation_tx,
    ) {
        Ok(watcher) => Some(watcher),
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
                    telegram_acl,
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

    // Startup is complete; runtime commands may now be accepted.
    daemon_lifecycle.mark_ready();
    startup_failure_guard.disarm();

    // Abort early signal handling so the main loop owns it.
    early_shutdown_handle.abort();

    // SIGTERM -> graceful shutdown on Unix. Other platforms use pending() so the
    // select! structure stays uniform.
    #[cfg(unix)]
    let mut sigterm = {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|err| miette!("failed to install SIGTERM handler: {err}"))?
    };
    #[cfg(not(unix))]
    let sigterm_never = std::future::pending::<Option<()>>();

    let mut sleep_running = false;
    let mut sleep_status = SleepDashboardStatus::default();
    let mut shutdown_completion_tx = None;
    let mut ctrl_c_disabled = false;
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
            Some(RuntimeDaemonControlCommand::ShutdownRequested { completion_tx }) = daemon_control_rx.recv() => {
                shutdown_completion_tx = Some(completion_tx);
                break;
            }
            signal = tokio::signal::ctrl_c(), if !ctrl_c_disabled => {
                match signal {
                    Ok(()) => {
                        tracing::info!("daemon received SIGINT, shutting down");
                        break;
                    }
                    Err(err) => {
                        tracing::warn!("ctrl_c listener failed: {err}");
                        ctrl_c_disabled = true;
                    }
                }
            }
            _ = {
                #[cfg(unix)] { sigterm.recv() }
                #[cfg(not(unix))] { sigterm_never }
            } => {
                tracing::info!("daemon received SIGTERM, shutting down");
                break;
            }
        }
    }

    daemon_lifecycle.mark_stopping();
    drop(workspace_app_watcher);
    if let Some(handle) = telegram_transport {
        handle.abort();
    }
    let hindsight_config = context.config.hindsight.clone();
    let hindsight_llm_vars = hindsight_llm_env_vars(&context.config)
        .await
        .unwrap_or_default();
    context.shutdown().await;
    let managed = HindsightManagedServer::new(hindsight_config, hindsight_llm_vars);
    match tokio::time::timeout(Duration::from_secs(10), managed.stop()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!("[hindsight] stop failed: {err}");
        }
        Err(_) => {
            tracing::warn!("[hindsight] stop timed out during daemon shutdown");
        }
    }
    lock.release();
    if let Some(completion_tx) = shutdown_completion_tx.take() {
        let _ = completion_tx.send(());
    }
    let _ = server_shutdown_tx.send(());
    daemon_server.shutdown().await;
    Ok(())
}

struct DaemonStartupFailureGuard {
    lifecycle: DaemonLifecycleHandle,
    armed: bool,
}

impl DaemonStartupFailureGuard {
    fn new(lifecycle: DaemonLifecycleHandle) -> Self {
        Self {
            lifecycle,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DaemonStartupFailureGuard {
    fn drop(&mut self) {
        if self.armed {
            self.lifecycle.mark_failed_if_initializing();
        }
    }
}
