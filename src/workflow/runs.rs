//! Session-owned lifecycle management for concurrent workflow runs.
//!
//! Each `start` launches an independent workflow execution task and returns a
//! lightweight handle immediately. The main agent can then keep working and
//! await, inspect, or cancel any individual run by its `run_id`.

use std::collections::HashMap;
use std::sync::Arc;

use miette::{Result, miette};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

use super::{
    WorkflowCancellation, WorkflowExecutionContext, WorkflowInspectorPublisher, WorkflowInvocation,
    WorkflowInvocationResult, WorkflowInvocationStatus, WorkflowNodeStatus, WorkflowRunSnapshot,
    execute_run, invocation_status_from_node_status, publish_workflow_group,
    workflow_snapshot_message,
};
use super::group::WorkflowGroup;
use crate::context::Context;
use crate::schema_utils::validate_value_against_schema;

/// How many completed run results are retained before the oldest are dropped.
/// Active runs are never pruned.
const MAX_RETAINED_COMPLETED_RUNS: usize = 64;

type SharedResult = Arc<WorkflowInvocationResult>;

/// Compact, serializable status for one workflow run. It intentionally omits
/// the full snapshot, which clients load from live dashboard state instead.
#[derive(Clone, Debug, Serialize)]
pub struct WorkflowRunStatus {
    pub run_id: String,
    pub workflow_id: String,
    pub status: WorkflowInvocationStatus,
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

/// Session-owned registry of workflow runs.
///
/// Cloning shares the same underlying run table, so the runtime tools and the
/// runtime loop observe one consistent lifecycle.
#[derive(Clone, Default)]
pub struct WorkflowRunRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    runs: HashMap<String, RunState>,
    /// Session-scoped admission graph. Not a process-wide singleton: each
    /// `WorkflowRunRegistry` (and therefore each session context) owns one.
    group: WorkflowGroup,
}

struct RunState {
    snapshot: Arc<Mutex<WorkflowRunSnapshot>>,
    cancellation: WorkflowCancellation,
    result_rx: watch::Receiver<Option<SharedResult>>,
    result_tx: Option<watch::Sender<Option<SharedResult>>>,
    handle: Option<tokio::task::JoinHandle<()>>,
    /// False while the run is registered but still waiting for dependencies.
    spawned: bool,
    /// Captured at registration so a later completion can spawn this run
    /// without borrowing the live session `Context`.
    launch: Option<PendingLaunch>,
}

struct PendingLaunch {
    definition: super::WorkflowDefinition,
    input: Value,
    execution: WorkflowExecutionContext,
    dashboard_tx: Option<tokio::sync::watch::Sender<crate::dashboard::DashboardState>>,
    dashboard_history: Option<crate::dashboard::DashboardActivityHistoryStore>,
    cancellation_registry: super::WorkflowCancellationRegistry,
}

impl RegistryInner {
    fn prune_completed(&mut self) {
        let mut finished = self
            .runs
            .iter()
            .filter(|(_, state)| state.result_rx.borrow().is_some())
            .map(|(run_id, state)| {
                let completed_at = {
                    let snapshot = state.snapshot.lock();
                    snapshot.completed_at_ms.unwrap_or(snapshot.started_at_ms)
                };
                (run_id.clone(), completed_at)
            })
            .collect::<Vec<_>>();
        if finished.len() <= MAX_RETAINED_COMPLETED_RUNS {
            return;
        }
        finished.sort_by_key(|(_, completed_at)| *completed_at);
        let drop_count = finished.len() - MAX_RETAINED_COMPLETED_RUNS;
        for (run_id, _) in finished.into_iter().take(drop_count) {
            self.runs.remove(&run_id);
        }
    }

