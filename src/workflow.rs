use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use miette::{Result, miette};
use mlua::{
    AnyUserData, Function, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, ThreadStatus,
    UserData, Value as LuaValue,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    app::AppManager,
    context::Context,
    context_budget::is_context_budget_exceeded,
    core::{
        AgentTurnRetryObserver, AgentTurnRetryPolicy, ModelRequestOptions,
        complete_agent_turn_with_retry_using_policy_cancellable_with_observer,
    },
    memory::RuntimeStepConversation,
    plan::Plan,
    reasoning::runtime::{
        AgentMessage, AgentToolCall, AgentToolInputSpec, AgentToolSpec, AgentTurnRequest,
    },
    runtime::bootstrap::build_isolated_worker_apps,
    runtime_context::{MID_TURN_COMPACTION_MAX_RECOVERIES, maybe_compact_agent_messages},
    runtime_tools::{
        ToolExecutionResult, WorkerRuntimeToolCallContext,
        build_worker_runtime_tool_specs_for_apps, execute_worker_runtime_tool_call_for_apps,
    },
    sandbox::{SandboxAsyncChild, SandboxProcessOptions, SandboxStdio},
    schema_utils::{validate_model_facing_schema, validate_value_against_schema},
};

mod builtin_workflow_bindings {
    include!(concat!(env!("OUT_DIR"), "/builtin_workflows.rs"));
}

const WORKFLOW_TOOL_PREFIX: &str = "workflow__";
const LEGACY_BUILTIN_WORKFLOW_BACKUP_DIR: &str = "legacy-builtin-workflows";
const LEGACY_BUILTIN_GOAL_SHA256: &str =
    "d4b85cc9d7174465293e63c3dc08d99e0fb949e36b70c0edb765633771d13f84";
const LEGACY_BUILTIN_SEARCH_SHA256: &str =
    "b93e2c3bdbea0d4041e7359b1173704ec5284725a7fbc7e4604400b5d47740fe";
const WORKER_EXPLICIT_COMPLETION_MESSAGE: &str = "The worker has not completed. Do not end by only outputting text; keep calling tools, and explicitly call `finish_and_send` with the declared typed output when the final result is ready.";
const LUA_IO_MAX_BYTES: usize = 4 * 1024 * 1024;
const LUA_SHELL_MAX_BYTES: usize = 4 * 1024 * 1024;
const WORKFLOW_INTERRUPTED_ERROR: &str = "workflow interrupted";
const WORKFLOW_LUA_INTERRUPT_INSTRUCTION_INTERVAL: u32 = 1_000;

#[derive(Clone, Default)]
pub struct WorkflowCancellationRegistry {
    active: Arc<parking_lot::Mutex<Option<WorkflowCancellation>>>,
}

impl WorkflowCancellationRegistry {
    pub fn begin(&self) -> WorkflowCancellation {
        let cancellation = WorkflowCancellation::new();
        *self.active.lock() = Some(cancellation.clone());
        cancellation
    }

    pub fn interrupt_active(&self) -> bool {
        let active = self.active.lock().clone();
        active.is_some_and(|cancellation| {
            cancellation.interrupt();
            true
        })
    }

    pub fn clear(&self, cancellation: &WorkflowCancellation) {
        let mut active = self.active.lock();
        if active
            .as_ref()
            .is_some_and(|current| current.is_same(cancellation))
        {
            *active = None;
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowCancellation {
    interrupted: Arc<AtomicBool>,
}

impl WorkflowCancellation {
    pub fn new() -> Self {
        Self {
            interrupted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }

    fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }

    fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.interrupted, &other.interrupted)
    }
}

#[derive(Clone, Default)]
struct WorkflowCancellationState {
    external_interrupted: Arc<AtomicBool>,
    group_interrupted: Arc<AtomicBool>,
}

impl WorkflowCancellationState {
    fn is_interrupted(&self) -> bool {
        self.external_interrupted.load(Ordering::SeqCst)
            || self.group_interrupted.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct WorkflowWorkerCancellation {
    state: WorkflowCancellationState,
}

impl WorkflowWorkerCancellation {
    fn new(cancellation: Option<&WorkflowCancellation>) -> Self {
        Self {
            state: WorkflowCancellationState {
                external_interrupted: cancellation
                    .map(|cancellation| Arc::clone(&cancellation.interrupted))
                    .unwrap_or_default(),
                group_interrupted: Arc::new(AtomicBool::new(false)),
            },
        }
    }

    fn interrupt_group(&self) -> bool {
        self.state
            .group_interrupted
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn is_interrupted(&self) -> bool {
        self.state.is_interrupted()
    }

    fn is_externally_interrupted(&self) -> bool {
        self.state.external_interrupted.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
enum WorkflowCancellationRef<'a> {
    Workflow(&'a WorkflowCancellation),
    Worker(WorkflowWorkerCancellation),
}

impl WorkflowCancellationRef<'_> {
    fn is_interrupted(&self) -> bool {
        match self {
            Self::Workflow(cancellation) => cancellation.is_interrupted(),
            Self::Worker(cancellation) => cancellation.is_interrupted(),
        }
    }
}

impl<'a> From<&'a WorkflowCancellation> for WorkflowCancellationRef<'a> {
    fn from(cancellation: &'a WorkflowCancellation) -> Self {
        Self::Workflow(cancellation)
    }
}

impl From<WorkflowWorkerCancellation> for WorkflowCancellationRef<'_> {
    fn from(cancellation: WorkflowWorkerCancellation) -> Self {
        Self::Worker(cancellation)
    }
}

#[derive(Debug)]
struct WorkflowInterrupted;

impl std::fmt::Display for WorkflowInterrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(WORKFLOW_INTERRUPTED_ERROR)
    }
}

impl std::error::Error for WorkflowInterrupted {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub id: String,
    pub path: PathBuf,
    #[serde(skip)]
    source: String,
    pub input_schema: Value,
    pub output_schema: Value,
}

impl WorkflowDefinition {
    pub fn tool_name(&self) -> String {
        format!("{WORKFLOW_TOOL_PREFIX}{}", self.id)
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkflowCatalog {
    root: PathBuf,
    definitions: BTreeMap<String, WorkflowDefinition>,
    errors: Vec<WorkflowLoadError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowLoadError {
    pub path: PathBuf,
    pub message: String,
}

impl WorkflowCatalog {
    pub fn load() -> Self {
        let mut catalog = Self {
            root: crate::daat_locus_paths::daat_locus_paths_sync().workflows_dir(),
            definitions: BTreeMap::new(),
            errors: Vec::new(),
        };
        catalog.reload();
        catalog
    }

    pub fn definitions(&self) -> impl Iterator<Item = &WorkflowDefinition> {
        self.definitions.values()
    }

    pub fn get(&self, id: &str) -> Option<&WorkflowDefinition> {
        self.definitions.get(id)
    }

    pub fn errors(&self) -> &[WorkflowLoadError] {
        &self.errors
    }

    pub fn reload(&mut self) {
        self.definitions.clear();
        self.errors.clear();
        match fs::create_dir_all(&self.root) {
            Ok(()) => {
                self.migrate_legacy_builtin_workflows();
                let overridden_builtin_ids = self.load_definitions();
                self.load_builtin_definitions(&overridden_builtin_ids);
            }
            Err(err) => {
                self.errors.push(WorkflowLoadError {
                    path: self.root.clone(),
                    message: format!("failed to create workflow directory: {err}"),
                });
                self.load_builtin_definitions(&BTreeSet::new());
            }
        }
    }

    fn migrate_legacy_builtin_workflows(&mut self) {
        for (id, _) in builtin_workflow_bindings::BUILTIN_WORKFLOW_SOURCES {
            let path = self.root.join(format!("{id}.lua"));
            let existing_source = match fs::read_to_string(&path) {
                Ok(existing_source) => existing_source,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    self.errors.push(WorkflowLoadError {
                        path,
                        message: format!("failed to read workflow source while checking legacy builtin migration: {err}"),
                    });
                    continue;
                }
            };
            if !is_legacy_builtin_workflow_source(id, &existing_source) {
                continue;
            }

            let backup_dir = self.root.join(LEGACY_BUILTIN_WORKFLOW_BACKUP_DIR);
            if let Err(err) = fs::create_dir_all(&backup_dir) {
                self.errors.push(WorkflowLoadError {
                    path: backup_dir,
                    message: format!(
                        "failed to create legacy builtin workflow backup directory: {err}"
                    ),
                });
                continue;
            }
            let backup_path = backup_dir.join(format!("{id}.lua"));
            if backup_path.exists() {
                self.errors.push(WorkflowLoadError {
                    path,
                    message: format!(
                        "legacy builtin workflow was not migrated because {} already exists; rename or remove the backup, then reload",
                        backup_path.display()
                    ),
                });
                continue;
            }
            if let Err(err) = fs::rename(&path, &backup_path) {
                self.errors.push(WorkflowLoadError {
                    path,
                    message: format!(
                        "failed to migrate legacy builtin workflow to {}: {err}",
                        backup_path.display()
                    ),
                });
            }
        }
    }

    fn load_builtin_definitions(&mut self, overridden_builtin_ids: &BTreeSet<String>) {
        for (id, source) in builtin_workflow_bindings::BUILTIN_WORKFLOW_SOURCES {
            if overridden_builtin_ids.contains(*id) {
                continue;
            }
            let path = builtin_workflow_path(id);
            match load_workflow_definition_from_source(&path, source) {
                Ok(definition) => {
                    self.definitions.insert(definition.id.clone(), definition);
                }
                Err(err) => self.errors.push(WorkflowLoadError {
                    path,
                    message: err.to_string(),
                }),
            }
        }
    }

    fn load_definitions(&mut self) -> BTreeSet<String> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) => {
                self.errors.push(WorkflowLoadError {
                    path: self.root.clone(),
                    message: format!("failed to read workflow directory: {err}"),
                });
                return BTreeSet::new();
            }
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("lua"))
            .collect::<Vec<_>>();
        paths.sort();

        let mut overridden_builtin_ids = BTreeSet::new();
        for path in paths {
            if let Some(id) = path.file_stem().and_then(|value| value.to_str()) {
                overridden_builtin_ids.insert(id.to_string());
            }
            match load_workflow_definition(&path) {
                Ok(definition) => {
                    if self.definitions.contains_key(&definition.id) {
                        self.errors.push(WorkflowLoadError {
                            path,
                            message: format!("duplicate workflow id `{}`", definition.id),
                        });
                    } else {
                        self.definitions.insert(definition.id.clone(), definition);
                    }
                }
                Err(err) => self.errors.push(WorkflowLoadError {
                    path,
                    message: err.to_string(),
                }),
            }
        }
        overridden_builtin_ids
    }
}

fn builtin_workflow_path(id: &str) -> PathBuf {
    PathBuf::from("<builtin-workflows>").join(format!("{id}.lua"))
}

