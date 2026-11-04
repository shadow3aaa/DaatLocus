use crate::directory::{relative_path, validate_name};
use crate::store::{get, next_inode, put};
use crate::{DEFAULT_BRANCH, Directory, Error, ObjectId, Result, Store, missing};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const BLOCK: usize = 64 * 1024;

mod capture;
mod history;
pub use capture::CaptureProgress;
mod lifecycle;
pub(crate) use history::migrate_versions;
pub use history::{FileVersion, MergeOutcome, PublishOutcome, Resolution, Version};
pub use lifecycle::Storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub inode: u64,
    pub kind: EntryKind,
    pub size: u64,
    pub mode: u32,
    pub modified_ns: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Node {
    entry: Entry,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<ObjectId>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    children: BTreeMap<String, ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    include_ignored: Option<bool>,
    #[cfg(windows)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reparse: Option<crate::reparse::ReparsePoint>,
}

impl Node {
    fn save(&self, connection: &Connection) -> Result<ObjectId> {
        put(connection, 1, &serde_json::to_vec(self)?)
    }
    fn load(connection: &Connection, id: &ObjectId) -> Result<Self> {
        Ok(serde_json::from_slice(&get(connection, 1, id)?)?)
    }
    fn same_contents(&self, other: &Self) -> bool {
        #[cfg(windows)]
        if match (&self.reparse, &other.reparse) {
            (Some(left), Some(right)) => !left.same_target(right),
            (None, None) => false,
            _ => true,
        } {
            return false;
        }
        self.entry.kind == other.entry.kind
            && self.entry.mode == other.entry.mode
            && self.entry.size == other.entry.size
            && self.blocks == other.blocks
            && self.target == other.target
            && self.include_ignored.unwrap_or(true) == other.include_ignored.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    pub entry: Option<Entry>,
}

#[derive(Clone)]
pub struct Workspace {
    store: Store,
    origin: std::path::PathBuf,
    binding: String,
    project: i64,
    name: String,
    generation: i64,
}

impl Store {
    fn directory_node(&self, connection: &Connection, id: &ObjectId) -> Result<Arc<Node>> {
        if let Some(node) = self.directories.lock().get(id).cloned() {
            return Ok(node);
        }
        let node = Arc::new(Node::load(connection, id)?);
        if node.entry.kind == EntryKind::Directory {
            self.directories.lock().put(id.clone(), node.clone());
        }
        Ok(node)
    }

    fn lookup(&self, connection: &Connection, root: &ObjectId, path: &str) -> Result<Arc<Node>> {
        lookup_node(root, path, |id| self.directory_node(connection, id))
    }

