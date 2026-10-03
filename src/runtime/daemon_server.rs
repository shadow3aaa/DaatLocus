use std::{collections::HashMap, sync::Arc, time::Duration};

use miette::{Result, miette};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    daemon::{
        DAEMON_HOST_DISPLAY, DaemonControlCommand, DaemonLifecycleHandle, DaemonLifecycleState,
        DaemonLock, DaemonServerStartParams, SessionTokenStore, delete_session_by_id, session,
        session_client_for_id, session_ipc, spawn_detached_daemon_process, start_server,
        terminate_process_backed_sessions,
    },
    daemon_tray::{DaemonTrayHandle, DaemonTrayStartup},
    dashboard::{
        DashboardControlCommand, DashboardRuntimeActivity, DashboardRuntimeActivityStatus,
        DashboardRuntimeStatusLevel, DashboardState, dashboard_agent_name,
    },
    events::{EventStatus, TelegramIncomingEvent},
    runtime::bootstrap::{bootstrap_telegram_transport_state_from_acl, emit_startup_progress},
    telegram_acl::TelegramAclHandle,
    telegram_transport::{
        TelegramAuthVerifier, TelegramDeliveryClient, TelegramInputRouter,
        TelegramSessionCommandHandler, TelegramTransport,
        state::{PendingOutboundMessage, TelegramTransportState},
    },
};

const SESSION_STARTING_HEALTH_GRACE_MS: i64 = 60_000;

struct ManagerTelegramInputRouter {
    sessions: session::SessionRegistry,
    session_tokens: SessionTokenStore,
    telegram_defaults: session::TelegramSessionDefaults,
}

struct ManagerTelegramAuthVerifier {
    auth_registry: crate::daemon::DaemonTokenRegistryHandle,
}

#[async_trait::async_trait]
impl TelegramAuthVerifier for ManagerTelegramAuthVerifier {
    async fn authorize_telegram_verification(&self, token: &str) -> bool {
        self.auth_registry.authorize_token(token).await
    }
}

#[async_trait::async_trait]
impl TelegramInputRouter for ManagerTelegramInputRouter {
    async fn route_telegram_event(&self, event: TelegramIncomingEvent) -> Result<()> {
        let chat_id = event.chat_id.clone();
        let mapped_session = self.telegram_defaults.get(&chat_id).filter(|session_id| {
            self.sessions
                .get(session_id)
                .is_some_and(|info| !info.scope.is_study())
        });
        let session_id = match mapped_session {
            Some(session_id) => session_id,
            _ => {
                let info = self
                    .sessions
                    .create(
                        session::SessionScope::General,
                        Some(format!("Telegram {}", event.chat_title.trim())),
                    )
                    .await?;
                self.telegram_defaults
                    .set(chat_id.clone(), info.session_id.clone())
                    .await?;
                info.session_id
            }
        };
        let client =
            session_client_for_id(&self.sessions, &self.session_tokens, session_id.as_str())
                .await?;
        match client
            .request(session_ipc::SessionIpcRequest::EnqueueTelegramEvent { event })
            .await?
        {
            session_ipc::SessionIpcResponse::Submitted { .. } => Ok(()),
            session_ipc::SessionIpcResponse::Error { message, .. } => {
                Err(miette!("session rejected telegram event: {message}"))
            }
            _ => Err(miette!("unexpected session IPC telegram route response")),
        }
    }
}