fn is_legacy_builtin_workflow_source(id: &str, source: &str) -> bool {
    let digest = hex::encode(Sha256::digest(source.as_bytes()));
    match id {
        "goal" => digest == LEGACY_BUILTIN_GOAL_SHA256,
        "search" => digest == LEGACY_BUILTIN_SEARCH_SHA256,
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowInvocation {
    pub workflow_id: String,
    pub input: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowInvocationStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTransitionKind {
    Await,
    Verify,
    Revision,
    Retry,
}

impl WorkflowTransitionKind {
    fn from_lua(value: &str) -> mlua::Result<Self> {
        match value {
            "await" => Ok(Self::Await),
            "verify" => Ok(Self::Verify),
            "revision" => Ok(Self::Revision),
            "retry" => Ok(Self::Retry),
            other => Err(mlua::Error::external(format!(
                "workflow transition kind must be `await`, `verify`, `revision`, or `retry`, got `{other}`"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowTransitionSnapshot {
    pub source_worker_id: String,
    pub target_worker_id: String,
    pub kind: WorkflowTransitionKind,
}

fn legacy_workflow_worker_role() -> String {
    "agent".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowWorkerSnapshot {
    pub worker_id: String,
    /// Opaque identity of the `workflow.agent(...)` actor that produced this
    /// run. A single actor can have multiple sequential worker runs while
    /// retaining its isolated conversation and App state.
    #[serde(default)]
    pub actor_id: String,
    pub await_group_id: String,
    #[serde(default = "legacy_workflow_worker_role")]
    pub role: String,
    pub model: String,
    pub status: WorkflowNodeStatus,
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    /// Backend-measured time spent executing this worker. It is updated from a
    /// monotonic clock while `run_worker` runs and excludes workflow waiting
    /// after the worker completes.
    #[serde(default)]
    pub agent_run_time_ms: u64,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Number of persisted semantic activity items currently available for this
    /// worker. The activity itself is loaded through the worker activity page
    /// API instead of being embedded in every workflow snapshot.
    #[serde(default)]
    pub activity_count: usize,
    /// Monotonically increases whenever this worker's persisted activity
    /// changes, including coalescing updates that do not change the count.
    #[serde(default)]
    pub activity_revision: i64,
    /// Bounded recent tail used for immediate rendering and backwards
    /// compatibility. Older activity is retrieved through the worker activity
    /// page API.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activity: Vec<crate::dashboard::SessionActivityEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowAwaitGroupSnapshot {
    pub group_id: String,
    pub sequence: usize,
    pub status: WorkflowNodeStatus,
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    #[serde(default)]
    pub worker_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowRunSnapshot {
    pub run_id: String,
    pub workflow_id: String,
    pub status: WorkflowNodeStatus,
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub await_groups: Vec<WorkflowAwaitGroupSnapshot>,
    #[serde(default)]
    pub transitions: Vec<WorkflowTransitionSnapshot>,
    #[serde(default)]
    pub workers: Vec<WorkflowWorkerSnapshot>,
}

impl WorkflowRunSnapshot {
    fn new(workflow_id: String, input: Value) -> Self {
        Self {
            run_id: uuid::Uuid::new_v4().to_string(),
            workflow_id,
            status: WorkflowNodeStatus::Running,
            started_at_ms: current_time_ms(),
            completed_at_ms: None,
            input,
            output: None,
            error: None,
            await_groups: Vec::new(),
            transitions: Vec::new(),
            workers: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct WorkflowInspectorPublisher {
    snapshot: Arc<parking_lot::Mutex<WorkflowRunSnapshot>>,
    dashboard_tx: Option<tokio::sync::watch::Sender<crate::dashboard::DashboardState>>,
    activity_history: Option<crate::dashboard::DashboardActivityHistoryStore>,
}

impl WorkflowInspectorPublisher {
    #[cfg(test)]
    fn new(
        workflow_id: String,
        input: Value,
        dashboard_tx: Option<tokio::sync::watch::Sender<crate::dashboard::DashboardState>>,
    ) -> Self {
        Self::new_with_history(workflow_id, input, dashboard_tx, None)
    }

    fn new_with_history(
        workflow_id: String,
        input: Value,
        dashboard_tx: Option<tokio::sync::watch::Sender<crate::dashboard::DashboardState>>,
        activity_history: Option<crate::dashboard::DashboardActivityHistoryStore>,
    ) -> Self {
        let publisher = Self {
            snapshot: Arc::new(parking_lot::Mutex::new(WorkflowRunSnapshot::new(
                workflow_id,
                input,
            ))),
            dashboard_tx,
            activity_history,
        };
        publisher.publish();
        publisher
    }

    fn snapshot(&self) -> WorkflowRunSnapshot {
        self.snapshot.lock().clone()
    }

    fn transport_snapshot(&self) -> WorkflowRunSnapshot {
        let mut snapshot = self.snapshot();
        for worker in &mut snapshot.workers {
            if worker.activity_count < worker.activity.len() {
                worker.activity_count = worker.activity.len();
            }
            if worker.activity_revision == 0 && !worker.activity.is_empty() {
                worker.activity_revision = i64::try_from(worker.activity.len()).unwrap_or(i64::MAX);
            }
            // Session-backed runs persist the complete stream and expose only
            // this bounded compatibility tail in live snapshots. The Inspector
            // uses activity_count/revision to page older events on demand.
            worker.activity = tail_worker_activity(std::mem::take(&mut worker.activity));
        }
        snapshot
    }

    fn publish(&self) {
        let Some(tx) = &self.dashboard_tx else {
            return;
        };
        let snapshot = self.transport_snapshot();
        let live_key = format!("workflow:{}", snapshot.run_id);
        let event = crate::dashboard::SessionActivityEvent::Workflow(
            crate::dashboard::WorkflowActivityData {
                workflow_id: snapshot.workflow_id.clone(),
                status: invocation_status_from_node_status(snapshot.status),
                output: snapshot.output.clone(),
                message: workflow_snapshot_message(&snapshot),
                snapshot: Some(snapshot.clone()),
            },
        );
        tx.send_modify(|state| {
            if matches!(
                snapshot.status,
                WorkflowNodeStatus::Pending | WorkflowNodeStatus::Running
            ) {
                if let Some(existing) = state
                    .active_workflow_runs
                    .iter_mut()
                    .find(|run| run.run_id == snapshot.run_id)
                {
                    *existing = snapshot.clone();
                } else {
                    state.active_workflow_runs.push(snapshot.clone());
                }
                if let Some(existing) = state
                    .live_activity_events
                    .iter_mut()
                    .find(|live| live.key == live_key)
                {
                    existing.event = event.clone();
                } else {
                    state
                        .live_activity_events
                        .push(crate::dashboard::LiveActivityEvent {
                            key: live_key,
                            event,
                        });
                }
            } else {
                state
                    .active_workflow_runs
                    .retain(|run| run.run_id != snapshot.run_id);
                state
                    .live_activity_events
                    .retain(|live| live.key != live_key);
            }
        });
    }

    fn begin_group(
        &self,
        worker_count: usize,
        transition_kind: WorkflowTransitionKind,
    ) -> (String, Vec<String>) {
        let mut snapshot = self.snapshot.lock();
        let previous_worker_ids = snapshot
            .await_groups
            .iter()
            .rev()
            .find_map(|group| (!group.worker_ids.is_empty()).then(|| group.worker_ids.clone()))
            .unwrap_or_default();
        let sequence = snapshot.await_groups.len() + 1;
        let group_id = format!("await-{sequence}");
        let worker_ids = (0..worker_count)
            .map(|index| format!("{group_id}-worker-{}", index + 1))
            .collect::<Vec<_>>();
        for source_worker_id in previous_worker_ids {
            for target_worker_id in &worker_ids {
                if source_worker_id != *target_worker_id {
                    snapshot.transitions.push(WorkflowTransitionSnapshot {
                        source_worker_id: source_worker_id.clone(),
                        target_worker_id: target_worker_id.clone(),
                        kind: transition_kind,
                    });
                }
            }
        }
        snapshot.await_groups.push(WorkflowAwaitGroupSnapshot {
            group_id: group_id.clone(),
            sequence,
            status: WorkflowNodeStatus::Running,
            started_at_ms: current_time_ms(),
            completed_at_ms: None,
            worker_ids: worker_ids.clone(),
        });
        drop(snapshot);
        self.publish();
        (group_id, worker_ids)
    }

    fn begin_worker(
        &self,
        group_id: &str,
        worker_id: String,
        actor_id: String,
        definition: &WorkerDefinition,
        input: Value,
    ) {
        let run_id = self.snapshot.lock().run_id.clone();
        if let Some(history) = self.activity_history.as_ref()
            && let Err(err) = history.register_workflow_worker(&run_id, &worker_id)
        {
            tracing::warn!(
                run_id,
                worker_id,
                "register workflow worker activity stream failed: {err:?}"
            );
        }
        self.snapshot.lock().workers.push(WorkflowWorkerSnapshot {
            worker_id,
            actor_id,
            await_group_id: group_id.to_string(),
            role: definition.role.clone(),
            model: definition.model.label().to_string(),
            status: WorkflowNodeStatus::Running,
            started_at_ms: current_time_ms(),
            completed_at_ms: None,
            agent_run_time_ms: 0,
            input,
            output: None,
            error: None,
            activity_count: 0,
            activity_revision: 0,
            activity: Vec::new(),
        });
        self.publish();
    }

    fn append_worker_activity(
        &self,
        worker_id: &str,
        event: crate::dashboard::SessionActivityEvent,
    ) {
        let event_tail = event.clone();
        let run_id = self.snapshot.lock().run_id.clone();
        let persisted = self.activity_history.as_ref().and_then(|history| {
            match history.append_workflow_worker_activity(&run_id, worker_id, &event) {
                Ok(result) => Some(result),
                Err(err) => {
                    let message = format!("workflow worker activity history write failed: {err:#}");
                    tracing::error!(
                        run_id,
                        worker_id,
                        error = %message,
                        "workflow worker activity history degraded to bounded compatibility tail"
                    );
                    None
                }
            }
        });
        if let Some(worker) = self
            .snapshot
            .lock()
            .workers
            .iter_mut()
            .find(|worker| worker.worker_id == worker_id)
        {
            append_worker_activity_tail(&mut worker.activity, event_tail);
            if let Some(persisted) = persisted {
                // A session-backed run has a durable worker activity stream. Keep
                // only its counters and a bounded compatibility tail in the live
                // snapshot; the Inspector loads the complete stream on demand.
                worker.activity_count = persisted.activity_count;
                worker.activity_revision = persisted.revision;
            } else {
                // Unit/evaluation contexts without a session store, and degraded
                // stores, retain monotonic fallback counters alongside the tail.
                worker.activity_count = worker.activity_count.saturating_add(1);
                worker.activity_revision = worker.activity_revision.saturating_add(1);
            }
        }
        self.publish();
    }

    fn update_worker_run_time(&self, worker_id: &str, agent_run_time_ms: u64) {
        let mut updated = false;
        if let Some(worker) = self
            .snapshot
            .lock()
            .workers
            .iter_mut()
            .find(|worker| worker.worker_id == worker_id)
        {
            let next_run_time_ms = worker.agent_run_time_ms.max(agent_run_time_ms);
            if worker.agent_run_time_ms != next_run_time_ms {
                worker.agent_run_time_ms = next_run_time_ms;
                updated = true;
            }
        }
        if updated {
            self.publish();
        }
    }

    fn finish_worker(
        &self,
        worker_id: &str,
        result: &Result<WorkflowWorkerResult>,
        agent_run_time_ms: u64,
    ) {
        if let Some(worker) = self
            .snapshot
            .lock()
            .workers
            .iter_mut()
            .find(|worker| worker.worker_id == worker_id)
        {
            worker.completed_at_ms = Some(current_time_ms());
            worker.agent_run_time_ms = worker.agent_run_time_ms.max(agent_run_time_ms);
            match result {
                Ok(result) => {
                    worker.status = WorkflowNodeStatus::Completed;
                    worker.output = Some(result.output.clone());
                    worker.error = None;
                }
                Err(error) => {
                    worker.status = if is_workflow_interrupted_error(error) {
                        WorkflowNodeStatus::Interrupted
                    } else {
                        WorkflowNodeStatus::Failed
                    };
                    worker.error = Some(error.to_string());
                }
            }
        }
        self.publish();
    }

    fn finish_group(&self, group_id: &str) {
        let mut snapshot = self.snapshot.lock();
        let worker_statuses = snapshot
            .workers
            .iter()
            .filter(|worker| worker.await_group_id == group_id)
            .map(|worker| worker.status)
            .collect::<Vec<_>>();
        let status = aggregate_node_status(&worker_statuses);
        if let Some(group) = snapshot
            .await_groups
            .iter_mut()
            .find(|group| group.group_id == group_id)
        {
            group.completed_at_ms = Some(current_time_ms());
            group.status = status;
        }
        drop(snapshot);
        self.publish();
    }

    fn finish_run(
        &self,
        status: WorkflowNodeStatus,
        output: Option<Value>,
        error: Option<String>,
    ) -> WorkflowRunSnapshot {
        {
            let mut snapshot = self.snapshot.lock();
            snapshot.status = status;
            snapshot.completed_at_ms = Some(current_time_ms());
            snapshot.output = output;
            snapshot.error = error;
        }
        self.publish();
        self.transport_snapshot()
    }
}

pub(crate) const WORKFLOW_WORKER_ACTIVITY_TAIL_LIMIT: usize = 16;

fn append_worker_activity_tail(
    activity: &mut Vec<crate::dashboard::SessionActivityEvent>,
    event: crate::dashboard::SessionActivityEvent,
) {
    activity.push(event);
    *activity = crate::dashboard::coalesce_activity_events(std::mem::take(activity));
    *activity = tail_worker_activity(std::mem::take(activity));
}

pub(crate) fn tail_worker_activity(
    mut activity: Vec<crate::dashboard::SessionActivityEvent>,
) -> Vec<crate::dashboard::SessionActivityEvent> {
    if activity.len() > WORKFLOW_WORKER_ACTIVITY_TAIL_LIMIT {
        let keep_from = activity.len() - WORKFLOW_WORKER_ACTIVITY_TAIL_LIMIT;
        activity.drain(..keep_from);
    }
    activity
}

fn current_time_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

const fn invocation_status_from_node_status(
    status: WorkflowNodeStatus,
) -> WorkflowInvocationStatus {
    match status {
        WorkflowNodeStatus::Pending | WorkflowNodeStatus::Running => {
            WorkflowInvocationStatus::Running
        }
        WorkflowNodeStatus::Completed => WorkflowInvocationStatus::Completed,
        WorkflowNodeStatus::Failed => WorkflowInvocationStatus::Failed,
        WorkflowNodeStatus::Interrupted => WorkflowInvocationStatus::Interrupted,
    }
}

fn aggregate_node_status(statuses: &[WorkflowNodeStatus]) -> WorkflowNodeStatus {
    if statuses.contains(&WorkflowNodeStatus::Interrupted) {
        WorkflowNodeStatus::Interrupted
    } else if statuses.contains(&WorkflowNodeStatus::Failed) {
        WorkflowNodeStatus::Failed
    } else if statuses
        .iter()
        .all(|status| *status == WorkflowNodeStatus::Completed)
    {
        WorkflowNodeStatus::Completed
    } else {
        WorkflowNodeStatus::Running
    }
}

fn workflow_snapshot_message(snapshot: &WorkflowRunSnapshot) -> String {
    match snapshot.status {
        WorkflowNodeStatus::Running | WorkflowNodeStatus::Pending => format!(
            "running {} worker{} across {} await group{}",
            snapshot.workers.len(),
            if snapshot.workers.len() == 1 { "" } else { "s" },
            snapshot.await_groups.len(),
            if snapshot.await_groups.len() == 1 {
                ""
            } else {
                "s"
            },
        ),
        WorkflowNodeStatus::Completed => "workflow completed".to_string(),
        WorkflowNodeStatus::Interrupted => "workflow interrupted".to_string(),
        WorkflowNodeStatus::Failed => snapshot
            .error
            .clone()
            .unwrap_or_else(|| "workflow failed".to_string()),
    }
}

pub struct WorkflowInvocationResult {
    pub workflow_id: String,
    pub status: WorkflowInvocationStatus,
    pub output: Option<Value>,
    pub message: String,
    pub snapshot: WorkflowRunSnapshot,
}

#[derive(Clone, Debug)]
struct WorkerDefinition {
    role: String,
    model: WorkerModel,
    input_schema: Value,
    output_schema: Value,
    instruction: String,
    extra_tools: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkerModel {
    Main,
    Efficient,
}
impl WorkerModel {
    const fn label(&self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Efficient => "efficient",
        }
    }
}

#[derive(Clone)]
struct LocalToolDefinition {
    name: String,
    input_schema: Value,
    output_schema: Value,
    run: Function,
    lua: Lua,
}

#[derive(Clone)]
struct WorkerInvocation {
    actor: Arc<tokio::sync::Mutex<WorkflowWorkerActor>>,
    input: Value,
}

struct WorkflowWorkerActor {
    actor_id: String,
    definition: WorkerDefinition,
    apps: AppManager,
    tools: Vec<WorkerTool>,
    runtime: WorkerRuntimeState,
    conversation: RuntimeStepConversation,
    running: Arc<AtomicBool>,
}

#[derive(Clone)]
struct WorkflowWorkerActorHandle(Arc<tokio::sync::Mutex<WorkflowWorkerActor>>);

impl UserData for WorkflowWorkerActorHandle {}

#[derive(Clone)]
struct WorkerActorFactoryContext {
    execution_cwd: PathBuf,
    sandbox_policy: crate::sandbox::RuntimeSandboxPolicy,
}

impl WorkerActorFactoryContext {
    fn from_context(context: &Context) -> Self {
        Self {
            execution_cwd: context.execution_cwd.clone(),
            sandbox_policy: context.sandbox_policy.clone(),
        }
    }

    fn build_apps(&self) -> Result<AppManager> {
        build_worker_apps(&self.execution_cwd, &self.sandbox_policy)
    }
}

impl WorkflowWorkerActor {
    fn new(
        factory_context: &WorkerActorFactoryContext,
        definition: WorkerDefinition,
        local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
    ) -> Result<Self> {
        let apps = factory_context.build_apps()?;
        let tools = build_worker_tools(&definition, local_tools)?;
        let runtime = WorkerRuntimeState::new();
        let conversation = worker_conversation(&definition);
        Ok(Self {
            actor_id: uuid::Uuid::new_v4().to_string(),
            definition,
            apps,
            tools,
            runtime,
            conversation,
            running: Arc::new(AtomicBool::new(false)),
        })
    }

    fn begin_run(&self) -> Result<WorkflowWorkerRunGuard> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(miette!(
                "workflow actor cannot run more than one handle concurrently"
            ));
        }
        Ok(WorkflowWorkerRunGuard {
            running: Arc::clone(&self.running),
        })
    }

    fn reset(&mut self, factory_context: &WorkerActorFactoryContext) -> Result<()> {
        if self.running.load(Ordering::SeqCst) {
            return Err(miette!(
                "workflow actor cannot reset while one of its handles is running"
            ));
        }
        self.apps = factory_context.build_apps()?;
        self.runtime = WorkerRuntimeState::new();
        self.conversation = worker_conversation(&self.definition);
        Ok(())
    }
}

struct WorkflowWorkerRunGuard {
    running: Arc<AtomicBool>,
}

impl Drop for WorkflowWorkerRunGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone)]
enum WorkflowYield {
    Worker {
        worker: WorkerInvocation,
        transition_kind: WorkflowTransitionKind,
    },
    Workers {
        workers: Vec<WorkerInvocation>,
        transition_kind: WorkflowTransitionKind,
    },
    ResetActor {
        actor: Arc<tokio::sync::Mutex<WorkflowWorkerActor>>,
    },
}

#[derive(Clone, Debug)]
struct WorkflowWorkerResult {
    output: Value,
}

#[derive(Clone, Default)]
pub struct WorkerRuntimeState {
    pub(crate) completed_output: Option<Value>,
    worker_plan: Plan,
    visible_source_lines: std::collections::HashSet<
        crate::runtime::runtime_loop::coding_source_elision::CodingSourceLineKey,
    >,
    image_state_dir: PathBuf,
    turn_epoch: u64,
}

struct WorkerToolCallContext<'a> {
    apps: &'a mut AppManager,
    context: &'a Context,
    definition: &'a WorkerDefinition,
    worker_runtime: &'a mut WorkerRuntimeState,
    tools: &'a [WorkerTool],
    local_tools: &'a Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
    cancellation: Option<WorkflowCancellationRef<'a>>,
}

impl WorkerRuntimeState {
    fn new() -> Self {
        let worker_id = uuid::Uuid::new_v4().to_string();
        let image_state_dir = crate::daat_locus_paths::daat_locus_paths_sync()
            .state_dir()
            .join("workflow_workers")
            .join(worker_id)
            .join("viewed_images");
        Self {
            completed_output: None,
            worker_plan: Plan::default(),
            visible_source_lines: std::collections::HashSet::new(),
            image_state_dir,
            turn_epoch: 0,
        }
    }

    const fn next_turn_epoch(&mut self) -> u64 {
        self.turn_epoch = self.turn_epoch.wrapping_add(1);
        self.turn_epoch
    }
}

#[derive(Clone, Debug)]
struct WorkerTool {
    name: String,
    description: String,
    input_schema: Value,
    output_schema: Value,
}

pub async fn invoke(
    context: &Context,
    invocation: WorkflowInvocation,
) -> Result<WorkflowInvocationResult> {
    let cancellation = context.workflow_cancellation.begin();
    let result = invoke_with_cancellation(context, invocation, Some(&cancellation)).await;
    context.workflow_cancellation.clear(&cancellation);
    result
}

pub async fn invoke_with_cancellation(
    context: &Context,
    invocation: WorkflowInvocation,
    cancellation: Option<&WorkflowCancellation>,
) -> Result<WorkflowInvocationResult> {
    let definition = context
        .workflows
        .get(&invocation.workflow_id)
        .cloned()
        .ok_or_else(|| miette!("unknown workflow `{}`", invocation.workflow_id))?;
    validate_value_against_schema(
        &invocation.input,
        &definition.input_schema,
        "workflow input",
    )?;
    let source = definition.source.clone();
    let inspector = WorkflowInspectorPublisher::new_with_history(
        definition.id.clone(),
        invocation.input.clone(),
        context.dashboard_tx.clone(),
        context.dashboard_history.clone(),
    );

    match run_workflow_script(
        &source,
        &definition,
        invocation.input,
        context,
        cancellation,
        &inspector,
    )
    .await
    {
        Ok(output) => {
            if let Err(err) =
                validate_value_against_schema(&output, &definition.output_schema, "workflow output")
            {
                let message = err.to_string();
                let snapshot =
                    inspector.finish_run(WorkflowNodeStatus::Failed, None, Some(message.clone()));
                return Ok(WorkflowInvocationResult {
                    workflow_id: definition.id,
                    status: WorkflowInvocationStatus::Failed,
                    output: None,
                    message,
                    snapshot,
                });
            }
            let snapshot =
                inspector.finish_run(WorkflowNodeStatus::Completed, Some(output.clone()), None);
            Ok(WorkflowInvocationResult {
                workflow_id: definition.id,
                status: WorkflowInvocationStatus::Completed,
                output: Some(output),
                message: "workflow completed".to_string(),
                snapshot,
            })
        }
        Err(err) if is_workflow_interrupted_error(&err) => {
            let snapshot = inspector.finish_run(
                WorkflowNodeStatus::Interrupted,
                None,
                Some(WORKFLOW_INTERRUPTED_ERROR.to_string()),
            );
            Ok(WorkflowInvocationResult {
                workflow_id: definition.id,
                status: WorkflowInvocationStatus::Interrupted,
                output: None,
                message: WORKFLOW_INTERRUPTED_ERROR.to_string(),
                snapshot,
            })
        }
        Err(err) => {
            let message = err.to_string();
            let snapshot =
                inspector.finish_run(WorkflowNodeStatus::Failed, None, Some(message.clone()));
            Ok(WorkflowInvocationResult {
                workflow_id: definition.id,
                status: WorkflowInvocationStatus::Failed,
                output: None,
                message,
                snapshot,
            })
        }
    }
}

fn load_workflow_definition(path: &Path) -> Result<WorkflowDefinition> {
    let source = fs::read_to_string(path)
        .map_err(|err| miette!("failed to read workflow source {}: {err}", path.display()))?;
    load_workflow_definition_from_source(path, &source)
}

fn load_workflow_definition_from_source(path: &Path, source: &str) -> Result<WorkflowDefinition> {
    let lua = new_lua().map_err(|err| lua_error(&err))?;
    let definition_slot = Arc::new(Mutex::new(None::<WorkflowDefinition>));
    let path = path.to_path_buf();
    let source = source.to_string();
    let source_name = path.display().to_string();
    lua.scope(|scope| {
        let workflow = lua.create_table()?;
        let slot = definition_slot.clone();
        let definition_path = path.clone();
        let definition_source = source.clone();
        workflow.set(
            "define",
            scope.create_function(move |lua, table: Table| {
                let input_schema: Value = lua.from_value(table.get("input")?)?;
                let output_schema: Value = lua.from_value(table.get("output")?)?;
                validate_model_facing_schema(&input_schema).map_err(mlua::Error::external)?;
                validate_model_facing_schema(&output_schema).map_err(mlua::Error::external)?;
                if !matches!(table.get::<LuaValue>("run")?, LuaValue::Function(_)) {
                    return Err(mlua::Error::external("workflow.define requires a run function"));
                }
                let id = definition_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        mlua::Error::external("workflow file must have a UTF-8 filename stem")
                    })?
                    .to_string();
                if !is_valid_workflow_name(&id) {
                    return Err(mlua::Error::external(
                        "workflow filename stem must use lowercase letters, digits, and single underscores",
                    ));
                }
                let mut guard = slot
                    .lock()
                    .map_err(|_| mlua::Error::external("workflow definition lock poisoned"))?;
                if guard.is_some() {
                    return Err(mlua::Error::external(
                        "workflow may call workflow.define only once",
                    ));
                }
                *guard = Some(WorkflowDefinition {
                    id,
                    path: definition_path.clone(),
                    source: definition_source.clone(),
                    input_schema,
                    output_schema,
                });
                drop(guard);
                Ok(())
            })?,
        )?;
        install_loading_stubs(scope, &workflow)?;
        lua.globals().set("workflow", workflow)?;
        lua.load(&source).set_name(&source_name).exec()
    })
    .map_err(|err| lua_error(&err))?;
    definition_slot
        .lock()
        .map_err(|_| miette!("workflow definition lock poisoned"))?
        .clone()
        .ok_or_else(|| miette!("workflow {} did not call workflow.define", path.display()))
}

fn install_loading_stubs<'scope, 'env>(
    scope: &'scope mlua::Scope<'scope, 'env>,
    workflow: &Table,
) -> mlua::Result<()>
where
    'env: 'scope,
{
    workflow.set(
        "agent",
        scope.create_function(|lua, table: Table| {
            worker_definition_from_lua(lua, &table)?;
            Ok(())
        })?,
    )?;
    workflow.set(
        "await",
        scope.create_function(|_, _: mlua::MultiValue| Ok(LuaValue::Nil))?,
    )?;
    workflow.set(
        "await_all",
        scope.create_function(|lua, _: mlua::MultiValue| lua.create_table())?,
    )?;
    workflow.set("tool", scope.create_function(|_, _: Table| Ok(()))?)?;
    Ok(())
}

async fn run_workflow_script(
    source: &str,
    definition: &WorkflowDefinition,
    input: Value,
    context: &Context,
    cancellation: Option<&WorkflowCancellation>,
    inspector: &WorkflowInspectorPublisher,
) -> Result<Value> {
    ensure_workflow_not_interrupted(cancellation)?;
    let lua = new_lua().map_err(|err| lua_error(&err))?;
    install_workflow_interrupt_hook(&lua, cancellation).map_err(|err| lua_error(&err))?;
    install_sandboxed_lua_environment(&lua, context).map_err(|err| lua_error(&err))?;
    let run_slot = Arc::new(Mutex::new(None::<Function>));
    let local_tools = Arc::new(Mutex::new(BTreeMap::<String, LocalToolDefinition>::new()));
    let source_name = definition.path.display().to_string();
    let expected_definition = definition.clone();

    let workflow = lua.create_table().map_err(|err| lua_error(&err))?;
    let define_slot = run_slot.clone();
    let define_definition = expected_definition.clone();
    workflow
        .set(
            "define",
            lua.create_function(move |lua, table: Table| {
                let input_schema: Value = lua.from_value(table.get("input")?)?;
                let output_schema: Value = lua.from_value(table.get("output")?)?;
                if input_schema != define_definition.input_schema
                    || output_schema != define_definition.output_schema
                {
                    return Err(mlua::Error::external(
                        "workflow definition changed after loading; reload it before invoking",
                    ));
                }
                let run = table.get::<Function>("run")?;
                *define_slot
                    .lock()
                    .map_err(|_| mlua::Error::external("workflow run-function lock poisoned"))? =
                    Some(run);
                Ok(())
            })
            .map_err(|err| lua_error(&err))?,
        )
        .map_err(|err| lua_error(&err))?;
    lua.globals()
        .set("workflow", workflow.clone())
        .map_err(|err| lua_error(&err))?;
    let factory_context = WorkerActorFactoryContext::from_context(context);
    install_execution_functions(
        &lua,
        &workflow,
        factory_context.clone(),
        local_tools.clone(),
    )
    .map_err(|err| lua_error(&err))?;
    let local_tool_slot = local_tools.clone();
    workflow
        .set(
            "tool",
            lua.create_function(move |lua, table: Table| {
                let local_tool = local_tool_definition_from_lua(lua, &table)?;
                let mut tools = local_tool_slot
                    .lock()
                    .map_err(|_| mlua::Error::external("workflow local-tool lock poisoned"))?;
                if tools.contains_key(&local_tool.name) {
                    return Err(mlua::Error::external(format!(
                        "workflow declares duplicate local tool `{}`",
                        local_tool.name
                    )));
                }
                tools.insert(local_tool.name.clone(), local_tool);
                drop(tools);
                Ok(())
            })
            .map_err(|err| lua_error(&err))?,
        )
        .map_err(|err| lua_error(&err))?;
    lua.load(source)
        .set_name(&source_name)
        .exec()
        .map_err(|err| lua_error(&err))?;

    let run = run_slot
        .lock()
        .map_err(|_| miette!("workflow run-function lock poisoned"))?
        .clone()
        .ok_or_else(|| {
            miette!(
                "workflow `{}` did not provide a run function",
                definition.id
            )
        })?;
    let lua_input = lua.to_value(&input).map_err(|err| lua_error(&err))?;
    let lua_context = lua.create_table().map_err(|err| lua_error(&err))?;
    let thread = lua.create_thread(run).map_err(|err| lua_error(&err))?;
    let mut yielded: LuaValue = thread
        .resume((lua_input, lua_context))
        .map_err(|err| lua_error(&err))?;
    while thread.status() == ThreadStatus::Resumable {
        ensure_workflow_not_interrupted(cancellation)?;
        let request = workflow_yield_from_lua(&lua, yielded).map_err(|err| lua_error(&err))?;
        let result = match request {
            WorkflowYield::Worker {
                worker,
                transition_kind,
            } => {
                let (group_id, mut worker_ids) = inspector.begin_group(1, transition_kind);
                let worker_id = worker_ids
                    .pop()
                    .expect("single workflow await group has one worker id");
                let (definition, actor_id) =
                    worker_inspector_identity_from_actor(&worker.actor).await;
                inspector.begin_worker(
                    &group_id,
                    worker_id.clone(),
                    actor_id,
                    &definition,
                    worker.input.clone(),
                );
                let result = run_worker_with_timing(
                    context,
                    worker.actor,
                    worker.input,
                    &local_tools,
                    cancellation.map(Into::into),
                    inspector,
                    &worker_id,
                )
                .await;
                inspector.finish_group(&group_id);
                result?.output
            }
            WorkflowYield::Workers {
                workers,
                transition_kind,
            } => {
                let (group_id, worker_ids) = inspector.begin_group(workers.len(), transition_kind);
                for (worker, worker_id) in workers.iter().zip(&worker_ids) {
                    let (definition, actor_id) =
                        worker_inspector_identity_from_actor(&worker.actor).await;
                    inspector.begin_worker(
                        &group_id,
                        worker_id.clone(),
                        actor_id,
                        &definition,
                        worker.input.clone(),
                    );
                }
                let worker_cancellation = WorkflowWorkerCancellation::new(cancellation);
                let worker_futures =
                    workers
                        .into_iter()
                        .zip(worker_ids)
                        .map(|(worker, worker_id)| {
                            let local_tools = Arc::clone(&local_tools);
                            let worker_cancellation = worker_cancellation.clone();
                            async move {
                                let result = run_worker_with_timing(
                                    context,
                                    worker.actor,
                                    worker.input,
                                    &local_tools,
                                    Some(worker_cancellation.clone().into()),
                                    inspector,
                                    &worker_id,
                                )
                                .await;
                                let initiated_group_interrupt =
                                    result.is_err() && worker_cancellation.interrupt_group();
                                (
                                    result.map(|result| result.output),
                                    initiated_group_interrupt,
                                    worker_cancellation.is_externally_interrupted(),
                                )
                            }
                        });
                let results = futures_util::future::join_all(worker_futures).await;
                inspector.finish_group(&group_id);
                let mut interrupted_error = None;
                let mut first_error = None;
                let mut outputs = Vec::with_capacity(results.len());
                for (result, initiated_group_interrupt, externally_interrupted_at_finish) in results
                {
                    match result {
                        Ok(output) => outputs.push(output),
                        Err(error) if externally_interrupted_at_finish => {
                            interrupted_error.get_or_insert(error);
                        }
                        Err(error)
                            if !initiated_group_interrupt
                                && is_workflow_interrupted_error(&error) => {}
                        Err(error) if first_error.is_none() => first_error = Some(error),
                        Err(_) => {}
                    }
                }
                if let Some(error) = interrupted_error {
                    return Err(error);
                }
                if let Some(error) = first_error {
                    return Err(error);
                }
                Value::Array(outputs)
            }
            WorkflowYield::ResetActor { actor } => {
                ensure_workflow_not_interrupted(cancellation)?;
                reset_workflow_worker_actor(&actor, &factory_context).await?;
                Value::Null
            }
        };
        yielded = thread
            .resume(lua.to_value(&result).map_err(|err| lua_error(&err))?)
            .map_err(|err| lua_error(&err))?;
    }
    lua.from_value(yielded).map_err(|err| {
        miette!(
            "workflow `{}` returned a non-JSON value: {err}",
            definition.id
        )
    })
}

fn install_execution_functions(
    lua: &Lua,
    workflow: &Table,
    factory_context: WorkerActorFactoryContext,
    local_tools: Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
) -> mlua::Result<()> {
    let worker_factory_context = factory_context.clone();
    workflow.set(
        "agent",
        lua.create_function(move |lua, table: Table| {
            let definition = worker_definition_from_lua(lua, &table)?;
            let actor = Arc::new(tokio::sync::Mutex::new(
                WorkflowWorkerActor::new(&worker_factory_context, definition, &local_tools)
                    .map_err(mlua::Error::external)?,
            ));
            let factory = lua.create_table()?;
            factory.set(
                "actor",
                lua.create_userdata(WorkflowWorkerActorHandle(actor))?,
            )?;
            factory.set(
                "run",
                lua.create_function(|lua, (factory, input): (Table, LuaValue)| {
                    let handle = lua.create_table()?;
                    handle.set("actor", factory.get::<AnyUserData>("actor")?)?;
                    handle.set("input", input)?;
                    Ok(handle)
                })?,
            )?;
            factory.set(
                "reset",
                lua.create_function(|lua, factory: Table| {
                    let request = lua.create_table()?;
                    request.set("reset_actor", factory.get::<AnyUserData>("actor")?)?;
                    Ok(request)
                })?,
            )?;
            Ok(factory)
        })?,
    )?;
    lua.load(
        r#"
        local function mark_handle_awaited(handle)
          if handle.awaited then
            error("workflow handle was already awaited")
          end
          handle.awaited = true
        end

        function workflow.await(handle, transition)
          if handle.reset_actor then
            return coroutine.yield({ handle = handle, transition = transition })
          end
          mark_handle_awaited(handle)
          return coroutine.yield({ handle = handle, transition = transition })
        end

        function workflow.await_all(handles, transition)
          local actors = {}
          for _, handle in ipairs(handles) do
            if handle.reset_actor then
              error("workflow.await_all accepts only worker run handles")
            end
            mark_handle_awaited(handle)
            local actor = handle.actor
            if actors[actor] then
              error("workflow.await_all cannot run more than one handle for the same actor")
            end
            actors[actor] = true
          end
          return coroutine.yield({ handles = handles, transition = transition })
        end
        "#,
    )
    .set_name("workflow execution functions")
    .exec()?;
    Ok(())
}

fn workflow_yield_from_lua(lua: &Lua, yielded: LuaValue) -> mlua::Result<WorkflowYield> {
    let yielded = match yielded {
        LuaValue::Table(yielded) => yielded,
        value => {
            return Err(mlua::Error::external(format!(
                "workflow yielded an unsupported value `{}`; use workflow.await(handle) or workflow.await_all(handles)",
                value.type_name()
            )));
        }
    };
    let transition_kind = yielded
        .get::<Option<String>>("transition")?
        .as_deref()
        .map(WorkflowTransitionKind::from_lua)
        .transpose()?
        .unwrap_or(WorkflowTransitionKind::Await);
    if let Ok(handles) = yielded.get::<Table>("handles") {
        let mut workers = Vec::new();
        for handle in handles.sequence_values::<Table>() {
            workers.push(worker_invocation_from_lua(lua, &handle?)?);
        }
        return Ok(WorkflowYield::Workers {
            workers,
            transition_kind,
        });
    }
    let handle = yielded.get::<Table>("handle").unwrap_or(yielded);
    if let Some(actor) = handle.get::<Option<AnyUserData>>("reset_actor")? {
        let actor = actor.borrow::<WorkflowWorkerActorHandle>()?.0.clone();
        return Ok(WorkflowYield::ResetActor { actor });
    }
    Ok(WorkflowYield::Worker {
        worker: worker_invocation_from_lua(lua, &handle)?,
        transition_kind,
    })
}

fn worker_invocation_from_lua(lua: &Lua, handle: &Table) -> mlua::Result<WorkerInvocation> {
    let actor = handle
        .get::<AnyUserData>("actor")?
        .borrow::<WorkflowWorkerActorHandle>()?
        .0
        .clone();
    let input: Value = lua.from_value(handle.get("input")?)?;
    Ok(WorkerInvocation { actor, input })
}

fn worker_conversation(definition: &WorkerDefinition) -> RuntimeStepConversation {
    RuntimeStepConversation::new(vec![AgentMessage::system(worker_system_instruction(
        &definition.instruction,
    ))])
}

fn worker_input_message(input: &Value) -> String {
    serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string())
}

async fn worker_inspector_identity_from_actor(
    actor: &Arc<tokio::sync::Mutex<WorkflowWorkerActor>>,
) -> (WorkerDefinition, String) {
    let actor = actor.lock().await;
    (actor.definition.clone(), actor.actor_id.clone())
}

async fn reset_workflow_worker_actor(
    actor: &Arc<tokio::sync::Mutex<WorkflowWorkerActor>>,
    factory_context: &WorkerActorFactoryContext,
) -> Result<()> {
    actor.lock().await.reset(factory_context)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

async fn run_worker_with_timing(
    context: &Context,
    actor: Arc<tokio::sync::Mutex<WorkflowWorkerActor>>,
    input: Value,
    local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
    cancellation: Option<WorkflowCancellationRef<'_>>,
    inspector: &WorkflowInspectorPublisher,
    worker_id: &str,
) -> Result<WorkflowWorkerResult> {
    let started_at = Instant::now();
    let worker = run_worker(
        context,
        actor,
        input,
        local_tools,
        cancellation,
        inspector,
        worker_id,
    );
    tokio::pin!(worker);
    let mut runtime_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    runtime_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let result = loop {
        tokio::select! {
            result = &mut worker => break result,
            _ = runtime_tick.tick() => {
                inspector.update_worker_run_time(worker_id, duration_millis(started_at.elapsed()));
            }
        }
    };
    inspector.finish_worker(worker_id, &result, duration_millis(started_at.elapsed()));
    result
}

async fn run_worker(
    context: &Context,
    actor: Arc<tokio::sync::Mutex<WorkflowWorkerActor>>,
    input: Value,
    local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
    cancellation: Option<WorkflowCancellationRef<'_>>,
    inspector: &WorkflowInspectorPublisher,
    worker_id: &str,
) -> Result<WorkflowWorkerResult> {
    ensure_workflow_not_interrupted(cancellation.as_ref())?;
    let run_guard = {
        let actor = actor.lock().await;
        actor.begin_run()?
    };
    let result = run_worker_turn(
        context,
        &actor,
        input,
        local_tools,
        cancellation,
        inspector,
        worker_id,
    )
    .await;
    drop(run_guard);
    result
}

async fn run_worker_turn(
    context: &Context,
    actor: &Arc<tokio::sync::Mutex<WorkflowWorkerActor>>,
    input: Value,
    local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
    cancellation: Option<WorkflowCancellationRef<'_>>,
    inspector: &WorkflowInspectorPublisher,
    worker_id: &str,
) -> Result<WorkflowWorkerResult> {
    let mut actor = actor.lock().await;
    validate_value_against_schema(&input, &actor.definition.input_schema, "worker input")?;
    let input_message = worker_input_message(&input);
    actor
        .conversation
        .push_agent_message(AgentMessage::user(&input_message));
    if let Some(event) = crate::dashboard::user_activity_cell(&input_message) {
        inspector.append_worker_activity(worker_id, event);
    }
    let mut budget_recoveries = 0usize;

    loop {
        ensure_workflow_not_interrupted(cancellation.as_ref())?;
        let model = actor.definition.model.clone();
        let tools = worker_tool_specs(
            &actor.apps,
            &actor.definition,
            &actor.tools,
            worker_model_supports_vision(context, &model),
        );
        if maybe_compact_agent_messages(
            context,
            worker_model_provider(context, &model),
            &mut actor.conversation,
            &tools,
            &context.token_estimate_baseline,
            false,
        )
        .await?
        {
            continue;
        }

        let request = AgentTurnRequest {
            messages: actor.conversation.clone_agent_messages(),
            tools: tools.clone(),
        };
        let response =
            match complete_workflow_worker_turn(context, &model, request, cancellation.clone())
                .await
            {
                Ok(response) => response,
                Err(error)
                    if is_context_budget_exceeded(&error)
                        && budget_recoveries < MID_TURN_COMPACTION_MAX_RECOVERIES =>
                {
                    if maybe_compact_agent_messages(
                        context,
                        worker_model_provider(context, &model),
                        &mut actor.conversation,
                        &tools,
                        &context.token_estimate_baseline,
                        true,
                    )
                    .await?
                    {
                        budget_recoveries += 1;
                        continue;
                    }
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
        let protocol = response.protocol();
        let follow_up_message =
            protocol.follow_up_message(true, WORKER_EXPLICIT_COMPLETION_MESSAGE);
        if protocol.tool_calls.is_empty() {
            let final_message = protocol.final_assistant_message.unwrap_or_default();
            if !final_message.trim().is_empty() {
                if let Some(event) = crate::dashboard::assistant_activity_cell(&final_message) {
                    inspector.append_worker_activity(worker_id, event);
                }
                actor
                    .conversation
                    .push_agent_message(AgentMessage::assistant(&final_message));
            }
            actor.conversation.push_agent_message(AgentMessage::user(
                follow_up_message.expect("workers always require explicit completion"),
            ));
        } else {
            let assistant_text = protocol.assistant_text.clone();
            if let Some(content) = assistant_text
                .as_deref()
                .filter(|content| !content.trim().is_empty())
                && let Some(event) = crate::dashboard::assistant_activity_cell(content)
            {
                inspector.append_worker_activity(worker_id, event);
            }
            actor.conversation.push_agent_message(
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    assistant_text,
                    protocol.reasoning_content.clone(),
                    protocol.tool_calls.clone(),
                ),
            );
            for call in protocol.tool_calls {
                ensure_workflow_not_interrupted(cancellation.as_ref())?;
                let WorkflowWorkerActor {
                    apps,
                    definition,
                    tools,
                    runtime,
                    ..
                } = &mut *actor;
                let result = execute_worker_tool(
                    &call,
                    WorkerToolCallContext {
                        apps,
                        context,
                        definition,
                        worker_runtime: runtime,
                        tools,
                        local_tools,
                        cancellation: cancellation.clone(),
                    },
                )
                .await;
                match &result {
                    Ok(result) => {
                        if let Some(event) = result.activity_event.clone() {
                            inspector.append_worker_activity(worker_id, event);
                        }
                    }
                    Err(error) => inspector.append_worker_activity(
                        worker_id,
                        crate::dashboard::SessionActivityEvent::Error(
                            crate::activity_event::TextActivityDescriptor {
                                title: format!(
                                    "{} failed",
                                    crate::app::AppId::render_exposed_tool_name(&call.name)
                                ),
                                body_lines: error
                                    .to_string()
                                    .lines()
                                    .take(12)
                                    .map(ToOwned::to_owned)
                                    .collect(),
                            }
                            .into(),
                        ),
                    ),
                }
                let WorkflowWorkerActor {
                    conversation,
                    runtime,
                    ..
                } = &mut *actor;
                append_worker_tool_result_message(conversation, call, result, runtime);
            }
        }

        if let Some(output) = actor.runtime.completed_output.take() {
            return Ok(WorkflowWorkerResult { output });
        }
    }
}

fn worker_tool_specs(
    apps: &AppManager,
    definition: &WorkerDefinition,
    local_tools: &[WorkerTool],
    supports_vision: bool,
) -> Vec<AgentToolSpec> {
    let mut tools = build_worker_runtime_tool_specs_for_apps(
        apps,
        definition.output_schema.clone(),
        supports_vision,
    );
    tools.extend(local_tools.iter().map(|tool| AgentToolSpec {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_spec: AgentToolInputSpec::JsonSchema {
            schema: tool.input_schema.clone(),
        },
    }));
    tools
}

fn worker_model_supports_vision(context: &Context, model: &WorkerModel) -> bool {
    let model = match model {
        WorkerModel::Main => context.config.main_model_config(),
        WorkerModel::Efficient => context.config.efficient_model_config(),
    };
    model.supports_vision.unwrap_or_else(|| {
        crate::model_catalog::catalog_model_capacity(&model.model_id)
            .is_none_or(|capacity| capacity.supports_vision)
    })
}

fn worker_model_provider<'a>(
    context: &'a Context,
    model: &WorkerModel,
) -> &'a (dyn crate::core::ModelProvider + Send + Sync) {
    match model {
        WorkerModel::Main => context.model_provider.as_ref(),
        WorkerModel::Efficient => context.efficient_model_provider.as_ref(),
    }
}

fn append_worker_tool_result_message(
    conversation: &mut RuntimeStepConversation,
    call: AgentToolCall,
    result: Result<ToolExecutionResult>,
    worker_runtime: &mut WorkerRuntimeState,
) {
    match result {
        Ok(mut result) => {
            let model_content = if result.skip_source_elision {
                result.model_content()
            } else {
                crate::runtime::runtime_loop::coding_source_elision::elide_tool_model_content(
                    &mut worker_runtime.visible_source_lines,
                    &call,
                    &result.model_content(),
                )
            };
            let model_image_parts = std::mem::take(&mut result.model_image_parts);
            conversation.push_agent_message(AgentMessage::tool(
                call.id,
                call.name.clone(),
                model_content,
            ));
            if !model_image_parts.is_empty() {
                conversation.push_agent_message(AgentMessage::user_content(
                    crate::reasoning::runtime::AgentContent::multimodal(
                        format!(
                            "The `{}` tool attached image content for visual inspection.",
                            call.name
                        ),
                        model_image_parts,
                    ),
                ));
            }
        }
        Err(err) => conversation.push_agent_message(AgentMessage::tool(
            call.id,
            call.name,
            json!({ "ok": false, "error": err.to_string() }).to_string(),
        )),
    }
}

fn build_worker_apps(
    execution_cwd: &Path,
    sandbox_policy: &crate::sandbox::RuntimeSandboxPolicy,
) -> Result<AppManager> {
    let worker_id = uuid::Uuid::new_v4().to_string();
    let (apps, _workspace_apps) =
        build_isolated_worker_apps(execution_cwd, sandbox_policy, &worker_id);
    AppManager::new(apps)
}

fn build_worker_tools(
    definition: &WorkerDefinition,
    local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
) -> Result<Vec<WorkerTool>> {
    let mut tools = BTreeMap::<String, WorkerTool>::new();
    let declared = local_tools
        .lock()
        .map_err(|_| miette!("workflow local-tool lock poisoned"))?;
    for name in &definition.extra_tools {
        let local = declared
            .get(name)
            .ok_or_else(|| miette!("workflow worker references undeclared local tool `{name}`"))?;
        tools.insert(
            name.clone(),
            WorkerTool {
                name: name.clone(),
                description: format!("Workflow-local tool `{name}`."),
                input_schema: local.input_schema.clone(),
                output_schema: local.output_schema.clone(),
            },
        );
    }
    drop(declared);
    Ok(tools.into_values().collect())
}

async fn complete_workflow_worker_turn(
    context: &Context,
    model: &WorkerModel,
    request: AgentTurnRequest,
    cancellation: Option<WorkflowCancellationRef<'_>>,
) -> Result<crate::reasoning::runtime::AgentTurnStreamResult> {
    ensure_workflow_not_interrupted(cancellation.as_ref())?;
    let provider = worker_model_provider(context, model);
    let options = ModelRequestOptions::for_agent_turn(provider, &request, None)?;
    let result = complete_agent_turn_with_retry_using_policy_cancellable_with_observer(
        provider,
        request,
        options,
        AgentTurnRetryPolicy::default(),
        || {
            cancellation
                .as_ref()
                .is_some_and(|cancellation| cancellation.is_interrupted())
        },
        AgentTurnRetryObserver::default(),
    )
    .await;
    ensure_workflow_not_interrupted(cancellation.as_ref())?;
    result
        .map(|success| success.response)
        .map_err(|failure| failure.error)
}

async fn execute_worker_tool(
    call: &AgentToolCall,
    execution: WorkerToolCallContext<'_>,
) -> Result<ToolExecutionResult> {
    let WorkerToolCallContext {
        apps,
        context,
        definition,
        worker_runtime,
        tools,
        local_tools,
        cancellation,
    } = execution;
    ensure_workflow_not_interrupted(cancellation.as_ref())?;
    if call.name == "finish_and_send" {
        validate_value_against_schema(&call.arguments, &definition.output_schema, "worker output")?;
        worker_runtime.completed_output = Some(call.arguments.clone());
        return Ok(ToolExecutionResult::from_activity_event(
            "worker completed",
            call.arguments.clone(),
            None,
        ));
    }
    if let Some(tool) = tools.iter().find(|tool| tool.name == call.name) {
        validate_value_against_schema(&call.arguments, &tool.input_schema, "worker tool input")?;
        return execute_worker_local_tool(tool, call, local_tools, cancellation);
    }
    let model_config = match definition.model {
        WorkerModel::Main => context.config.main_model_config(),
        WorkerModel::Efficient => context.config.efficient_model_config(),
    };
    let turn_epoch = worker_runtime.next_turn_epoch();
    execute_worker_runtime_tool_call_for_apps(
        apps,
        call,
        WorkerRuntimeToolCallContext {
            execution_cwd: &context.execution_cwd,
            sandbox_policy: &context.sandbox_policy,
            tool_output_max_tokens: model_config.tool_output_max_tokens.max(1),
            supports_vision: Some(worker_model_supports_vision(context, &definition.model)),
            image_state_dir: &worker_runtime.image_state_dir,
            turn_epoch,
            output_schema: &definition.output_schema,
            worker_plan: &mut worker_runtime.worker_plan,
        },
    )
    .await
}

fn execute_worker_local_tool(
    tool: &WorkerTool,
    call: &AgentToolCall,
    local_tools: &Arc<Mutex<BTreeMap<String, LocalToolDefinition>>>,
    cancellation: Option<WorkflowCancellationRef<'_>>,
) -> Result<ToolExecutionResult> {
    let local = local_tools
        .lock()
        .map_err(|_| miette!("workflow local-tool lock poisoned"))?
        .get(&tool.name)
        .cloned()
        .ok_or_else(|| miette!("workflow local tool `{}` disappeared", tool.name))?;
    let lua_input = local
        .lua
        .to_value(&call.arguments)
        .map_err(|err| lua_error(&err))?;
    let lua_output: LuaValue = local.run.call(lua_input).map_err(|err| lua_error(&err))?;
    let output: Value = local
        .lua
        .from_value(lua_output)
        .map_err(|err| lua_error(&err))?;
    ensure_workflow_not_interrupted(cancellation.as_ref())?;
    validate_value_against_schema(&output, &tool.output_schema, "workflow local tool output")?;
    Ok(ToolExecutionResult::from_activity_event(
        format!("workflow local tool `{}` completed", tool.name),
        output,
        None,
    ))
}

fn worker_system_instruction(instruction: &str) -> String {
    format!(
        "You are an isolated workflow worker. You receive only the workflow instruction, typed input, worker-local App instances, and explicitly declared workflow-local tools. Do not claim or finish user events, do not assume access to session conversation history, and do not call workflow tools. When the work is complete, call `finish_and_send` exactly once with arguments matching your declared output schema. Do not end the turn with only assistant text.\n\nWorkflow instruction:\n{instruction}"
    )
}

fn worker_definition_from_lua(lua: &Lua, table: &Table) -> mlua::Result<WorkerDefinition> {
    let role = table
        .get::<String>("role")
        .map_err(|_| mlua::Error::external("workflow.agent requires a non-empty `role` string"))?;
    let role = role.trim();
    if role.is_empty() {
        return Err(mlua::Error::external(
            "workflow.agent role must not be empty",
        ));
    }
    let role = role.to_string();
    let model_name = table.get::<Option<String>>("model")?;
    let model = match model_name.as_deref().unwrap_or("main") {
        "main" => WorkerModel::Main,
        "efficient" => WorkerModel::Efficient,
        other => {
            return Err(mlua::Error::external(format!(
                "workflow.agent model must be `main` or `efficient`, got `{other}`"
            )));
        }
    };
    let input_schema: Value = lua.from_value(table.get("input")?)?;
    let output_schema: Value = lua.from_value(table.get("output")?)?;
    validate_model_facing_schema(&input_schema).map_err(mlua::Error::external)?;
    validate_model_facing_schema(&output_schema).map_err(mlua::Error::external)?;
    let instruction = table.get::<String>("instruction")?;
    if instruction.trim().is_empty() {
        return Err(mlua::Error::external(
            "workflow.agent instruction must not be empty",
        ));
    }
    if !matches!(table.get::<LuaValue>("capabilities")?, LuaValue::Nil) {
        return Err(mlua::Error::external(
            "workflow.agent `capabilities` has been removed; the host automatically provides allowed App tools",
        ));
    }
    let extra_tools = string_list_from_lua(table.get::<Option<Table>>("extra_tools")?)?;
    Ok(WorkerDefinition {
        role,
        model,
        input_schema,
        output_schema,
        instruction,
        extra_tools,
    })
}

#[cfg(test)]
#[allow(dead_code)]
fn worker_definition_to_lua(lua: &Lua, definition: &WorkerDefinition) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("role", definition.role.clone())?;
    table.set(
        "model",
        match definition.model {
            WorkerModel::Main => "main",
            WorkerModel::Efficient => "efficient",
        },
    )?;
    table.set("input", lua.to_value(&definition.input_schema)?)?;
    table.set("output", lua.to_value(&definition.output_schema)?)?;
    table.set("instruction", definition.instruction.clone())?;
    let extra_tools = lua.create_table()?;
    for local_tool in &definition.extra_tools {
        extra_tools.push(local_tool.clone())?;
    }
    table.set("extra_tools", extra_tools)?;
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{HashMap, VecDeque},
        sync::Arc,
    };

    use async_trait::async_trait;

    use crate::{
        app::AppManager,
        config::Config,
        context::Context,
        context_budget::{RequestBudgetLimits, TokenEstimateBaseline},
        core::{ModelProvider, ModelRequestOptions, TokenUsageInfo},
        events::EventStore,
        memory::Memory,
        openskills::OpenSkillsCatalog,
        pending_work::PendingWorkQueue,
        plan::Plan,
        reasoning::{
            compiled::CompiledPromptStore,
            runtime::{AgentTurnItem, AgentTurnStreamResult, PromptRequest},
        },
        runtime::bootstrap::DaatLocusHomeOverride,
        sandbox::RuntimeSandboxPolicy,
        telegram_acl::TelegramAclHandle,
        telegram_transport::state::TelegramTransportState,
        workspace_app::WorkspaceAppRegistry,
    };

    fn workflow_inspector_actor_id(definition: &WorkerDefinition) -> String {
        format!("{}-actor", definition.role)
    }

    #[test]
    fn workflow_inspector_snapshot_round_trips_with_nested_activity() {
        let snapshot = WorkflowRunSnapshot {
            run_id: "run-1".to_string(),
            workflow_id: "research".to_string(),
            status: WorkflowNodeStatus::Completed,
            started_at_ms: 10,
            completed_at_ms: Some(30),
            input: json!({ "topic": "rust" }),
            output: Some(json!({ "summary": "done" })),
            error: None,
            await_groups: vec![WorkflowAwaitGroupSnapshot {
                group_id: "await-1".to_string(),
                sequence: 1,
                status: WorkflowNodeStatus::Completed,
                started_at_ms: 11,
                completed_at_ms: Some(29),
                worker_ids: vec!["worker-1".to_string()],
            }],
            transitions: vec![WorkflowTransitionSnapshot {
                source_worker_id: "worker-1".to_string(),
                target_worker_id: "worker-2".to_string(),
                kind: WorkflowTransitionKind::Verify,
            }],
            workers: vec![WorkflowWorkerSnapshot {
                worker_id: "worker-1".to_string(),
                actor_id: "researcher-actor".to_string(),
                await_group_id: "await-1".to_string(),
                role: "researcher".to_string(),
                model: "main".to_string(),
                status: WorkflowNodeStatus::Completed,
                started_at_ms: 12,
                completed_at_ms: Some(28),
                agent_run_time_ms: 12_345,
                input: json!({ "query": "rust" }),
                output: Some(json!({ "answer": "ok" })),
                error: None,
                activity_count: 1,
                activity_revision: 1,
                activity: vec![
                    crate::dashboard::thinking_activity_cell("considering sources")
                        .expect("thinking activity"),
                ],
            }],
        };
        let encoded = serde_json::to_string(&snapshot).expect("encode workflow snapshot");
        let decoded: WorkflowRunSnapshot =
            serde_json::from_str(&encoded).expect("decode workflow snapshot");

        assert_eq!(decoded, snapshot);
        assert_eq!(decoded.workers[0].role, "researcher");
        assert_eq!(decoded.transitions[0].kind, WorkflowTransitionKind::Verify);
        assert_eq!(decoded.workers[0].agent_run_time_ms, 12_345);
        assert_eq!(decoded.workers[0].activity_count, 1);
        assert_eq!(decoded.workers[0].activity.len(), 1);
        assert!(encoded.contains("considering sources"));
    }
    #[test]
    fn workflow_transport_snapshot_size_does_not_grow_with_worker_activity_history() {
        let inspector =
            WorkflowInspectorPublisher::new("bounded-test".to_string(), json!({}), None);
        let definition = WorkerDefinition {
            role: "researcher".to_string(),
            model: WorkerModel::Main,
            input_schema: json!({}),
            output_schema: json!({}),
            instruction: String::new(),
            extra_tools: Vec::new(),
        };
        inspector.begin_worker(
            "await-1",
            "worker-1".to_string(),
            workflow_inspector_actor_id(&definition),
            &definition,
            json!({}),
        );
        let baseline = serde_json::to_vec(&inspector.transport_snapshot())
            .expect("encode baseline transport snapshot")
            .len();
        for index in 0..100 {
            inspector.append_worker_activity(
                "worker-1",
                crate::dashboard::assistant_activity_cell(&format!(
                    "unique worker activity payload {index:03}"
                ))
                .expect("assistant activity"),
            );
        }
        let transport = inspector.transport_snapshot();
        let encoded = serde_json::to_vec(&transport).expect("encode transport snapshot");
        assert_eq!(
            transport.workers[0].activity.len(),
            WORKFLOW_WORKER_ACTIVITY_TAIL_LIMIT
        );
        assert_eq!(transport.workers[0].activity_count, 100);
        assert!(
            encoded.len() < baseline + 2_000,
            "bounded snapshot grew to {} bytes",
            encoded.len()
        );
    }

    #[tokio::test]
    async fn workflow_worker_retries_transient_provider_failure() {
        let main = ScriptedWorkerProvider::with_responses(
            "main",
            vec![
                ScriptedWorkerResponse::error("connection reset"),
                ScriptedWorkerResponse::finish(json!({})),
            ],
        );
        let main_probe = main.clone();
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let definition = WorkerDefinition {
            role: "worker".to_string(),
            model: WorkerModel::Main,
            input_schema: worker_schema(),
            output_schema: worker_schema(),
            instruction: "Return the required output.".to_string(),
            extra_tools: Vec::new(),
        };
        let inspector =
            WorkflowInspectorPublisher::new("provider-retry-test".to_string(), json!({}), None);
        let (group_id, worker_ids) = inspector.begin_group(1, WorkflowTransitionKind::Await);
        let worker_id = worker_ids.first().expect("worker id").clone();
        inspector.begin_worker(
            &group_id,
            worker_id.clone(),
            workflow_inspector_actor_id(&definition),
            &definition,
            json!({}),
        );
        let local_tools = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let actor = Arc::new(tokio::sync::Mutex::new(
            WorkflowWorkerActor::new(
                &WorkerActorFactoryContext::from_context(&isolated.context),
                definition.clone(),
                &local_tools,
            )
            .expect("create workflow worker actor"),
        ));
        let result = run_worker_with_timing(
            &isolated.context,
            actor,
            json!({}),
            &local_tools,
            None,
            &inspector,
            &worker_id,
        )
        .await
        .expect("worker should retry a transient provider failure");

        assert_eq!(result.output, json!({}));
        assert_eq!(main_probe.requests().len(), 2);
        assert_eq!(
            inspector.snapshot().workers[0].status,
            WorkflowNodeStatus::Completed
        );
    }

    #[tokio::test]
    async fn session_backed_transport_keeps_bounded_activity_tail_and_history_is_pageable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let history = crate::dashboard::DashboardActivityHistoryStore::open_at_path_for_test(
            temp.path().join("history.sqlite3"),
        )
        .expect("history store");
        let (dashboard_tx, dashboard_rx) =
            tokio::sync::watch::channel(crate::dashboard::DashboardState::default());
        let inspector = WorkflowInspectorPublisher::new_with_history(
            "session-backed-workflow".to_string(),
            json!({}),
            Some(dashboard_tx),
            Some(history.clone()),
        );
        let definition = WorkerDefinition {
            role: "researcher".to_string(),
            model: WorkerModel::Main,
            input_schema: json!({}),
            output_schema: json!({}),
            instruction: "Inspect the project.".to_string(),
            extra_tools: Vec::new(),
        };
        let (group_id, worker_ids) = inspector.begin_group(1, WorkflowTransitionKind::Await);
        let worker_id = worker_ids.first().expect("worker id").clone();
        inspector.begin_worker(
            &group_id,
            worker_id.clone(),
            workflow_inspector_actor_id(&definition),
            &definition,
            json!({}),
        );
        let baseline = serde_json::to_vec(&inspector.transport_snapshot())
            .expect("encode baseline transport snapshot")
            .len();
        for index in 0..100 {
            inspector.append_worker_activity(
                &worker_id,
                crate::dashboard::assistant_activity_cell(&format!("session activity {index:03}"))
                    .expect("assistant activity"),
            );
        }
        let transport = inspector.transport_snapshot();
        let encoded = serde_json::to_vec(&transport).expect("encode transport snapshot");
        assert_eq!(
            transport.workers[0].activity.len(),
            WORKFLOW_WORKER_ACTIVITY_TAIL_LIMIT,
        );
        assert!(matches!(
            transport.workers[0].activity.last(),
            Some(crate::dashboard::SessionActivityEvent::Assistant(message))
                if message.content.contains("session activity 099")
        ));
        assert_eq!(transport.workers[0].activity_count, 100);
        assert!(
            encoded.len() <= baseline + WORKFLOW_WORKER_ACTIVITY_TAIL_LIMIT * 256,
            "bounded snapshot grew to {} bytes from {}",
            encoded.len(),
            baseline,
        );
        let dashboard_state = dashboard_rx.borrow().clone();
        let live_snapshot = dashboard_state
            .live_activity_events
            .iter()
            .find_map(|live| match &live.event {
                crate::dashboard::SessionActivityEvent::Workflow(workflow)
                    if workflow
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.run_id == transport.run_id) =>
                {
                    workflow.snapshot.as_ref()
                }
                _ => None,
            })
            .expect("workflow live event");
        assert_eq!(
            live_snapshot.workers[0].activity.len(),
            WORKFLOW_WORKER_ACTIVITY_TAIL_LIMIT,
        );
        let page = history
            .query_workflow_worker_activity(&transport.run_id, &worker_id, None, None, 20)
            .expect("query session-backed activity")
            .expect("worker activity stream");
        assert_eq!(page.activity_count, 100);
        assert_eq!(page.items.len(), 20);
        assert!(page.has_more_before);
    }

    #[test]
    fn workflow_inspector_coalesces_contiguous_explored_activity() {
        use crate::activity_event::ExploredCallActivityAction;
        use crate::dashboard::cells::{ExploredActivityData, ExploredCallActivityData};

        fn explored(summary: &str) -> crate::dashboard::SessionActivityEvent {
            crate::dashboard::SessionActivityEvent::Explored(ExploredActivityData {
                stable_id: "explored".to_string(),
                title: "Explored".to_string(),
                calls: vec![ExploredCallActivityData {
                    tool_name: "Read".to_string(),
                    action: Some(ExploredCallActivityAction::Read),
                    target: Some("src/dashboard/cells/mod.rs".to_string()),
                    secondary_target: None,
                    summary: summary.to_string(),
                    detail_lines: Vec::new(),
                    detail_title: None,
                }],
            })
        }

        let inspector =
            WorkflowInspectorPublisher::new("activity-test".to_string(), json!({}), None);
        let definition = WorkerDefinition {
            role: "researcher".to_string(),
            model: WorkerModel::Main,
            input_schema: json!({}),
            output_schema: json!({}),
            instruction: String::new(),
            extra_tools: Vec::new(),
        };
        inspector.begin_worker(
            "await-1",
            "worker-1".to_string(),
            workflow_inspector_actor_id(&definition),
            &definition,
            json!({}),
        );

        inspector.append_worker_activity("worker-1", explored("first read"));
        inspector.append_worker_activity("worker-1", explored("second read"));

        let snapshot = inspector.snapshot();
        assert_eq!(snapshot.workers[0].activity.len(), 1);
        let crate::dashboard::SessionActivityEvent::Explored(group) =
            &snapshot.workers[0].activity[0]
        else {
            panic!("expected explored activity");
        };
        assert_eq!(
            group
                .calls
                .iter()
                .map(|call| call.summary.as_str())
                .collect::<Vec<_>>(),
            vec!["first read", "second read"]
        );

        inspector.append_worker_activity(
            "worker-1",
            crate::dashboard::assistant_activity_cell("analysis boundary")
                .expect("assistant activity"),
        );
        inspector.append_worker_activity("worker-1", explored("third read"));

        let snapshot = inspector.snapshot();
        assert_eq!(snapshot.workers[0].activity.len(), 3);
        let crate::dashboard::SessionActivityEvent::Explored(group) =
            &snapshot.workers[0].activity[2]
        else {
            panic!("expected explored activity after boundary");
        };
        assert_eq!(
            group
                .calls
                .iter()
                .map(|call| call.summary.as_str())
                .collect::<Vec<_>>(),
            vec!["third read"]
        );
    }

    #[test]
    fn workflow_inspector_links_each_group_to_the_previous_non_empty_group() {
        let inspector =
            WorkflowInspectorPublisher::new("transition-test".to_string(), json!({}), None);
        let (_first_group_id, first_worker_ids) =
            inspector.begin_group(1, WorkflowTransitionKind::Await);
        inspector.begin_group(0, WorkflowTransitionKind::Await);
        let (_third_group_id, third_worker_ids) =
            inspector.begin_group(1, WorkflowTransitionKind::Revision);

        let snapshot = inspector.snapshot();
        assert_eq!(snapshot.await_groups.len(), 3);
        assert_eq!(
            snapshot.transitions,
            vec![WorkflowTransitionSnapshot {
                source_worker_id: first_worker_ids[0].clone(),
                target_worker_id: third_worker_ids[0].clone(),
                kind: WorkflowTransitionKind::Revision,
            }]
        );
    }

    #[test]
    fn workflow_catalog_keeps_builtin_workflows_when_directory_cannot_be_created() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let _home = runtime.block_on(DaatLocusHomeOverride::set(temp.path().to_path_buf()));
        let workflows_path = temp.path().join("workflows");
        fs::write(&workflows_path, "not a directory").expect("block workflow directory");

        let catalog = WorkflowCatalog::load();

        assert_eq!(
            catalog.get("goal").expect("builtin goal workflow").path,
            builtin_workflow_path("goal")
        );
        assert_eq!(
            catalog.get("search").expect("builtin search workflow").path,
            builtin_workflow_path("search")
        );
        assert!(
            catalog
                .errors()
                .iter()
                .any(|error| error.path == workflows_path)
        );
    }

    #[test]
    fn workflow_catalog_loads_builtin_in_memory_and_global_home_workflows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let _home = runtime.block_on(DaatLocusHomeOverride::set(temp.path().to_path_buf()));
        let global_workflows = temp.path().join("workflows");
        fs::create_dir_all(&global_workflows).expect("create global workflows directory");
        fs::write(
            global_workflows.join("global_only.lua"),
            include_str!("../examples/workflows/research_brief.lua"),
        )
        .expect("write global workflow");

        let session = tempfile::tempdir().expect("session tempdir");
        let session_workflows = session.path().join("workflows");
        fs::create_dir_all(&session_workflows).expect("create session workflows directory");
        fs::write(
            session_workflows.join("session_only.lua"),
            include_str!("../examples/workflows/research_brief.lua"),
        )
        .expect("write session workflow");

        let catalog = WorkflowCatalog::load();

        let goal = catalog.get("goal").expect("builtin goal workflow");
        assert_eq!(goal.path, builtin_workflow_path("goal"));
        assert_eq!(goal.source, include_str!("../workflows/goal.lua"));
        assert!(!global_workflows.join("goal.lua").exists());
        assert_eq!(goal.input_schema["required"], serde_json::json!(["goal"]));
        assert_eq!(
            goal.output_schema["required"],
            serde_json::json!([
                "achieved",
                "attempts",
                "summary",
                "verification",
                "remaining_work",
                "evidence"
            ])
        );

        let search = catalog.get("search").expect("builtin search workflow");
        assert_eq!(search.path, builtin_workflow_path("search"));
        assert_eq!(search.source, include_str!("../workflows/search.lua"));
        assert!(!global_workflows.join("search.lua").exists());
        assert_eq!(search.input_schema["required"], serde_json::json!(["goal"]));
        assert_eq!(
            search.output_schema["required"],
            serde_json::json!(["analysis", "sources", "verification", "rounds"])
        );
        assert!(
            include_str!("../workflows/search.lua").contains("while true"),
            "the search workflow must not impose a round limit"
        );
        assert!(catalog.get("global_only").is_some());
        assert!(catalog.get("session_only").is_none());
        assert!(catalog.errors().is_empty());
    }

    #[test]
    fn legacy_builtin_workflows_are_migrated_without_replacing_user_overrides() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let _home = runtime.block_on(DaatLocusHomeOverride::set(temp.path().to_path_buf()));
        let workflows = temp.path().join("workflows");
        fs::create_dir_all(&workflows).expect("create workflow directory");
        let legacy_goal = workflows.join("goal.lua");
        let legacy_search = workflows.join("search.lua");
        fs::write(
            &legacy_goal,
            include_str!("../tests/fixtures/workflows/legacy_goal.lua"),
        )
        .expect("write legacy goal workflow");
        fs::write(
            &legacy_search,
            include_str!("../tests/fixtures/workflows/legacy_search.lua"),
        )
        .expect("write legacy search workflow");
        let custom_source = include_str!("../examples/workflows/research_brief.lua");
        let custom_workflow = workflows.join("custom.lua");
        fs::write(&custom_workflow, custom_source).expect("write custom workflow");

        let catalog = WorkflowCatalog::load();

        assert_eq!(
            catalog.get("goal").expect("builtin goal workflow").path,
            builtin_workflow_path("goal")
        );
        assert_eq!(
            catalog.get("search").expect("builtin search workflow").path,
            builtin_workflow_path("search")
        );
        assert_eq!(
            catalog.get("custom").expect("custom workflow").path,
            custom_workflow
        );
        let backup_dir = workflows.join(LEGACY_BUILTIN_WORKFLOW_BACKUP_DIR);
        assert_eq!(
            fs::read_to_string(backup_dir.join("goal.lua")).expect("read migrated legacy goal"),
            include_str!("../tests/fixtures/workflows/legacy_goal.lua")
        );
        assert_eq!(
            fs::read_to_string(backup_dir.join("search.lua")).expect("read migrated legacy search"),
            include_str!("../tests/fixtures/workflows/legacy_search.lua")
        );
        assert!(!legacy_goal.exists());
        assert!(!legacy_search.exists());
        assert!(catalog.errors().is_empty());
    }

    #[test]
    fn user_override_matching_current_builtin_source_remains_an_override() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let _home = runtime.block_on(DaatLocusHomeOverride::set(temp.path().to_path_buf()));
        let workflows = temp.path().join("workflows");
        fs::create_dir_all(&workflows).expect("create workflow directory");
        let goal_override = workflows.join("goal.lua");
        fs::write(&goal_override, include_str!("../workflows/goal.lua"))
            .expect("write explicit override");

        let catalog = WorkflowCatalog::load();

        assert_eq!(
            catalog.get("goal").expect("goal override").path,
            goal_override
        );
        assert_eq!(
            catalog.get("goal").expect("goal override").source,
            include_str!("../workflows/goal.lua")
        );
        assert!(
            !workflows
                .join(LEGACY_BUILTIN_WORKFLOW_BACKUP_DIR)
                .join("goal.lua")
                .exists()
        );
        assert!(catalog.errors().is_empty());
    }

    #[test]
    fn invalid_builtin_name_override_blocks_builtin_fallback() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let _home = runtime.block_on(DaatLocusHomeOverride::set(temp.path().to_path_buf()));
        let workflows = temp.path().join("workflows");
        fs::create_dir_all(&workflows).expect("create workflow directory");
        let invalid_goal = workflows.join("goal.lua");
        fs::write(&invalid_goal, "this is not Lua").expect("write invalid goal override");

        let catalog = WorkflowCatalog::load();

        assert!(catalog.get("goal").is_none());
        assert!(
            catalog
                .errors()
                .iter()
                .any(|error| error.path == invalid_goal)
        );
    }

    #[test]
    fn existing_global_workflow_overrides_builtin_with_the_same_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let _home = runtime.block_on(DaatLocusHomeOverride::set(temp.path().to_path_buf()));
        let global_workflows = temp.path().join("workflows");
        fs::create_dir_all(&global_workflows).expect("create global workflows directory");
        let custom_goal = global_workflows.join("goal.lua");
        let custom_source = include_str!("../examples/workflows/research_brief.lua");
        fs::write(&custom_goal, custom_source).expect("write custom goal workflow");

        let catalog = WorkflowCatalog::load();

        let goal = catalog.get("goal").expect("overridden goal workflow");
        assert_eq!(goal.path, custom_goal);
        assert_eq!(
            fs::read_to_string(&goal.path).expect("read custom goal workflow"),
            custom_source
        );
        assert_eq!(
            goal.input_schema["required"],
            serde_json::json!(["topic", "sources"])
        );
        assert!(catalog.errors().is_empty());
    }

    #[derive(Clone)]
    enum ScriptedWorkerResponse {
        Finish(Value),
        Tool { name: String, arguments: Value },
        Error(String),
        ContextBudgetError,
    }

    impl ScriptedWorkerResponse {
        fn finish(arguments: Value) -> Self {
            Self::Finish(arguments)
        }

        fn tool(name: impl Into<String>, arguments: Value) -> Self {
            Self::Tool {
                name: name.into(),
                arguments,
            }
        }

        fn error(message: impl Into<String>) -> Self {
            Self::Error(message.into())
        }
    }

    #[derive(Clone)]
    struct ScriptedWorkerProvider {
        role: &'static str,
        responses: Arc<std::sync::Mutex<VecDeque<ScriptedWorkerResponse>>>,
        summaries: Arc<std::sync::Mutex<VecDeque<Value>>>,
        inputs: Arc<std::sync::Mutex<Vec<Value>>>,
        requests: Arc<std::sync::Mutex<Vec<AgentTurnRequest>>>,
        budgets: RequestBudgetLimits,
    }

    impl ScriptedWorkerProvider {
        fn new(role: &'static str, responses: Vec<Value>) -> Self {
            Self::with_responses(
                role,
                responses
                    .into_iter()
                    .map(ScriptedWorkerResponse::finish)
                    .collect(),
            )
        }

        fn with_responses(role: &'static str, responses: Vec<ScriptedWorkerResponse>) -> Self {
            Self::with_script(
                role,
                responses,
                Vec::new(),
                scripted_worker_default_budgets(),
            )
        }

        fn with_script(
            role: &'static str,
            responses: Vec<ScriptedWorkerResponse>,
            summaries: Vec<Value>,
            budgets: RequestBudgetLimits,
        ) -> Self {
            Self {
                role,
                responses: Arc::new(std::sync::Mutex::new(responses.into())),
                summaries: Arc::new(std::sync::Mutex::new(summaries.into())),
                inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
                requests: Arc::new(std::sync::Mutex::new(Vec::new())),
                budgets,
            }
        }

        fn inputs(&self) -> Vec<Value> {
            self.inputs.lock().expect("scripted inputs lock").clone()
        }

        fn requests(&self) -> Vec<AgentTurnRequest> {
            self.requests
                .lock()
                .expect("scripted requests lock")
                .clone()
        }
    }

    const fn scripted_worker_default_budgets() -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: 128_000,
            auto_compact_threshold_tokens: 128_000,
            reserved_output_tokens: 4_000,
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedWorkerProvider {
        async fn complete_json(
            &self,
            _request: PromptRequest,
            _options: ModelRequestOptions,
        ) -> Result<Value> {
            self.summaries
                .lock()
                .expect("scripted summaries lock")
                .pop_front()
                .ok_or_else(|| {
                    miette!(
                        "scripted {} worker ran out of compaction summaries",
                        self.role
                    )
                })
        }

        async fn complete_agent_turn(
            &self,
            request: AgentTurnRequest,
            _options: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            self.requests
                .lock()
                .expect("scripted requests lock")
                .push(request.clone());
            let invocation_input =
                request
                    .messages
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        AgentMessage::User { content } => {
                            serde_json::from_str(content.as_text()).ok()
                        }
                        _ => None,
                    });
            if let Some(input) = invocation_input {
                self.inputs
                    .lock()
                    .expect("scripted inputs lock")
                    .push(input);
            }
            let response = self
                .responses
                .lock()
                .expect("scripted responses lock")
                .pop_front()
                .ok_or_else(|| miette!("scripted {} worker ran out of responses", self.role))?;
            let (name, arguments) = match response {
                ScriptedWorkerResponse::Finish(arguments) => {
                    ("finish_and_send".to_string(), arguments)
                }
                ScriptedWorkerResponse::Tool { name, arguments } => (name, arguments),
                ScriptedWorkerResponse::Error(message) => return Err(miette!("{message}")),
                ScriptedWorkerResponse::ContextBudgetError => {
                    let budget = crate::context_budget::estimate_agent_turn_request(
                        &request.messages,
                        &request.tools,
                        self.budgets,
                    );
                    return Err(
                        crate::context_budget::ContextBudgetExceededError::for_request(
                            "scripted workflow worker request",
                            &self.model_name(),
                            &budget,
                            None,
                        )
                        .into(),
                    );
                }
            };
            Ok(AgentTurnStreamResult {
                items: vec![AgentTurnItem::ToolCall {
                    call: AgentToolCall {
                        id: format!("{}-{name}", self.role),
                        name,
                        arguments,
                    },
                }],
                raw_stream_follow_up: true,
                last_assistant_message: None,
                last_reasoning_content: None,
            })
        }

        fn request_budget_limits(&self) -> RequestBudgetLimits {
            self.budgets
        }

        fn token_usage_info(&self) -> TokenUsageInfo {
            TokenUsageInfo::default()
        }

        fn model_name(&self) -> String {
            format!("scripted-{}-worker", self.role)
        }
    }

    struct IsolatedWorkflowContext {
        context: Context,
        _home_override: DaatLocusHomeOverride,
        _home: tempfile::TempDir,
        _execution: tempfile::TempDir,
    }

    impl IsolatedWorkflowContext {
        async fn new(main: ScriptedWorkerProvider, efficient: ScriptedWorkerProvider) -> Self {
            let home = tempfile::tempdir().expect("test home");
            let execution = tempfile::tempdir().expect("test execution cwd");
            let home_override = DaatLocusHomeOverride::set(home.path().to_path_buf()).await;
            let telegram = TelegramTransportState::new();
            let (daemon_control_tx, _daemon_control_rx) = tokio::sync::mpsc::unbounded_channel();
            let context = Context {
                session_id: None,
                model_provider: Box::new(main),
                efficient_model_provider: Arc::new(efficient),
                config: Config::default(),
                memory: Memory::new().await,
                plan: Plan::new().await,
                events: EventStore::new().await,
                pending_work: PendingWorkQueue::new().await,
                openskills: OpenSkillsCatalog::default(),
                workflows: WorkflowCatalog::load(),
                workflow_cancellation: WorkflowCancellationRegistry::default(),
                active_skill_run: None,
                pending_skill_run_flushes: Vec::new(),
                current_work_origin: None,
                apps: AppManager::new(Vec::new()).expect("app manager"),
                workspace_apps: WorkspaceAppRegistry::default(),
                telegram: telegram.handle(),
                telegram_acl: TelegramAclHandle::load().await,
                compiled_prompts: CompiledPromptStore::from_entries(Vec::new()),
                execution_cwd: execution.path().to_path_buf(),
                coding_project_dir: None,
                sandbox_policy: RuntimeSandboxPolicy::disabled(),
                dashboard_tx: None,
                dashboard_history: None,
                daemon_control_tx,
                latest_context_composition: None,
                active_runtime_turn: false,
                active_runtime_phase: None,
                runtime_turn_started_at: None,
                runtime_turn_started_at_ms: None,
                runtime_turn_epoch: 0,
                runtime_overflow_failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                runtime_model_request_failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                live_progress_tx: Arc::new(parking_lot::Mutex::new(None)),
                telegram_live_drafts: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                claimed_event_ids: Vec::new(),
                afterclaim_context_fingerprint: None,
                delivered_root_instruction_fingerprint: None,
                visible_source_lines: std::collections::HashSet::new(),
                idle_since: None,
                last_idle_sleep_at: None,
                session_title: crate::runtime::session_title::SessionTitleState::default(),
                token_estimate_baseline: TokenEstimateBaseline::default(),
            };
            Self {
                context,
                _home_override: home_override,
                _home: home,
                _execution: execution,
            }
        }
    }

    #[derive(Clone)]
    struct AwaitAllTestProvider {
        started_tx: tokio::sync::mpsc::UnboundedSender<String>,
        release: Arc<tokio::sync::Notify>,
        fail_id: Option<String>,
        failure_released: Arc<AtomicBool>,
        finish_delay: std::time::Duration,
    }

    #[async_trait]
    impl ModelProvider for AwaitAllTestProvider {
        async fn complete_json(
            &self,
            _request: PromptRequest,
            _options: ModelRequestOptions,
        ) -> Result<Value> {
            Err(miette!("await_all test provider does not compact"))
        }

        async fn complete_agent_turn(
            &self,
            request: AgentTurnRequest,
            _options: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            let input = request
                .messages
                .iter()
                .find_map(|message| match message {
                    AgentMessage::User { content } => {
                        serde_json::from_str::<Value>(content.as_text()).ok()
                    }
                    _ => None,
                })
                .ok_or_else(|| miette!("await_all test worker input missing"))?;
            let id = input["id"]
                .as_str()
                .ok_or_else(|| miette!("await_all test worker id missing"))?
                .to_string();
            self.started_tx
                .send(id.clone())
                .map_err(|_| miette!("await_all test start receiver dropped"))?;
            if self.fail_id.as_deref() == Some(id.as_str()) {
                while !self.failure_released.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                return Err(miette!(
                    "http 400 bad request: await_all test worker {id} failed"
                ));
            }
            self.release.notified().await;
            if !self.finish_delay.is_zero() {
                tokio::time::sleep(self.finish_delay).await;
            }
            Ok(AgentTurnStreamResult {
                items: vec![AgentTurnItem::ToolCall {
                    call: AgentToolCall {
                        id: format!("finish-{id}"),
                        name: "finish_and_send".to_string(),
                        arguments: json!({ "id": id }),
                    },
                }],
                raw_stream_follow_up: true,
                last_assistant_message: None,
                last_reasoning_content: None,
            })
        }

        fn request_budget_limits(&self) -> RequestBudgetLimits {
            RequestBudgetLimits {
                context_window_tokens: 128_000,
                auto_compact_threshold_tokens: 128_000,
                reserved_output_tokens: 4_000,
            }
        }

        fn token_usage_info(&self) -> TokenUsageInfo {
            TokenUsageInfo::default()
        }

        fn model_name(&self) -> String {
            "await-all-test-worker".to_string()
        }
    }

    #[tokio::test]
    async fn await_all_rejects_concurrent_handles_for_the_same_actor() {
        let main = ScriptedWorkerProvider::new("main", Vec::new());
        let main_probe = main.clone();
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let source = r#"
local Schema = {
  type = "object",
  properties = { id = { type = "string" } },
  required = { "id" },
  additionalProperties = false,
}
local worker = workflow.agent({
  role = "parallel",
  model = "main",
  input = Schema,
  output = Schema,
  instruction = "Return the id.",
  extra_tools = {},
})
workflow.define({
  input = Schema,
  output = Schema,
  run = function(input, ctx)
    workflow.await_all({
      worker:run({ id = input.id .. "-first" }),
      worker:run({ id = input.id .. "-second" }),
    })
    return input
  end,
})
"#;
        let workflow_path = isolated._execution.path().join("same_actor_parallel.lua");
        fs::write(&workflow_path, source).expect("write same actor parallel workflow");
        let definition =
            load_workflow_definition(&workflow_path).expect("load same actor parallel workflow");
        let inspector =
            WorkflowInspectorPublisher::new(definition.id.clone(), json!({ "id": "job" }), None);

        let result = run_workflow_script(
            source,
            &definition,
            json!({ "id": "job" }),
            &isolated.context,
            None,
            &inspector,
        )
        .await
        .expect_err("await_all must reject concurrent handles for the same actor");

        assert!(
            result
                .to_string()
                .contains("workflow.await_all cannot run more than one handle for the same actor")
        );
        assert!(main_probe.requests().is_empty());
    }

    #[tokio::test]
    async fn await_all_starts_workers_concurrently_and_preserves_handle_order() {
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let main = ScriptedWorkerProvider::new("main", Vec::new());
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let mut isolated = IsolatedWorkflowContext::new(main, efficient).await;
        isolated.context.model_provider = Box::new(AwaitAllTestProvider {
            started_tx,
            release: Arc::clone(&release),
            fail_id: None,
            failure_released: Arc::new(AtomicBool::new(false)),
            finish_delay: std::time::Duration::from_millis(20),
        });
        let source = r#"
local Schema = {
  type = "object",
  properties = { id = { type = "string" } },
  required = { "id" },
  additionalProperties = false,
}
local worker_one = workflow.agent({
  role = "parallel",
  model = "main",
  input = Schema,
  output = Schema,
  instruction = "Return the id.",
  extra_tools = {},
})
local worker_two = workflow.agent({
  role = "parallel",
  model = "main",
  input = Schema,
  output = Schema,
  instruction = "Return the id.",
  extra_tools = {},
})
workflow.define({
  input = Schema,
  output = {
    type = "object",
    properties = {
      ids = { type = "array", items = { type = "string" } },
    },
    required = { "ids" },
    additionalProperties = false,
  },
  run = function(input, ctx)
    local outputs = workflow.await_all({
      worker_one:run({ id = input.id .. "-first" }),
      worker_two:run({ id = input.id .. "-second" }),
    })
    return { ids = { outputs[1].id, outputs[2].id } }
  end,
})
"#;
        let workflow_path = isolated._execution.path().join("parallel_order.lua");
        fs::write(&workflow_path, source).expect("write await_all test workflow");
        let definition = load_workflow_definition(&workflow_path).expect("load await_all workflow");
        let inspector =
            WorkflowInspectorPublisher::new(definition.id.clone(), json!({ "id": "job" }), None);
        let invocation = run_workflow_script(
            source,
            &definition,
            json!({ "id": "job" }),
            &isolated.context,
            None,
            &inspector,
        );
        tokio::pin!(invocation);

        let first_started = tokio::select! {
            result = &mut invocation => panic!("await_all completed before both workers were released: {result:?}"),
            started = started_rx.recv() => started.expect("first worker should start"),
        };
        let second_started =
            tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
                .await
                .expect("second worker should start while the first is blocked")
                .expect("second worker start signal");
        assert_ne!(first_started, second_started);

        release.notify_waiters();
        let output = invocation
            .await
            .expect("concurrent await_all should complete");
        assert_eq!(output, json!({ "ids": ["job-first", "job-second"] }));
        let snapshot = inspector.snapshot();
        assert_eq!(snapshot.await_groups.len(), 1);
        assert_eq!(
            snapshot.await_groups[0].status,
            WorkflowNodeStatus::Completed
        );
        assert_eq!(snapshot.workers.len(), 2);
        assert_ne!(snapshot.workers[0].actor_id, snapshot.workers[1].actor_id);
        assert!(
            snapshot
                .workers
                .iter()
                .all(|worker| worker.status == WorkflowNodeStatus::Completed)
        );
        assert!(
            snapshot
                .workers
                .iter()
                .all(|worker| worker.agent_run_time_ms > 0)
        );
    }
    #[tokio::test]
    async fn await_all_propagates_failure_and_interrupts_siblings() {
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let failure_released = Arc::new(AtomicBool::new(false));
        let main = ScriptedWorkerProvider::new("main", Vec::new());
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let mut isolated = IsolatedWorkflowContext::new(main, efficient).await;
        isolated.context.model_provider = Box::new(AwaitAllTestProvider {
            started_tx,
            release,
            fail_id: Some("job-first".to_string()),
            failure_released: Arc::clone(&failure_released),
            finish_delay: std::time::Duration::ZERO,
        });
        let source = r#"
local Schema = {
  type = "object",
  properties = { id = { type = "string" } },
  required = { "id" },
  additionalProperties = false,
}
local worker_one = workflow.agent({
  role = "parallel-one",
  model = "main",
  input = Schema,
  output = Schema,
  instruction = "Return the id.",
  extra_tools = {},
})
local worker_two = workflow.agent({
  role = "parallel-two",
  model = "main",
  input = Schema,
  output = Schema,
  instruction = "Return the id.",
  extra_tools = {},
})
workflow.define({
  input = Schema,
  output = Schema,
  run = function(input, ctx)
    workflow.await_all({
      worker_one:run({ id = input.id .. "-first" }),
      worker_two:run({ id = input.id .. "-second" }),
    })
    return input
  end,
})
"#;
        let workflow_path = isolated._execution.path().join("parallel_failure.lua");
        fs::write(&workflow_path, source).expect("write await_all failure workflow");
        let definition =
            load_workflow_definition(&workflow_path).expect("load await_all failure workflow");
        let inspector =
            WorkflowInspectorPublisher::new(definition.id.clone(), json!({ "id": "job" }), None);
        let invocation = run_workflow_script(
            source,
            &definition,
            json!({ "id": "job" }),
            &isolated.context,
            None,
            &inspector,
        );
        tokio::pin!(invocation);
        let first_started = tokio::select! {
            result = &mut invocation => panic!("await_all failed before both workers started: {result:?}"),
            started = started_rx.recv() => started.expect("first worker start signal"),
        };
        let second_started =
            tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
                .await
                .expect("second worker should start before failure is released")
                .expect("second worker start signal");
        failure_released.store(true, Ordering::SeqCst);
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut invocation)
            .await
            .expect("failed worker should cancel its blocked sibling promptly")
            .expect_err("await_all should propagate the worker failure");

        assert!(result.to_string().contains("job-first failed"));
        let mut started = vec![first_started, second_started];
        started.sort();
        assert_eq!(started, vec!["job-first", "job-second"]);
        let snapshot = inspector.snapshot();
        assert_eq!(
            snapshot.await_groups[0].status,
            WorkflowNodeStatus::Interrupted
        );
        assert!(snapshot.workers.iter().any(|worker| {
            worker.status == WorkflowNodeStatus::Failed
                && worker
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("job-first failed"))
        }));
        assert!(
            snapshot
                .workers
                .iter()
                .any(|worker| worker.status == WorkflowNodeStatus::Interrupted)
        );
    }

    #[tokio::test]
    async fn await_all_propagates_external_interruption() {
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let main = ScriptedWorkerProvider::new("main", Vec::new());
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let mut isolated = IsolatedWorkflowContext::new(main, efficient).await;
        isolated.context.model_provider = Box::new(AwaitAllTestProvider {
            started_tx,
            release,
            fail_id: None,
            failure_released: Arc::new(AtomicBool::new(false)),
            finish_delay: std::time::Duration::ZERO,
        });
        let source = r#"
local Schema = {
  type = "object",
  properties = { id = { type = "string" } },
  required = { "id" },
  additionalProperties = false,
}
local worker_one = workflow.agent({
  role = "parallel-one",
  model = "main",
  input = Schema,
  output = Schema,
  instruction = "Return the id.",
  extra_tools = {},
})
local worker_two = workflow.agent({
  role = "parallel-two",
  model = "main",
  input = Schema,
  output = Schema,
  instruction = "Return the id.",
  extra_tools = {},
})
workflow.define({
  input = Schema,
  output = Schema,
  run = function(input, ctx)
    workflow.await_all({
      worker_one:run({ id = input.id .. "-first" }),
      worker_two:run({ id = input.id .. "-second" }),
    })
    return input
  end,
})
"#;
        let workflow_path = isolated._execution.path().join("parallel_interrupt.lua");
        fs::write(&workflow_path, source).expect("write await_all interrupt workflow");
        let definition =
            load_workflow_definition(&workflow_path).expect("load await_all interrupt workflow");
        let inspector =
            WorkflowInspectorPublisher::new(definition.id.clone(), json!({ "id": "job" }), None);
        let cancellation = WorkflowCancellation::new();
        let invocation = run_workflow_script(
            source,
            &definition,
            json!({ "id": "job" }),
            &isolated.context,
            Some(&cancellation),
            &inspector,
        );
        tokio::pin!(invocation);

        tokio::select! {
            result = &mut invocation => panic!("await_all completed before interruption: {result:?}"),
            started = started_rx.recv() => started.expect("first worker start signal"),
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
            .await
            .expect("second worker should start before interruption")
            .expect("second worker start signal");
        cancellation.interrupt();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut invocation)
            .await
            .expect("external interruption should stop all workers promptly")
            .expect_err("await_all should propagate external interruption");

        assert!(is_workflow_interrupted_error(&result));
        let snapshot = inspector.snapshot();
        assert_eq!(
            snapshot.await_groups[0].status,
            WorkflowNodeStatus::Interrupted
        );
        assert!(
            snapshot
                .workers
                .iter()
                .all(|worker| worker.status == WorkflowNodeStatus::Interrupted)
        );
    }

    #[tokio::test]
    async fn goal_workflow_retries_with_verifier_feedback_until_achieved() {
        let main = ScriptedWorkerProvider::new(
            "main",
            vec![
                json!({ "summary": "first attempt", "evidence": ["worker-one"] }),
                json!({ "summary": "second attempt", "evidence": ["worker-two"] }),
            ],
        );
        let efficient = ScriptedWorkerProvider::new(
            "efficient",
            vec![
                json!({
                    "achieved": false,
                    "summary": "not complete",
                    "findings": [{
                        "requirement": "the requested artifact exists",
                        "observed": "the artifact is absent",
                        "evidence": "checked workspace/artifact.txt: not found",
                        "required_fix": "create workspace/artifact.txt",
                        "recheck": "read workspace/artifact.txt"
                    }],
                    "evidence": ["verifier-one"]
                }),
                json!({
                    "achieved": true,
                    "summary": "goal verified",
                    "findings": [],
                    "evidence": ["verifier-two"]
                }),
            ],
        );
        let main_probe = main.clone();
        let efficient_probe = efficient.clone();

        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let result = invoke(
            &isolated.context,
            WorkflowInvocation {
                workflow_id: "goal".to_string(),
                input: json!({ "goal": "complete the requested implementation" }),
            },
        )
        .await
        .expect("invoke goal workflow");
        drop(isolated);

        assert_eq!(
            result.status,
            WorkflowInvocationStatus::Completed,
            "{}",
            result.message
        );
        assert_eq!(
            result.output,
            Some(json!({
                "achieved": true,
                "attempts": 2,
                "summary": "second attempt",
                "verification": "goal verified",
                "remaining_work": "",
                "evidence": ["worker-two", "verifier-two"]
            }))
        );
        assert_eq!(
            result
                .snapshot
                .workers
                .iter()
                .map(|worker| worker.role.as_str())
                .collect::<Vec<_>>(),
            vec![
                "implementation",
                "verification",
                "implementation",
                "verification"
            ]
        );
        assert_eq!(
            result
                .snapshot
                .transitions
                .iter()
                .map(|transition| transition.kind)
                .collect::<Vec<_>>(),
            vec![
                WorkflowTransitionKind::Verify,
                WorkflowTransitionKind::Revision,
                WorkflowTransitionKind::Verify,
            ]
        );
        let worker_inputs = main_probe.inputs();
        assert_eq!(worker_inputs.len(), 2);
        assert_eq!(
            worker_inputs[0],
            json!({
                "goal": "complete the requested implementation",
                "attempt": 1,
                "verifier_feedback": "",
            })
        );
        assert_eq!(
            worker_inputs[1],
            json!({
                "goal": "complete the requested implementation",
                "attempt": 2,
                "verifier_feedback": "Requirement: the requested artifact exists\nObserved: the artifact is absent\nEvidence: checked workspace/artifact.txt: not found\nRequired fix: create workspace/artifact.txt\nRecheck: read workspace/artifact.txt\n\n",
            })
        );
        let verifier_inputs = efficient_probe.inputs();
        assert_eq!(verifier_inputs.len(), 2);
        assert_eq!(verifier_inputs[0]["worker_summary"], "first attempt");
        assert_eq!(verifier_inputs[1]["worker_summary"], "second attempt");
    }

    #[tokio::test]
    async fn goal_workflow_fails_when_verifier_rejects_without_blocking_findings() {
        let main = ScriptedWorkerProvider::new(
            "main",
            vec![json!({ "summary": "attempted work", "evidence": ["worker-one"] })],
        );
        let main_probe = main.clone();
        let efficient = ScriptedWorkerProvider::new(
            "efficient",
            vec![json!({
                "achieved": false,
                "summary": "verification is incomplete",
                "findings": [],
                "evidence": ["verifier-one"]
            })],
        );

        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let result = invoke(
            &isolated.context,
            WorkflowInvocation {
                workflow_id: "goal".to_string(),
                input: json!({ "goal": "complete the requested implementation" }),
            },
        )
        .await
        .expect("invoke goal workflow");
        drop(isolated);

        assert_eq!(result.status, WorkflowInvocationStatus::Failed);
        assert!(
            result
                .message
                .contains("verifier rejected the work without direct blocking findings"),
            "{}",
            result.message
        );
        assert_eq!(
            main_probe.inputs(),
            vec![json!({
                "goal": "complete the requested implementation",
                "attempt": 1,
                "verifier_feedback": "",
            })]
        );
    }

    #[tokio::test]
    async fn goal_workflow_fails_when_verifier_finding_is_incomplete() {
        let main = ScriptedWorkerProvider::new(
            "main",
            vec![json!({ "summary": "attempted work", "evidence": ["worker-one"] })],
        );
        let main_probe = main.clone();
        let efficient = ScriptedWorkerProvider::new(
            "efficient",
            vec![json!({
                "achieved": false,
                "summary": "verification found an issue",
                "findings": [{
                    "requirement": "the requested artifact exists",
                    "observed": "the artifact is absent",
                    "evidence": "checked workspace/artifact.txt: not found",
                    "required_fix": "",
                    "recheck": "read workspace/artifact.txt"
                }],
                "evidence": ["verifier-one"]
            })],
        );

        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let result = invoke(
            &isolated.context,
            WorkflowInvocation {
                workflow_id: "goal".to_string(),
                input: json!({ "goal": "complete the requested implementation" }),
            },
        )
        .await
        .expect("invoke goal workflow");
        drop(isolated);

        assert_eq!(result.status, WorkflowInvocationStatus::Failed);
        assert!(
            result
                .message
                .contains("verifier rejected the work with incomplete blocking findings"),
            "{}",
            result.message
        );
        assert_eq!(
            main_probe.inputs(),
            vec![json!({
                "goal": "complete the requested implementation",
                "attempt": 1,
                "verifier_feedback": "",
            })]
        );
    }

    #[tokio::test]
    async fn goal_workflow_continues_until_verifier_achieves_goal() {
        let main = ScriptedWorkerProvider::new(
            "main",
            (1..=4)
                .map(|attempt| {
                    json!({
                        "summary": format!("attempt {attempt}"),
                        "evidence": [format!("worker-{attempt}")]
                    })
                })
                .collect(),
        );
        let efficient = ScriptedWorkerProvider::new(
            "efficient",
            (1..=4)
                .map(|attempt| {
                    json!({
                        "achieved": attempt == 4,
                        "summary": format!("verification {attempt}"),
                        "findings": if attempt == 4 {
                            json!([])
                        } else {
                            json!([{
                                "requirement": format!("requirement {attempt}"),
                                "observed": format!("requirement {attempt} is unmet"),
                                "evidence": format!("checked artifact-{attempt}: missing"),
                                "required_fix": format!("implement requirement {attempt}"),
                                "recheck": format!("check artifact-{attempt}")
                            }])
                        },
                        "evidence": [format!("verifier-{attempt}")]
                    })
                })
                .collect(),
        );
        let main_probe = main.clone();
        let efficient_probe = efficient.clone();

        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let result = invoke(
            &isolated.context,
            WorkflowInvocation {
                workflow_id: "goal".to_string(),
                input: json!({ "goal": "complete all required work" }),
            },
        )
        .await
        .expect("invoke goal workflow");
        drop(isolated);

        assert_eq!(
            result.status,
            WorkflowInvocationStatus::Completed,
            "{}",
            result.message
        );
        assert_eq!(
            result.output,
            Some(json!({
                "achieved": true,
                "attempts": 4,
                "summary": "attempt 4",
                "verification": "verification 4",
                "remaining_work": "",
                "evidence": ["worker-4", "verifier-4"]
            }))
        );
        let worker_inputs = main_probe.inputs();
        assert_eq!(worker_inputs.len(), 4);
        assert_eq!(
            worker_inputs[3],
            json!({
                "goal": "complete all required work",
                "attempt": 4,
                "verifier_feedback": "Requirement: requirement 3\nObserved: requirement 3 is unmet\nEvidence: checked artifact-3: missing\nRequired fix: implement requirement 3\nRecheck: check artifact-3\n\n",
            })
        );
        assert_eq!(efficient_probe.inputs().len(), 4);
    }

    #[tokio::test]
    async fn search_workflow_replans_concurrent_searchers_until_verification_succeeds() {
        let main = ScriptedWorkerProvider::new(
            "main",
            vec![
                json!({
                    "routes": [
                        { "query": "first route one", "purpose": "find first evidence" },
                        { "query": "first route two", "purpose": "find second evidence" }
                    ]
                }),
                json!({
                    "achieved": false,
                    "verification": "more evidence is required",
                    "remaining_goal": "find the missing second source",
                    "verified_evidence": "first evidence verified"
                }),
                json!({
                    "routes": [
                        { "query": "second route", "purpose": "find the missing source" }
                    ]
                }),
                json!({
                    "achieved": true,
                    "verification": "the goal is verified",
                    "remaining_goal": "",
                    "verified_evidence": "second evidence verified"
                }),
                json!({
                    "analysis": "final analysis",
                    "sources": ["https://example.test/one", "https://example.test/two"]
                }),
            ],
        );
        let efficient = ScriptedWorkerProvider::new(
            "efficient",
            vec![
                json!({
                    "query": "first route one",
                    "purpose": "find first evidence",
                    "status": "completed",
                    "findings": "first finding",
                    "sources": "https://example.test/one"
                }),
                json!({
                    "query": "first route two",
                    "purpose": "find second evidence",
                    "status": "no_results",
                    "findings": "no second finding yet",
                    "sources": ""
                }),
                json!({
                    "query": "second route",
                    "purpose": "find the missing source",
                    "status": "completed",
                    "findings": "second finding",
                    "sources": "https://example.test/two"
                }),
            ],
        );
        let main_probe = main.clone();
        let efficient_probe = efficient.clone();
        let isolated = IsolatedWorkflowContext::new(main, efficient).await;

        let result = invoke(
            &isolated.context,
            WorkflowInvocation {
                workflow_id: "search".to_string(),
                input: json!({ "goal": "find two independent sources" }),
            },
        )
        .await
        .expect("invoke search workflow");
        drop(isolated);

        assert_eq!(
            result.status,
            WorkflowInvocationStatus::Completed,
            "{}",
            result.message
        );
        assert_eq!(
            result.output,
            Some(json!({
                "analysis": "final analysis",
                "sources": ["https://example.test/one", "https://example.test/two"],
                "verification": "the goal is verified",
                "rounds": 2
            }))
        );
        assert_eq!(
            result
                .snapshot
                .workers
                .iter()
                .map(|worker| worker.role.as_str())
                .collect::<Vec<_>>(),
            vec![
                "planning",
                "search",
                "search",
                "verification",
                "planning",
                "search",
                "verification",
                "analysis",
            ]
        );
        assert_eq!(
            result
                .snapshot
                .transitions
                .iter()
                .map(|transition| transition.kind)
                .collect::<Vec<_>>(),
            vec![
                WorkflowTransitionKind::Await,
                WorkflowTransitionKind::Await,
                WorkflowTransitionKind::Verify,
                WorkflowTransitionKind::Verify,
                WorkflowTransitionKind::Revision,
                WorkflowTransitionKind::Await,
                WorkflowTransitionKind::Verify,
                WorkflowTransitionKind::Await,
            ]
        );

        let main_inputs = main_probe.inputs();
        assert_eq!(main_inputs.len(), 5);
        assert_eq!(
            main_inputs[0]["remaining_goal"],
            "find two independent sources"
        );
        assert_eq!(main_inputs[0]["previous_verification"], "");
        assert_eq!(
            main_inputs[2]["remaining_goal"],
            "find the missing second source"
        );
        assert_eq!(
            main_inputs[2]["previous_verification"],
            "more evidence is required"
        );
        assert_eq!(main_inputs[2]["prior_evidence"], "first evidence verified");
        assert_eq!(
            main_inputs[3]["search_results"].as_array().map(Vec::len),
            Some(3)
        );
        assert_eq!(
            main_inputs[4]["verified_evidence"],
            "first evidence verified\n\nsecond evidence verified"
        );
        assert_eq!(
            main_inputs[4]["search_results"].as_array().map(Vec::len),
            Some(3)
        );

        let search_inputs = efficient_probe.inputs();
        assert_eq!(search_inputs.len(), 3);
        assert_eq!(search_inputs[0]["query"], "first route one");
        assert_eq!(search_inputs[1]["query"], "first route two");
        assert_eq!(search_inputs[2]["query"], "second route");
    }

    #[test]
    fn reset_rejects_a_running_actor() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let main = ScriptedWorkerProvider::new("main", Vec::new());
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let isolated = runtime.block_on(IsolatedWorkflowContext::new(main, efficient));
        let definition = WorkerDefinition {
            role: "reset-race".to_string(),
            model: WorkerModel::Main,
            input_schema: json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false,
            }),
            output_schema: json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false,
            }),
            instruction: "Return the id.".to_string(),
            extra_tools: Vec::new(),
        };
        let local_tools = Arc::new(Mutex::new(BTreeMap::new()));
        let mut actor = WorkflowWorkerActor::new(
            &WorkerActorFactoryContext::from_context(&isolated.context),
            definition,
            &local_tools,
        )
        .expect("create reset race actor");
        actor.running.store(true, Ordering::SeqCst);

        let error = actor
            .reset(&WorkerActorFactoryContext::from_context(&isolated.context))
            .expect_err("reset must reject a running actor");
        assert!(
            error
                .to_string()
                .contains("workflow actor cannot reset while one of its handles is running")
        );
    }

    #[tokio::test]
    async fn workflow_actor_persists_state_until_explicit_reset_and_isolates_actors_and_invocations()
     {
        let main = ScriptedWorkerProvider::new(
            "main",
            vec![
                json!({ "value": "first-a" }),
                json!({ "value": "first-b" }),
                json!({ "value": "second-a" }),
                json!({ "value": "first-reset" }),
                json!({ "value": "first-after-reset" }),
            ],
        );
        let main_probe = main.clone();
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let mut isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let marker_path = isolated._execution.path().join("actor-state-marker.txt");
        let marker_path_lua = marker_path.to_string_lossy().replace('\\', "\\\\");
        let source = format!(
            r#"
local Schema = {{
  type = "object",
  properties = {{ value = {{ type = "string" }} }},
  required = {{ "value" }},
  additionalProperties = false,
}}
local first = workflow.agent({{
  role = "first", model = "main", input = Schema, output = Schema,
  instruction = "Return the value.", extra_tools = {{}},
}})
local second = workflow.agent({{
  role = "second", model = "main", input = Schema, output = Schema,
  instruction = "Return the value.", extra_tools = {{}},
}})
workflow.define({{
  input = Schema,
  output = {{
    type = "object",
    properties = {{
      first = {{ type = "string" }}, second = {{ type = "string" }},
      after_reset = {{ type = "string" }},
    }},
    required = {{ "first", "second", "after_reset" }},
    additionalProperties = false,
  }},
  run = function(input, ctx)
    workflow.await(first:run({{ value = "first-a" }}))
    workflow.await(first:run({{ value = "first-b" }}))
    workflow.await(second:run({{ value = "second-a" }}))
    workflow.await(first:reset())
    workflow.await(first:run({{ value = "first-reset" }}))
    local marker = io.open("{marker_path_lua}", "w")
    marker:write("persisted")
    marker:close()
    local after_reset = workflow.await(first:run({{ value = "first-after-reset" }}))
    return {{ first = "first", second = "second", after_reset = after_reset.value }}
  end,
}})
"#
        );
        let workflow_path = isolated._execution.path().join("actor_state.lua");
        fs::write(&workflow_path, &source).expect("write actor state workflow");
        let definition =
            load_workflow_definition(&workflow_path).expect("load actor state workflow");
        let inspector = WorkflowInspectorPublisher::new(
            definition.id.clone(),
            json!({ "value": "input" }),
            None,
        );
        let output = run_workflow_script(
            &source,
            &definition,
            json!({ "value": "input" }),
            &isolated.context,
            None,
            &inspector,
        )
        .await
        .expect("run actor state workflow");
        assert_eq!(output["after_reset"], "first-after-reset");
        assert_eq!(
            fs::read_to_string(&marker_path).expect("read persisted actor state marker"),
            "persisted"
        );

        let requests = main_probe.requests();
        assert_eq!(requests.len(), 5);
        assert_eq!(
            user_json_values(&requests[0]),
            vec![json!({ "value": "first-a" })]
        );
        assert_eq!(
            user_json_values(&requests[1]),
            vec![json!({ "value": "first-a" }), json!({ "value": "first-b" })]
        );
        assert_eq!(
            user_json_values(&requests[2]),
            vec![json!({ "value": "second-a" })]
        );
        assert_eq!(
            user_json_values(&requests[3]),
            vec![json!({ "value": "first-reset" })]
        );
        assert_eq!(
            user_json_values(&requests[4]),
            vec![
                json!({ "value": "first-reset" }),
                json!({ "value": "first-after-reset" })
            ]
        );

        let next_main = ScriptedWorkerProvider::new(
            "main",
            vec![
                json!({ "value": "first-a" }),
                json!({ "value": "first-b" }),
                json!({ "value": "second-a" }),
                json!({ "value": "first-reset" }),
                json!({ "value": "first-after-reset" }),
            ],
        );
        let next_probe = next_main.clone();
        isolated.context.model_provider = Box::new(next_main);
        let next_inspector = WorkflowInspectorPublisher::new(
            definition.id.clone(),
            json!({ "value": "input" }),
            None,
        );
        run_workflow_script(
            &source,
            &definition,
            json!({ "value": "input" }),
            &isolated.context,
            None,
            &next_inspector,
        )
        .await
        .expect("run separate workflow invocation");
        assert_eq!(
            user_json_values(&next_probe.requests()[0]),
            vec![json!({ "value": "first-a" })]
        );
        let snapshot = inspector.snapshot();
        assert_eq!(snapshot.workers.len(), 5);
        let first_actor_runs = snapshot
            .workers
            .iter()
            .filter(|worker| worker.role == "first")
            .collect::<Vec<_>>();
        let second_actor_runs = snapshot
            .workers
            .iter()
            .filter(|worker| worker.role == "second")
            .collect::<Vec<_>>();
        assert_eq!(first_actor_runs.len(), 4);
        assert_eq!(second_actor_runs.len(), 1);
        assert!(
            first_actor_runs
                .iter()
                .all(|worker| worker.actor_id == first_actor_runs[0].actor_id)
        );
        assert_ne!(first_actor_runs[0].actor_id, second_actor_runs[0].actor_id);
        let reset_worker = first_actor_runs
            .iter()
            .find(|worker| worker.input["value"] == "first-reset")
            .expect("first reset worker run");
        let after_reset_worker = first_actor_runs
            .iter()
            .find(|worker| worker.input["value"] == "first-after-reset")
            .expect("first post-reset worker run");
        assert_eq!(reset_worker.actor_id, after_reset_worker.actor_id);
    }

    #[tokio::test]
    async fn workflow_worker_compacts_before_request_and_preserves_actor_state() {
        let main = ScriptedWorkerProvider::with_script(
            "main",
            vec![ScriptedWorkerResponse::finish(json!({ "value": "second" }))],
            vec![json!({ "summary": "worker context retained" })],
            RequestBudgetLimits {
                context_window_tokens: 32_000,
                auto_compact_threshold_tokens: 30_000,
                reserved_output_tokens: 64,
            },
        );
        let main_probe = main.clone();
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let definition = WorkerDefinition {
            role: "compact".to_string(),
            model: WorkerModel::Main,
            input_schema: json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false,
            }),
            output_schema: json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false,
            }),
            instruction: "Return the value.".to_string(),
            extra_tools: Vec::new(),
        };
        let local_tools = Arc::new(Mutex::new(BTreeMap::new()));
        let actor = Arc::new(tokio::sync::Mutex::new(
            WorkflowWorkerActor::new(
                &WorkerActorFactoryContext::from_context(&isolated.context),
                definition,
                &local_tools,
            )
            .expect("create compacting workflow worker actor"),
        ));
        let inspector =
            WorkflowInspectorPublisher::new("worker-compaction-test".to_string(), json!({}), None);

        {
            let mut actor = actor.lock().await;
            actor
                .conversation
                .push_agent_message(AgentMessage::user("x".repeat(128_000)));
            assert!(
                actor
                    .runtime
                    .worker_plan
                    .replace(vec![crate::plan::PlanStep {
                        step: "retain worker-local plan".to_string(),
                        status: crate::plan::PlanStatus::InProgress,
                        created_at_ms: 0,
                        last_updated_at_ms: 0,
                    }])
            );
        }

        let second = run_worker(
            &isolated.context,
            actor.clone(),
            json!({ "value": "second" }),
            &local_tools,
            None,
            &inspector,
            "worker-1",
        )
        .await
        .expect("worker should compact and complete");
        assert_eq!(second.output, json!({ "value": "second" }));

        let requests = main_probe.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(user_json_values(&requests[0]), Vec::<Value>::new());
        assert!(requests[0].messages.iter().any(|message| matches!(
            message,
            AgentMessage::Assistant { content } if content.contains("worker context retained")
        )));
        let actor = actor.lock().await;
        assert_eq!(actor.runtime.worker_plan.steps().len(), 1);
        assert_eq!(
            actor.runtime.worker_plan.steps()[0].step,
            "retain worker-local plan"
        );
        assert!(actor
            .conversation
            .agent_messages()
            .iter()
            .any(|message| matches!(message, AgentMessage::Assistant { content } if content.contains("worker context retained"))));
    }

    #[tokio::test]
    async fn workflow_worker_recovers_context_budget_error_with_shared_compaction() {
        let main = ScriptedWorkerProvider::with_script(
            "main",
            vec![
                ScriptedWorkerResponse::ContextBudgetError,
                ScriptedWorkerResponse::finish(json!({ "value": "done" })),
            ],
            vec![json!({ "summary": "overflow-recovered worker context" })],
            RequestBudgetLimits {
                context_window_tokens: 32_000,
                auto_compact_threshold_tokens: 30_000,
                reserved_output_tokens: 64,
            },
        );
        let main_probe = main.clone();
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let definition = WorkerDefinition {
            role: "overflow".to_string(),
            model: WorkerModel::Main,
            input_schema: json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false,
            }),
            output_schema: json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false,
            }),
            instruction: "Return the value.".to_string(),
            extra_tools: Vec::new(),
        };
        let local_tools = Arc::new(Mutex::new(BTreeMap::new()));
        let actor = Arc::new(tokio::sync::Mutex::new(
            WorkflowWorkerActor::new(
                &WorkerActorFactoryContext::from_context(&isolated.context),
                definition,
                &local_tools,
            )
            .expect("create overflow workflow worker actor"),
        ));
        let inspector =
            WorkflowInspectorPublisher::new("worker-overflow-test".to_string(), json!({}), None);

        let result = run_worker(
            &isolated.context,
            actor,
            json!({ "value": "done" }),
            &local_tools,
            None,
            &inspector,
            "worker-1",
        )
        .await
        .expect("worker should recover from context budget error");
        assert_eq!(result.output, json!({ "value": "done" }));

        let requests = main_probe.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            user_json_values(&requests[0]),
            vec![json!({ "value": "done" })]
        );
        assert_eq!(user_json_values(&requests[1]), Vec::<Value>::new());
        assert!(requests[1].messages.iter().any(|message| matches!(
            message,
            AgentMessage::Assistant { content } if content.contains("overflow-recovered worker context")
        )));
    }

    fn user_json_values(request: &AgentTurnRequest) -> Vec<Value> {
        request
            .messages
            .iter()
            .filter_map(|message| match message {
                AgentMessage::User { content } => serde_json::from_str(content.as_text()).ok(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn worker_result_messages_preserve_model_content_and_image_parts() {
        let mut conversation = RuntimeStepConversation::new(Vec::new());
        let mut worker_runtime = WorkerRuntimeState::new();
        let call = AgentToolCall {
            id: "worker-image".to_string(),
            name: "view_image".to_string(),
            arguments: json!({}),
        };
        let result = ToolExecutionResult::from_activity_event("image", json!({}), None)
            .with_model_content("attached image")
            .with_model_image_part(crate::reasoning::runtime::AgentContentPart::Image {
                path: "worker-image.png".to_string(),
                media_type: "image/png".to_string(),
                description: Some("worker fixture".to_string()),
            });

        append_worker_tool_result_message(&mut conversation, call, Ok(result), &mut worker_runtime);
        let messages = conversation.agent_messages();

        assert!(matches!(
            &messages[0],
            AgentMessage::Tool { content, .. } if content == "attached image"
        ));
        assert!(matches!(
            &messages[1],
            AgentMessage::User { content }
                if matches!(content.parts(), [crate::reasoning::runtime::AgentContentPart::Image { media_type, .. }]
                    if media_type == "image/png")
        ));
    }

    async fn run_worker_with_inspector(
        isolated: &IsolatedWorkflowContext,
        definition: WorkerDefinition,
        input: Value,
    ) -> Result<WorkflowRunSnapshot> {
        let inspector =
            WorkflowInspectorPublisher::new("worker-activity-test".to_string(), json!({}), None);
        let (group_id, worker_ids) = inspector.begin_group(1, WorkflowTransitionKind::Await);
        let worker_id = worker_ids.first().expect("worker id").clone();
        inspector.begin_worker(
            &group_id,
            worker_id.clone(),
            workflow_inspector_actor_id(&definition),
            &definition,
            input.clone(),
        );
        let local_tools = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let actor = Arc::new(tokio::sync::Mutex::new(WorkflowWorkerActor::new(
            &WorkerActorFactoryContext::from_context(&isolated.context),
            definition,
            &local_tools,
        )?));
        let result = run_worker_with_timing(
            &isolated.context,
            actor,
            input,
            &local_tools,
            None,
            &inspector,
            &worker_id,
        )
        .await;
        inspector.finish_group(&group_id);
        result?;
        Ok(inspector.snapshot())
    }

    #[tokio::test]
    async fn workflow_worker_activity_starts_with_external_input() {
        let main = ScriptedWorkerProvider::new("main", vec![json!({})]);
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());
        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let definition = WorkerDefinition {
            role: "worker".to_string(),
            model: WorkerModel::Main,
            input_schema: worker_schema(),
            output_schema: worker_schema(),
            instruction: "Return the required output.".to_string(),
            extra_tools: Vec::new(),
        };
        let snapshot = run_worker_with_inspector(&isolated, definition, json!({}))
            .await
            .expect("worker should complete");
        drop(isolated);

        assert!(matches!(
            snapshot.workers[0].activity.first(),
            Some(crate::dashboard::SessionActivityEvent::User(input)) if input.content == "{}"
        ));
    }

    #[tokio::test]
    async fn workflow_worker_records_only_semantic_coding_open_project_activity() {
        let main = ScriptedWorkerProvider::with_responses(
            "main",
            vec![
                ScriptedWorkerResponse::tool(
                    "coding__open_project",
                    json!({ "project_root": "." }),
                ),
                ScriptedWorkerResponse::finish(json!({})),
            ],
        );
        let efficient = ScriptedWorkerProvider::new("efficient", Vec::new());

        let isolated = IsolatedWorkflowContext::new(main, efficient).await;
        let definition = WorkerDefinition {
            role: "worker".to_string(),
            model: WorkerModel::Main,
            input_schema: worker_schema(),
            output_schema: worker_schema(),
            instruction: "Open the coding project, then finish.".to_string(),
            extra_tools: Vec::new(),
        };
        let snapshot = run_worker_with_inspector(&isolated, definition, json!({}))
            .await
            .expect("worker should complete");
        drop(isolated);

        let activity = &snapshot.workers[0].activity;
        assert!(activity.iter().any(|event| matches!(
            event,
            crate::dashboard::SessionActivityEvent::CodingOpenProject(_)
        )));
        assert!(!activity.iter().any(|event| matches!(
            event,
            crate::dashboard::SessionActivityEvent::GenericApp(event)
                if event.title == "open_project"
        )));
    }

    fn worker_schema() -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        })
    }

    fn worker_table(lua: &Lua) -> Table {
        let table = lua.create_table().expect("create worker table");
        let schema = lua
            .to_value(&worker_schema())
            .expect("serialize worker schema");
        table.set("role", "worker").expect("set worker role");
        table
            .set("input", schema.clone())
            .expect("set worker input");
        table.set("output", schema).expect("set worker output");
        table
            .set("instruction", "Return the required output.")
            .expect("set worker instruction");
        table
            .set(
                "extra_tools",
                lua.create_table().expect("create extra_tools table"),
            )
            .expect("set worker extra_tools");
        table
    }

    #[test]
    fn worker_definition_requires_a_non_empty_role() {
        let lua = new_lua().expect("create Lua runtime");
        let missing_role = worker_table(&lua);
        missing_role
            .set("role", LuaValue::Nil)
            .expect("remove worker role");
        let missing_role_error = worker_definition_from_lua(&lua, &missing_role)
            .expect_err("worker without a role should be rejected");
        assert!(
            missing_role_error
                .to_string()
                .contains("requires a non-empty `role`")
        );

        let empty_role = worker_table(&lua);
        empty_role.set("role", "  ").expect("set empty worker role");
        let empty_role_error = worker_definition_from_lua(&lua, &empty_role)
            .expect_err("worker with an empty role should be rejected");
        assert!(
            empty_role_error
                .to_string()
                .contains("role must not be empty")
        );
    }

    #[test]
    fn workflow_load_rejects_agents_without_roles() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("missing_role.lua");
        fs::write(
            &path,
            r#"
workflow.agent({
  input = { type = "object", properties = {}, required = {}, additionalProperties = false },
  output = { type = "object", properties = {}, required = {}, additionalProperties = false },
  instruction = "return the output"
})
workflow.define({
  input = { type = "object", properties = {}, required = {}, additionalProperties = false },
  output = { type = "object", properties = {}, required = {}, additionalProperties = false },
  run = function() return {} end
})
"#,
        )
        .expect("write workflow");

        let error = load_workflow_definition(&path).expect_err("workflow should fail to load");
        assert!(error.to_string().contains("requires a non-empty `role`"));
    }

    #[test]
    fn worker_definition_rejects_removed_capabilities() {
        let lua = new_lua().expect("create Lua runtime");
        let table = worker_table(&lua);
        let capabilities = lua.create_table().expect("create capabilities table");
        capabilities
            .push("browser__browser_snapshot")
            .expect("add capability");
        table
            .set("capabilities", capabilities)
            .expect("set capabilities");

        let error = worker_definition_from_lua(&lua, &table)
            .expect_err("removed capabilities should be rejected");

        assert!(
            error
                .to_string()
                .contains("`capabilities` has been removed")
        );
    }

    #[test]
    fn worker_definition_accepts_automatic_app_tools_without_capabilities() {
        let lua = new_lua().expect("create Lua runtime");
        let table = worker_table(&lua);

        let definition = worker_definition_from_lua(&lua, &table)
            .expect("worker without capabilities should load");

        assert!(definition.extra_tools.is_empty());
    }
}