    fn publish_group(
        &self,
        dashboard_tx: Option<&tokio::sync::watch::Sender<crate::dashboard::DashboardState>>,
    ) {
        let mut snapshot = self.group.active_snapshot();
        for node in &mut snapshot.nodes {
            let Some(run) = self.runs.get(&node.run_id) else {
                continue;
            };
            let live = run.snapshot.lock().clone();
            node.status = live.status;
            node.completed_at_ms = live.completed_at_ms;
            node.output = live.output.clone();
            node.error = live.error.clone();
            node.snapshot = Some(live);
        }
        publish_workflow_group(dashboard_tx, &snapshot);
    }
}

fn pending_run_snapshot(
    run_id: String,
    workflow_id: String,
    input: Value,
    started_at_ms: i64,
) -> WorkflowRunSnapshot {
    WorkflowRunSnapshot {
        run_id,
        workflow_id,
        status: WorkflowNodeStatus::Pending,
        started_at_ms,
        completed_at_ms: None,
        input,
        output: None,
        error: None,
        await_groups: Vec::new(),
        transitions: Vec::new(),
        workers: Vec::new(),
    }
}

fn pending_invocation_result(
    run_id: String,
    workflow_id: String,
    snapshot: WorkflowRunSnapshot,
) -> WorkflowInvocationResult {
    WorkflowInvocationResult {
        run_id,
        workflow_id,
        status: WorkflowInvocationStatus::Running,
        output: None,
        message: "pending".to_string(),
        snapshot,
    }
}

fn run_status(state: &RunState) -> WorkflowRunStatus {
    if let Some(result) = state.result_rx.borrow().clone() {
        return WorkflowRunStatus {
            run_id: result.run_id.clone(),
            workflow_id: result.workflow_id.clone(),
            status: result.status.clone(),
            started_at_ms: result.snapshot.started_at_ms,
            completed_at_ms: result.snapshot.completed_at_ms,
            message: result.message.clone(),
            output: result.output.clone(),
        };
    }
    let snapshot = state.snapshot.lock().clone();
    let message = if snapshot.status == WorkflowNodeStatus::Pending && !state.spawned {
        "pending".to_string()
    } else {
        workflow_snapshot_message(&snapshot)
    };
    WorkflowRunStatus {
        run_id: snapshot.run_id.clone(),
        workflow_id: snapshot.workflow_id.clone(),
        status: invocation_status_from_node_status(snapshot.status),
        started_at_ms: snapshot.started_at_ms,
        completed_at_ms: snapshot.completed_at_ms,
        message,
        output: snapshot.output.clone(),
    }
}

impl WorkflowRunRegistry {
    /// Validate the invocation, record it in this session's group, and either
    /// launch it immediately or keep it pending until every dependency has
    /// completed. The pending node and its wait edges commit together.
    pub fn start(
        &self,
        context: &Context,
        invocation: WorkflowInvocation,
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

        let run_id = uuid::Uuid::new_v4().to_string();
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let launch_now = {
            let mut inner = self.inner.lock();
            inner.prune_completed();
            inner.group.insert_pending(
                &run_id,
                &definition.id,
                invocation.input.clone(),
                started_at_ms,
            );
            if let Err(err) = inner.group.add_waits(&run_id, &invocation.waits_for) {
                inner.group.remove_node(&run_id);
                return Err(miette!(err));
            }
            let ready = inner.group.is_ready(&run_id);
            if !ready {
                inner.publish_group(context.dashboard_tx.as_ref());
            }
            ready
        };

        if launch_now {
            self.spawn_registered(
                context,
                definition,
                invocation.input,
                run_id,
                started_at_ms,
            )
        } else {
            self.install_pending(context, definition, invocation.input, run_id, started_at_ms)
        }
    }