    /// Reclaim obsolete blocks and trees while retaining every live branch,
    /// checkpoint, unfinished merge and native unlinked-file handle.
    pub fn collect_garbage(&self) -> Result<crate::Collection> {
        let mut connection = self.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let pending: Vec<ObjectId> = transaction.prepare(
            "SELECT base FROM branches UNION SELECT head FROM branches UNION SELECT root FROM versions UNION SELECT head FROM merges WHERE state IN ('prepared','applying','interrupted') UNION SELECT root FROM native_files"
        )?.query_map([], |row| Ok(ObjectId(row.get(0)?)))?.collect::<std::result::Result<_, _>>()?;
        let live = reachable(&transaction, pending)?;
        transaction.execute_batch("CREATE TEMP TABLE IF NOT EXISTS workspace_live_objects(id TEXT PRIMARY KEY); DELETE FROM workspace_live_objects;")?;
        {
            let mut insert =
                transaction.prepare("INSERT INTO workspace_live_objects VALUES(?1)")?;
            for id in live {
                insert.execute([id.0])?;
            }
        }
        let (removed_objects, removed_bytes): (i64, i64) = transaction.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(data)),0) FROM objects WHERE id NOT IN (SELECT id FROM workspace_live_objects)",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        transaction.execute(
            "DELETE FROM objects WHERE id NOT IN (SELECT id FROM workspace_live_objects)",
            [],
        )?;
        transaction.commit()?;
        let vacuum: u32 = connection.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
        if vacuum != 2 {
            // Upgrade stores created before incremental page reclamation was
            // enabled. This happens only for an explicit collection request.
            connection.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;")?;
        }
        connection.execute_batch("PRAGMA incremental_vacuum; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(crate::Collection {
            removed_objects: removed_objects as u64,
            removed_bytes: removed_bytes as u64,
        })
    }

    /// Open an existing branch without taking a new snapshot or creating state.
    pub fn open_branch(&self, origin: Directory, name: &str) -> Result<Workspace> {
        origin.check_binding()?;
        let workspace = self.open_recorded_branch(origin.path(), name)?;
        if workspace.binding != origin.binding() {
            return Err(Error::Conflict(
                "source directory was moved or replaced".into(),
            ));
        }
        Ok(workspace)
    }

    /// Look up a canonical project path returned by Workspace::origin without
    /// reopening the original native directory. Immutable branches stay usable
    /// after a user moves or replaces that directory; publishing still checks
    /// its original filesystem identity against a freshly authorized Directory.
    pub fn open_recorded_branch(&self, path: &std::path::Path, name: &str) -> Result<Workspace> {
        self.read(|connection| {
            validate_branch(name)?;

            let (project, binding): (i64, String) = connection
                .query_row(
                    "SELECT id,binding FROM projects WHERE path=?1",
                    [path
                        .to_str()
                        .ok_or_else(|| Error::Invalid("project path must be Unicode".into()))?],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| missing("workspace project"))?;
            roots(connection, project, name)?;
            let generation = connection.query_row(
                "SELECT generation FROM branches WHERE project=?1 AND name=?2",
                params![project, name],
                |row| row.get(0),
            )?;
            Ok(Workspace {
                store: self.clone(),
                origin: path.to_owned(),
                binding,
                project,
                name: name.into(),
                generation,
            })
        })
    }

    /// Reopening `(directory, name)` retains its exact live branch. Creating a
    /// branch from default captures the directory once; it never uses the live
    /// host directory as an overlay lower layer.
    pub fn branch(&self, origin: Directory, name: &str, from: &str) -> Result<Workspace> {
        validate_branch(name)?;
        if from != DEFAULT_BRANCH {
            validate_branch(from)?;
        }
        if from == DEFAULT_BRANCH && self.path().starts_with(origin.path()) {
            return Err(Error::Invalid(
                "a branch source cannot contain its own content store".into(),
            ));
        }
        let mut connection = self.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let project = project(&transaction, &origin)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM branches WHERE project=?1 AND name=?2)",
            params![project, name],
            |row| row.get(0),
        )?;
        if !exists {
            let version = if from == DEFAULT_BRANCH {
                history::capture_default(
                    &transaction,
                    &origin,
                    project,
                    "Capture native directory",
                )?
            } else {
                history::save_branch(&transaction, project, from, "Fork workspace")?
            };
            transaction.execute(
                "INSERT INTO branches(project,name,base,head,generation) VALUES(?1,?2,?3,?3,?4)",
                params![
                    project,
                    name,
                    version.root.0,
                    next_inode(&transaction)? as i64
                ],
            )?;
            history::set_tip(&transaction, project, name, &version.id)?;
        }
        transaction.commit()?;
        drop(connection);
        self.open_recorded_branch(origin.path(), name)
    }

    pub fn branches(&self, origin: &Directory, offset: usize, limit: usize) -> Result<Vec<String>> {
        self.branches_at(origin.path(), offset, limit)
    }

    pub fn branches_at(
        &self,
        path: &std::path::Path,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<String>> {
        self.read(|connection| {
            let id: Option<i64> = connection
                .query_row(
                    "SELECT id FROM projects WHERE path=?1",
                    [path
                        .to_str()
                        .ok_or_else(|| Error::Invalid("project path must be Unicode".into()))?],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = id else {
                return Ok(vec![]);
            };
            let mut query = connection.prepare(
                "SELECT name FROM branches WHERE project=?1 ORDER BY name LIMIT ?2 OFFSET ?3",
            )?;
            let offset = i64::try_from(offset)
                .map_err(|_| Error::Invalid("offset is not representable".into()))?;
            Ok(query
                .query_map(params![id, limit.min(200) as i64, offset], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?)
        })
    }
}

impl Workspace {
    fn roots(&self, connection: &Connection) -> Result<(ObjectId, ObjectId)> {
        connection
            .prepare_cached(
                "SELECT base,head FROM branches WHERE project=?1 AND name=?2 AND generation=?3",
            )?
            .query_row(params![self.project, self.name, self.generation], |row| {
                Ok((ObjectId(row.get(0)?), ObjectId(row.get(1)?)))
            })
            .optional()?
            .ok_or_else(|| Error::Conflict("workspace branch was deleted or replaced".into()))
    }
    pub(crate) fn project_id(&self) -> i64 {
        self.project
    }
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn backing_path(&self) -> &std::path::Path {
        self.store.path()
    }
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn detached_file(&self, path: &str) -> Result<DetachedFile> {
        let mut connection = self.store.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let root = self.roots(&transaction)?.1;
        let node = lookup(&transaction, &root, path)?;
        let pin = uuid::Uuid::new_v4().to_string();
        let root = node.save(&transaction)?;
        transaction.execute(
            "INSERT INTO native_files VALUES(?1,?2)",
            params![pin, root.0],
        )?;
        transaction.commit()?;
        Ok(DetachedFile {
            store: self.store.clone(),
            node,
            pin,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn origin(&self) -> &std::path::Path {
        &self.origin
    }
    pub fn key(&self) -> String {
        if self.generation == 0 {
            format!("{}:{}", self.project, self.name)
        } else {
            format!("{}:{}:{}", self.project, self.name, self.generation)
        }
    }
    pub fn root(&self) -> Result<ObjectId> {
        self.store.read(|connection| Ok(self.roots(connection)?.1))
    }
    pub fn fork(&self, name: &str) -> Result<Self> {
        self.store
            .fork_at(&self.origin, name, &self.commit("Fork workspace")?.id)
    }
    pub fn entry(&self, path: &str) -> Result<Entry> {
        self.store.read(|connection| {
            let root = self.roots(connection)?.1;
            Ok(self.store.lookup(connection, &root, path)?.entry.clone())
        })
    }
    pub fn read_dir(&self, path: &str) -> Result<Vec<(String, Entry)>> {
        self.store.read(|connection| {
            let root = self.roots(connection)?.1;
            let directory = self.store.lookup(connection, &root, path)?;
            if directory.entry.kind != EntryKind::Directory {
                return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
            }
            directory
                .children
                .iter()
                .map(|(name, id)| Ok((name.clone(), Node::load(connection, id)?.entry)))
                .collect()
        })
    }
    pub fn read(&self, path: &str, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.store.read(|connection| {
            let root = self.roots(connection)?.1;
            read_node(
                connection,
                self.store.lookup(connection, &root, path)?.as_ref(),
                offset,
                length,
            )
        })
    }
    pub fn read_link(&self, path: &str) -> Result<String> {
        self.store.read(|connection| {
            let root = self.roots(connection)?.1;
            self.store
                .lookup(connection, &root, path)?
                .target
                .clone()
                .ok_or_else(|| Error::Invalid("not a symlink".into()))
        })
    }
    fn mutate<T>(
        &self,
        change: impl FnOnce(&Connection, &ObjectId) -> Result<(ObjectId, T)>,
    ) -> Result<T> {
        let mut connection = self.store.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let root = self.roots(&transaction)?.1;
        let (root, result) = change(&transaction, &root)?;
        transaction.execute(
            "UPDATE branches SET head=?3,revision=revision+1 WHERE project=?1 AND name=?2",
            params![self.project, self.name, root.0],
        )?;
        transaction.commit()?;
        Ok(result)
    }
    pub fn create(&self, path: &str, kind: EntryKind, mode: u32) -> Result<Entry> {
        if kind == EntryKind::Symlink {
            return Err(Error::Invalid("use symlink to provide a target".into()));
        }
        self.mutate(|connection, root| {
            if lookup_optional(connection, root, path)?.is_some() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
            }
            let node = Node {
                entry: Entry {
                    inode: next_inode(connection)?,
                    kind,
                    size: 0,
                    mode: mode & 0o777,
                    modified_ns: now_ns(),
                },
                blocks: vec![],
                children: BTreeMap::new(),
                target: None,
                include_ignored: None,
                #[cfg(windows)]
                reparse: None,
            };
            Ok((
                replace(connection, root, path, Some(node.save(connection)?))?,
                node.entry,
            ))
        })
    }
    pub fn symlink(&self, path: &str, target: &str) -> Result<Entry> {
        if target.contains('\0') {
            return Err(Error::Invalid("symlink target contains NUL".into()));
        }
        self.mutate(|connection, root| {
            if lookup_optional(connection, root, path)?.is_some() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
            }
            let node = Node {
                entry: Entry {
                    inode: next_inode(connection)?,
                    kind: EntryKind::Symlink,
                    size: target.len() as u64,
                    mode: if cfg!(windows) { 0o644 } else { 0o777 },
                    modified_ns: now_ns(),
                },
                include_ignored: None,
                blocks: vec![],
                children: BTreeMap::new(),
                target: Some(target.into()),
                #[cfg(windows)]
                reparse: Some(crate::reparse::ReparsePoint::symlink(
                    target,
                    lookup_optional(connection, root, target)
                        .ok()
                        .flatten()
                        .is_some_and(|node| node.entry.kind == EntryKind::Directory),
                )?),
            };
            Ok((
                replace(connection, root, path, Some(node.save(connection)?))?,
                node.entry,
            ))
        })
    }
    #[cfg(windows)]
    pub(crate) fn reparse_point(&self, path: &str) -> Result<Option<crate::reparse::ReparsePoint>> {
        self.store.read(|connection| {
            let (_, root) = self.roots(connection)?;
            let node = self.store.lookup(connection, &root, path)?;
            if node.entry.kind != EntryKind::Symlink {
                return Ok(None);
            }
            let point = match node.reparse.clone() {
                Some(point) => point,
                None => crate::reparse::ReparsePoint::symlink(
                    node.target
                        .as_deref()
                        .ok_or_else(|| Error::Corrupt("missing link target".into()))?,
                    false,
                )?,
            };
            point.target()?;
            Ok(Some(point))
        })
    }

    #[cfg(windows)]
    pub(crate) fn set_reparse_point(
        &self,
        path: &str,
        point: Option<crate::reparse::ReparsePoint>,
    ) -> Result<()> {
        self.mutate(|connection, root| {
            let mut node = lookup(connection, root, path)?;
            if !node.children.is_empty() {
                return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty).into());
            }
            match point {
                Some(point) => {
                    if node.entry.kind == EntryKind::File && node.entry.size != 0 {
                        return Err(Error::Invalid("a link requires an empty file".into()));
                    }
                    if let Some(previous) = &node.reparse
                        && previous.tag()? != point.tag()?
                    {
                        return Err(Error::Invalid("cannot change a link's reparse tag".into()));
                    }
                    if node.entry.kind != EntryKind::Symlink
                        && (node.entry.kind == EntryKind::Directory) != point.directory
                    {
                        return Err(Error::Invalid("link directory attribute mismatch".into()));
                    }
                    let target = point.target()?;
                    node.entry.kind = EntryKind::Symlink;
                    node.entry.mode = if node.entry.mode & 0o222 == 0 {
                        0o444
                    } else {
                        0o644
                    };
                    node.entry.size = target.len() as u64;
                    node.target = Some(target);
                    node.reparse = Some(point);
                }
                None => {
                    let point = node
                        .reparse
                        .take()
                        .ok_or_else(|| Error::Invalid("not a reparse point".into()))?;
                    node.entry.kind = if point.directory {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    };
                    node.entry.size = 0;
                    node.target = None;
                }
            }
            node.blocks.clear();
            node.entry.modified_ns = now_ns();
            Ok((
                replace(connection, root, path, Some(node.save(connection)?))?,
                (),
            ))
        })
    }

    pub fn write(&self, path: &str, offset: u64, bytes: &[u8]) -> Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.mutate(|connection, root| {
            let node = write_node(connection, lookup(connection, root, path)?, offset, bytes)?;
            Ok((
                replace(connection, root, path, Some(node.save(connection)?))?,
                bytes.len(),
            ))
        })
    }
    pub fn truncate(&self, path: &str, size: u64) -> Result<()> {
        self.mutate(|connection, root| {
            let node = truncate_node(connection, lookup(connection, root, path)?, size)?;
            Ok((
                replace(connection, root, path, Some(node.save(connection)?))?,
                (),
            ))
        })
    }
    pub fn set_metadata(
        &self,
        path: &str,
        mode: Option<u32>,
        modified_ns: Option<i64>,
    ) -> Result<()> {
        self.mutate(|connection, root| {
            let mut node = lookup(connection, root, path)?;
            if let Some(mode) = mode {
                node.entry.mode = mode & 0o777;
            }
            if let Some(time) = modified_ns {
                node.entry.modified_ns = time;
            }
            let id = node.save(connection)?;
            Ok((
                if path.is_empty() {
                    id
                } else {
                    replace(connection, root, path, Some(id))?
                },
                (),
            ))
        })
    }
    pub fn remove(&self, path: &str, directory: bool) -> Result<()> {
        self.mutate(|connection, root| {
            let node = lookup(connection, root, path)?;
            if (node.entry.kind == EntryKind::Directory) != directory {
                return Err(Error::Invalid("entry type mismatch".into()));
            }
            if !node.children.is_empty() {
                return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty).into());
            }
            Ok((replace(connection, root, path, None)?, ()))
        })
    }
    pub fn rename(&self, from: &str, to: &str, replace_existing: bool) -> Result<()> {
        relative_path(from)?;
        relative_path(to)?;
        if from == to {
            return Ok(());
        }
        if to.starts_with(&format!("{from}/")) {
            return Err(Error::Invalid("cannot move a directory into itself".into()));
        }
        self.mutate(|connection, root| {
            let node = lookup(connection, root, from)?;
            if let Some(target) = lookup_optional(connection, root, to)? {
                if !replace_existing {
                    return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
                }
                if (target.entry.kind == EntryKind::Directory)
                    != (node.entry.kind == EntryKind::Directory)
                {
                    return Err(Error::Invalid("rename entry type mismatch".into()));
                }
                if !target.children.is_empty() {
                    return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty).into());
                }
            }
            let next = replace(connection, root, from, None)?;
            Ok((
                replace(connection, &next, to, Some(node.save(connection)?))?,
                (),
            ))
        })
    }
    pub fn changes(&self, offset: usize, limit: usize) -> Result<Vec<Change>> {
        let mut database = self.store.connection.lock();
        let transaction = database.transaction()?;
        let connection = &transaction;
        let result = (|| -> Result<_> {
            let (base, head) = self.roots(connection)?;
            let head = capture::saved_tree(connection, &head, &base)?;
            let before = flatten(connection, &base)?;
            let after = flatten(connection, &head)?;
            Ok(changed_paths(&before, &after)
                .into_iter()
                .skip(offset)
                .take(limit.min(200))
                .map(|path| Change {
                    kind: if !before.contains_key(&path) {
                        ChangeKind::Added
                    } else if !after.contains_key(&path) {
                        ChangeKind::Deleted
                    } else {
                        ChangeKind::Modified
                    },
                    entry: after.get(&path).map(|node| node.entry.clone()),
                    path,
                })
                .collect())
        })()?;
        transaction.commit()?;
        Ok(result)
    }
}

