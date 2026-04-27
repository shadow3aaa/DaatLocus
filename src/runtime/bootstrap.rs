use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};

use crate::{
    app::{AppId, AppManager},
    browser_app::BrowserApp,
    context::Context,
    daat_locus_paths::daat_locus_paths,
    events::EventStore,
    hindsight::{HindsightClient, llm_proxy::HindsightLlmProxy, managed::HindsightManagedServer},
    memory::Memory,
    pending_work::PendingWorkQueue,
    plan::Plan,
    providers::build_llm,
    reasoning::{
        compiled::{
            CompiledPromptStore, load_all_compiled_programs_for_model,
            load_compiled_runtime_system_prompt_for_model,
        },
        runtime::PromptMemoryContext,
    },
    sandbox::RuntimeSandboxPolicy,
    telegram_acl::TelegramAclHandle,
    telegram_transport::state::TelegramTransportState,
    terminal_app::TerminalApp,
    workflow::WorkflowStore,
    workspace_app::paths::{resolve_runtime_workspace_dir, workspace_apps_dir},
    workspace_app::{WorkspaceAppRegistry, bootstrap_workspace_apps},
};
use miette::Result;

pub(crate) struct RuntimeAppsBootstrap {
    pub(crate) apps: Vec<Box<dyn crate::app::App>>,
    pub(crate) workspace_registry: WorkspaceAppRegistry,
}

pub(crate) fn emit_startup_progress(message: impl AsRef<str>) {
    tracing::info!("{}", message.as_ref());
}

async fn tail_hindsight_log(profile: &str) {
    use tokio::io::{AsyncBufReadExt, AsyncSeekExt};

    let log_path = match std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
    {
        Some(home) => home
            .join(".hindsight")
            .join("profiles")
            .join(format!("{profile}.log")),
        None => return,
    };

    // Wait up to 8 s for the log file to appear (daemon creates it on first run).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while !log_path.exists() {
        if std::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    let mut file = match tokio::fs::File::open(&log_path).await {
        Ok(f) => f,
        Err(_) => return,
    };
    // Start from the end so we only show new output from this run.
    let _ = file.seek(std::io::SeekFrom::End(0)).await;

    let mut reader = tokio::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => tokio::time::sleep(std::time::Duration::from_millis(150)).await,
            Ok(_) => {
                let t = line.trim();
                if !t.is_empty() {
                    emit_startup_progress(format!("[hindsight] {t}"));
                }
            }
            Err(_) => break,
        }
    }
}

/// Connect to the hindsight daemon, optionally ensuring a fresh start.
///
/// `ensure_fresh = true`: always reconfigure the profile and restart any
/// already-running daemon so that config changes (e.g. a new model) take
/// effect immediately.  Use this only at daemon startup.
///
/// `ensure_fresh = false`: connect to a running daemon as-is (used by
/// background tasks that just need a client handle).
pub(crate) async fn connect_bootstrapped_hindsight(
    config: &crate::config::Config,
    ensure_fresh: bool,
) -> Result<HindsightClient> {
    let hindsight_config = config.hindsight.clone();
    emit_startup_progress(format!(
        "[hindsight] initializing daemon (profile={}, port={}, bank={}/{})",
        hindsight_config.profile,
        hindsight_config.port,
        hindsight_config.namespace,
        hindsight_config.bank_id,
    ));
    emit_startup_progress("[hindsight] starting local LLM proxy...");
    let llm_proxy = HindsightLlmProxy::start(config).await?;
    emit_startup_progress("[hindsight] local LLM proxy ready");
    let llm_env_vars = llm_proxy.env_vars();
    let server = HindsightManagedServer::new(hindsight_config.clone(), llm_env_vars.clone());
    if ensure_fresh {
        // Daemon health is not a reliable signal when the worker is wedged by
        // retained async jobs. Stop best-effort regardless, then start with the
        // current profile and LLM proxy.
        emit_startup_progress("[hindsight] force restarting daemon to apply config...");
        if let Err(err) = server.force_stop().await {
            tracing::warn!("[hindsight] force stop before startup failed: {err:?}");
        }
        emit_startup_progress(
            "[hindsight] starting daemon (first run may take a few minutes to download embedding models)...",
        );
        let profile = hindsight_config.profile.clone();
        let log_tail = tokio::spawn(async move { tail_hindsight_log(&profile).await });
        let result = server.start().await;
        log_tail.abort();
        result?;
        emit_startup_progress("[hindsight] daemon ready");
    } else if server.check_health().await {
        emit_startup_progress("[hindsight] daemon already running, reusing");
    } else {
        emit_startup_progress(
            "[hindsight] starting daemon (first run may take a few minutes to download embedding models)...",
        );
        let profile = hindsight_config.profile.clone();
        let log_tail = tokio::spawn(async move { tail_hindsight_log(&profile).await });
        let result = server.start().await;
        log_tail.abort();
        result?;
        emit_startup_progress("[hindsight] daemon ready");
    }
    emit_startup_progress(format!(
        "[hindsight] connecting to bank '{}/{}'",
        hindsight_config.namespace, hindsight_config.bank_id,
    ));
    let hindsight = HindsightClient::connect(&hindsight_config)
        .await?
        .with_restart_support(llm_env_vars)
        .with_llm_proxy(llm_proxy);
    hindsight.bootstrap_bank().await?;
    emit_startup_progress("[hindsight] bank ready");
    Ok(hindsight)
}