#[async_trait::async_trait]
impl TelegramSessionCommandHandler for ManagerTelegramInputRouter {
    async fn handle_session_command(
        &self,
        chat_id: &str,
        chat_title: &str,
        command: &str,
    ) -> Result<Option<String>> {
        let command = command.trim();
        let Some(parsed) = parse_telegram_session_command(command) else {
            return Ok(None);
        };
        match parsed {
            TelegramSessionCommand::SessionList => Ok(Some(self.telegram_session_list(chat_id))),
            TelegramSessionCommand::SessionNew { title } => {
                let title = title
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| default_telegram_session_title(chat_id, chat_title));
                let info = self
                    .sessions
                    .create(session::SessionScope::General, Some(title))
                    .await?;
                self.telegram_defaults
                    .set(chat_id.to_string(), info.session_id.clone())
                    .await?;
                Ok(Some(format!(
                    "created and attached session `{}`",
                    info.session_id.as_str()
                )))
            }
            TelegramSessionCommand::SessionAttach { reference } => {
                let Some(reference) = reference else {
                    return Ok(Some(
                        "usage: /session_attach <session_id_or_unique_prefix>".to_string(),
                    ));
                };
                match resolve_session_reference(&self.sessions, &reference) {
                    Ok(info) if info.scope.is_study() => Ok(Some(
                        "the study session is not available to Telegram".to_string(),
                    )),
                    Ok(info) => {
                        self.telegram_defaults
                            .set(chat_id.to_string(), info.session_id.clone())
                            .await?;
                        Ok(Some(format!(
                            "attached this chat to session `{}` ({})",
                            info.session_id.as_str(),
                            session_display_title(&info)
                        )))
                    }
                    Err(message) => Ok(Some(message)),
                }
            }
            TelegramSessionCommand::SessionDelete { reference } => {
                let Some(reference) = reference else {
                    return Ok(Some(
                        "usage: /session_delete <session_id_or_unique_prefix>".to_string(),
                    ));
                };
                let info = match resolve_session_reference(&self.sessions, &reference) {
                    Ok(info) if info.scope.is_study() => {
                        return Ok(Some(
                            "the study session is not available to Telegram".to_string(),
                        ));
                    }
                    Ok(info) => info,
                    Err(message) => return Ok(Some(message)),
                };
                let deleted = delete_session_by_id(
                    &self.sessions,
                    &self.session_tokens,
                    &info.session_id,
                    "telegram session_delete command",
                )
                .await?;
                if !deleted {
                    return Ok(Some(format!("session `{reference}` was not found")));
                }
                let removed_defaults = self
                    .telegram_defaults
                    .remove_by_session(&info.session_id)
                    .await?;
                let current_chat_note = if removed_defaults.iter().any(|removed| removed == chat_id)
                {
                    " This chat now has no attached session; the next normal message will create one."
                } else {
                    ""
                };
                Ok(Some(format!(
                    "deleted session `{}` and removed {} Telegram default mapping(s).{}",
                    info.session_id.as_str(),
                    removed_defaults.len(),
                    current_chat_note
                )))
            }
            TelegramSessionCommand::Ask => Ok(Some(
                "ask mode is only available from the dashboard composer or `daat-locus send`."
                    .to_string(),
            )),
            TelegramSessionCommand::AttachRejected | TelegramSessionCommand::DeleteRejected => Ok(
                Some("usage: pass exactly one session id or unique prefix".to_string()),
            ),
        }
    }

    async fn handle_dashboard_command(&self, chat_id: &str, command: &str) -> Result<String> {
        let Some(session_id) = self.telegram_defaults.get(chat_id) else {
            return Ok(
                "no attached session. Use /session_list to inspect sessions or /session_new [title] to create one."
                    .to_string(),
            );
        };
        if self.sessions.get(&session_id).is_none() {
            return Ok(format!(
                "attached session `{}` no longer exists. Use /session_list or /session_attach <session_id_or_unique_prefix>.",
                session_id.as_str()
            ));
        }
        let client =
            session_client_for_id(&self.sessions, &self.session_tokens, session_id.as_str())
                .await?;
        let response = client
            .request(session_ipc::SessionIpcRequest::DashboardCommand {
                command: command.trim().to_string(),
            })
            .await;
        Ok(match response {
            Ok(session_ipc::SessionIpcResponse::DashboardCommandResult { output }) => output,
            Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => message,
            Ok(_) => "unexpected session IPC dashboard command response".to_string(),
            Err(err) => format!("session dashboard command failed: {err:?}"),
        })
    }
}

impl ManagerTelegramInputRouter {
    fn telegram_session_list(&self, chat_id: &str) -> String {
        let current = self.telegram_defaults.get(chat_id).filter(|session_id| {
            self.sessions
                .get(session_id)
                .is_some_and(|info| !info.scope.is_study())
        });
        let mut sessions = self
            .sessions
            .list()
            .into_iter()
            .filter(|info| !info.scope.is_study())
            .collect::<Vec<_>>();
        sessions.sort_by_key(|info| info.started_at_ms);
        if sessions.is_empty() {
            return "no sessions
/session_new [title] creates and attaches one"
                .to_string();
        }

        let mut lines = Vec::with_capacity(sessions.len() + 2);
        match current.as_ref() {
            Some(session_id) => lines.push(format!("attached: `{}`", session_id.as_str())),
            None => lines
                .push("attached: none. The next normal message will create a session.".to_string()),
        }
        lines.push("sessions:".to_string());
        lines.extend(sessions.into_iter().map(|info| {
            let marker = if current.as_ref() == Some(&info.session_id) {
                "current"
            } else {
                "      "
            };
            format!(
                "{} `{}` | {} | {}",
                marker,
                info.session_id.as_str(),
                session_display_title(&info),
                session_scope_label(&info.scope)
            )
        }));
        lines.join(
            "
",
        )
    }
}

enum TelegramSessionCommand {
    SessionList,
    SessionNew { title: Option<String> },
    SessionAttach { reference: Option<String> },
    SessionDelete { reference: Option<String> },
    AttachRejected,
    DeleteRejected,
    Ask,
}