/// An unlinked file remains usable through its existing native open handles.
/// Its final close releases the handle; it never resurrects a directory entry.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) struct DetachedFile {
    store: Store,
    node: Node,
    pin: String,
}

#[cfg(any(windows, target_os = "linux"))]
impl DetachedFile {
    #[cfg(windows)]
    pub(crate) fn reparse_point(&self) -> Option<crate::reparse::ReparsePoint> {
        self.node.reparse.clone()
    }
    pub(crate) fn entry(&self) -> Entry {
        self.node.entry.clone()
    }
    pub(crate) fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>> {
        read_node(&self.store.connection.lock(), &self.node, offset, length)
    }
    pub(crate) fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<usize> {
        self.update(|connection, node| write_node(connection, node, offset, bytes))?;
        Ok(bytes.len())
    }
    pub(crate) fn truncate(&mut self, size: u64) -> Result<()> {
        self.update(|connection, node| truncate_node(connection, node, size))
    }
    fn update(&mut self, update: impl FnOnce(&Connection, Node) -> Result<Node>) -> Result<()> {
        let mut connection = self.store.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let node = update(&transaction, self.node.clone())?;
        let root = node.save(&transaction)?;
        if transaction.execute(
            "UPDATE native_files SET root=?2 WHERE id=?1",
            params![self.pin, root.0],
        )? != 1
        {
            return Err(Error::Conflict("native file handle was released".into()));
        }
        transaction.commit()?;
        self.node = node;
        Ok(())
    }
}