    fn install_pending(
        &self,
        context: &Context,
        definition: super::WorkflowDefinition,
        input: Value,
        run_id: String,
        started_at_ms: i64,
    ) -> Result<WorkflowInvocationResult> {
        let workflow_id = definition.id.clone();
        let snapshot = Arc::new(Mutex::new(pending_run_snapshot(
            run_id.clone(),
            workflow_id.clone(),
            input.clone(),
            started_at_ms,
        )));
        let initial_snapshot = snapshot.lock().clone();
        let cancellation = WorkflowCancellation::new();
        let (result_tx, result_rx) = watch::channel(None);
        let launch = PendingLaunch {
            definition,
            input,
            execution: WorkflowExecutionContext::from(&*context),
            dashboard_tx: context.dashboard_tx.clone(),
            dashboard_history: context.dashboard_history.clone(),
            cancellation_registry: context.workflow_cancellation.clone(),
        };
        {
            let mut inner = self.inner.lock();
            inner.group.remember_snapshot(&initial_snapshot);
            inner.runs.insert(
                run_id.clone(),
                RunState {
                    snapshot,
                    cancellation,
                    result_rx,
                    result_tx: Some(result_tx),
                    handle: None,
                    spawned: false,
                    launch: Some(launch),
                },
            );
            inner.publish_group(context.dashboard_tx.as_ref());
        }
        Ok(pending_invocation_result(
            run_id,
            workflow_id,
            initial_snapshot,
        ))
    }

    fn spawn_registered(
        &self,
        context: &Context,
        definition: super::WorkflowDefinition,
        input: Value,
        run_id: String,
        started_at_ms: i64,
    ) -> Result<WorkflowInvocationResult> {
        let inspector = WorkflowInspectorPublisher::new_with_history_publishing(
            definition.id.clone(),
            input.clone(),
            context.dashboard_tx.clone(),
            context.dashboard_history.clone(),
            false,
        );
        {
            let shared = inspector.shared_snapshot();
            let mut snapshot = shared.lock();
            snapshot.run_id = run_id.clone();
            snapshot.started_at_ms = started_at_ms;
        }
        inspector.publish();
        let running_snapshot = inspector.snapshot();

        let cancellation = context.workflow_cancellation.begin(&run_id);
        let cancellation_registry = context.workflow_cancellation.clone();
        let execution = WorkflowExecutionContext::from(&*context);
        let initial = WorkflowInvocationResult {
            run_id: run_id.clone(),
            workflow_id: definition.id.clone(),
            status: WorkflowInvocationStatus::Running,
            output: None,
            message: workflow_snapshot_message(&running_snapshot),
            snapshot: running_snapshot.clone(),
        };

        let (result_tx, result_rx) = watch::channel(None);
        {
            let mut inner = self.inner.lock();
            inner.group.remember_snapshot(&running_snapshot);
            inner.runs.insert(
                run_id.clone(),
                RunState {
                    snapshot: inspector.shared_snapshot(),
                    cancellation: cancellation.clone(),
                    result_rx,
                    result_tx: Some(result_tx.clone()),
                    handle: None,
                    spawned: true,
                    launch: None,
                },
            );
            inner.publish_group(context.dashboard_tx.as_ref());
        }

        let registry = self.clone();
        let dashboard_tx = context.dashboard_tx.clone();
        let run_id_for_task = run_id.clone();
        let cancellation_for_task = cancellation.clone();
        let handle = tokio::spawn(async move {
            let result = execute_run(
                &execution,
                &definition,
                input,
                &cancellation_for_task,
                &inspector,
            )
            .await;
            cancellation_registry.clear(&run_id_for_task, &cancellation_for_task);
            registry.finish_run(dashboard_tx.as_ref(), result.clone());
            // A watch send is level-triggered: waiters observe the value even if
            // they subscribe after completion, so no wakeup can be lost.
            let _ = result_tx.send(Some(Arc::new(result)));
        });

        let mut inner = self.inner.lock();
        if let Some(state) = inner.runs.get_mut(&run_id) {
            state.handle = Some(handle);
        }
        Ok(initial)
    }

