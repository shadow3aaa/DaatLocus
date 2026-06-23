use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use miette::{Result, miette};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    app::AppManager,
    context::Context,
    daemon::{
        DaemonControlCommand, DaemonLifecycleHandle, DaemonLifecycleState, DaemonLock,
        session::SessionId,
        session_ipc::{
            InputAttachment, IpcResponseEnvelope, SessionIpcRequest, SessionIpcResponse,
            SessionIpcServer, SessionIpcStreamEvent, SessionRuntimeStatus, read_request,
            write_response, write_stream_event,
        },
    },
    dashboard::render::{
        current_plan_step_for_dashboard, pending_user_inputs_from_sources,
        primitive_optimization_snapshot_for_dashboard, render_activity_for_dashboard,
        render_app_status_outputs_for_dashboard, render_dashboard_footer_context,
        render_sleep_status_output_for_dashboard, render_status_command_output_for_dashboard,
        render_system_prompt_output_for_dashboard, render_telegram_status_for_dashboard,
        runtime_activity_for_dashboard, runtime_optimization_snapshot_for_dashboard,
        status_command_snapshot_for_dashboard, token_usage_snapshot_for_dashboard,
    },
    dashboard::{
        DashboardAction, DashboardActivityHistoryStore, DashboardControlCommand,
        DashboardPendingUserInputMoveDirection, DashboardRuntimeActivity,
        DashboardRuntimeActivityStatus, DashboardRuntimeStatusLevel, DashboardState, ReducedMotion,
        activity_cells_from_history_items, dashboard_agent_name, execute_control_command,
        execute_dashboard_action, sync_web_activity_state,
    },
    events::{
        EventPayload, EventStatus, EventStore, TelegramIncomingEvent, TerminalIncomingAttachment,
        TerminalIncomingAttachmentKind, TerminalIncomingEvent,
    },
    memory::Memory,
    openskills::load_openskills_for_runtime,
    pending_work::{PendingEventMoveDirection, PendingWork, PendingWorkQueue},
    plan::Plan,
    preturn_state::PreTurnState,
    providers::build_llm,
    runtime::bootstrap::{
        PersistentTokenUsageRole, bootstrap_telegram_transport_state_from_acl, build_runtime_apps,
        emit_startup_progress, load_compiled_prompts_only, load_persistent_token_usage_store,
        load_token_estimate_baseline, sandbox_policy_for_runtime,
        wrap_llm_with_persistent_token_usage,
    },
    runtime::runtime_loop::{
        SleepTaskResult, daat_locus_loop, handle_dashboard_control_command,
        handle_sleep_task_result, reset_cancelled_runtime_turn,
    },
    runtime_context::build_preturn_context_text,
    sleep_status::{SleepStatusSnapshot, load_sleep_status_snapshot},
    telegram_acl::TelegramAclHandle,
    telegram_transport::state::{
        PendingOutboundMessage, TelegramTransportState, TelegramTransportStateHandle,
    },
    workflow::PrimitiveStore,
    workspace_app::paths::{resolve_runtime_workspace_dir, workspace_apps_dir},
    workspace_app::{WorkspaceAppInvalidation, start_workspace_app_watcher},
};

#[derive(Debug, Clone)]
pub(crate) struct SessionServeArgs {
    pub session_id: String,
    pub ipc_name: String,
    pub ipc_token: String,
    pub project_dir: Option<PathBuf>,
}