fn local_tool_definition_from_lua(lua: &Lua, table: &Table) -> mlua::Result<LocalToolDefinition> {
    let name = table.get::<String>("name")?;
    if !is_valid_workflow_name(&name) {
        return Err(mlua::Error::external(
            "workflow.tool name must use lowercase letters, digits, and single underscores",
        ));
    }
    let input_schema: Value = lua.from_value(table.get("input")?)?;
    let output_schema: Value = lua.from_value(table.get("output")?)?;
    validate_model_facing_schema(&input_schema).map_err(mlua::Error::external)?;
    validate_model_facing_schema(&output_schema).map_err(mlua::Error::external)?;
    let run = table
        .get::<Function>("run")
        .map_err(|_| mlua::Error::external("workflow.tool requires a run function"))?;
    Ok(LocalToolDefinition {
        name,
        input_schema,
        output_schema,
        run,
        lua: lua.clone(),
    })
}

fn string_list_from_lua(table: Option<Table>) -> mlua::Result<Vec<String>> {
    let Some(table) = table else {
        return Ok(Vec::new());
    };
    let mut values = BTreeSet::new();
    for item in table.sequence_values::<String>() {
        let value = item?;
        if value.trim().is_empty() {
            return Err(mlua::Error::external(
                "workflow local-tool names must not be empty",
            ));
        }
        values.insert(value);
    }
    Ok(values.into_iter().collect())
}