    /// Apply a finished inner snapshot, then spawn every pending run that this
    /// completion made ready. Failed and interrupted dependencies do not release
    /// waiters and do not cascade into waiter failures.
    fn finish_run(
        &self,
        dashboard_tx: Option<&tokio::sync::watch::Sender<crate::dashboard::DashboardState>>,
        result: WorkflowInvocationResult,
    ) {
        let ready = {
            let mut inner = self.inner.lock();
            inner.group.remember_snapshot(&result.snapshot);
            inner.group.prune_settled_components();
            inner.publish_group(dashboard_tx);
            inner.group.ready_pending_run_ids()
        };
        for run_id in ready {
            if let Err(err) = self.spawn_ready(&run_id) {
                tracing::warn!("failed to release ready workflow run `{run_id}`: {err:?}");
            }
        }
    }

    fn spawn_ready(&self, run_id: &str) -> Result<()> {
        let prepared = {
            let mut inner = self.inner.lock();
            if !inner.group.is_ready(run_id) {
                return Ok(());
            }
            let state = inner
                .runs
                .get_mut(run_id)
                .ok_or_else(|| miette!("unknown workflow run `{run_id}`"))?;
            if state.spawned || state.result_rx.borrow().is_some() {
                return Ok(());
            }
            let launch = state
                .launch
                .take()
                .ok_or_else(|| miette!("workflow run `{run_id}` has no pending launch"))?;
            let started_at_ms = state.snapshot.lock().started_at_ms;
            let already_interrupted = state.cancellation.is_interrupted();
            let result_tx = state
                .result_tx
                .clone()
                .ok_or_else(|| miette!("workflow run `{run_id}` cannot publish a result"))?;
            state.spawned = true;
            (launch, started_at_ms, already_interrupted, result_tx)
        };
        let (launch, started_at_ms, already_interrupted, result_tx) = prepared;
        let PendingLaunch {
            definition,
            input,
            execution,
            dashboard_tx,
            dashboard_history,
            cancellation_registry,
        } = launch;
        let inspector = WorkflowInspectorPublisher::new_with_history_publishing(
            definition.id.clone(),
            input.clone(),
            dashboard_tx.clone(),
            dashboard_history,
            false,
        );
        {
            let shared = inspector.shared_snapshot();
            let mut snapshot = shared.lock();
            snapshot.run_id = run_id.to_string();
            snapshot.started_at_ms = started_at_ms;
        }
        let cancellation = cancellation_registry.begin(run_id);
        if already_interrupted {
            cancellation.interrupt();
        }
        inspector.publish();
        let running_snapshot = inspector.snapshot();
        {
            let mut inner = self.inner.lock();
            let state = inner
                .runs
                .get_mut(run_id)
                .ok_or_else(|| miette!("unknown workflow run `{run_id}`"))?;
            state.snapshot = inspector.shared_snapshot();
            state.cancellation = cancellation.clone();
            state.spawned = true;
            inner.group.remember_snapshot(&running_snapshot);
            inner.publish_group(dashboard_tx.as_ref());
        }

        let registry = self.clone();
        let run_id_owned = run_id.to_string();
        let cancellation_for_task = cancellation.clone();
        let handle = tokio::spawn(async move {
            let result = execute_run(
                &execution,
                &definition,
                input,
                &cancellation_for_task,
                &inspector,
            )
            .await;
            cancellation_registry.clear(&run_id_owned, &cancellation_for_task);
            registry.finish_run(dashboard_tx.as_ref(), result.clone());
            let _ = result_tx.send(Some(Arc::new(result)));
        });
        let mut inner = self.inner.lock();
        if let Some(state) = inner.runs.get_mut(run_id) {
            state.handle = Some(handle);
        }
        Ok(())
    }

    /// Wait for one run to finish and return its final result. Awaiting an
    /// already-finished run returns its retained result immediately.
    pub async fn wait(&self, run_id: &str) -> Result<WorkflowInvocationResult> {
        let mut receiver = {
            let inner = self.inner.lock();
            let state = inner
                .runs
                .get(run_id)
                .ok_or_else(|| miette!("unknown workflow run `{run_id}`"))?;
            state.result_rx.clone()
        };
        if let Some(result) = receiver.borrow().clone() {
            return Ok((*result).clone());
        }
        loop {
            if receiver.changed().await.is_err() {
                return Err(miette!("workflow run `{run_id}` is no longer available"));
            }
            if let Some(result) = receiver.borrow().clone() {
                return Ok((*result).clone());
            }
        }
    }