pub(crate) async fn run_session_serve(
    config: crate::config::Config,
    args: SessionServeArgs,
) -> Result<()> {
    let session_id = SessionId::from_string(args.session_id.clone())?;
    let mut lock =
        DaemonLock::acquire_with_suffix(&format!("session-{}", session_id.as_str())).await?;
    let daemon_lifecycle = DaemonLifecycleHandle::new(DaemonLifecycleState::Initializing);

    let telegram_acl = TelegramAclHandle::load().await;
    let events = EventStore::with_session(session_id.as_str()).await;
    let pending_work = PendingWorkQueue::with_session(session_id.as_str()).await;
    let dashboard_history =
        DashboardActivityHistoryStore::with_session(session_id.as_str()).await?;
    let initial_activity_history = dashboard_history.load_initial_window();
    let (tx, _rx) = watch::channel(DashboardState {
        agent_name: dashboard_agent_name(),
        runtime_status: Some("Session initializing".to_string()),
        runtime_status_level: Some(DashboardRuntimeStatusLevel::Info),
        runtime_activity: DashboardRuntimeActivity::new(
            DashboardRuntimeActivityStatus::Running,
            "Running",
            Some("Session initializing".to_string()),
        ),
        footer_context: "Session is initializing; runtime commands are disabled until ready."
            .to_string(),
        activity_history: initial_activity_history,
        ..DashboardState::default()
    });
    let (dashboard_control_tx, mut dashboard_control_rx) =
        mpsc::unbounded_channel::<DashboardControlCommand>();
    let (runtime_interrupt_tx, mut runtime_interrupt_rx) = mpsc::unbounded_channel::<()>();
    let (sleep_result_tx, mut sleep_result_rx) = mpsc::unbounded_channel::<SleepTaskResult>();
    let (workspace_app_invalidation_tx, mut workspace_app_invalidation_rx) =
        mpsc::unbounded_channel::<WorkspaceAppInvalidation>();
    let (daemon_control_tx, mut daemon_control_rx) =
        mpsc::unbounded_channel::<DaemonControlCommand>();
    let telegram = TelegramTransportState::with_session(session_id.as_str());
    let telegram_handle = telegram.handle();
    bootstrap_telegram_transport_state_from_acl(&telegram_handle, &telegram_acl);

    let ipc_server = SessionIpcServer::bind(&args.ipc_name).await?;
    let ipc_task = tokio::spawn(run_ipc_server(SessionIpcServerState {
        server: ipc_server,
        expected_session_id: session_id.as_str().to_string(),
        expected_ipc_token: args.ipc_token.clone(),
        lifecycle: daemon_lifecycle.clone(),
        dashboard_rx: tx.subscribe(),
        dashboard_tx: tx.clone(),
        dashboard_history: dashboard_history.clone(),
        events: events.clone(),
        pending_work: pending_work.clone(),
        telegram: telegram_handle.clone(),
        telegram_acl: telegram_acl.clone(),
        dashboard_control_tx: dashboard_control_tx.clone(),
        runtime_interrupt_tx: runtime_interrupt_tx.clone(),
        daemon_control_tx: daemon_control_tx.clone(),
    }));

    emit_startup_progress(format!(
        "[session] {} listening on local IPC {}",
        session_id.as_str(),
        args.ipc_name
    ));

    tokio::spawn(crate::browser_install::maybe_setup_browser_runtime());
    let compiled_prompts = load_compiled_prompts_only(&config).await?;
    let memory = Memory::with_session(session_id.as_str()).await;
    let plan = Plan::with_session(session_id.as_str()).await;
    let workflows = PrimitiveStore::new().await;
    let token_usage_store = load_persistent_token_usage_store(Some(session_id.as_str())).await;
    let client = build_llm(&config.main_model, &config)?;
    let client = wrap_llm_with_persistent_token_usage(
        PersistentTokenUsageRole::Main,
        config.main_model_config().model_id.clone(),
        client,
        token_usage_store.clone(),
    );
    let judge_model_key = config
        .judge
        .model
        .as_deref()
        .unwrap_or(&config.main_model)
        .to_string();
    let judge_model_id = config
        .models
        .get(&judge_model_key)
        .map(|model| model.model_id.clone())
        .unwrap_or_else(|| judge_model_key.clone());
    let judge_client = build_llm(&judge_model_key, &config)?;
    let judge_client = wrap_llm_with_persistent_token_usage(
        PersistentTokenUsageRole::Judge,
        judge_model_id,
        judge_client,
        token_usage_store.clone(),
    );
    let efficient_client = build_llm(&config.efficient_model, &config)?;
    let efficient_client = wrap_llm_with_persistent_token_usage(
        PersistentTokenUsageRole::Efficient,
        config.efficient_model_config().model_id.clone(),
        efficient_client,
        token_usage_store,
    );
    let coding_project_dir = args.project_dir;
    let execution_cwd = if let Some(project_dir) = coding_project_dir.as_ref() {
        if !project_dir.is_dir() {
            return Err(miette!(
                "session project directory does not exist: {}",
                project_dir.display()
            ));
        }
        project_dir.clone()
    } else {
        resolve_runtime_workspace_dir()?
    };
    tokio::fs::create_dir_all(&execution_cwd)
        .await
        .map_err(|err| {
            miette!(
                "failed to create session workspace {}: {err}",
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
    let sandbox_policy = sandbox_policy_for_runtime(&config, Some(&execution_cwd)).await;
    let runtime_apps = build_runtime_apps(&execution_cwd, &sandbox_policy);
    let apps = AppManager::new(None, runtime_apps.apps).await?;
    let openskills = load_openskills_for_runtime(&execution_cwd);
    let mut context = Context {
        session_id: Some(session_id.as_str().to_string()),
        llm: client,
        judge_llm: judge_client,
        efficient_llm: efficient_client,
        config,
        memory,
        plan,
        events,
        pending_work,
        workflows,
        openskills,
        bound_primitive_composition: None,
        bound_primitive_id: None,
        active_primitive_run: None,
        pending_primitive_run_flushes: Vec::new(),
        current_work_origin: None,
        workflow_step_started_bound_id: None,
        apps,
        workspace_apps: runtime_apps.workspace_registry,
        telegram: telegram_handle,
        telegram_acl: telegram_acl.clone(),
        compiled_prompts,
        execution_cwd,
        coding_project_dir,
        sandbox_policy,
        dashboard_tx: Some(tx.clone()),
        dashboard_history: Some(dashboard_history.clone()),
        daemon_control_tx: daemon_control_tx.clone(),
        latest_context_composition: None,
        active_runtime_turn: false,
        active_runtime_phase: None,
        runtime_turn_started_at: None,
        runtime_turn_started_at_ms: None,
        runtime_turn_epoch: 0,
        active_app_notices: HashMap::new(),
        runtime_overflow_failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        runtime_model_request_failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        suppressed_app_notices: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        live_progress_tx: Arc::new(parking_lot::Mutex::new(None)),
        telegram_live_drafts: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        claimed_event_ids: Vec::new(),
        claimed_app_notices: Vec::new(),
        afterclaim_context_fingerprint: None,
        idle_since: None,
        last_idle_sleep_at: None,
        session_title: crate::runtime::session_title::SessionTitleState::default(),
        token_estimate_baseline: load_token_estimate_baseline().await,
    };

    let mut sleep_status = load_sleep_status_snapshot().await;
    let startup_preturn_state = PreTurnState::new(&mut context).await;
    let startup_preturn_context_output =
        build_preturn_context_text(&context, &startup_preturn_state);
    let app_renders = context.apps.state_renders();
    let activity_history = dashboard_history.load_initial_window();
    tx.send_modify(|state| {
        *state = DashboardState {
            agent_name: dashboard_agent_name(),
            session_title: context.session_title.snapshot(),
            status_output: render_status_command_output_for_dashboard(&context, &app_renders),
            status_command: status_command_snapshot_for_dashboard(&context),
            sleep_status_output: render_sleep_status_output_for_dashboard(&context, &sleep_status),
            inspect_telegram_output: render_telegram_status_for_dashboard(&context),
            system_prompt_output: render_system_prompt_output_for_dashboard(&context),
            preturn_context_output: startup_preturn_context_output,
            app_status_outputs: render_app_status_outputs_for_dashboard(&context),
            skills: context.openskills.dashboard_summaries(),
            skill_errors: context.openskills.dashboard_errors(),
            pending_access_requests: context.telegram_acl.pending_requests(),
            pending_user_inputs: pending_user_inputs_from_sources(
                &context.events,
                &context.pending_work,
            ),
            activity_cells: if activity_history.items.is_empty() {
                render_activity_for_dashboard(&context)
            } else {
                activity_cells_from_history_items(&activity_history.items)
            },
            live_activity_cells: Vec::new(),
            web_activity_version: crate::dashboard::default_web_activity_version(),
            web_activity_items: Vec::new(),
            live_web_activity_items: Vec::new(),
            activity_history,
            last_cycle_elapsed_ms: None,
            runtime_status: None,
            runtime_status_level: None,
            runtime_activity: runtime_activity_for_dashboard(&context, &sleep_status, None, None),
            current_plan_step: current_plan_step_for_dashboard(&context),
            token_usage: token_usage_snapshot_for_dashboard(&context),
            runtime_optimization: runtime_optimization_snapshot_for_dashboard(&sleep_status),
            primitive_optimization: primitive_optimization_snapshot_for_dashboard(&sleep_status),
            context_composition: None,
            reduced_motion: ReducedMotion::default(),
            footer_context: render_dashboard_footer_context(&context, None),
            footer_estimated_input_tokens: None,
        };
        sync_web_activity_state(state);
    });
    crate::runtime::session_title::sync_session_title_placeholder(&mut context, &tx);

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

    daemon_lifecycle.mark_ready();

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|err| miette!("failed to install SIGTERM handler: {err}"))?;
    let mut sleep_running = sleep_status.running;
    let mut shutdown_completion_tx = None;
    let mut ctrl_c_disabled = false;
    let mut restart_requested = false;
    loop {
        if (SessionBoundaryRuntimeControlDrain {
            context: &mut context,
            tx: &tx,
            sleep_result_tx: &sleep_result_tx,
            sleep_running: &mut sleep_running,
            sleep_status: &mut sleep_status,
            dashboard_control_rx: &mut dashboard_control_rx,
            sleep_result_rx: &mut sleep_result_rx,
            daemon_control_rx: &mut daemon_control_rx,
            shutdown_completion_tx: &mut shutdown_completion_tx,
            restart_requested: &mut restart_requested,
        })
        .drain()
        .await
        {
            break;
        }

        tokio::select! {
            _ = daat_locus_loop(
                &mut context,
                &tx,
                &sleep_result_tx,
                &mut sleep_running,
                &mut sleep_status,
                &mut workspace_app_invalidation_rx,
            ) => {}
            Some(command) = daemon_control_rx.recv() => {
                reset_cancelled_runtime_turn(&mut context, "session daemon control interrupt");
                apply_session_daemon_control_command(
                    command,
                    &mut shutdown_completion_tx,
                    &mut restart_requested,
                );
                break;
            }
            Some(()) = runtime_interrupt_rx.recv() => {
                handle_dashboard_control_command(
                    &mut context,
                    &tx,
                    &sleep_result_tx,
                    &mut sleep_running,
                    &mut sleep_status,
                    DashboardControlCommand::InterruptRuntime,
                )
                .await;
            }
            signal = tokio::signal::ctrl_c(), if !ctrl_c_disabled => {
                match signal {
                    Ok(()) => {
                        tracing::info!("session received SIGINT, shutting down");
                        reset_cancelled_runtime_turn(&mut context, "SIGINT interrupt");
                        break;
                    }
                    Err(err) => {
                        tracing::warn!("ctrl_c listener failed: {err}");
                        reset_cancelled_runtime_turn(&mut context, "ctrl_c listener failure");
                        ctrl_c_disabled = true;
                    }
                }
            }
            _ = {
                #[cfg(unix)] { sigterm.recv() }
                #[cfg(not(unix))] { std::future::pending::<Option<()>>() }
            } => {
                tracing::info!("session received SIGTERM, shutting down");
                reset_cancelled_runtime_turn(&mut context, "SIGTERM interrupt");
                break;
            }
        }
    }

    daemon_lifecycle.mark_stopping();
    drop(workspace_app_watcher);
    context.dashboard_tx = None;
    context.shutdown().await;
    lock.release();
    if let Some(completion_tx) = shutdown_completion_tx.take() {
        let _ = completion_tx.send(());
    }
    drop(tx);
    ipc_task.abort();
    Ok(())
}

struct SessionIpcServerState {
    server: SessionIpcServer,
    expected_session_id: String,
    expected_ipc_token: String,
    lifecycle: DaemonLifecycleHandle,
    dashboard_rx: watch::Receiver<DashboardState>,
    dashboard_tx: watch::Sender<DashboardState>,
    dashboard_history: DashboardActivityHistoryStore,
    events: EventStore,
    pending_work: PendingWorkQueue,
    telegram: TelegramTransportStateHandle,
    telegram_acl: TelegramAclHandle,
    dashboard_control_tx: mpsc::UnboundedSender<DashboardControlCommand>,
    runtime_interrupt_tx: mpsc::UnboundedSender<()>,
    daemon_control_tx: mpsc::UnboundedSender<DaemonControlCommand>,
}

async fn run_ipc_server(state: SessionIpcServerState) {
    let state = Arc::new(state);
    loop {
        match state.server.accept().await {
            Ok(mut stream) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_ipc_connection(state, &mut stream).await {
                        tracing::warn!("session IPC connection failed: {err:?}");
                    }
                });
            }
            Err(err) => {
                tracing::error!("session IPC accept failed: {err:?}");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

async fn handle_ipc_connection(
    state: Arc<SessionIpcServerState>,
    stream: &mut interprocess::local_socket::tokio::Stream,
) -> Result<()> {
    let request = read_request(stream)
        .await
        .map_err(|err| miette!("read session IPC request failed: {err:?}"))?;
    let request_id = request.request_id.clone();
    let request_id_for_log = request_id.clone();
    let request_kind = request.body.kind();
    if let Some(response) = validate_ipc_request(
        &request,
        &state.expected_session_id,
        &state.expected_ipc_token,
    ) {
        write_response(stream, &response).await.map_err(|err| {
            miette!(
                "write session IPC validation response failed request_id={} request_kind={}: {err:?}",
                request_id,
                request_kind
            )
        })?;
        return Ok(());
    }

    if matches!(request.body, SessionIpcRequest::SubscribeDashboard) {
        return stream_dashboard_snapshots(state.dashboard_rx.clone(), stream, &request_id).await;
    }

    let response = match request.body {
        SessionIpcRequest::Status => IpcResponseEnvelope::ok(
            request_id,
            SessionIpcResponse::Status {
                runtime_status: runtime_status_from_state(&state),
            },
        ),
        SessionIpcRequest::StatusSummary => {
            let snapshot = state.dashboard_rx.borrow().clone();
            IpcResponseEnvelope::ok(
                request_id,
                SessionIpcResponse::StatusSummary {
                    summary: Box::new(crate::daemon::session_ipc::SessionStatusSummary {
                        runtime_status: runtime_status_from_state(&state),
                        session_title: snapshot.session_title.clone(),
                        dashboard:
                            crate::daemon::session_ipc::SessionStatusDashboard::from_dashboard_state(
                                &snapshot,
                            ),
                    }),
                },
            )
        }
        SessionIpcRequest::SubmitUserInput {
            origin,
            text,
            attachments,
            wait_for_reply,
        } => {
            let response = submit_user_input(
                &state.events,
                &state.pending_work,
                origin,
                text,
                attachments,
                wait_for_reply,
                request_id,
            )
            .await;
            refresh_pending_user_inputs(&state);
            response
        }
        SessionIpcRequest::DashboardCommand { command } => {
            let snapshot = state.dashboard_rx.borrow().clone();
            let output = execute_control_command(
                command.trim(),
                &state.telegram_acl,
                &snapshot,
                &state.dashboard_control_tx,
            );
            IpcResponseEnvelope::ok(
                request_id,
                SessionIpcResponse::DashboardCommandResult { output },
            )
        }
        SessionIpcRequest::DashboardAction { action } => {
            let result = execute_session_dashboard_action(action, &state);
            IpcResponseEnvelope::ok(
                request_id,
                SessionIpcResponse::DashboardActionResult { result },
            )
        }
        SessionIpcRequest::EnqueueTelegramEvent { event } => {
            enqueue_telegram_event(&state.events, &state.pending_work, event, request_id).await
        }
        SessionIpcRequest::DashboardSnapshot => IpcResponseEnvelope::ok(
            request_id,
            SessionIpcResponse::DashboardSnapshot {
                state: Box::new(state.dashboard_rx.borrow().clone()),
            },
        ),
        SessionIpcRequest::DashboardHistoryPage {
            before,
            after,
            limit,
        } => match query_dashboard_history(&state.dashboard_history, before, after, limit) {
            Ok(page) => IpcResponseEnvelope::ok(
                request_id,
                SessionIpcResponse::DashboardHistoryPage { page },
            ),
            Err(err) => IpcResponseEnvelope::error(
                request_id,
                "dashboard_history_failed",
                format!("{err:?}"),
                true,
            ),
        },
        SessionIpcRequest::DashboardInputHistory { limit } => {
            match state.dashboard_history.query_recent_user_inputs(limit) {
                Ok(history) => IpcResponseEnvelope::ok(
                    request_id,
                    SessionIpcResponse::DashboardInputHistory { history },
                ),
                Err(err) => IpcResponseEnvelope::error(
                    request_id,
                    "dashboard_input_history_failed",
                    format!("{err:?}"),
                    true,
                ),
            }
        }
        SessionIpcRequest::DashboardHistoryCount => {
            match state.dashboard_history.query_user_input_count() {
                Ok(count) => IpcResponseEnvelope::ok(
                    request_id,
                    SessionIpcResponse::DashboardHistoryCount { count },
                ),
                Err(err) => IpcResponseEnvelope::error(
                    request_id,
                    "dashboard_history_count_failed",
                    format!("{err:?}"),
                    true,
                ),
            }
        }
        SessionIpcRequest::DrainTelegramOutbox => IpcResponseEnvelope::ok(
            request_id,
            SessionIpcResponse::TelegramOutbox {
                messages: drain_telegram_outbox(&state.telegram),
            },
        ),
        SessionIpcRequest::RecordTelegramDelivery {
            event_id,
            status,
            note,
        } => match state.events.set_status(&event_id, status, note) {
            Ok(()) => IpcResponseEnvelope::ok(request_id, SessionIpcResponse::DeliveryRecorded),
            Err(err) => IpcResponseEnvelope::error(
                request_id,
                "record_delivery_failed",
                format!("{err:?}"),
                true,
            ),
        },
        SessionIpcRequest::RequeueTelegramOutbound { message } => {
            match state.telegram.requeue_outbound_front(message) {
                Ok(()) => IpcResponseEnvelope::ok(
                    request_id,
                    SessionIpcResponse::TelegramOutboundRequeued,
                ),
                Err(err) => IpcResponseEnvelope::error(
                    request_id,
                    "requeue_telegram_outbound_failed",
                    format!("{err:?}"),
                    true,
                ),
            }
        }
        SessionIpcRequest::SubscribeDashboard => unreachable!(),
        SessionIpcRequest::Shutdown { reason } => {
            tracing::info!("session shutdown requested over IPC: {reason}");
            let (completion_tx, _completion_rx) = oneshot::channel();
            let _ = state
                .daemon_control_tx
                .send(DaemonControlCommand::ShutdownRequested { completion_tx });
            IpcResponseEnvelope::ok(request_id, SessionIpcResponse::ShutdownAccepted)
        }
    };
    write_response(stream, &response).await.map_err(|err| {
        miette!(
            "write session IPC response failed request_id={} request_kind={}: {err:?}",
            request_id_for_log,
            request_kind
        )
    })
}

fn validate_ipc_request(
    request: &crate::daemon::session_ipc::IpcRequestEnvelope,
    expected_session_id: &str,
    expected_ipc_token: &str,
) -> Option<IpcResponseEnvelope> {
    if request.protocol_version != crate::daemon::session_ipc::SESSION_IPC_PROTOCOL_VERSION {
        return Some(IpcResponseEnvelope::error(
            request.request_id.clone(),
            "protocol_version_mismatch",
            "unsupported session IPC protocol version",
            false,
        ));
    }
    if request.session_id != expected_session_id || request.ipc_token != expected_ipc_token {
        return Some(IpcResponseEnvelope::error(
            request.request_id.clone(),
            "unauthorized",
            "invalid session IPC credentials",
            false,
        ));
    }
    None
}

fn runtime_status_from_state(state: &SessionIpcServerState) -> SessionRuntimeStatus {
    let snapshot = state.dashboard_rx.borrow();
    SessionRuntimeStatus {
        ready: state.lifecycle.get() == DaemonLifecycleState::Ready,
        status: state.lifecycle.get().to_string(),
        pending_work_count: state.pending_work.pending_count(),
        active_runtime_turn: matches!(
            snapshot.runtime_activity.status,
            DashboardRuntimeActivityStatus::Running
        ),
    }
}

fn execute_session_dashboard_action(
    action: DashboardAction,
    state: &SessionIpcServerState,
) -> crate::dashboard::DashboardActionResult {
    match action {
        DashboardAction::InterruptRuntime => match state.runtime_interrupt_tx.send(()) {
            Ok(()) => crate::dashboard::DashboardActionResult {
                success: true,
                message: "queued runtime interrupt".to_string(),
                detail: None,
            },
            Err(err) => crate::dashboard::DashboardActionResult {
                success: false,
                message: format!("failed to queue interrupt: {err}"),
                detail: None,
            },
        },
        DashboardAction::DismissPendingUserInput { event_id } => {
            let result = dismiss_pending_user_input(&state.events, &state.pending_work, event_id);
            if result.success {
                refresh_pending_user_inputs(state);
            }
            result
        }
        DashboardAction::ClearPendingUserInputs => {
            let result = clear_pending_user_inputs(&state.events, &state.pending_work);
            if result.success {
                refresh_pending_user_inputs(state);
            }
            result
        }
        DashboardAction::UpdatePendingUserInput {
            event_id,
            incoming_text,
        } => {
            let result = update_pending_user_input(&state.events, event_id, incoming_text);
            if result.success {
                refresh_pending_user_inputs(state);
            }
            result
        }
        DashboardAction::MovePendingUserInput {
            event_id,
            direction,
        } => {
            let result =
                move_pending_user_input(&state.events, &state.pending_work, event_id, direction);
            if result.success {
                refresh_pending_user_inputs(state);
            }
            result
        }
        DashboardAction::MovePendingUserInputToPosition {
            event_id,
            target_position,
        } => {
            let result = move_pending_user_input_to_position(
                &state.events,
                &state.pending_work,
                event_id,
                target_position,
            );
            if result.success {
                refresh_pending_user_inputs(state);
            }
            result
        }
        DashboardAction::PreemptPendingUserInput { event_id } => {
            let result = preempt_pending_user_input(
                &state.events,
                &state.pending_work,
                &state.runtime_interrupt_tx,
                event_id,
            );
            if result.success {
                refresh_pending_user_inputs(state);
            }
            result
        }
        other => execute_dashboard_action(other, &state.telegram_acl, &state.dashboard_control_tx),
    }
}

fn refresh_pending_user_inputs(state: &SessionIpcServerState) {
    state.dashboard_tx.send_modify(|dashboard| {
        dashboard.pending_user_inputs =
            pending_user_inputs_from_sources(&state.events, &state.pending_work);
    });
}

fn dismiss_pending_user_input(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    event_id: uuid::Uuid,
) -> crate::dashboard::DashboardActionResult {
    match validate_pending_terminal_event(events, event_id) {
        Ok(()) => {}
        Err(err) => {
            return crate::dashboard::DashboardActionResult {
                success: false,
                message: "pending input is not dismissible".to_string(),
                detail: Some(format!("{err:?}")),
            };
        }
    }
    if let Err(err) = pending_work.consume(PendingWork::Event { event_id }) {
        return crate::dashboard::DashboardActionResult {
            success: false,
            message: "failed to remove pending input from queue".to_string(),
            detail: Some(format!("{err:?}")),
        };
    }
    match events.set_status(&event_id.to_string(), EventStatus::Dismissed, None) {
        Ok(()) => crate::dashboard::DashboardActionResult {
            success: true,
            message: "dismissed pending input".to_string(),
            detail: None,
        },
        Err(err) => crate::dashboard::DashboardActionResult {
            success: false,
            message: "failed to dismiss pending input".to_string(),
            detail: Some(format!("{err:?}")),
        },
    }
}

fn clear_pending_user_inputs(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
) -> crate::dashboard::DashboardActionResult {
    let mut dismissed = 0usize;
    let mut first_error: Option<String> = None;
    for event_id in pending_work.pending_event_ids() {
        if let Err(err) = validate_pending_terminal_event(events, event_id) {
            tracing::debug!("skipping non-user pending input during queue clear: {err:?}");
            continue;
        }
        match pending_work.consume(PendingWork::Event { event_id }) {
            Ok(true) => {
                match events.set_status(&event_id.to_string(), EventStatus::Dismissed, None) {
                    Ok(()) => dismissed += 1,
                    Err(err) => {
                        first_error.get_or_insert_with(|| format!("{err:?}"));
                    }
                }
            }
            Ok(false) => {}
            Err(err) => {
                first_error.get_or_insert_with(|| format!("{err:?}"));
            }
        }
    }

    if let Some(err) = first_error {
        return crate::dashboard::DashboardActionResult {
            success: false,
            message: format!("cleared {dismissed} pending input(s), but some could not be cleared"),
            detail: Some(err),
        };
    }

    crate::dashboard::DashboardActionResult {
        success: true,
        message: if dismissed == 0 {
            "no pending inputs to clear".to_string()
        } else {
            format!("cleared {dismissed} pending input(s)")
        },
        detail: None,
    }
}

fn update_pending_user_input(
    events: &EventStore,
    event_id: uuid::Uuid,
    incoming_text: String,
) -> crate::dashboard::DashboardActionResult {
    match validate_pending_terminal_event(events, event_id) {
        Ok(()) => {}
        Err(err) => {
            return crate::dashboard::DashboardActionResult {
                success: false,
                message: "pending input is not editable".to_string(),
                detail: Some(format!("{err:?}")),
            };
        }
    }
    match events.update_terminal_incoming_text(&event_id.to_string(), incoming_text) {
        Ok(()) => crate::dashboard::DashboardActionResult {
            success: true,
            message: "updated pending input".to_string(),
            detail: None,
        },
        Err(err) => crate::dashboard::DashboardActionResult {
            success: false,
            message: "failed to update pending input".to_string(),
            detail: Some(format!("{err:?}")),
        },
    }
}

fn move_pending_user_input(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    event_id: uuid::Uuid,
    direction: DashboardPendingUserInputMoveDirection,
) -> crate::dashboard::DashboardActionResult {
    match validate_pending_terminal_event(events, event_id) {
        Ok(()) => {}
        Err(err) => {
            return crate::dashboard::DashboardActionResult {
                success: false,
                message: "pending input is not movable".to_string(),
                detail: Some(format!("{err:?}")),
            };
        }
    }
    let direction = match direction {
        DashboardPendingUserInputMoveDirection::Up => PendingEventMoveDirection::Up,
        DashboardPendingUserInputMoveDirection::Down => PendingEventMoveDirection::Down,
    };
    match pending_work.move_pending_event(event_id, direction) {
        Ok(true) => crate::dashboard::DashboardActionResult {
            success: true,
            message: "moved pending input".to_string(),
            detail: None,
        },
        Ok(false) => crate::dashboard::DashboardActionResult {
            success: true,
            message: "pending input order unchanged".to_string(),
            detail: None,
        },
        Err(err) => crate::dashboard::DashboardActionResult {
            success: false,
            message: "failed to move pending input".to_string(),
            detail: Some(format!("{err:?}")),
        },
    }
}

fn preempt_pending_user_input(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    runtime_interrupt_tx: &mpsc::UnboundedSender<()>,
    event_id: uuid::Uuid,
) -> crate::dashboard::DashboardActionResult {
    match validate_pending_terminal_event(events, event_id) {
        Ok(()) => {}
        Err(err) => {
            return crate::dashboard::DashboardActionResult {
                success: false,
                message: "pending input is not runnable".to_string(),
                detail: Some(format!("{err:?}")),
            };
        }
    }

    let moved = match pending_work.move_pending_event_to_front(event_id) {
        Ok(moved) => moved,
        Err(err) => {
            return crate::dashboard::DashboardActionResult {
                success: false,
                message: "failed to prioritize pending input".to_string(),
                detail: Some(format!("{err:?}")),
            };
        }
    };

    match runtime_interrupt_tx.send(()) {
        Ok(()) => crate::dashboard::DashboardActionResult {
            success: true,
            message: if moved {
                "prioritized pending input and queued runtime interrupt".to_string()
            } else {
                "pending input already first; queued runtime interrupt".to_string()
            },
            detail: None,
        },
        Err(err) => crate::dashboard::DashboardActionResult {
            success: false,
            message: "failed to queue runtime interrupt".to_string(),
            detail: Some(format!("{err}")),
        },
    }
}

fn move_pending_user_input_to_position(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    event_id: uuid::Uuid,
    target_position: usize,
) -> crate::dashboard::DashboardActionResult {
    match validate_pending_terminal_event(events, event_id) {
        Ok(()) => {}
        Err(err) => {
            return crate::dashboard::DashboardActionResult {
                success: false,
                message: "pending input is not movable".to_string(),
                detail: Some(format!("{err:?}")),
            };
        }
    }
    match pending_work.move_pending_event_to_position(event_id, target_position) {
        Ok(true) => crate::dashboard::DashboardActionResult {
            success: true,
            message: "moved pending input".to_string(),
            detail: None,
        },
        Ok(false) => crate::dashboard::DashboardActionResult {
            success: true,
            message: "pending input order unchanged".to_string(),
            detail: None,
        },
        Err(err) => crate::dashboard::DashboardActionResult {
            success: false,
            message: "failed to move pending input".to_string(),
            detail: Some(format!("{err:?}")),
        },
    }
}

fn validate_pending_terminal_event(events: &EventStore, event_id: uuid::Uuid) -> Result<()> {
    let event = events.view(&event_id.to_string())?;
    if !matches!(event.status, EventStatus::Pending) {
        return Err(miette!("event {event_id} is not pending"));
    }
    if !matches!(event.payload, EventPayload::TerminalIncoming(_)) {
        return Err(miette!("event {event_id} is not a user input event"));
    }
    Ok(())
}

async fn submit_user_input(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    origin: crate::daemon::session_ipc::UserInputOrigin,
    text: String,
    attachments: Vec<InputAttachment>,
    wait_for_reply: bool,
    request_id: String,
) -> IpcResponseEnvelope {
    let text = text.trim().to_string();
    if text.is_empty() {
        return IpcResponseEnvelope::error(request_id, "empty_input", "empty input", false);
    }
    let event_id = match register_terminal_event(events, pending_work, origin, text, attachments) {
        Ok(event_id) => event_id,
        Err(err) => {
            return IpcResponseEnvelope::error(
                request_id,
                "submit_failed",
                format!("{err:?}"),
                true,
            );
        }
    };
    if !wait_for_reply {
        return IpcResponseEnvelope::ok(
            request_id,
            SessionIpcResponse::Submitted {
                event_id: event_id.to_string(),
                reply_message: None,
                terminal_status: None,
            },
        );
    }
    match wait_for_send_reply(events.clone(), event_id).await {
        Ok((status, reply_message, note)) => IpcResponseEnvelope::ok(
            request_id,
            SessionIpcResponse::Submitted {
                event_id: event_id.to_string(),
                reply_message,
                terminal_status: note.or(Some(status)),
            },
        ),
        Err(err) => IpcResponseEnvelope::error(
            request_id,
            "wait_for_reply_failed",
            format!("{err:?}"),
            true,
        ),
    }
}

async fn enqueue_telegram_event(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    event: TelegramIncomingEvent,
    request_id: String,
) -> IpcResponseEnvelope {
    match events.register_telegram_incoming(event) {
        Ok(event_id) => match pending_work.enqueue(PendingWork::Event { event_id }) {
            Ok(_) => IpcResponseEnvelope::ok(
                request_id,
                SessionIpcResponse::Submitted {
                    event_id: event_id.to_string(),
                    reply_message: None,
                    terminal_status: None,
                },
            ),
            Err(err) => {
                IpcResponseEnvelope::error(request_id, "enqueue_failed", format!("{err:?}"), true)
            }
        },
        Err(err) => IpcResponseEnvelope::error(
            request_id,
            "register_telegram_failed",
            format!("{err:?}"),
            true,
        ),
    }
}

fn register_terminal_event(
    events: &EventStore,
    pending_work: &PendingWorkQueue,
    origin: crate::daemon::session_ipc::UserInputOrigin,
    incoming_text: String,
    attachments: Vec<InputAttachment>,
) -> Result<uuid::Uuid> {
    let event_id = events.register_terminal_incoming(TerminalIncomingEvent {
        origin: origin.terminal_origin_label().to_string(),
        incoming_text,
        attachments: attachments
            .into_iter()
            .map(|attachment| TerminalIncomingAttachment {
                kind: TerminalIncomingAttachmentKind::Image,
                media_type: attachment.media_type,
                local_path: attachment.local_path,
                description: attachment.description,
            })
            .collect(),
    })?;
    pending_work.enqueue(PendingWork::Event { event_id })?;
    Ok(event_id)
}

async fn wait_for_send_reply(
    events: EventStore,
    event_id: uuid::Uuid,
) -> Result<(String, Option<String>, Option<String>)> {
    const SEND_POLL_INTERVAL: Duration = Duration::from_millis(250);
    loop {
        match events.view(&event_id.to_string()) {
            Ok(event) if event.status.is_send_terminal_status() => {
                return Ok((
                    event.status.as_snake_case().to_string(),
                    event.reply_message,
                    event.last_error,
                ));
            }
            Ok(_) => {}
            Err(err) => return Err(miette!("failed to inspect send event: {err}")),
        }
        tokio::time::sleep(SEND_POLL_INTERVAL).await;
    }
}

fn query_dashboard_history(
    store: &DashboardActivityHistoryStore,
    before: Option<i64>,
    after: Option<i64>,
    limit: usize,
) -> Result<crate::dashboard::DashboardActivityHistoryPage> {
    if let Some(after) = after {
        store.query_after(Some(after), limit)
    } else {
        store.query_before(before, limit)
    }
}

fn drain_telegram_outbox(handle: &TelegramTransportStateHandle) -> Vec<PendingOutboundMessage> {
    let mut messages = Vec::new();
    while let Some(message) = handle.take_next_outbound() {
        messages.push(message);
    }
    messages
}

async fn stream_dashboard_snapshots(
    mut rx: watch::Receiver<DashboardState>,
    stream: &mut interprocess::local_socket::tokio::Stream,
    request_id: &str,
) -> Result<()> {
    let initial = rx.borrow_and_update().clone();
    write_stream_event(
        stream,
        &SessionIpcStreamEvent::DashboardSnapshot {
            state: Box::new(initial),
        },
    )
    .await
    .map_err(|err| {
        miette!(
            "write session IPC dashboard stream initial snapshot failed request_id={} request_kind=subscribe_dashboard: {err:?}",
            request_id
        )
    })?;
    loop {
        match rx.changed().await {
            Ok(()) => {
                let snapshot = rx.borrow().clone();
                write_stream_event(
                    stream,
                    &SessionIpcStreamEvent::DashboardSnapshot {
                        state: Box::new(snapshot),
                    },
                )
                .await
                .map_err(|err| {
                    miette!(
                        "write session IPC dashboard stream snapshot failed request_id={} request_kind=subscribe_dashboard: {err:?}",
                        request_id
                    )
                })?;
            }
            Err(_) => {
                write_stream_event(
                    stream,
                    &SessionIpcStreamEvent::DashboardClosed {
                        reason: "dashboard state channel closed".to_string(),
                    },
                )
                .await
                .map_err(|err| {
                    miette!(
                        "write session IPC dashboard stream close event failed request_id={} request_kind=subscribe_dashboard: {err:?}",
                        request_id
                    )
                })?;
                return Ok(());
            }
        }
    }
}

struct SessionBoundaryRuntimeControlDrain<'a> {
    context: &'a mut Context,
    tx: &'a watch::Sender<DashboardState>,
    sleep_result_tx: &'a mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &'a mut bool,
    sleep_status: &'a mut SleepStatusSnapshot,
    dashboard_control_rx: &'a mut mpsc::UnboundedReceiver<DashboardControlCommand>,
    sleep_result_rx: &'a mut mpsc::UnboundedReceiver<SleepTaskResult>,
    daemon_control_rx: &'a mut mpsc::UnboundedReceiver<DaemonControlCommand>,
    shutdown_completion_tx: &'a mut Option<oneshot::Sender<()>>,
    restart_requested: &'a mut bool,
}

impl SessionBoundaryRuntimeControlDrain<'_> {
    async fn drain(&mut self) -> bool {
        loop {
            if drain_session_daemon_control_commands(
                self.daemon_control_rx,
                self.shutdown_completion_tx,
                self.restart_requested,
            ) {
                return true;
            }

            let mut drained = false;
            while let Ok(result) = self.sleep_result_rx.try_recv() {
                *self.sleep_running = false;
                handle_sleep_task_result(self.context, self.tx, self.sleep_status, result).await;
                drained = true;
            }

            while let Ok(command) = self.dashboard_control_rx.try_recv() {
                handle_dashboard_control_command(
                    self.context,
                    self.tx,
                    self.sleep_result_tx,
                    self.sleep_running,
                    self.sleep_status,
                    command,
                )
                .await;
                drained = true;
                if drain_session_daemon_control_commands(
                    self.daemon_control_rx,
                    self.shutdown_completion_tx,
                    self.restart_requested,
                ) {
                    return true;
                }
            }

            if !drained {
                return false;
            }
        }
    }
}

fn drain_session_daemon_control_commands(
    daemon_control_rx: &mut mpsc::UnboundedReceiver<DaemonControlCommand>,
    shutdown_completion_tx: &mut Option<oneshot::Sender<()>>,
    restart_requested: &mut bool,
) -> bool {
    let mut should_shutdown = false;
    while let Ok(command) = daemon_control_rx.try_recv() {
        apply_session_daemon_control_command(command, shutdown_completion_tx, restart_requested);
        should_shutdown = true;
    }
    should_shutdown
}

fn apply_session_daemon_control_command(
    command: DaemonControlCommand,
    shutdown_completion_tx: &mut Option<oneshot::Sender<()>>,
    restart_requested: &mut bool,
) {
    match command {
        DaemonControlCommand::ShutdownRequested { completion_tx } => {
            *shutdown_completion_tx = Some(completion_tx);
        }
        DaemonControlCommand::RestartRequested => {
            *restart_requested = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{
        session::SessionId,
        session_ipc::{
            IpcRequestEnvelope, SESSION_IPC_PROTOCOL_VERSION, SessionIpcRequest, SessionIpcResponse,
        },
    };

    fn request(session_id: &str, ipc_token: &str) -> IpcRequestEnvelope {
        IpcRequestEnvelope::new(
            &SessionId::from_string(session_id.to_string()).expect("valid session id"),
            ipc_token.to_string(),
            SessionIpcRequest::Status,
        )
    }

    fn assert_error_response(
        response: IpcResponseEnvelope,
        request_id: &str,
        expected_code: &str,
        expected_retryable: bool,
    ) {
        assert_eq!(response.request_id, request_id);
        match response.body {
            SessionIpcResponse::Error {
                code, retryable, ..
            } => {
                assert_eq!(code, expected_code);
                assert_eq!(retryable, expected_retryable);
            }
            _ => panic!("expected IPC error response"),
        }
    }

    #[test]
    fn validate_ipc_request_rejects_protocol_mismatch_before_credentials() {
        let mut request = request("wrong-session", "wrong-token");
        request.protocol_version = SESSION_IPC_PROTOCOL_VERSION + 1;
        let request_id = request.request_id.clone();

        let response = validate_ipc_request(&request, "expected-session", "expected-token")
            .expect("protocol mismatch response");

        assert_error_response(response, &request_id, "protocol_version_mismatch", false);
    }

    #[test]
    fn validate_ipc_request_rejects_wrong_session_id_or_token() {
        let wrong_session = request("wrong-session", "expected-token");
        let wrong_session_id = wrong_session.request_id.clone();
        let response = validate_ipc_request(&wrong_session, "expected-session", "expected-token")
            .expect("wrong session response");
        assert_error_response(response, &wrong_session_id, "unauthorized", false);

        let wrong_token = request("expected-session", "wrong-token");
        let wrong_token_id = wrong_token.request_id.clone();
        let response = validate_ipc_request(&wrong_token, "expected-session", "expected-token")
            .expect("wrong token response");
        assert_error_response(response, &wrong_token_id, "unauthorized", false);
    }

    #[test]
    fn validate_ipc_request_accepts_matching_protocol_session_and_token() {
        let request = request("expected-session", "expected-token");
        assert!(validate_ipc_request(&request, "expected-session", "expected-token").is_none());
    }
}