fn is_valid_workflow_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && !name.ends_with('_')
        && !name.contains("__")
        && chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

fn new_lua() -> mlua::Result<Lua> {
    let libraries =
        StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH;
    Lua::new_with(libraries, LuaOptions::default())
}

fn install_sandboxed_lua_environment(lua: &Lua, context: &Context) -> mlua::Result<()> {
    let execution_cwd = context.execution_cwd.clone();
    let sandbox_policy = context.sandbox_policy.clone();
    let io_table = lua.create_table()?;
    let open_cwd = execution_cwd.clone();
    let open_policy = sandbox_policy.clone();
    io_table.set(
        "open",
        lua.create_function(move |lua, (path, mode): (String, Option<String>)| {
            let mode = mode.unwrap_or_else(|| "r".to_string());
            let resolved = crate::sandbox::RuntimeSandboxPolicy::resolve_path(
                Path::new(&path),
                Some(&open_cwd),
            );
            let writes = mode.contains('w') || mode.contains('a') || mode.contains('+');
            if writes {
                open_policy
                    .ensure_path_writable(&resolved, "workflow io.open target")
                    .map_err(mlua::Error::external)?;
            } else {
                open_policy
                    .ensure_path_readable(&resolved, "workflow io.open target")
                    .map_err(mlua::Error::external)?;
            }
            let file = match mode.as_str() {
                "r" | "rb" => fs::File::open(&resolved),
                "w" | "wb" => fs::File::create(&resolved),
                "a" | "ab" => std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&resolved),
                _ => {
                    return Err(mlua::Error::external(
                        "workflow io.open supports only r, rb, w, wb, a, or ab modes",
                    ));
                }
            }
            .map_err(mlua::Error::external)?;
            let userdata = lua.create_userdata(WorkflowLuaFile {
                file: Arc::new(Mutex::new(file)),
                readable: !writes,
                writable: writes,
            })?;
            Ok(LuaValue::UserData(userdata))
        })?,
    )?;
    let popen_cwd = execution_cwd.clone();
    let popen_policy = sandbox_policy.clone();
    io_table.set(
        "popen",
        lua.create_function(move |lua, command: String| {
            let output =
                run_sandboxed_shell(&popen_policy, &popen_cwd, &command, LUA_SHELL_MAX_BYTES)
                    .map_err(mlua::Error::external)?;
            let userdata = lua.create_userdata(WorkflowLuaPipe {
                output: Arc::new(Mutex::new(Some(output))),
            })?;
            Ok(LuaValue::UserData(userdata))
        })?,
    )?;
    lua.globals().set("io", io_table)?;

    let os_table = lua.create_table()?;
    let execute_cwd = execution_cwd;
    let execute_policy = sandbox_policy;
    os_table.set(
        "execute",
        lua.create_function(move |_, command: String| {
            let output =
                run_sandboxed_shell(&execute_policy, &execute_cwd, &command, LUA_SHELL_MAX_BYTES)
                    .map_err(mlua::Error::external)?;
            Ok((
                output.status.success(),
                "exit".to_string(),
                output.status.code().unwrap_or(-1),
            ))
        })?,
    )?;
    lua.globals().set("os", os_table)?;
    Ok(())
}

