//! Actor-local selection over the device's shared, durable workspace branches.
use std::path::Path;

use async_trait::async_trait;
use daat_locus_macros::model_schema;
use daat_locus_workspace::{DEFAULT_BRANCH, Directory, Location};
use miette::{IntoDiagnostic, Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    actors::ActorSystem,
    app::{
        ActorCallContext, App, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec,
        EmptyAppArgs,
    },
    model_protocol::AgentToolCall,
    schema_utils::model_schema_for,
};

pub struct WorkspaceApp {
    actor_id: String,
    system: ActorSystem,
    pending: Vec<tokio::task::JoinHandle<()>>,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SelectArgs {
    /// Original project directory, or a directory returned by Workspace. Null uses the current project.
    path: Option<String>,
    /// Shared branch name. The reserved name default selects the original native directory.
    branch: String,
    /// Source branch when creating a new branch. Null uses the current branch for this project, otherwise default. Existing branches are reopened unchanged.
    from: Option<String>,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: Option<String>,
    cursor: Option<usize>,
    limit: usize,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BranchArgs {
    path: Option<String>,
    /// Null uses the current branch when it belongs to the requested project.
    branch: Option<String>,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ChangesArgs {
    path: Option<String>,
    branch: Option<String>,
    cursor: Option<usize>,
    limit: usize,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct LocationOutput {
    path: String,
    branch: String,
    directory: String,
}
impl From<Location> for LocationOutput {
    fn from(location: Location) -> Self {
        Self {
            path: location.path.to_string_lossy().into_owned(),
            branch: location.branch,
            directory: location.directory.to_string_lossy().into_owned(),
        }
    }
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct BranchList {
    path: String,
    branches: Vec<String>,
    next_cursor: Option<usize>,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct EntryOutput {
    kind: String,
    size: u64,
    mode: u32,
    modified_ns: i64,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct ChangeOutput {
    path: String,
    kind: String,
    entry: Option<EntryOutput>,
}
impl From<daat_locus_workspace::Change> for ChangeOutput {
    fn from(change: daat_locus_workspace::Change) -> Self {
        use daat_locus_workspace::{ChangeKind, EntryKind};
        Self {
            path: change.path,
            kind: match change.kind {
                ChangeKind::Added => "added",
                ChangeKind::Modified => "modified",
                ChangeKind::Deleted => "deleted",
            }
            .into(),
            entry: change.entry.map(|entry| EntryOutput {
                kind: match entry.kind {
                    EntryKind::File => "file",
                    EntryKind::Directory => "directory",
                    EntryKind::Symlink => "symlink",
                }
                .into(),
                size: entry.size,
                mode: entry.mode,
                modified_ns: entry.modified_ns,
            }),
        }
    }
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct ChangesOutput {
    path: String,
    branch: String,
    changes: Vec<ChangeOutput>,
    next_cursor: Option<usize>,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct MergeOutput {
    path: String,
    branch: String,
    id: String,
    applied: Vec<String>,
    conflicts: Vec<String>,
    uncertain: Option<String>,
    error: Option<String>,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct CollectionOutput {
    removed_objects: u64,
    removed_bytes: u64,
}

impl WorkspaceApp {
    pub fn new(actor_id: String, system: ActorSystem) -> Self {
        Self {
            actor_id,
            system,
            pending: Vec::new(),
        }
    }

    async fn blocking<T: Send + 'static>(
        &mut self,
        operation: impl FnOnce() -> daat_locus_workspace::Result<T> + Send + 'static,
    ) -> Result<T> {
        self.pending.retain(|task| !task.is_finished());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.pending.push(tokio::task::spawn_blocking(move || {
            let _ = sender.send(operation());
        }));
        // Keep the JoinHandle in the App when this invocation future is
        // cancelled. shutdown must settle background I/O before stop returns.
        receiver.await.into_diagnostic()?.into_diagnostic()
    }

    fn caller<'a>(
        &self,
        context: &'a AppToolExecutionContext,
        write: bool,
    ) -> Result<&'a ActorCallContext> {
        let caller = context
            .actor_call
            .as_ref()
            .ok_or_else(|| miette!("Workspace requires an active Actor request"))?;
        if caller.actor_id != self.actor_id || self.actor_id == crate::stage::shared::ACTOR_ID {
            return Err(miette!("Workspace Actor identity mismatch"));
        }
        let actor = self.system.record_for_tool(&self.actor_id, caller)?;
        if actor.config.workspace != context.execution_cwd {
            return Err(miette!(
                "Workspace selection changed; obtain a fresh execution context"
            ));
        }
        if write && (actor.remote_read_only || context.sandbox_policy.filesystem.deny_all_writes) {
            return Err(miette!(
                "Remote read-only authority cannot change workspaces"
            ));
        }
        Ok(caller)
    }

    fn project(&self, context: &AppToolExecutionContext, path: Option<&str>) -> Result<Location> {
        let manager = self.system.workspaces()?;
        let directory = path
            .map(|path| context.resolve_tool_path(Path::new(path), None))
            .unwrap_or_else(|| context.execution_cwd.clone());
        let directory = crate::actors::permissions::canonical_path(&directory)?;
        manager.locate(&directory).into_diagnostic()
    }

    fn origin(&self, location: &Location, caller: &ActorCallContext) -> Result<Directory> {
        Directory::open(
            &location.path,
            self.system.workspace_authorize(Some(caller.clone()))?,
        )
        .into_diagnostic()
    }
}

#[async_trait]
impl App for WorkspaceApp {
    fn id(&self) -> AppId {
        AppId::new("workspace").expect("built-in App id")
    }
    fn can_passivate(&self) -> bool {
        self.pending
            .iter()
            .all(tokio::task::JoinHandle::is_finished)
    }
    async fn shutdown(&mut self) -> Result<()> {
        let mut first_error = None;
        for task in std::mem::take(&mut self.pending) {
            if let Err(error) = task.await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error).into_diagnostic();
        }
        Ok(())
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![
            AppToolSpec { name: "current".into(), description: "Read this Actor's project, shared branch name and execution directory.".into(), input_schema: model_schema_for::<EmptyAppArgs>() },
            AppToolSpec { name: "select".into(), description: "Select a shared workspace by original directory and branch name. default uses the native directory; a new branch snapshots its source with shared storage. Existing branches reopen unchanged. Pass the returned directory as an Actor creation workspace to share it. Running processes keep their cwd. Selection grants no filesystem access.".into(), input_schema: model_schema_for::<SelectArgs>() },
            AppToolSpec { name: "list".into(), description: "List a page of shared branches for a project, including default. Continue with next_cursor.".into(), input_schema: model_schema_for::<ListArgs>() },
            AppToolSpec { name: "changes".into(), description: "List a page of a named branch's changes since its base. Use Files at the branch directory to inspect contents.".into(), input_schema: model_schema_for::<ChangesArgs>() },
            AppToolSpec { name: "merge".into(), description: "Merge a named branch into its original native directory. Resolve reported conflicts before completing branch work, or report the unresolved changes. Check applied, uncertain and error after interruption; effects are never automatically replayed. Success records the branch head as integrated while retaining its isolated contents.".into(), input_schema: model_schema_for::<BranchArgs>() },
            AppToolSpec { name: "compact".into(), description: "Reclaim obsolete workspace storage. Preserves all branches, checkpoints, unfinished merge versions and open-file contents.".into(), input_schema: model_schema_for::<EmptyAppArgs>() },
        ]
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        let caller = self
            .caller(
                context,
                matches!(call.name.as_str(), "select" | "merge" | "compact"),
            )?
            .clone();
        if !self.system.workspace_available()? {
            return Err(miette!("Workspace filesystem backend is unavailable"));
        }
        let manager = self.system.workspaces()?;
        match call.name.as_str() {
            "compact" => {
                let _: EmptyAppArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let collection = self
                    .blocking(move || manager.store().collect_garbage())
                    .await?;
                AppToolExecutionResult::structured(
                    "workspace storage reclaimed",
                    &CollectionOutput {
                        removed_objects: collection.removed_objects,
                        removed_bytes: collection.removed_bytes,
                    },
                    None,
                )
            }
            "current" => {
                let _: EmptyAppArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, None)?;
                AppToolExecutionResult::structured(
                    "current workspace",
                    &LocationOutput::from(location),
                    None,
                )
            }
            "select" => {
                let args: SelectArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let current = self.project(context, None)?;
                let from = args.from.unwrap_or_else(|| {
                    if location.path == current.path {
                        current.branch
                    } else {
                        DEFAULT_BRANCH.into()
                    }
                });
                let authorize = self.system.workspace_authorize(Some(caller.clone()))?;
                let selected = self
                    .blocking(move || {
                        if let Some(selected) =
                            manager.select_recorded(&location.path, &args.branch, &from)?
                        {
                            return Ok(selected);
                        }
                        let origin = Directory::open(&location.path, authorize)?;
                        manager.select(origin, &args.branch, &from)
                    })
                    .await?;
                // The mount may have finished after cancellation or a competing
                // lifecycle change. Selection commits only for the original cwd
                // and still-running authenticated request.
                self.caller(context, true)?;
                self.system.store().select_actor_workspace(
                    &self.actor_id,
                    &caller.message_id,
                    &context.execution_cwd,
                    &selected.directory,
                )?;
                AppToolExecutionResult::structured(
                    "workspace selected",
                    &LocationOutput::from(selected),
                    None,
                )
            }
            "list" => {
                let args: ListArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                page_limit(args.limit)?;
                let location = self.project(context, args.path.as_deref())?;
                let offset = args.cursor.unwrap_or(0);
                let mut branches = manager
                    .store()
                    .branches_at(&location.path, offset.saturating_sub(1), args.limit + 1)
                    .into_diagnostic()?;
                if offset == 0 {
                    branches.insert(0, DEFAULT_BRANCH.into());
                }
                let more = branches.len() > args.limit;
                branches.truncate(args.limit);
                AppToolExecutionResult::structured(
                    "workspace branches",
                    &BranchList {
                        path: location.path.to_string_lossy().into_owned(),
                        next_cursor: more.then(|| offset.saturating_add(branches.len())),
                        branches,
                    },
                    None,
                )
            }
            "changes" => {
                let args: ChangesArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                page_limit(args.limit)?;
                let location = self.project(context, args.path.as_deref())?;
                let branch = args.branch.unwrap_or(location.branch);
                let workspace = manager
                    .store()
                    .open_recorded_branch(&location.path, &branch)
                    .into_diagnostic()?;
                let offset = args.cursor.unwrap_or(0);
                let mut changes = self
                    .blocking(move || workspace.changes(offset, args.limit + 1))
                    .await?;
                let more = changes.len() > args.limit;
                changes.truncate(args.limit);
                AppToolExecutionResult::structured(
                    "workspace changes",
                    &ChangesOutput {
                        path: location.path.to_string_lossy().into_owned(),
                        branch,
                        next_cursor: more.then(|| offset.saturating_add(changes.len())),
                        changes: changes.into_iter().map(Into::into).collect(),
                    },
                    None,
                )
            }
            "merge" => {
                let args: BranchArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let origin = self.origin(&location, &caller)?;
                let branch = args.branch.unwrap_or(location.branch);
                let workspace = manager
                    .store()
                    .open_branch(origin.clone(), &branch)
                    .into_diagnostic()?;
                let outcome = self
                    .blocking(move || workspace.merge_to_default(&origin))
                    .await?;
                let mut result = AppToolExecutionResult::structured(
                    "workspace merge",
                    &MergeOutput {
                        path: location.path.to_string_lossy().into_owned(),
                        branch,
                        id: outcome.id,
                        applied: outcome.applied,
                        conflicts: outcome.conflicts,
                        uncertain: outcome.uncertain,
                        error: outcome.error.clone(),
                    },
                    None,
                )?;
                result.failure = outcome.error;
                Ok(result)
            }
            _ => Err(miette!("Unknown Workspace tool")),
        }
    }
}

fn page_limit(limit: usize) -> Result<()> {
    if !(1..=100).contains(&limit) {
        return Err(miette!("Workspace page limit must be between 1 and 100"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, task::Poll};

    #[tokio::test]
    async fn workspace_shutdown_joins_io_after_the_tool_future_is_cancelled() {
        let temp = crate::sandbox::test_support::scoped_directory();
        let system = ActorSystem::open(
            temp.path().join("data"),
            temp.path().join("workspace"),
            crate::config::Config::default(),
        )
        .await
        .unwrap();
        let mut app = WorkspaceApp::new("test-worker".into(), system.clone());
        let marker = temp.path().join("settled.txt");
        let output = marker.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let mut invocation = Box::pin(app.blocking(move || {
            let _ = started.send(());
            blocked.recv().map_err(std::io::Error::other)?;
            std::fs::write(output, b"settled")?;
            Ok(())
        }));
        tokio::select! {
            result = &mut invocation => panic!("I/O finished before release: {result:?}"),
            result = waiting => result.unwrap(),
        }
        drop(invocation);
        assert!(!app.can_passivate());
        let mut shutdown = Box::pin(app.shutdown());
        std::future::poll_fn(|context| {
            assert!(
                shutdown.as_mut().poll(context).is_pending(),
                "shutdown detached live I/O"
            );
            Poll::Ready(())
        })
        .await;
        assert!(!marker.exists());
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), shutdown)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(marker).unwrap(), b"settled");
        system.shutdown().await;
    }
}