/// Parse a Telegram session command once.
/// `session_new` keeps the raw remainder as the title.
/// `session_attach` / `session_switch` and `session_delete` accept exactly one reference.
fn parse_telegram_session_command(command: &str) -> Option<TelegramSessionCommand> {
    let command = command.trim().trim_start_matches('/');
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let mut chars = command.char_indices();
    let verb_end = chars
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(command.len());
    let verb = &command[..verb_end];
    let remainder = command[verb_end..].trim();
    match verb {
        "session_list" => Some(TelegramSessionCommand::SessionList),
        "session_new" => Some(TelegramSessionCommand::SessionNew {
            title: (!remainder.is_empty()).then(|| remainder.to_string()),
        }),
        "session_attach" | "session_switch" => parse_single_reference(remainder, true),
        "session_delete" => parse_single_reference(remainder, false),
        "ask" => Some(TelegramSessionCommand::Ask),
        _ => None,
    }
}

fn parse_single_reference(remainder: &str, attach: bool) -> Option<TelegramSessionCommand> {
    if remainder.is_empty() {
        return Some(if attach {
            TelegramSessionCommand::SessionAttach { reference: None }
        } else {
            TelegramSessionCommand::SessionDelete { reference: None }
        });
    }
    let words = crate::dashboard::tokenize_shell_words(remainder)?;
    match words.as_slice() {
        [reference] => Some(if attach {
            TelegramSessionCommand::SessionAttach {
                reference: Some(reference.clone()),
            }
        } else {
            TelegramSessionCommand::SessionDelete {
                reference: Some(reference.clone()),
            }
        }),
        _ => Some(if attach {
            TelegramSessionCommand::AttachRejected
        } else {
            TelegramSessionCommand::DeleteRejected
        }),
    }
}

fn default_telegram_session_title(chat_id: &str, chat_title: &str) -> String {
    let title = chat_title.trim();
    if title.is_empty() {
        format!("Telegram {chat_id}")
    } else {
        format!("Telegram {title}")
    }
}

fn resolve_session_reference(
    sessions: &session::SessionRegistry,
    reference: &str,
) -> std::result::Result<session::SessionInfo, String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err("session id cannot be empty".to_string());
    }
    let sessions = sessions.list();
    if let Some(info) = sessions
        .iter()
        .find(|info| info.session_id.as_str() == reference)
    {
        return Ok(info.clone());
    }
    let matches = sessions
        .into_iter()
        .filter(|info| info.session_id.as_str().starts_with(reference))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [info] => Ok(info.clone()),
        [] => Err(format!("session `{reference}` was not found")),
        _ => Err(format!(
            "session prefix `{reference}` is ambiguous; use more characters"
        )),
    }
}

fn session_display_title(info: &session::SessionInfo) -> String {
    info.title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| info.session_id.as_str())
        .to_string()
}

fn session_scope_label(scope: &session::SessionScope) -> String {
    match scope {
        session::SessionScope::General => "general".to_string(),
        session::SessionScope::Project { project_dir } => {
            format!("project {}", project_dir.display())
        }
        session::SessionScope::Study => "study".to_string(),
    }
}

pub async fn run_daemon_serve(config: crate::config::Config) -> Result<()> {
    run_daemon_serve_inner(config, None, None).await
}

pub async fn run_daemon_serve_with_tray(
    config: crate::config::Config,
    tray_handle: DaemonTrayHandle,
    tray_startup_tx: oneshot::Sender<DaemonTrayStartup>,
) -> Result<()> {
    run_daemon_serve_inner(config, Some(tray_handle), Some(tray_startup_tx)).await
}

