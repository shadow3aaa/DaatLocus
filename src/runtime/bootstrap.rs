use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};

use crate::{
    app::{AppId, AppManager},
    browser_app::BrowserApp,
    coding_app::CodingApp,
    context::Context,
    context_budget::TokenEstimateBaseline,
    daat_locus_paths::daat_locus_paths,
    events::EventStore,
    memory::Memory,
    pending_work::PendingWorkQueue,
    persistence::PersistenceStore,
    plan::Plan,
    providers::build_llm,
    reasoning::compiled::{
        CompiledPromptStore, load_all_compiled_programs_for_model,
        load_compiled_runtime_system_prompt_for_model,
    },
    sandbox::RuntimeSandboxPolicy,
    telegram_acl::TelegramAclHandle,
    telegram_transport::state::TelegramTransportState,
    terminal_app::TerminalApp,
    workflow::WorkflowStore,
    workspace_app::paths::{resolve_runtime_workspace_dir, workspace_apps_dir},
    workspace_app::{WorkspaceAppRegistry, bootstrap_workspace_apps},
};

pub(crate) struct RuntimeAppsBootstrap {
    pub(crate) apps: Vec<Box<dyn crate::app::App>>,
    pub(crate) workspace_registry: WorkspaceAppRegistry,
}

pub(crate) fn emit_startup_progress(message: impl AsRef<str>) {
    tracing::info!("{}", message.as_ref());
}

const TOKEN_ESTIMATE_BASELINE_FILE: &str = "token_estimate_baseline.json";

pub(crate) async fn load_token_estimate_baseline() -> TokenEstimateBaseline {
    let persistence = PersistenceStore::runtime().await;
    persistence
        .read_json_state_or_default(TOKEN_ESTIMATE_BASELINE_FILE, "token estimate baseline")
        .await
}

pub async fn save_token_estimate_baseline(baseline: &TokenEstimateBaseline) {
    let persistence = PersistenceStore::runtime().await;
    if let Err(err) = persistence
        .write_json_state(TOKEN_ESTIMATE_BASELINE_FILE, baseline)
        .await
    {
        tracing::warn!("failed to persist token estimate baseline: {err}");
    }
}

pub(crate) async fn sandbox_policy_for_runtime(
    config: &crate::config::Config,
) -> RuntimeSandboxPolicy {
    if !config.sandbox.enabled {
        return RuntimeSandboxPolicy::disabled();
    }

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
    let mut apps: Vec<Box<dyn crate::app::App>> = vec![
        Box::new(BrowserApp::new()),
        Box::new(TerminalApp::new()),
        Box::new(CodingApp::new()),
    ];
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
    let (daemon_control_tx, _daemon_control_rx) = tokio::sync::mpsc::unbounded_channel();

    Context {
        llm: client,
        judge_llm: judge_client,
        config,
        memory,
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
        dashboard_history: None,
        daemon_control_tx,
        latest_context_composition: None,
        active_runtime_turn: false,
        active_runtime_phase: None,
        runtime_turn_started_at: None,
        runtime_turn_epoch: 0,
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
        token_estimate_baseline: load_token_estimate_baseline().await,
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