#[cfg(any(windows, target_os = "linux"))]
impl Drop for DetachedFile {
    fn drop(&mut self) {
        // A failed cleanup conservatively retains the pin until exclusive host
        // recovery; collection must never guess that a handle's data is dead.
        let _ = self
            .store
            .connection
            .lock()
            .execute("DELETE FROM native_files WHERE id=?1", [&self.pin]);
    }
}

fn journal(connection: &Connection, outcome: &PublishOutcome, state: &str) -> Result<()> {
    connection.execute(
        "UPDATE merges SET state=?2,report=?3 WHERE id=?1",
        params![outcome.id, state, serde_json::to_string(outcome)?],
    )?;
    Ok(())
}

fn validate_branch(name: &str) -> Result<()> {
    validate_name(name)?;
    if name == DEFAULT_BRANCH {
        return Err(Error::Invalid(
            "default is the original directory, not an isolated branch".into(),
        ));
    }
    if history::is_version_id(name) {
        return Err(Error::Invalid(
            "branch name is reserved for immutable version IDs".into(),
        ));
    }
    Ok(())
}

fn reachable(transaction: &Connection, mut pending: Vec<ObjectId>) -> Result<BTreeSet<ObjectId>> {
    let mut live = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !live.insert(id.clone()) {
            continue;
        }
        let domain: u8 =
            transaction.query_row("SELECT domain FROM objects WHERE id=?1", [&id.0], |row| {
                row.get(0)
            })?;
        match domain {
            0 => {}
            1 => {
                let node = Node::load(transaction, &id)?;
                pending.extend(node.children.into_values());
                pending.extend(node.blocks);
            }
            _ => return Err(Error::Corrupt(id.0)),
        }
    }
    Ok(live)
}