#[derive(Clone)]
struct WorkflowLuaFile {
    file: Arc<Mutex<fs::File>>,
    readable: bool,
    writable: bool,
}

impl mlua::UserData for WorkflowLuaFile {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("read", |_, this, (): ()| {
            if !this.readable {
                return Err(mlua::Error::external("workflow file is not readable"));
            }
            let mut file = this
                .file
                .lock()
                .map_err(|_| mlua::Error::external("workflow file lock poisoned"))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(mlua::Error::external)?;
            drop(file);
            if bytes.len() > LUA_IO_MAX_BYTES {
                return Err(mlua::Error::external(
                    "workflow io.read exceeded the output limit",
                ));
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        });
        methods.add_method_mut("write", |_, this, data: String| {
            if !this.writable {
                return Err(mlua::Error::external("workflow file is not writable"));
            }
            this.file
                .lock()
                .map_err(|_| mlua::Error::external("workflow file lock poisoned"))?
                .write_all(data.as_bytes())
                .map_err(mlua::Error::external)?;
            Ok(())
        });
        methods.add_method("close", |_, _, (): ()| Ok(true));
    }
}

#[derive(Clone)]
struct WorkflowLuaPipe {
    output: Arc<Mutex<Option<WorkflowShellOutput>>>,
}

impl mlua::UserData for WorkflowLuaPipe {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("read", |_, this, (): ()| {
            let output = this
                .output
                .lock()
                .map_err(|_| mlua::Error::external("workflow shell output lock poisoned"))?
                .take()
                .unwrap_or_default();
            Ok(output.text)
        });
        methods.add_method_mut("close", |_, this, (): ()| {
            let output = this
                .output
                .lock()
                .map_err(|_| mlua::Error::external("workflow shell output lock poisoned"))?
                .take()
                .unwrap_or_default();
            Ok((
                output.status.success(),
                "exit".to_string(),
                output.status.code().unwrap_or(-1),
            ))
        });
    }
}