    /// Request interruption of one run. A still-pending run is finished as
    /// interrupted without spawning it and without failing runs that wait on it.
    /// Returns `false` when the run had already finished.
    pub fn cancel(&self, run_id: &str) -> Result<bool> {
        let pending = {
            let inner = self.inner.lock();
            let state = inner
                .runs
                .get(run_id)
                .ok_or_else(|| miette!("unknown workflow run `{run_id}`"))?;
            if state.result_rx.borrow().is_some() {
                return Ok(false);
            }
            state.cancellation.interrupt();
            !state.spawned
        };
        if pending {
            self.finish_pending_cancelled(run_id)?;
        }
        Ok(true)
    }

    fn finish_pending_cancelled(&self, run_id: &str) -> Result<()> {
        let (result_tx, dashboard_tx, snapshot) = {
            let mut inner = self.inner.lock();
            let state = inner
                .runs
                .get_mut(run_id)
                .ok_or_else(|| miette!("unknown workflow run `{run_id}`"))?;
            if state.spawned || state.result_rx.borrow().is_some() {
                return Ok(());
            }
            state.spawned = true;
            let dashboard_tx = state
                .launch
                .as_ref()
                .and_then(|launch| launch.dashboard_tx.clone());
            state.launch = None;
            let mut snapshot = state.snapshot.lock().clone();
            snapshot.status = WorkflowNodeStatus::Interrupted;
            snapshot.completed_at_ms = Some(chrono::Utc::now().timestamp_millis());
            snapshot.error = Some(super::WORKFLOW_INTERRUPTED_ERROR.to_string());
            *state.snapshot.lock() = snapshot.clone();
            let result_tx = state.result_tx.clone();
            (result_tx, dashboard_tx, snapshot)
        };
        let result = WorkflowInvocationResult {
            run_id: snapshot.run_id.clone(),
            workflow_id: snapshot.workflow_id.clone(),
            status: WorkflowInvocationStatus::Interrupted,
            output: None,
            message: super::WORKFLOW_INTERRUPTED_ERROR.to_string(),
            snapshot,
        };
        if let Some(result_tx) = result_tx {
            let _ = result_tx.send(Some(Arc::new(result.clone())));
        }
        self.finish_run(dashboard_tx.as_ref(), result);
        Ok(())
    }

    pub fn status(&self, run_id: &str) -> Result<WorkflowRunStatus> {
        let inner = self.inner.lock();
        let state = inner
            .runs
            .get(run_id)
            .ok_or_else(|| miette!("unknown workflow run `{run_id}`"))?;
        Ok(run_status(state))
    }

    /// All tracked runs, running ones first, then most-recently-started.
    pub fn list(&self) -> Vec<WorkflowRunStatus> {
        let inner = self.inner.lock();
        let mut runs = inner.runs.values().map(run_status).collect::<Vec<_>>();
        runs.sort_by(|left, right| {
            let left_running = left.status == WorkflowInvocationStatus::Running;
            let right_running = right.status == WorkflowInvocationStatus::Running;
            right_running
                .cmp(&left_running)
                .then_with(|| right.started_at_ms.cmp(&left.started_at_ms))
        });
        runs
    }

    /// Interrupt every active run and wait for their tasks to finish cleanup.
    pub async fn shutdown(&self) {
        let handles = {
            let mut inner = self.inner.lock();
            let mut handles = Vec::new();
            for state in inner.runs.values_mut() {
                if state.result_rx.borrow().is_none() {
                    state.cancellation.interrupt();
                }
                if let Some(handle) = state.handle.take() {
                    handles.push(handle);
                }
            }
            handles
        };
        for handle in handles {
            let _ = handle.await;
        }
    }
}