fn project(connection: &Connection, origin: &Directory) -> Result<i64> {
    origin.check_binding()?;
    let path = origin
        .path()
        .to_str()
        .ok_or_else(|| Error::Invalid("workspace root must be Unicode".into()))?;
    if let Some((id, binding)) = connection
        .query_row(
            "SELECT id,binding FROM projects WHERE path=?1",
            [path],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if binding != origin.binding() {
            return Err(Error::Conflict("workspace directory was replaced".into()));
        }
        return Ok(id);
    }
    connection.execute(
        "INSERT INTO projects(path,binding) VALUES(?1,?2)",
        params![path, origin.binding()],
    )?;
    Ok(connection.last_insert_rowid())
}

fn roots(connection: &Connection, project: i64, name: &str) -> Result<(ObjectId, ObjectId)> {
    connection
        .prepare_cached("SELECT base,head FROM branches WHERE project=?1 AND name=?2")?
        .query_row(params![project, name], |row| {
            Ok((ObjectId(row.get(0)?), ObjectId(row.get(1)?)))
        })
        .optional()?
        .ok_or_else(|| missing("workspace branch"))
}

fn snapshot(
    connection: &Connection,
    origin: &Directory,
    project: i64,
    path: &str,
) -> Result<ObjectId> {
    capture::snapshot(connection, origin, project, path)
}