struct WorkflowShellOutput {
    text: String,
    status: std::process::ExitStatus,
}

impl Default for WorkflowShellOutput {
    fn default() -> Self {
        Self {
            text: String::new(),
            status: successful_exit_status(),
        }
    }
}

fn run_sandboxed_shell(
    policy: &crate::sandbox::RuntimeSandboxPolicy,
    execution_cwd: &Path,
    command: &str,
    max_bytes: usize,
) -> Result<WorkflowShellOutput> {
    let (program, args) = workflow_shell_invocation(command);
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|err| miette!("workflow shell requires an active Tokio runtime: {err}"))?;
    tokio::task::block_in_place(|| {
        runtime.block_on(async {
            let mut child = SandboxAsyncChild::spawn_shell(
                policy,
                program,
                args,
                SandboxProcessOptions {
                    current_dir: Some(execution_cwd.to_path_buf()),
                    stdin: SandboxStdio::Null,
                    stdout: SandboxStdio::Piped,
                    stderr: SandboxStdio::Piped,
                },
            )
            .map_err(|err| miette!("failed to start workflow shell command: {err}"))?;
            let mut stdout = child
                .take_stdout()
                .ok_or_else(|| miette!("workflow shell command did not provide stdout"))?;
            let mut stderr = child
                .take_stderr()
                .ok_or_else(|| miette!("workflow shell command did not provide stderr"))?;
            let stdout_task = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.map(|_| bytes)
            });
            let stderr_task = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await.map(|_| bytes)
            });
            let status = loop {
                if let Some(status) = child
                    .try_wait()
                    .map_err(|err| miette!("failed to wait for workflow shell command: {err}"))?
                {
                    break status;
                }
            };
            let stdout = stdout_task
                .await
                .map_err(|err| miette!("workflow shell stdout task failed: {err}"))?
                .map_err(|err| miette!("failed to read workflow shell stdout: {err}"))?;
            let stderr = stderr_task
                .await
                .map_err(|err| miette!("workflow shell stderr task failed: {err}"))?
                .map_err(|err| miette!("failed to read workflow shell stderr: {err}"))?;
            if stdout.len().saturating_add(stderr.len()) > max_bytes {
                return Err(miette!(
                    "workflow shell output exceeded the {} byte limit",
                    max_bytes
                ));
            }
            let mut text = String::from_utf8_lossy(&stdout).into_owned();
            if !stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&stderr));
            }
            Ok(WorkflowShellOutput { text, status })
        })
    })
}