pub(crate) async fn sandbox_policy_for_runtime(
    config: &crate::config::Config,
) -> RuntimeSandboxPolicy {
    let daat_locus_home = daat_locus_paths().await.root().to_path_buf();
    RuntimeSandboxPolicy::protect_daat_locus_runtime_with_strong_filesystem(
        &daat_locus_home,
        daat_locus_source_root().as_deref(),
        config.protected_secret_env_vars(),
        config.sandbox.strong_filesystem,
    )
}

fn daat_locus_source_root() -> Option<PathBuf> {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    source_root.exists().then_some(source_root)
}

pub(crate) fn build_runtime_apps(
    execution_cwd: &Path,
    sandbox_policy: &RuntimeSandboxPolicy,
) -> RuntimeAppsBootstrap {
    let mut apps: Vec<Box<dyn crate::app::App>> =
        vec![Box::new(BrowserApp::new()), Box::new(TerminalApp::new())];
    let bootstrap = bootstrap_workspace_apps(execution_cwd, sandbox_policy);
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
    let sandbox_policy = sandbox_policy_for_runtime(&config).await;
    let memory = Memory::new().await;
    let plan = Plan::new().await;
    let events = EventStore::new().await;
    let pending_work = PendingWorkQueue::new().await;
    let workflows = WorkflowStore::new().await;
    let telegram_acl = TelegramAclHandle::load().await;
    let telegram = TelegramTransportState::new();
    let telegram_handle = telegram.handle();
    bootstrap_telegram_transport_state_from_acl(&telegram_handle, &telegram_acl);
    let runtime_apps = build_runtime_apps(&execution_cwd, &sandbox_policy);
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
    let hindsight = connect_bootstrapped_hindsight(&config, false)
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
        telegram_acl,
        compiled_prompts,
        execution_cwd,
        sandbox_policy,
        dashboard_tx: None,
        active_runtime_turn: false,
        active_runtime_phase: None,
        runtime_turn_started_at: None,
        active_app_notices: std::collections::HashMap::new(),
        runtime_overflow_failures: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        suppressed_app_notices: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        live_progress_tx: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        telegram_live_drafts: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        claimed_event_ids: Vec::new(),
        claimed_app_notices: Vec::new(),
        afterclaim_context_fingerprint: None,
        idle_since: None,
        last_idle_sleep_at: None,
    }
}

pub(crate) fn bootstrap_telegram_transport_state_from_acl(
    telegram_handle: &crate::telegram_transport::state::TelegramTransportStateHandle,
    telegram_acl: &TelegramAclHandle,
) {
    for chat in telegram_acl.approved_chats() {
        telegram_handle.register_known_chat(chat.chat_id.to_string(), chat.title);
    }
}

pub(crate) async fn load_compiled_prompts_only(
    config: &crate::config::Config,
) -> miette::Result<CompiledPromptStore> {
    let compiled =
        load_all_compiled_programs_for_model(&config.main_model_config().model_id).await?;
    let runtime_system_prompt =
        load_compiled_runtime_system_prompt_for_model(&config.main_model_config().model_id).await?;
    Ok(CompiledPromptStore::from_entries(compiled)
        .with_runtime_system_prompt(runtime_system_prompt))
}

pub(crate) fn summarize_sleep_summary(summary: &crate::reasoning::sleep::SleepSummary) -> String {
    let correction = &summary.runtime_error_correction;
    let workflow = &summary.workflow_improvement;
    format!(
        "sleep completed: runtime error cases consumed/cases/reflections/candidates/evaluations={}/{}/{}/{}/{}, runtime contract additions={}, workflow evidence/reflections/patch/merge/evaluations/frontier={}/{}/{}/{}/{}/{}, workflow lineage={}/{}/{}, applied patch/merge={}/{}, rollbacks={}",
        correction.consumed_error_cases,
        correction.runtime_error_cases,
        correction.reflections,
        correction.candidates,
        correction.candidate_evaluations,
        correction.applied_system_additions,
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
