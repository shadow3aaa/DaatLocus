use std::{
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedMutexGuard;

use crate::{
    app::AppManager,
    browser_app::BrowserApp,
    coding_app::CodingApp,
    context::Context,
    context_budget::TokenEstimateBaseline,
    core::{Llm, TokenUsageInfo},
    daat_locus_paths::daat_locus_paths,
    events::EventStore,
    memory::Memory,
    openskills::load_openskills_for_runtime,
    pending_work::PendingWorkQueue,
    persistence::PersistenceStore,
    plan::Plan,
    providers::build_llm,
    reasoning::{
        compiled::{
            CompiledPromptStore, load_all_compiled_programs_for_model,
            load_compiled_runtime_system_prompt_for_model,
        },
        runtime::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest},
    },
    sandbox::{RuntimeSandboxPolicy, WritableRoot},
    telegram_acl::TelegramAclHandle,
    telegram_transport::state::TelegramTransportState,
    terminal_app::TerminalApp,
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
const TOKEN_USAGE_FILE: &str = "token_usage.json";

#[derive(Clone, Copy, Debug)]
pub(crate) enum PersistentTokenUsageRole {
    Main,
    Judge,
    Efficient,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedTokenUsageSnapshot {
    #[serde(default)]
    main: PersistedTokenUsageRole,
    #[serde(default)]
    judge: PersistedTokenUsageRole,
    #[serde(default)]
    efficient: PersistedTokenUsageRole,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct PersistedTokenUsageRole {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<TokenUsageInfo>,
}

#[derive(Debug)]
pub(crate) struct PersistentTokenUsageStore {
    persistence: PersistenceStore,
    snapshot: parking_lot::Mutex<PersistedTokenUsageSnapshot>,
}

pub(crate) async fn load_persistent_token_usage_store(
    session_id: Option<&str>,
) -> Arc<PersistentTokenUsageStore> {
    let persistence = PersistenceStore::for_session(session_id).await;
    let snapshot = persistence
        .read_json_state_or_default(TOKEN_USAGE_FILE, "token usage")
        .await;
    Arc::new(PersistentTokenUsageStore {
        persistence,
        snapshot: parking_lot::Mutex::new(snapshot),
    })
}

pub(crate) fn wrap_llm_with_persistent_token_usage(
    role: PersistentTokenUsageRole,
    configured_model: String,
    inner: Box<dyn Llm + Send + Sync>,
    store: Arc<PersistentTokenUsageStore>,
) -> Box<dyn Llm + Send + Sync> {
    Box::new(PersistentTokenUsageLlm::new(
        role,
        configured_model,
        inner,
        store,
    ))
}

struct PersistentTokenUsageLlm {
    role: PersistentTokenUsageRole,
    configured_model: String,
    baseline: TokenUsageInfo,
    inner: Box<dyn Llm + Send + Sync>,
    store: Arc<PersistentTokenUsageStore>,
}

impl PersistentTokenUsageLlm {
    fn new(
        role: PersistentTokenUsageRole,
        configured_model: String,
        inner: Box<dyn Llm + Send + Sync>,
        store: Arc<PersistentTokenUsageStore>,
    ) -> Self {
        let actual_model = inner
            .model_name()
            .unwrap_or_else(|| configured_model.clone());
        let baseline = store.baseline_for_role(role, &actual_model);
        Self {
            role,
            configured_model,
            baseline,
            inner,
            store,
        }
    }

    fn actual_model(&self) -> String {
        self.inner
            .model_name()
            .unwrap_or_else(|| self.configured_model.clone())
    }

    fn merged_token_usage_info(&self) -> Option<TokenUsageInfo> {
        let process_usage = self.inner.token_usage_info();
        match process_usage {
            Some(process_usage) => Some(self.baseline.merged_with_process_usage(&process_usage)),
            None if !self.baseline.total_token_usage.is_zero()
                || !self.baseline.last_token_usage.is_zero()
                || !self.baseline.daily_token_usage.is_empty() =>
            {
                Some(self.baseline.clone())
            }
            None => None,
        }
    }

    async fn persist_current_usage(&self) {
        if let Some(usage) = self.merged_token_usage_info() {
            self.store
                .persist_role(self.role, self.actual_model(), usage)
                .await;
        }
    }
}

#[async_trait]
impl Llm for PersistentTokenUsageLlm {
    async fn run_json(
        &self,
        context: &Context,
        request: PromptRequest,
    ) -> miette::Result<serde_json::Value> {
        let result = self.inner.run_json(context, request).await;
        self.persist_current_usage().await;
        result
    }

    async fn run_agent_turn(
        &self,
        context: &Context,
        request: AgentTurnRequest,
    ) -> miette::Result<AgentTurnStreamResult> {
        let result = self.inner.run_agent_turn(context, request).await;
        self.persist_current_usage().await;
        result
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        self.merged_token_usage_info()
    }

    fn model_name(&self) -> Option<String> {
        self.inner
            .model_name()
            .or_else(|| Some(self.configured_model.clone()))
    }
}

impl PersistentTokenUsageStore {
    fn baseline_for_role(
        &self,
        role: PersistentTokenUsageRole,
        current_model: &str,
    ) -> TokenUsageInfo {
        self.snapshot
            .lock()
            .role(role)
            .usage_for_model(current_model)
            .unwrap_or_default()
    }

    async fn persist_role(
        &self,
        role: PersistentTokenUsageRole,
        model: String,
        usage: TokenUsageInfo,
    ) {
        let snapshot = {
            let mut snapshot = self.snapshot.lock();
            *snapshot.role_mut(role) = PersistedTokenUsageRole {
                model: Some(model),
                usage: Some(usage),
            };
            snapshot.clone()
        };

        if let Err(err) = self
            .persistence
            .write_json_state(TOKEN_USAGE_FILE, &snapshot)
            .await
        {
            tracing::warn!("failed to persist token usage: {err}");
        }
    }
}

impl PersistedTokenUsageSnapshot {
    fn role(&self, role: PersistentTokenUsageRole) -> &PersistedTokenUsageRole {
        match role {
            PersistentTokenUsageRole::Main => &self.main,
            PersistentTokenUsageRole::Judge => &self.judge,
            PersistentTokenUsageRole::Efficient => &self.efficient,
        }
    }

    fn role_mut(&mut self, role: PersistentTokenUsageRole) -> &mut PersistedTokenUsageRole {
        match role {
            PersistentTokenUsageRole::Main => &mut self.main,
            PersistentTokenUsageRole::Judge => &mut self.judge,
            PersistentTokenUsageRole::Efficient => &mut self.efficient,
        }
    }
}

impl PersistedTokenUsageRole {
    fn usage_for_model(&self, current_model: &str) -> Option<TokenUsageInfo> {
        let usage = self.usage.clone()?;
        match self.model.as_deref() {
            Some(model) if model != current_model => None,
            _ => Some(usage),
        }
    }
}

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
    execution_cwd: Option<&Path>,
) -> RuntimeSandboxPolicy {
    if !config.sandbox.enabled {
        return RuntimeSandboxPolicy::disabled();
    }

    let daat_locus_home = daat_locus_paths().await.root().to_path_buf();
    let mut policy = RuntimeSandboxPolicy::protect_daat_locus_runtime_with_strong_filesystem(
        &daat_locus_home,
        daat_locus_source_root().as_deref(),
        config.protected_secret_env_vars(),
        config.sandbox.strong_filesystem,
    );
    if let Some(execution_cwd) = execution_cwd {
        allow_execution_workspace_writes(&mut policy, execution_cwd);
    }
    policy
}

fn allow_execution_workspace_writes(policy: &mut RuntimeSandboxPolicy, execution_cwd: &Path) {
    let root = normalize_workspace_root(execution_cwd);
    policy.filesystem.deny_write_paths.retain(|denied| {
        let denied = normalize_workspace_root(denied);
        !root.starts_with(&denied) && !denied.starts_with(&root)
    });
    if !policy.filesystem.full_disk_write
        && !policy
            .filesystem
            .writable_roots
            .iter()
            .any(|existing| normalize_workspace_root(&existing.root) == root)
    {
        policy.filesystem.writable_roots.push(WritableRoot {
            root,
            read_only_subpaths: Vec::new(),
        });
    }
}

fn normalize_workspace_root(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
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
    let sandbox_policy = sandbox_policy_for_runtime(&config, Some(&execution_cwd)).await;
    let memory = Memory::new().await;
    let plan = Plan::new().await;
    let events = EventStore::new().await;
    let pending_work = PendingWorkQueue::new().await;
    let openskills = load_openskills_for_runtime(&execution_cwd);
    let telegram_acl = TelegramAclHandle::load().await;
    let telegram = TelegramTransportState::new();
    let telegram_handle = telegram.handle();
    bootstrap_telegram_transport_state_from_acl(&telegram_handle, &telegram_acl);
    let runtime_apps = build_runtime_apps(&execution_cwd, &sandbox_policy);
    let apps = AppManager::new(None, runtime_apps.apps).await.unwrap();
    let token_usage_store = load_persistent_token_usage_store(None).await;
    let client = build_llm(&config.main_model, &config)
        .unwrap_or_else(|err| panic!("failed to construct main LLM client: {err:?}"));
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
    let judge_client = build_llm(&judge_model_key, &config)
        .unwrap_or_else(|err| panic!("failed to construct judge LLM client: {err:?}"));
    let judge_client = wrap_llm_with_persistent_token_usage(
        PersistentTokenUsageRole::Judge,
        judge_model_id,
        judge_client,
        token_usage_store.clone(),
    );
    let efficient_client = build_llm(&config.efficient_model, &config)
        .unwrap_or_else(|err| panic!("failed to construct efficient LLM client: {err:?}"));
    let efficient_client = wrap_llm_with_persistent_token_usage(
        PersistentTokenUsageRole::Efficient,
        config.efficient_model_config().model_id.clone(),
        efficient_client,
        token_usage_store,
    );
    let (daemon_control_tx, _daemon_control_rx) = tokio::sync::mpsc::unbounded_channel();

    Context {
        session_id: None,
        llm: client,
        judge_llm: judge_client,
        efficient_llm: efficient_client,
        config,
        memory,
        plan,
        events,
        pending_work,
        openskills,
        active_skill_run: None,
        pending_skill_run_flushes: Vec::new(),
        current_work_origin: None,
        apps,
        workspace_apps: runtime_apps.workspace_registry,
        telegram: telegram_handle,
        telegram_acl,
        compiled_prompts,
        execution_cwd,
        coding_project_dir: None,
        sandbox_policy,
        dashboard_tx: None,
        dashboard_history: None,
        daemon_control_tx,
        latest_context_composition: None,
        active_runtime_turn: false,
        active_runtime_phase: None,
        runtime_turn_started_at: None,
        runtime_turn_started_at_ms: None,
        runtime_turn_epoch: 0,
        active_app_notices: std::collections::HashMap::new(),
        runtime_overflow_failures: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        runtime_model_request_failures: std::sync::Arc::new(
            parking_lot::Mutex::new(HashMap::new()),
        ),
        suppressed_app_notices: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        live_progress_tx: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        telegram_live_drafts: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        claimed_event_ids: Vec::new(),
        claimed_app_notices: Vec::new(),
        afterclaim_context_fingerprint: None,
        visible_source_lines: HashSet::new(),
        delivered_root_instruction_fingerprint: None,
        idle_since: None,
        last_idle_sleep_at: None,
        session_title: crate::runtime::session_title::SessionTitleState::default(),
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
    let skill = &summary.workflow_improvement;
    format!(
        "sleep completed: runtime error cases consumed/cases/reflections/candidates/evaluations={}/{}/{}/{}/{}, runtime contract additions={}, skill evidence records={}, skill patches applied={}",
        correction.consumed_error_cases,
        correction.runtime_error_cases,
        correction.reflections,
        correction.candidates,
        correction.candidate_evaluations,
        correction.applied_system_additions,
        skill.evidence_run_records,
        skill.patch_applied,
    )
}

pub(crate) struct DaatLocusHomeOverride {
    previous: Option<String>,
    _guard: OwnedMutexGuard<()>,
}

impl DaatLocusHomeOverride {
    pub(crate) async fn set(path: PathBuf) -> Self {
        let guard = daat_locus_home_override_lock().lock_owned().await;
        let previous = env::var("DAAT_LOCUS_HOME").ok();
        unsafe {
            env::set_var("DAAT_LOCUS_HOME", path);
        }
        Self {
            previous,
            _guard: guard,
        }
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

fn daat_locus_home_override_lock() -> Arc<tokio::sync::Mutex<()>> {
    static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    Arc::clone(LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(()))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{DailyTokenUsage, TokenUsage};

    fn sample_usage(total_tokens: i64) -> TokenUsageInfo {
        let usage = TokenUsage {
            input_tokens: total_tokens - 1,
            cached_input_tokens: 1,
            output_tokens: 1,
            reasoning_output_tokens: 0,
            total_tokens,
        };
        TokenUsageInfo {
            total_token_usage: usage.clone(),
            last_token_usage: usage.clone(),
            model_context_window: Some(1000),
            daily_token_usage: vec![DailyTokenUsage {
                date: "2026-06-21".to_string(),
                usage,
            }],
        }
    }

    #[tokio::test]
    async fn persistent_token_usage_store_round_trips_session_role() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = DaatLocusHomeOverride::set(temp.path().to_path_buf()).await;
        let store = load_persistent_token_usage_store(Some("session-a")).await;
        store
            .persist_role(
                PersistentTokenUsageRole::Main,
                "model-a".to_string(),
                sample_usage(42),
            )
            .await;

        let reloaded = load_persistent_token_usage_store(Some("session-a")).await;
        let restored = reloaded.baseline_for_role(PersistentTokenUsageRole::Main, "model-a");
        assert_eq!(restored.total_token_usage.total_tokens, 42);
        assert_eq!(restored.last_token_usage.total_tokens, 42);
        assert_eq!(restored.daily_token_usage.len(), 1);
    }

    #[tokio::test]
    async fn persistent_token_usage_store_ignores_other_models() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = DaatLocusHomeOverride::set(temp.path().to_path_buf()).await;
        let store = load_persistent_token_usage_store(Some("session-b")).await;
        store
            .persist_role(
                PersistentTokenUsageRole::Judge,
                "model-a".to_string(),
                sample_usage(24),
            )
            .await;

        let reloaded = load_persistent_token_usage_store(Some("session-b")).await;
        let ignored = reloaded.baseline_for_role(PersistentTokenUsageRole::Judge, "model-b");
        assert!(ignored.total_token_usage.is_zero());
        assert!(ignored.daily_token_usage.is_empty());
    }
}