fn lookup(connection: &Connection, root: &ObjectId, path: &str) -> Result<Node> {
    lookup_node(root, path, |id| Node::load(connection, id))
}

fn lookup_node<N: std::borrow::Borrow<Node>>(
    root: &ObjectId,
    path: &str,
    mut load: impl FnMut(&ObjectId) -> Result<N>,
) -> Result<N> {
    relative_path(path)?;
    let mut node = load(root)?;
    for name in path.split('/').filter(|name| !name.is_empty()) {
        let current = node.borrow();
        if current.entry.kind != EntryKind::Directory {
            return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
        }
        node = load(current.children.get(name).ok_or_else(|| missing(path))?)?;
    }
    Ok(node)
}

fn lookup_optional(connection: &Connection, root: &ObjectId, path: &str) -> Result<Option<Node>> {
    match lookup(connection, root, path) {
        Ok(node) => Ok(Some(node)),
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn replace(
    connection: &Connection,
    root: &ObjectId,
    path: &str,
    value: Option<ObjectId>,
) -> Result<ObjectId> {
    relative_path(path)?;
    if path.is_empty() {
        return Err(Error::Invalid("cannot replace the workspace root".into()));
    }
    let mut directory = Node::load(connection, root)?;
    if directory.entry.kind != EntryKind::Directory {
        return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
    }
    let (name, rest) = path.split_once('/').unwrap_or((path, ""));
    validate_name(name)?;
    if rest.is_empty() {
        match value {
            Some(id) => {
                directory.children.insert(name.into(), id);
            }
            None => {
                directory
                    .children
                    .remove(name)
                    .ok_or_else(|| missing(path))?;
            }
        }
    } else {
        let child = directory.children.get(name).ok_or_else(|| missing(path))?;
        let next = replace(connection, child, rest, value)?;
        directory.children.insert(name.into(), next);
    }
    directory.entry.modified_ns = now_ns();
    directory.save(connection)
}

fn read_node(connection: &Connection, node: &Node, offset: u64, length: usize) -> Result<Vec<u8>> {
    if node.entry.kind != EntryKind::File {
        return Err(Error::Invalid("not a regular file".into()));
    }
    let count = (node.entry.size.saturating_sub(offset)).min(length as u64) as usize;
    let mut bytes = Vec::with_capacity(count);
    let mut position = offset;
    while bytes.len() < count {
        let block = node
            .blocks
            .get((position / BLOCK as u64) as usize)
            .ok_or_else(|| Error::Corrupt("file block is missing".into()))?;
        let data = get(connection, 0, block)?;
        let start = (position % BLOCK as u64) as usize;
        let stop = data.len().min(start + count - bytes.len());
        if stop <= start {
            return Err(Error::Corrupt("file block has an invalid size".into()));
        }
        bytes.extend_from_slice(&data[start..stop]);
        position += (stop - start) as u64;
    }
    Ok(bytes)
}

fn flatten(connection: &Connection, root: &ObjectId) -> Result<BTreeMap<String, Node>> {
    fn visit(
        connection: &Connection,
        id: &ObjectId,
        path: &str,
        output: &mut BTreeMap<String, Node>,
    ) -> Result<()> {
        let node = Node::load(connection, id)?;
        for (name, child) in &node.children {
            let path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            visit(connection, child, &path, output)?;
        }
        if !path.is_empty() {
            output.insert(path.into(), node);
        }
        Ok(())
    }
    let mut output = BTreeMap::new();
    visit(connection, root, "", &mut output)?;
    Ok(output)
}

fn equivalent(first: Option<&Node>, second: Option<&Node>) -> bool {
    match (first, second) {
        (None, None) => true,
        (Some(first), Some(second)) => first.same_contents(second),
        _ => false,
    }
}

fn changed_paths(first: &BTreeMap<String, Node>, second: &BTreeMap<String, Node>) -> Vec<String> {
    first
        .keys()
        .chain(second.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| !equivalent(first.get(*path), second.get(*path)))
        .cloned()
        .collect()
}

fn apply(
    connection: &Connection,
    origin: &Directory,
    project: i64,
    path: &str,
    expected: Option<&Node>,
    desired: Option<&Node>,
) -> Result<()> {
    let current = match snapshot(connection, origin, project, path) {
        Ok(id) => Some(Node::load(connection, &id)?),
        Err(Error::Io(error) | Error::Capture { source: error, .. })
            if error.kind() == io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    if !equivalent(current.as_ref(), expected) {
        return Err(Error::Conflict(format!(
            "target changed before writing {path}"
        )));
    }
    match desired {
        None => origin.remove(
            path,
            expected.is_some_and(|node| node.entry.kind == EntryKind::Directory),
        ),
        Some(node) => {
            if let Some(existing) = expected
                && existing.entry.kind != node.entry.kind
            {
                origin.remove(path, existing.entry.kind == EntryKind::Directory)?;
            }
            match node.entry.kind {
                EntryKind::File => origin.write_atomic(path, node.entry.mode, |file| {
                    use std::io::Write;
                    let mut remaining = node.entry.size;
                    for id in &node.blocks {
                        if remaining == 0 {
                            break;
                        }
                        origin.access(path, crate::Access::Write)?;
                        let bytes = get(connection, 0, id)?;
                        let count = remaining.min(bytes.len() as u64) as usize;
                        file.write_all(&bytes[..count])?;
                        remaining -= count as u64;
                    }
                    if remaining != 0 {
                        return Err(Error::Corrupt("file blocks do not cover its length".into()));
                    }
                    Ok(())
                }),
                EntryKind::Directory
                    if expected.is_none_or(|node| node.entry.kind != EntryKind::Directory) =>
                {
                    origin.mkdir(path, node.entry.mode)
                }
                EntryKind::Directory => origin.set_mode(path, node.entry.mode),
                EntryKind::Symlink => {
                    if expected.is_some_and(|node| node.entry.kind == EntryKind::Symlink) {
                        #[cfg(not(windows))]
                        origin.remove(path, false)?;
                        #[cfg(windows)]
                        origin.remove(
                            path,
                            expected
                                .and_then(|node| node.reparse.as_ref())
                                .is_some_and(|point| point.directory),
                        )?;
                    }
                    #[cfg(windows)]
                    if let Some(point) = &node.reparse {
                        return origin.write_reparse(path, point);
                    }
                    origin.symlink(
                        path,
                        node.target
                            .as_deref()
                            .ok_or_else(|| Error::Corrupt("symlink target is missing".into()))?,
                    )
                }
            }
        }
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_nanos().min(i64::MAX as u128) as i64
        })
}

fn write_node(connection: &Connection, mut node: Node, offset: u64, bytes: &[u8]) -> Result<Node> {
    if node.entry.kind != EntryKind::File {
        return Err(Error::Invalid("not a regular file".into()));
    }
    let end = offset
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| Error::Invalid("file offset overflow".into()))?;
    let size = node.entry.size.max(end);
    let previous_size = node.entry.size;
    let count = usize::try_from(size.div_ceil(BLOCK as u64))
        .map_err(|_| Error::Invalid("file size is not representable".into()))?;
    let zero = put(connection, 0, &vec![0; BLOCK])?;
    node.blocks.resize(count, zero);
    for index in 0..count {
        let block_start = (index as u64) * BLOCK as u64;
        let block_end = (block_start + BLOCK as u64).min(size);
        if block_end <= previous_size && (block_end <= offset || block_start >= end) {
            continue;
        }
        let mut block = get(connection, 0, &node.blocks[index])?;
        block.resize((block_end - block_start) as usize, 0);
        let start = offset.max(block_start);
        let stop = end.min(block_end);
        if stop > start {
            block[(start - block_start) as usize..(stop - block_start) as usize]
                .copy_from_slice(&bytes[(start - offset) as usize..(stop - offset) as usize]);
        }
        node.blocks[index] = put(connection, 0, &block)?;
    }
    node.entry.size = size;
    node.entry.modified_ns = now_ns();
    Ok(node)
}

fn truncate_node(connection: &Connection, mut node: Node, size: u64) -> Result<Node> {
    if node.entry.kind != EntryKind::File {
        return Err(Error::Invalid("not a regular file".into()));
    }
    let count = usize::try_from(size.div_ceil(BLOCK as u64))
        .map_err(|_| Error::Invalid("file size is not representable".into()))?;
    if count > node.blocks.len() {
        let zero = put(connection, 0, &vec![0; BLOCK])?;
        if let Some(last) = node.blocks.last_mut() {
            let mut bytes = get(connection, 0, last)?;
            bytes.resize(BLOCK, 0);
            *last = put(connection, 0, &bytes)?;
        }
        node.blocks.resize(count, zero);
    } else {
        node.blocks.truncate(count);
    }
    if let Some(last) = node.blocks.last_mut() {
        let mut bytes = get(connection, 0, last)?;
        bytes.resize(((size - 1) % BLOCK as u64 + 1) as usize, 0);
        *last = put(connection, 0, &bytes)?;
    }
    node.entry.size = size;
    node.entry.modified_ns = now_ns();
    Ok(node)
}