fn workflow_shell_invocation(command: &str) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell.exe",
            vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                command.to_string(),
            ],
        )
    } else {
        ("bash", vec!["-lc".to_string(), command.to_string()])
    }
}

#[cfg(unix)]
fn successful_exit_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn successful_exit_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

trait WorkflowCancellationSignal {
    fn is_interrupted(&self) -> bool;
}

impl WorkflowCancellationSignal for WorkflowCancellation {
    fn is_interrupted(&self) -> bool {
        WorkflowCancellation::is_interrupted(self)
    }
}

impl WorkflowCancellationSignal for WorkflowCancellationRef<'_> {
    fn is_interrupted(&self) -> bool {
        WorkflowCancellationRef::is_interrupted(self)
    }
}

fn ensure_workflow_not_interrupted<C>(cancellation: Option<&C>) -> Result<()>
where
    C: WorkflowCancellationSignal + ?Sized,
{
    if cancellation.is_some_and(WorkflowCancellationSignal::is_interrupted) {
        Err(miette!(WorkflowInterrupted))
    } else {
        Ok(())
    }
}

fn is_workflow_interrupted_error(error: &miette::Report) -> bool {
    error.to_string().contains(WORKFLOW_INTERRUPTED_ERROR)
}

fn install_workflow_interrupt_hook(
    lua: &Lua,
    cancellation: Option<&WorkflowCancellation>,
) -> mlua::Result<()> {
    let Some(cancellation) = cancellation.cloned() else {
        return Ok(());
    };
    lua.set_global_hook(
        HookTriggers {
            every_nth_instruction: Some(WORKFLOW_LUA_INTERRUPT_INSTRUCTION_INTERVAL),
            ..HookTriggers::default()
        },
        move |_, _| {
            if cancellation.is_interrupted() {
                Err(mlua::Error::external(WorkflowInterrupted))
            } else {
                Ok(mlua::VmState::Continue)
            }
        },
    )
}

fn lua_error(err: &mlua::Error) -> miette::Report {
    miette!("Lua workflow error: {err}")
}
