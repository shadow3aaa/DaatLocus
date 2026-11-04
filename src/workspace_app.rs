//! Actor-local selection over shared filesystem branches and immutable versions.
use std::path::Path;

use async_trait::async_trait;
use base64::Engine;
use daat_locus_macros::model_schema;
use daat_locus_workspace::{DEFAULT_BRANCH, Directory, Location, Resolution, Version};
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
    schema_utils::{Nullable, model_schema_for},
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
    /// Original project directory or a Workspace directory. Omission uses the current project.
    path: Option<String>,
    /// Existing shared branch, or default for the original native directory.
    branch: String,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectArgs {
    path: Option<String>,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    path: Option<String>,
    /// Named branch to remove; null removes the project's branches and history.
    branch: Option<String>,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct CaptureOutput {
    path: String,
    files: u64,
    bytes: u64,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct DeleteOutput {
    path: String,
    branch: Option<String>,
}

#[model_schema]
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StorageOutput {
    store_path: String,
    database_bytes: u64,
    wal_bytes: u64,
    objects: u64,
    stored_bytes: u64,
    exclusive_stored_bytes: u64,
    shared_stored_bytes: u64,
    versions: u64,
    branches: u64,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForkArgs {
    path: Option<String>,
    /// Fresh shared branch name.
    branch: String,
    /// Source branch or immutable version ID. Omission uses the current branch in this project, otherwise default.
    from: Option<String>,
    /// Include ignored files when capturing default. Omission respects .gitignore.
    #[serde(default)]
    include_ignored: bool,
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
struct CommitArgs {
    path: Option<String>,
    /// Branch to save. Omission uses the current branch in this project, otherwise default.
    branch: Option<String>,
    message: String,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryArgs {
    path: Option<String>,
    /// Branch or immutable version ID. Omission uses the current branch in this project, otherwise default.
    reference: Option<String>,
    cursor: Option<i64>,
    limit: usize,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DiffArgs {
    path: Option<String>,
    /// Branch or immutable version ID.
    left: String,
    right: String,
    cursor: Option<String>,
    limit: usize,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: Option<String>,
    reference: String,
    /// File path relative to the project root. Links are returned as links.
    file: String,
    offset: u64,
    /// Byte count, from 1 through 32768. Binary data is returned as base64.
    length: usize,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResolutionArgs {
    /// Conflicting path relative to the project root. A directory includes its subtree.
    path: String,
    /// Branch or immutable version containing the resolved path. Null deletes it. Edit a working fork to supply custom content.
    source: Nullable<String>,
}
impl From<ResolutionArgs> for Resolution {
    fn from(value: ResolutionArgs) -> Self {
        Self {
            path: value.path,
            source: match value.source {
                Nullable::Null => None,
                Nullable::Value(source) => Some(source),
            },
        }
    }
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MergeArgs {
    path: Option<String>,
    left: String,
    right: String,
    /// Fresh branch receiving the merged result.
    branch: String,
    message: Option<String>,
    resolutions: Option<Vec<ResolutionArgs>>,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PublishArgs {
    path: Option<String>,
    /// Branch or immutable version ID. Omission uses the current branch in this project.
    reference: Option<String>,
    /// Target version returned by the conflicting publication. Required with resolutions; native changes require a fresh comparison.
    target: Option<String>,
    resolutions: Option<Vec<ResolutionArgs>>,
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
struct ForkOutput {
    path: String,
    branch: String,
    version: VersionOutput,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
struct VersionOutput {
    id: String,
    parents: Vec<String>,
    message: String,
    created_ms: i64,
}
impl From<Version> for VersionOutput {
    fn from(value: Version) -> Self {
        Self {
            id: value.id,
            parents: value.parents,
            message: value.message,
            created_ms: value.created_ms,
        }
    }
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[schemars(extend("x-dl-content" = "workspace"))]
struct HistoryOutput {
    path: String,
    reference: String,
    versions: Vec<VersionOutput>,
    next_cursor: Option<i64>,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[schemars(extend("x-dl-content" = "workspace"))]
struct DiffOutput {
    path: String,
    left: String,
    right: String,
    changes: Vec<ChangeOutput>,
    next_cursor: Option<String>,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[schemars(extend("x-dl-content" = "workspace"))]
struct ReadOutput {
    entry: EntryOutput,
    target: Option<String>,
    text: Option<String>,
    base64: Option<String>,
    next_offset: Option<u64>,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[schemars(extend("x-dl-content" = "workspace"))]
struct MergeOutput {
    path: String,
    base: String,
    left: String,
    right: String,
    branch: Option<String>,
    version: Option<VersionOutput>,
    conflicts: Vec<String>,
}

#[model_schema]
#[derive(Serialize, JsonSchema)]
#[schemars(extend("x-dl-content" = "workspace"))]
struct PublishOutput {
    path: String,
    id: String,
    base: String,
    source: String,
    target: String,
    version: Option<VersionOutput>,
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
        use daat_locus_workspace::ChangeKind;
        Self {
            path: change.path,
            kind: match change.kind {
                ChangeKind::Added => "added",
                ChangeKind::Modified => "modified",
                ChangeKind::Deleted => "deleted",
            }
            .into(),
            entry: change.entry.map(entry_output),
        }
    }
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
        let location = manager.locate(&directory).into_diagnostic()?;
        (self
            .system
            .workspace_authorize(context.actor_call.clone())?)(
            &location.path,
            daat_locus_workspace::Access::Read,
        )
        .into_diagnostic()?;
        Ok(location)
    }

    fn origin(
        &self,
        location: &Location,
        caller: &ActorCallContext,
        call: &AgentToolCall,
    ) -> Result<Directory> {
        let system = self.system.clone();
        let authority = caller.clone();
        let call_id = call.id.clone();
        let reported = parking_lot::Mutex::new(None::<std::time::Instant>);
        Directory::open(
            &location.path,
            self.system.workspace_authorize(Some(caller.clone()))?,
        )
        .map(|directory| {
            directory.with_progress(move |progress| {
                let mut reported = reported.lock();
                if !progress.path.is_empty()
                    && reported
                        .is_some_and(|time| time.elapsed() < std::time::Duration::from_secs(1))
                {
                    return Ok(());
                }
                let result = (|| -> Result<()> {
                    system.record_for_tool(&authority.actor_id, &authority)?;
                    let output = CaptureOutput {
                        path: progress.path.clone(),
                        files: progress.files,
                        bytes: progress.bytes,
                    };
                    let result = AppToolExecutionResult::structured(
                        format!(
                            "Capturing {} files, {} bytes: {}",
                            output.files, output.bytes, output.path
                        ),
                        &output,
                        None,
                    )?;
                    system.store().tool_progress(
                        &authority.actor_id,
                        &authority.message_id,
                        &call_id,
                        result,
                    )
                })();
                result.map_err(|error| std::io::Error::other(error.to_string()))?;
                *reported = Some(std::time::Instant::now());
                Ok(())
            })
        })
        .into_diagnostic()
    }
    fn current_reference(
        &self,
        context: &AppToolExecutionContext,
        location: &Location,
    ) -> Result<String> {
        let current = self.project(context, None)?;
        Ok(if current.path == location.path {
            current.branch
        } else {
            DEFAULT_BRANCH.into()
        })
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
            AppToolSpec { name:"current".into(), description:"Read the current project, shared branch and execution directory.".into(), input_schema:model_schema_for::<EmptyAppArgs>() },
            AppToolSpec { name:"rebind".into(), description:"Bind a recreated original directory to its existing project, preserving branches and saved versions. Move Actors out of named branches first.".into(), input_schema:model_schema_for::<ProjectArgs>() },
            AppToolSpec { name:"delete".into(), description:"Delete an unused named branch; branch=null deletes the project's branches and saved history. Native files remain. Move every Actor out of the affected workspace first; open native handles also prevent deletion. Branch deletion retains saved versions until project deletion.".into(), input_schema:model_schema_for::<DeleteArgs>() },
            AppToolSpec { name:"storage".into(), description:"Locate the local CAS database and report project content bytes shared with or exclusive of other projects. Content counts are compressed object bytes; database/WAL sizes include all projects and metadata.".into(), input_schema:model_schema_for::<ProjectArgs>() },
            AppToolSpec { name:"select".into(), description:"Switch cwd to an existing shared branch or the native default directory. Pass the returned directory to another Actor to collaborate. Running processes retain their cwd.".into(), input_schema:model_schema_for::<SelectArgs>() },
            AppToolSpec { name:"list".into(), description:"List shared project branches, including the native default directory. Continue with next_cursor.".into(), input_schema:model_schema_for::<ListArgs>() },
            AppToolSpec { name:"fork".into(), description:"Create an isolated shared branch from a branch or saved version. Native captures respect project .gitignore rules and retain tracked files; include_ignored=true captures all native contents. Saved versions retain their capture policy. Use select to work there.".into(), input_schema:model_schema_for::<ForkArgs>() },
            AppToolSpec { name:"commit".into(), description:"Save current branch contents as an immutable version for recovery, comparison or sharing. default captures the current native directory. Unchanged contents reuse the saved version.".into(), input_schema:model_schema_for::<CommitArgs>() },
            AppToolSpec { name:"history".into(), description:"Read saved versions and their parents, newest first. default reads captured native history. Continue with next_cursor.".into(), input_schema:model_schema_for::<HistoryArgs>() },
            AppToolSpec { name:"diff".into(), description:"List changed paths between branches or saved versions. default captures the current native directory. Read each version for content comparison; continue with next_cursor.".into(), input_schema:model_schema_for::<DiffArgs>() },
            AppToolSpec { name:"read".into(), description:"Read a bounded file range or link target from a branch or saved version. default captures the current native directory. Continue file reads with next_offset.".into(), input_schema:model_schema_for::<ReadArgs>() },
            AppToolSpec { name:"merge".into(), description:"Merge two branches or saved versions into a fresh branch. Returns fixed base, left and right versions and conflict paths. Resolve paths from an edited working fork or choose a source version, then merge those fixed versions with resolutions.".into(), input_schema:model_schema_for::<MergeArgs>() },
            AppToolSpec { name:"publish".into(), description:"Integrate a branch or saved version into the original native directory using its common ancestor and current disk contents. Resolve conflicts explicitly. After interruption inspect applied, uncertain, error and actual files before continuing.".into(), input_schema:model_schema_for::<PublishArgs>() },
            AppToolSpec { name:"compact".into(), description:"Reclaim obsolete working storage while retaining saved history, branches and open-file contents.".into(), input_schema:model_schema_for::<EmptyAppArgs>() },
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
                matches!(
                    call.name.as_str(),
                    "select"
                        | "fork"
                        | "commit"
                        | "merge"
                        | "publish"
                        | "compact"
                        | "rebind"
                        | "delete"
                ),
            )?
            .clone();
        if !self.system.workspace_available()? {
            return Err(miette!("Workspace filesystem backend is unavailable"));
        }
        let manager = self.system.workspaces()?;
        match call.name.as_str() {
            "current" => {
                let _: EmptyAppArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                AppToolExecutionResult::structured(
                    "current workspace",
                    &LocationOutput::from(self.project(context, None)?),
                    None,
                )
            }
            "select" => {
                let args: SelectArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let origin = if args.branch == DEFAULT_BRANCH {
                    Some(self.origin(&location, &caller, call)?)
                } else {
                    None
                };
                let system = self.system.clone();
                let actor_id = self.actor_id.clone();
                let message_id = caller.message_id.clone();
                let expected = context.execution_cwd.clone();
                let selected = self
                    .blocking(move || {
                        let _metadata = system.workspace_metadata();
                        let selected = if let Some(origin) = origin {
                            manager.select(origin, DEFAULT_BRANCH, DEFAULT_BRANCH)?
                        } else {
                            manager
                                .select_recorded(&location.path, &args.branch, &args.branch)?
                                .ok_or_else(|| {
                                    daat_locus_workspace::Error::Invalid(
                                        "select an existing workspace branch".into(),
                                    )
                                })?
                        };
                        system
                            .store()
                            .select_actor_workspace(
                                &actor_id,
                                &message_id,
                                &expected,
                                &selected.directory,
                            )
                            .map_err(|error| std::io::Error::other(error.to_string()))?;
                        Ok(selected)
                    })
                    .await?;
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
            "fork" => {
                let args: ForkArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let from = args
                    .from
                    .unwrap_or(self.current_reference(context, &location)?);
                let origin = if from == DEFAULT_BRANCH {
                    Some(
                        self.origin(&location, &caller, call)?
                            .with_ignored(args.include_ignored),
                    )
                } else {
                    if args.include_ignored {
                        return Err(miette!("include_ignored applies to captures of default"));
                    }
                    None
                };
                let path = location.path.to_string_lossy().into_owned();
                let branch = args.branch.clone();
                let version = self
                    .blocking(move || {
                        let from = capture_reference(manager.store(), origin.as_ref(), &from)?;
                        manager
                            .store()
                            .fork_at(&location.path, &args.branch, &from)?
                            .commit("Fork workspace")
                    })
                    .await?;
                AppToolExecutionResult::structured(
                    "workspace branch created",
                    &ForkOutput {
                        path,
                        branch,
                        version: version.into(),
                    },
                    None,
                )
            }
            "commit" => {
                let args: CommitArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let branch = args
                    .branch
                    .unwrap_or(self.current_reference(context, &location)?);
                let origin = if branch == DEFAULT_BRANCH {
                    Some(self.origin(&location, &caller, call)?)
                } else {
                    None
                };
                let version = self
                    .blocking(move || {
                        if let Some(origin) = origin {
                            manager.store().commit_default(&origin, &args.message)
                        } else {
                            manager
                                .store()
                                .open_recorded_branch(&location.path, &branch)?
                                .commit(&args.message)
                        }
                    })
                    .await?;
                AppToolExecutionResult::structured(
                    "workspace version saved",
                    &VersionOutput::from(version),
                    None,
                )
            }
            "history" => {
                let args: HistoryArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                page_limit(args.limit)?;
                let location = self.project(context, args.path.as_deref())?;
                let reference = args
                    .reference
                    .unwrap_or(self.current_reference(context, &location)?);
                let path = location.path.to_string_lossy().into_owned();
                let ref_copy = reference.clone();
                let mut versions = self
                    .blocking(move || {
                        manager.store().history(
                            &location.path,
                            &ref_copy,
                            args.cursor,
                            args.limit + 1,
                        )
                    })
                    .await?;
                let more = versions.len() > args.limit;
                versions.truncate(args.limit);
                let next_cursor = if more {
                    versions.last().map(|v| v.cursor)
                } else {
                    None
                };
                AppToolExecutionResult::structured(
                    "workspace history",
                    &HistoryOutput {
                        path,
                        reference,
                        versions: versions.into_iter().map(Into::into).collect(),
                        next_cursor,
                    },
                    None,
                )
            }
            "diff" => {
                let args: DiffArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                page_limit(args.limit)?;
                let location = self.project(context, args.path.as_deref())?;
                let path = location.path.to_string_lossy().into_owned();
                let origin = if args.left == DEFAULT_BRANCH || args.right == DEFAULT_BRANCH {
                    Some(self.origin(&location, &caller, call)?)
                } else {
                    None
                };
                let authorize = self.system.workspace_authorize(Some(caller.clone()))?;
                let source_path = location.path.clone();
                let (left, right, mut changes) = self
                    .blocking(move || {
                        let left = capture_reference(manager.store(), origin.as_ref(), &args.left)?;
                        let right =
                            capture_reference(manager.store(), origin.as_ref(), &args.right)?;
                        let changes = manager.store().diff(
                            &location.path,
                            &left,
                            &right,
                            args.cursor.as_deref(),
                            args.limit + 1,
                        )?;
                        Ok((left, right, changes))
                    })
                    .await?;
                let more = changes.len() > args.limit;
                changes.truncate(args.limit);
                for change in &changes {
                    authorize(
                        &source_path.join(&change.path),
                        daat_locus_workspace::Access::Read,
                    )
                    .into_diagnostic()?;
                }
                let next_cursor = if more {
                    changes.last().map(|c| c.path.clone())
                } else {
                    None
                };
                AppToolExecutionResult::structured(
                    "workspace differences",
                    &DiffOutput {
                        path,
                        left,
                        right,
                        next_cursor,
                        changes: changes.into_iter().map(Into::into).collect(),
                    },
                    None,
                )
            }
            "read" => {
                let args: ReadArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                if args.length == 0 || args.length > 32768 {
                    return Err(miette!("length must be between 1 and 32768"));
                }
                let location = self.project(context, args.path.as_deref())?;
                let origin = if args.reference == DEFAULT_BRANCH {
                    Some(self.origin(&location, &caller, call)?)
                } else {
                    None
                };
                if args.file.contains(['\\', '\0'])
                    || Path::new(&args.file)
                        .components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return Err(miette!("file must be a relative path with / separators"));
                }
                (self.system.workspace_authorize(Some(caller.clone()))?)(
                    &location.path.join(&args.file),
                    daat_locus_workspace::Access::Read,
                )
                .into_diagnostic()?;
                let file = self
                    .blocking(move || {
                        let reference =
                            capture_reference(manager.store(), origin.as_ref(), &args.reference)?;
                        manager.store().read_version(
                            &location.path,
                            &reference,
                            &args.file,
                            args.offset,
                            args.length,
                        )
                    })
                    .await?;
                let next = args.offset.saturating_add(file.bytes.len() as u64);
                let next_offset = (next < file.entry.size
                    && file.entry.kind == daat_locus_workspace::EntryKind::File)
                    .then_some(next);
                let (text, base64) = match String::from_utf8(file.bytes) {
                    Ok(text) => (Some(text), None),
                    Err(error) => (
                        None,
                        Some(base64::engine::general_purpose::STANDARD.encode(error.as_bytes())),
                    ),
                };
                AppToolExecutionResult::structured(
                    "workspace file version",
                    &ReadOutput {
                        entry: entry_output(file.entry),
                        target: file.target,
                        text,
                        base64,
                        next_offset,
                    },
                    None,
                )
            }
            "merge" => {
                let args: MergeArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let path = location.path.to_string_lossy().into_owned();
                let resolutions: Vec<Resolution> = args
                    .resolutions
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .collect();
                let needs_native = args.left == DEFAULT_BRANCH
                    || args.right == DEFAULT_BRANCH
                    || resolutions
                        .iter()
                        .any(|r| r.source.as_deref() == Some(DEFAULT_BRANCH));
                let origin = if needs_native {
                    Some(self.origin(&location, &caller, call)?)
                } else {
                    None
                };
                let outcome = self
                    .blocking(move || {
                        let left = capture_reference(manager.store(), origin.as_ref(), &args.left)?;
                        let right =
                            capture_reference(manager.store(), origin.as_ref(), &args.right)?;
                        let resolutions =
                            capture_resolutions(manager.store(), origin.as_ref(), resolutions)?;
                        manager.store().merge(
                            &location.path,
                            &left,
                            &right,
                            &args.branch,
                            args.message.as_deref().unwrap_or("Merge workspaces"),
                            &resolutions,
                        )
                    })
                    .await?;
                AppToolExecutionResult::structured(
                    "workspace merge",
                    &MergeOutput {
                        path,
                        base: outcome.base,
                        left: outcome.left,
                        right: outcome.right,
                        branch: outcome.branch,
                        version: outcome.version.map(Into::into),
                        conflicts: outcome.conflicts,
                    },
                    None,
                )
            }
            "publish" => {
                let args: PublishArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let reference = args
                    .reference
                    .unwrap_or(self.current_reference(context, &location)?);
                if reference == DEFAULT_BRANCH {
                    return Err(miette!("publish a named branch or saved version"));
                }
                let origin = self.origin(&location, &caller, call)?;
                let path = location.path.to_string_lossy().into_owned();
                let resolutions = args
                    .resolutions
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .collect();
                let outcome = self
                    .blocking(move || {
                        let resolutions =
                            capture_resolutions(manager.store(), Some(&origin), resolutions)?;
                        manager.store().publish(
                            &origin,
                            &reference,
                            args.target.as_deref(),
                            &resolutions,
                        )
                    })
                    .await?;
                let mut result = AppToolExecutionResult::structured(
                    "workspace publication",
                    &PublishOutput {
                        path,
                        id: outcome.id,
                        base: outcome.base,
                        source: outcome.source,
                        target: outcome.target,
                        version: outcome.version.map(Into::into),
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
            "compact" => {
                let _: EmptyAppArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let result = self
                    .blocking(move || manager.store().collect_garbage())
                    .await?;
                AppToolExecutionResult::structured(
                    "workspace storage reclaimed",
                    &CollectionOutput {
                        removed_objects: result.removed_objects,
                        removed_bytes: result.removed_bytes,
                    },
                    None,
                )
            }
            "rebind" => {
                let args: ProjectArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let origin = self.origin(&location, &caller, call)?;
                let path = location.path.clone();
                let system = self.system.clone();
                let authority = caller.clone();
                self.blocking(move || {
                    let _metadata = system.workspace_metadata();
                    system
                        .record_for_tool(&authority.actor_id, &authority)
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                    system
                        .ensure_workspace_unused(&path, None, true)
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                    manager.rebind(&origin)
                })
                .await?;
                AppToolExecutionResult::structured(
                    "workspace directory rebound",
                    &LocationOutput::from(location),
                    None,
                )
            }
            "delete" => {
                let args: DeleteArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let system = self.system.clone();
                let path = location.path.clone();
                let branch = args.branch.clone();
                let authority = caller.clone();
                self.blocking(move || {
                    let _metadata = system.workspace_metadata();
                    system
                        .record_for_tool(&authority.actor_id, &authority)
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                    system
                        .ensure_workspace_unused(&path, branch.as_deref(), false)
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                    manager.delete(&path, branch.as_deref())
                })
                .await?;
                AppToolExecutionResult::structured(
                    "workspace deleted",
                    &DeleteOutput {
                        path: location.path.to_string_lossy().into_owned(),
                        branch: args.branch,
                    },
                    None,
                )
            }
            "storage" => {
                let args: ProjectArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let location = self.project(context, args.path.as_deref())?;
                let storage = self
                    .blocking(move || manager.store().storage(&location.path))
                    .await?;
                let storage: StorageOutput =
                    serde_json::from_value(serde_json::to_value(storage).into_diagnostic()?)
                        .into_diagnostic()?;
                AppToolExecutionResult::structured("workspace storage", &storage, None)
            }
            _ => Err(miette!("Unknown Workspace tool")),
        }
    }
}

fn capture_reference(
    store: &daat_locus_workspace::Store,
    origin: Option<&Directory>,
    reference: &str,
) -> daat_locus_workspace::Result<String> {
    if reference == DEFAULT_BRANCH {
        Ok(store
            .commit_default(
                origin.ok_or_else(|| {
                    daat_locus_workspace::Error::Invalid("native source is required".into())
                })?,
                "Capture native directory",
            )?
            .id)
    } else {
        Ok(reference.to_owned())
    }
}

fn capture_resolutions(
    store: &daat_locus_workspace::Store,
    origin: Option<&Directory>,
    resolutions: Vec<Resolution>,
) -> daat_locus_workspace::Result<Vec<Resolution>> {
    resolutions
        .into_iter()
        .map(|r| {
            Ok(Resolution {
                path: r.path,
                source: r
                    .source
                    .as_deref()
                    .map(|source| capture_reference(store, origin, source))
                    .transpose()?,
            })
        })
        .collect()
}

fn entry_output(entry: daat_locus_workspace::Entry) -> EntryOutput {
    EntryOutput {
        kind: match entry.kind {
            daat_locus_workspace::EntryKind::File => "file",
            daat_locus_workspace::EntryKind::Directory => "directory",
            daat_locus_workspace::EntryKind::Symlink => "symlink",
        }
        .into(),
        size: entry.size,
        mode: entry.mode,
        modified_ns: entry.modified_ns,
    }
}

fn page_limit(limit: usize) -> Result<()> {
    if !(1..=100).contains(&limit) {
        return Err(miette!("limit must be between 1 and 100"));
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
        let observer = system.clone();
        let mut context = tokio::task::spawn_blocking(move || observer.owner_app_context());
        let responsive = tokio::time::timeout(std::time::Duration::from_secs(2), &mut context)
            .await
            .is_ok();
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
        if !responsive {
            context.await.unwrap().unwrap();
        }
        assert!(responsive, "workspace I/O held the Actor metadata lock");
        system.shutdown().await;
    }
}
