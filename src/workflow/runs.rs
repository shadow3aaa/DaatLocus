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
    WorkflowInvocationResult, WorkflowInvocationStatus, WorkflowRunSnapshot, execute_run,
    invocation_status_from_node_status, workflow_snapshot_message,
};
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
}

struct RunState {
    snapshot: Arc<Mutex<WorkflowRunSnapshot>>,
    cancellation: WorkflowCancellation,
    result_rx: watch::Receiver<Option<SharedResult>>,
    handle: Option<tokio::task::JoinHandle<()>>,
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
    WorkflowRunStatus {
        run_id: snapshot.run_id.clone(),
        workflow_id: snapshot.workflow_id.clone(),
        status: invocation_status_from_node_status(snapshot.status),
        started_at_ms: snapshot.started_at_ms,
        completed_at_ms: snapshot.completed_at_ms,
        message: workflow_snapshot_message(&snapshot),
        output: snapshot.output.clone(),
    }
}

impl WorkflowRunRegistry {
    /// Validate the invocation, launch it in the background, and return the
    /// initial running handle. The run continues even if the caller stops
    /// waiting on it.
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

        let inspector = WorkflowInspectorPublisher::new_with_history(
            definition.id.clone(),
            invocation.input.clone(),
            context.dashboard_tx.clone(),
            context.dashboard_history.clone(),
        );
        let snapshot = inspector.shared_snapshot();
        let run_id = snapshot.lock().run_id.clone();

        let cancellation = context.workflow_cancellation.begin(&run_id);
        let cancellation_registry = context.workflow_cancellation.clone();
        let execution = WorkflowExecutionContext::from(context);

        let initial_snapshot = inspector.snapshot();
        let initial = WorkflowInvocationResult {
            run_id: run_id.clone(),
            workflow_id: definition.id.clone(),
            status: WorkflowInvocationStatus::Running,
            output: None,
            message: workflow_snapshot_message(&initial_snapshot),
            snapshot: initial_snapshot,
        };

        let (result_tx, result_rx) = watch::channel(None);
        {
            let mut inner = self.inner.lock();
            inner.prune_completed();
            inner.runs.insert(
                run_id.clone(),
                RunState {
                    snapshot,
                    cancellation: cancellation.clone(),
                    result_rx,
                    handle: None,
                },
            );
        }

        let run_id_for_task = run_id.clone();
        let cancellation_for_task = cancellation.clone();
        let handle = tokio::spawn(async move {
            let result = execute_run(
                &execution,
                &definition,
                invocation.input,
                &cancellation_for_task,
                &inspector,
            )
            .await;
            cancellation_registry.clear(&run_id_for_task, &cancellation_for_task);
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

    /// Request interruption of one run. Returns `false` when the run had
    /// already finished.
    pub fn cancel(&self, run_id: &str) -> Result<bool> {
        let inner = self.inner.lock();
        let state = inner
            .runs
            .get(run_id)
            .ok_or_else(|| miette!("unknown workflow run `{run_id}`"))?;
        if state.result_rx.borrow().is_some() {
            return Ok(false);
        }
        state.cancellation.interrupt();
        Ok(true)
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