async fn run_daemon_serve_inner(
    config: crate::config::Config,
    tray_handle: Option<DaemonTrayHandle>,
    tray_startup_tx: Option<oneshot::Sender<DaemonTrayStartup>>,
) -> Result<()> {
    let mut lock = DaemonLock::acquire().await?;
    let daemon_token_registry = crate::daemon::load_or_create_daemon_token_registry().await?;
    let daemon_lifecycle = DaemonLifecycleHandle::new(DaemonLifecycleState::Initializing);

    let telegram_acl = TelegramAclHandle::load().await;
    let sessions = session::SessionRegistry::load().await?;
    let telegram_defaults = session::TelegramSessionDefaults::load().await?;
    let session_tokens: SessionTokenStore = Arc::new(parking_lot::RwLock::new(HashMap::new()));
    hydrate_session_tokens(&sessions, &session_tokens).await;
    let telegram_sessions = sessions.clone();
    let telegram_session_tokens = session_tokens.clone();
    let (dashboard_tx, _dashboard_rx) = watch::channel(manager_dashboard_state(&telegram_acl));
    let (dashboard_control_tx, mut dashboard_control_rx) =
        mpsc::unbounded_channel::<DashboardControlCommand>();
    let (daemon_control_tx, mut daemon_control_rx) =
        mpsc::unbounded_channel::<DaemonControlCommand>();
    let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel();
    let mut server_shutdown_tx = Some(server_shutdown_tx);

    let daemon_server = start_server(DaemonServerStartParams {
        port: config.daemon.port,
        auth_registry: daemon_token_registry.clone(),
        lifecycle: daemon_lifecycle.clone(),
        dashboard_rx: dashboard_tx.subscribe(),
        telegram_acl: telegram_acl.clone(),
        dashboard_control_tx: dashboard_control_tx.clone(),
        daemon_control_tx: daemon_control_tx.clone(),
        sessions: sessions.clone(),
        session_tokens: session_tokens.clone(),
        shutdown_rx: server_shutdown_rx,
    })
    .await?;
    emit_startup_progress(format!(
        "[manager] listening on http://{}:{}",
        DAEMON_HOST_DISPLAY, daemon_server.port
    ));
    if let Some(tray_startup_tx) = tray_startup_tx {
        let _ = tray_startup_tx.send(DaemonTrayStartup {
            port: daemon_server.port,
            control_tx: daemon_control_tx.clone(),
        });
    }

    tokio::spawn(async {
        if let Err(err) = crate::model_catalog::refresh_models_dev_cache().await {
            tracing::warn!("models.dev cache refresh failed: {err}");
        }
    });

    let mut telegram_transport =
        if config.telegram.enabled && config.telegram.has_real_credentials() {
            let telegram = TelegramTransportState::new();
            let telegram_handle = telegram.handle();
            bootstrap_telegram_transport_state_from_acl(&telegram_handle, &telegram_acl);
            let telegram_router = Arc::new(ManagerTelegramInputRouter {
                sessions: telegram_sessions.clone(),
                session_tokens: telegram_session_tokens.clone(),
                telegram_defaults: telegram_defaults.clone(),
            });
            let telegram_auth_verifier = Arc::new(ManagerTelegramAuthVerifier {
                auth_registry: daemon_token_registry.clone(),
            });
            Some(tokio::spawn(
                TelegramTransport::new(
                    config.telegram.clone(),
                    telegram_handle,
                    telegram_acl.clone(),
                    telegram_auth_verifier,
                    telegram_router.clone(),
                    telegram_router,
                )
                .run(),
            ))
        } else {
            None
        };
    let mut telegram_outbox_delivery =
        if config.telegram.enabled && config.telegram.has_real_credentials() {
            Some(tokio::spawn(run_session_telegram_outbox_delivery(
                TelegramDeliveryClient::new(config.telegram.clone(), telegram_acl.clone()),
                sessions.clone(),
                session_tokens.clone(),
            )))
        } else {
            None
        };
    let session_health_checks = tokio::spawn(run_session_health_checks(
        sessions.clone(),
        session_tokens.clone(),
    ));

    daemon_lifecycle.mark_ready();

    let mut config_fingerprint =
        crate::config_hot_reload::config_file_fingerprint_at(&config_poll_path());
    let mut config_watch_interval = tokio::time::interval(Duration::from_secs(3));
    let mut last_telegram_config = toml::to_string(&config.telegram).ok();

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|err| miette!("failed to install SIGTERM handler: {err}"))?;
    let mut ctrl_c_disabled = false;
    let mut shutdown_action = ManagerShutdownAction::Stop;
    let mut shutdown_completion_tx = None;

    loop {
        tokio::select! {
            Some(command) = daemon_control_rx.recv() => {
                apply_daemon_control_command(
                    command,
                    &mut shutdown_completion_tx,
                    &mut shutdown_action,
                );
                break;
            }
            Some(command) = dashboard_control_rx.recv() => {
                // Session-scoped commands are executed by the session runtime's
                // handle_dashboard_control_command. This manager channel only
                // acts on daemon restart; anything else is ignored here instead
                // of being treated as handled.
                if matches!(command, DashboardControlCommand::RestartDaemon) {
                    shutdown_action = ManagerShutdownAction::Restart;
                    break;
                }
            }
            _ = config_watch_interval.tick() => {
                let fingerprint =
                    crate::config_hot_reload::config_file_fingerprint_at(&config_poll_path());
                if fingerprint.is_some() && fingerprint != config_fingerprint {
                    config_fingerprint = fingerprint;
                    reload_telegram_transport(
                        &mut telegram_transport,
                        &mut telegram_outbox_delivery,
                        &mut last_telegram_config,
                        &telegram_sessions,
                        &telegram_session_tokens,
                        &telegram_defaults,
                        &sessions,
                        &session_tokens,
                        &telegram_acl,
                        &daemon_token_registry,
                    )
                    .await;
                }
            }
            signal = tokio::signal::ctrl_c(), if !ctrl_c_disabled => {
                match signal {
                    Ok(()) => {
                        tracing::info!("manager received SIGINT, shutting down");
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
                #[cfg(not(unix))] { std::future::pending::<Option<()>>() }
            } => {
                tracing::info!("manager received SIGTERM, shutting down");
                break;
            }
        }
    }

    daemon_lifecycle.mark_stopping();
    if let Some(tray_handle) = &tray_handle {
        tray_handle.shutdown();
    }
    if let Some(server_shutdown_tx) = server_shutdown_tx.take() {
        let _ = server_shutdown_tx.send(());
    }
    if let Some(handle) = telegram_transport {
        handle.abort();
    }
    if let Some(handle) = telegram_outbox_delivery {
        handle.abort();
    }
    session_health_checks.abort();
    let session_shutdown_error = terminate_process_backed_sessions(
        &sessions,
        &session_tokens,
        shutdown_action.session_shutdown_reason(),
    )
    .await
    .err();
    lock.release();
    if let Some(completion_tx) = shutdown_completion_tx.take() {
        let _ = completion_tx.send(());
    }
    drop(dashboard_tx);
    let _ = tokio::time::timeout(Duration::from_secs(15), daemon_server.shutdown()).await;
    if let Some(err) = session_shutdown_error {
        return Err(err);
    }
    if shutdown_action.should_restart() {
        spawn_detached_daemon_process().await?;
    }
    Ok(())
}

struct ProcessBackedSession {
    session_id: session::SessionId,
    status: session::SessionStatus,
    ipc_name: Option<String>,
    pid: Option<u32>,
    last_seen_at_ms: Option<i64>,
}

fn process_backed_sessions(sessions: &session::SessionRegistry) -> Vec<ProcessBackedSession> {
    sessions
        .list()
        .into_iter()
        .filter(|info| info.status.is_process_backed())
        .map(|info| ProcessBackedSession {
            session_id: info.session_id,
            status: info.status,
            ipc_name: info.ipc_name,
            pid: info.pid,
            last_seen_at_ms: info.last_seen_at_ms,
        })
        .collect()
}

async fn run_session_health_checks(
    sessions: session::SessionRegistry,
    session_tokens: SessionTokenStore,
) {
    loop {
        for target in process_backed_sessions(&sessions) {
            let Some(ipc_name) = target.ipc_name.clone() else {
                let _ = sessions.mark_dead(&target.session_id).await;
                continue;
            };
            let Some(ipc_token) = session_tokens.read().get(&target.session_id).cloned() else {
                let _ = sessions.mark_dead(&target.session_id).await;
                continue;
            };
            let client =
                session_ipc::SessionIpcClient::new(target.session_id.clone(), ipc_name, ipc_token)
                    .with_timeout(Duration::from_secs(2));
            match client.request(session_ipc::SessionIpcRequest::Status).await {
                Ok(session_ipc::SessionIpcResponse::Status { runtime_status })
                    if runtime_status.ready =>
                {
                    let _ = sessions.mark_ready(&target.session_id).await;
                }
                Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => {
                    tracing::warn!(
                        "session {} health check error: {message}",
                        target.session_id
                    );
                    if probe_persisted_session_ipc_token_fields(
                        &session_tokens,
                        &target.session_id,
                        target.ipc_name.as_deref(),
                    )
                    .await
                    {
                        continue;
                    }
                }
                Ok(session_ipc::SessionIpcResponse::Status { .. } | _) => {}
                Err(err) => {
                    if session_is_within_starting_health_grace_fields(
                        target.status,
                        target.last_seen_at_ms,
                    ) {
                        tracing::debug!(
                            "session {} health check skipped during startup grace: {err:?}",
                            target.session_id
                        );
                        continue;
                    }
                    tracing::warn!("session {} health check failed: {err:?}", target.session_id);
                    if probe_persisted_session_ipc_token_fields(
                        &session_tokens,
                        &target.session_id,
                        target.ipc_name.as_deref(),
                    )
                    .await
                    {
                        continue;
                    }
                    session_tokens.write().remove(&target.session_id);
                    if let Some(pid) = target.pid
                        && pid != std::process::id()
                        && !crate::daemon::force_terminate_process(pid).await
                    {
                        tracing::warn!(
                            "session {} pid {pid} did not exit after forced termination",
                            target.session_id
                        );
                    }
                    let _ = sessions.mark_dead(&target.session_id).await;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn session_is_within_starting_health_grace_fields(
    status: session::SessionStatus,
    last_seen_at_ms: Option<i64>,
) -> bool {
    session_is_within_starting_health_grace_at(
        status,
        last_seen_at_ms,
        chrono::Utc::now().timestamp_millis(),
    )
}

fn session_is_within_starting_health_grace_at(
    status: session::SessionStatus,
    last_seen_at_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    if status != session::SessionStatus::Starting {
        return false;
    }
    let Some(last_seen_at_ms) = last_seen_at_ms else {
        return false;
    };
    now_ms.saturating_sub(last_seen_at_ms) <= SESSION_STARTING_HEALTH_GRACE_MS
}

async fn probe_persisted_session_ipc_token_fields(
    session_tokens: &SessionTokenStore,
    session_id: &session::SessionId,
    ipc_name: Option<&str>,
) -> bool {
    let Ok(Some(token)) = session::load_session_ipc_token(session_id).await else {
        return false;
    };
    let Some(ipc_name) = ipc_name else {
        return false;
    };
    let probe =
        session_ipc::SessionIpcClient::new(session_id.clone(), ipc_name.to_string(), token.clone())
            .with_timeout(Duration::from_secs(2));
    match probe.request(session_ipc::SessionIpcRequest::Status).await {
        Ok(session_ipc::SessionIpcResponse::Status { .. }) => {
            session_tokens.write().insert(session_id.clone(), token);
            true
        }
        _ => false,
    }
}

async fn run_session_telegram_outbox_delivery(
    delivery: TelegramDeliveryClient,
    sessions: session::SessionRegistry,
    session_tokens: SessionTokenStore,
) {
    loop {
        for target in process_backed_sessions(&sessions) {
            if !session_tokens.read().contains_key(&target.session_id) {
                continue;
            }
            let client =
                match session_client_for_id(&sessions, &session_tokens, target.session_id.as_str())
                    .await
                {
                    Ok(client) => client,
                    Err(err) => {
                        tracing::debug!(
                            "skip telegram outbox drain for session {}: {err:?}",
                            target.session_id
                        );
                        continue;
                    }
                };
            let messages = match client
                .request(session_ipc::SessionIpcRequest::DrainTelegramOutbox)
                .await
            {
                Ok(session_ipc::SessionIpcResponse::TelegramOutbox { messages }) => messages,
                Ok(session_ipc::SessionIpcResponse::Error { message, .. }) => {
                    tracing::warn!(
                        "session {} rejected telegram outbox drain: {message}",
                        target.session_id
                    );
                    continue;
                }
                Ok(_) => {
                    tracing::warn!(
                        "session {} returned unexpected telegram outbox response",
                        target.session_id
                    );
                    continue;
                }
                Err(err) => {
                    tracing::debug!(
                        "telegram outbox drain failed for session {}: {err:?}",
                        target.session_id
                    );
                    continue;
                }
            };

            for message in messages {
                if let Err(err) =
                    deliver_session_telegram_message(&client, &delivery, message).await
                {
                    tracing::warn!(
                        "telegram delivery failed for session {}: {err:?}",
                        target.session_id
                    );
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn deliver_session_telegram_message(
    client: &session_ipc::SessionIpcClient,
    delivery: &TelegramDeliveryClient,
    message: PendingOutboundMessage,
) -> Result<()> {
    match delivery.send_pending_outbound(&message).await {
        Ok(()) => {
            if let Some(event_id) = message.related_event_id.as_deref() {
                record_telegram_delivery(
                    client,
                    event_id,
                    message
                        .settle_status_on_delivery
                        .unwrap_or(EventStatus::Resolved),
                    message.settle_note_on_delivery.clone(),
                )
                .await?;
            }
            Ok(())
        }
        Err(err) => {
            let reason = format!("{err:?}");
            if message.draft_id.is_some() {
                tracing::warn!("telegram draft delivery failed; dropping draft update: {reason}");
                return Ok(());
            }
            if let Some(event_id) = message.related_event_id.as_deref() {
                let _ = record_telegram_delivery(
                    client,
                    event_id,
                    EventStatus::AwaitingDelivery,
                    Some(reason.clone()),
                )
                .await;
            }
            requeue_telegram_outbound(client, message).await?;
            Err(miette!("telegram outbound delivery failed: {reason}"))
        }
    }
}

async fn record_telegram_delivery(
    client: &session_ipc::SessionIpcClient,
    event_id: &str,
    status: EventStatus,
    note: Option<String>,
) -> Result<()> {
    match client
        .request(session_ipc::SessionIpcRequest::RecordTelegramDelivery {
            event_id: event_id.to_string(),
            status,
            note,
        })
        .await?
    {
        session_ipc::SessionIpcResponse::DeliveryRecorded => Ok(()),
        session_ipc::SessionIpcResponse::Error { message, .. } => {
            Err(miette!("record telegram delivery failed: {message}"))
        }
        _ => Err(miette!("unexpected telegram delivery record response")),
    }
}

async fn requeue_telegram_outbound(
    client: &session_ipc::SessionIpcClient,
    message: PendingOutboundMessage,
) -> Result<()> {
    match client
        .request(session_ipc::SessionIpcRequest::RequeueTelegramOutbound { message })
        .await?
    {
        session_ipc::SessionIpcResponse::TelegramOutboundRequeued => Ok(()),
        session_ipc::SessionIpcResponse::Error { message, .. } => {
            Err(miette!("requeue telegram outbound failed: {message}"))
        }
        _ => Err(miette!("unexpected telegram outbound requeue response")),
    }
}

async fn hydrate_session_tokens(
    sessions: &session::SessionRegistry,
    session_tokens: &SessionTokenStore,
) {
    for info in sessions.list() {
        if !info.status.is_process_backed() {
            continue;
        }
        match session::load_session_ipc_token(&info.session_id).await {
            Ok(Some(token)) => {
                if info
                    .ipc_token_hash
                    .as_deref()
                    .is_some_and(|hash| hash == session::hash_ipc_token(&token))
                {
                    session_tokens.write().insert(info.session_id, token);
                } else {
                    tracing::warn!(
                        "discarding IPC token for session {} because its hash does not match registry",
                        info.session_id
                    );
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    "failed to load IPC token for session {}: {err:?}",
                    info.session_id
                );
            }
        }
    }
}

fn manager_dashboard_state(telegram_acl: &TelegramAclHandle) -> DashboardState {
    DashboardState {
        agent_name: dashboard_agent_name(),
        status_output: "Manager daemon is running.
Select or create a session to view runtime state."
            .to_string(),
        inspect_telegram_output: manager_telegram_status_output(telegram_acl),
        pending_access_requests: telegram_acl.pending_requests(),
        runtime_status: Some("Manager ready".to_string()),
        runtime_status_level: Some(DashboardRuntimeStatusLevel::Info),
        runtime_activity: DashboardRuntimeActivity::new(
            DashboardRuntimeActivityStatus::Idle,
            "Manager",
            Some("Routing session traffic".to_string()),
        ),
        footer_context:
            "Manager daemon: session runtime state is available through selected sessions."
                .to_string(),
        ..DashboardState::default()
    }
}

fn manager_telegram_status_output(telegram_acl: &TelegramAclHandle) -> String {
    let pending = telegram_acl.pending_requests();
    if pending.is_empty() {
        return "Telegram ACL: no pending access requests".to_string();
    }

    let mut lines = vec!["Telegram ACL pending access requests:".to_string()];
    lines.extend(pending.into_iter().map(|request| {
        format!(
            "  {} | {} | {} | {}",
            request.chat_id, request.title, request.sender, request.last_message_preview
        )
    }));
    lines.join(
        "
",
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagerShutdownAction {
    Stop,
    Restart,
}

impl ManagerShutdownAction {
    const fn session_shutdown_reason(self) -> &'static str {
        match self {
            Self::Stop => "daemon stop",
            Self::Restart => "daemon restart",
        }
    }

    const fn should_restart(self) -> bool {
        matches!(self, Self::Restart)
    }
}

fn apply_daemon_control_command(
    command: DaemonControlCommand,
    shutdown_completion_tx: &mut Option<oneshot::Sender<()>>,
    shutdown_action: &mut ManagerShutdownAction,
) {
    match command {
        DaemonControlCommand::ShutdownRequested { completion_tx } => {
            *shutdown_completion_tx = Some(completion_tx);
        }
        DaemonControlCommand::RestartRequested => {
            *shutdown_action = ManagerShutdownAction::Restart;
        }
    }
}

fn config_poll_path() -> std::path::PathBuf {
    crate::daat_locus_paths::daat_locus_paths_sync().config_file("config.toml")
}

/// Rebuild the manager Telegram transport (and outbox delivery) from the
/// current config on disk. Used by the config hot-reload watcher.
#[allow(clippy::too_many_arguments)]
async fn reload_telegram_transport(
    telegram_transport: &mut Option<tokio::task::JoinHandle<()>>,
    telegram_outbox_delivery: &mut Option<tokio::task::JoinHandle<()>>,
    last_telegram_config: &mut Option<String>,
    telegram_sessions: &session::SessionRegistry,
    telegram_session_tokens: &SessionTokenStore,
    telegram_defaults: &session::TelegramSessionDefaults,
    sessions: &session::SessionRegistry,
    session_tokens: &SessionTokenStore,
    telegram_acl: &TelegramAclHandle,
    daemon_token_registry: &crate::daemon::DaemonTokenRegistryHandle,
) {
    let config = match crate::config::load_config().await {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!("telegram transport hot reload skipped; config reload failed: {err}");
            return;
        }
    };
    let telegram_toml = match toml::to_string(&config.telegram) {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!("telegram transport hot reload skipped; serialize failed: {err}");
            return;
        }
    };
    if last_telegram_config.as_deref() == Some(telegram_toml.as_str()) {
        return;
    }
    *last_telegram_config = Some(telegram_toml);
    if let Some(handle) = telegram_transport.take() {
        handle.abort();
    }
    if let Some(handle) = telegram_outbox_delivery.take() {
        handle.abort();
    }
    if config.telegram.enabled && config.telegram.has_real_credentials() {
        let telegram = TelegramTransportState::new();
        let telegram_handle = telegram.handle();
        bootstrap_telegram_transport_state_from_acl(&telegram_handle, telegram_acl);
        let telegram_router = Arc::new(ManagerTelegramInputRouter {
            sessions: telegram_sessions.clone(),
            session_tokens: telegram_session_tokens.clone(),
            telegram_defaults: telegram_defaults.clone(),
        });
        let telegram_auth_verifier = Arc::new(ManagerTelegramAuthVerifier {
            auth_registry: daemon_token_registry.clone(),
        });
        *telegram_transport = Some(tokio::spawn(
            TelegramTransport::new(
                config.telegram.clone(),
                telegram_handle,
                telegram_acl.clone(),
                telegram_auth_verifier,
                telegram_router.clone(),
                telegram_router,
            )
            .run(),
        ));
        *telegram_outbox_delivery = Some(tokio::spawn(run_session_telegram_outbox_delivery(
            TelegramDeliveryClient::new(config.telegram.clone(), telegram_acl.clone()),
            sessions.clone(),
            session_tokens.clone(),
        )));
    }
    emit_startup_progress("[manager] telegram transport hot-reloaded from config");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_shutdown_actions_share_session_shutdown_reason_path() {
        assert_eq!(
            ManagerShutdownAction::Stop.session_shutdown_reason(),
            "daemon stop"
        );
        assert!(!ManagerShutdownAction::Stop.should_restart());

        assert_eq!(
            ManagerShutdownAction::Restart.session_shutdown_reason(),
            "daemon restart"
        );
        assert!(ManagerShutdownAction::Restart.should_restart());
    }

    #[test]
    fn daemon_control_commands_select_manager_shutdown_action() {
        let mut shutdown_action = ManagerShutdownAction::Stop;
        let mut completion = None;

        let (completion_tx, _completion_rx) = oneshot::channel();
        apply_daemon_control_command(
            DaemonControlCommand::ShutdownRequested { completion_tx },
            &mut completion,
            &mut shutdown_action,
        );
        assert_eq!(shutdown_action, ManagerShutdownAction::Stop);
        assert!(completion.is_some());

        let mut shutdown_action = ManagerShutdownAction::Stop;
        let mut completion = None;
        apply_daemon_control_command(
            DaemonControlCommand::RestartRequested,
            &mut completion,
            &mut shutdown_action,
        );
        assert_eq!(shutdown_action, ManagerShutdownAction::Restart);
        assert!(completion.is_none());
    }

    #[test]
    fn starting_sessions_get_health_check_grace() {
        let mut info = session::SessionInfo::new(
            session::SessionId::from_string("session-a").unwrap(),
            session::SessionScope::General,
            None,
        );
        info.status = session::SessionStatus::Starting;
        info.last_seen_at_ms = Some(1_000);

        assert!(session_is_within_starting_health_grace_at(
            info.status,
            info.last_seen_at_ms,
            1_000 + SESSION_STARTING_HEALTH_GRACE_MS
        ));
        assert!(!session_is_within_starting_health_grace_at(
            info.status,
            info.last_seen_at_ms,
            1_001 + SESSION_STARTING_HEALTH_GRACE_MS
        ));

        info.status = session::SessionStatus::Ready;
        assert!(!session_is_within_starting_health_grace_at(
            info.status,
            info.last_seen_at_ms,
            1_000
        ));
    }

    #[test]
    fn telegram_session_commands_parse_into_one_enum() {
        assert!(matches!(
            parse_telegram_session_command("/session_list"),
            Some(TelegramSessionCommand::SessionList)
        ));
        assert!(matches!(
            parse_telegram_session_command("session_new a title with spaces"),
            Some(TelegramSessionCommand::SessionNew { title: Some(title) }) if title == "a title with spaces"
        ));
        assert!(matches!(
            parse_telegram_session_command("session_new"),
            Some(TelegramSessionCommand::SessionNew { title: None })
        ));
        assert!(matches!(
            parse_telegram_session_command("session_new keep 'raw remainder'"),
            Some(TelegramSessionCommand::SessionNew { title: Some(title) }) if title == "keep 'raw remainder'"
        ));
        assert!(matches!(
            parse_telegram_session_command("session_switch abc"),
            Some(TelegramSessionCommand::SessionAttach { reference: Some(reference) }) if reference == "abc"
        ));
        assert!(matches!(
            parse_telegram_session_command("session_attach 'sess ion'"),
            Some(TelegramSessionCommand::SessionAttach { reference: Some(reference) }) if reference == "sess ion"
        ));
        assert!(matches!(
            parse_telegram_session_command("session_delete one two"),
            Some(TelegramSessionCommand::DeleteRejected)
        ));
        assert!(matches!(
            parse_telegram_session_command("session_attach"),
            Some(TelegramSessionCommand::SessionAttach { reference: None })
        ));
        assert!(parse_telegram_session_command("status").is_none());
    }
}
