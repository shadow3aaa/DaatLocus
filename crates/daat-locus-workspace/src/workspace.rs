use crate::directory::{relative_path, validate_name};
use crate::store::{get, next_inode, put};
use crate::{DEFAULT_BRANCH, Directory, Error, ObjectId, Result, Store, missing};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    time::{SystemTime, UNIX_EPOCH},
};

const BLOCK: usize = 64 * 1024;

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
struct Node {
    entry: Entry,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<ObjectId>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    children: BTreeMap<String, ObjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeOutcome {
    pub id: String,
    pub applied: Vec<String>,
    pub conflicts: Vec<String>,
    pub uncertain: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct Workspace {
    store: Store,
    origin: std::path::PathBuf,
    binding: String,
    project: i64,
    name: String,
}

impl Store {
    /// Reclaim obsolete blocks and trees while retaining every live branch,
    /// checkpoint, unfinished merge and native unlinked-file handle.
    pub fn collect_garbage(&self) -> Result<crate::Collection> {
        let mut connection = self.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut pending: Vec<ObjectId> = transaction.prepare(
            "SELECT base FROM branches UNION SELECT head FROM branches UNION SELECT root FROM checkpoints UNION SELECT head FROM merges WHERE state IN ('prepared','applying','interrupted') UNION SELECT root FROM native_files"
        )?.query_map([], |row| Ok(ObjectId(row.get(0)?)))?.collect::<std::result::Result<_, _>>()?;
        let mut live = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !live.insert(id.clone()) {
                continue;
            }
            let domain: u8 = transaction.query_row(
                "SELECT domain FROM objects WHERE id=?1",
                [&id.0],
                |row| row.get(0),
            )?;
            match domain {
                0 => {}
                1 => {
                    let node = Node::load(&transaction, &id)?;
                    pending.extend(node.children.into_values());
                    pending.extend(node.blocks);
                }
                _ => return Err(Error::Corrupt(id.0)),
            }
        }
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
        validate_branch(name)?;
        let connection = self.connection.lock();
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
        roots(&connection, project, name)?;
        Ok(Workspace {
            store: self.clone(),
            origin: path.to_owned(),
            binding,
            project,
            name: name.into(),
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
            let (base, head) = if from == DEFAULT_BRANCH {
                let root = snapshot(&transaction, &origin, project, "")?;
                (root.clone(), root)
            } else {
                roots(&transaction, project, from)?
            };
            transaction.execute(
                "INSERT INTO branches(project,name,base,head) VALUES(?1,?2,?3,?4)",
                params![project, name, base.0, head.0],
            )?;
        }
        transaction.commit()?;
        Ok(Workspace {
            store: self.clone(),
            origin: origin.path().to_owned(),
            binding: origin.binding(),
            project,
            name: name.into(),
        })
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
        let connection = self.connection.lock();
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
    }
}

impl Workspace {
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
        let root = roots(&transaction, self.project, &self.name)?.1;
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
        format!("{}:{}", self.project, self.name)
    }
    pub fn root(&self) -> Result<ObjectId> {
        Ok(roots(&self.store.connection.lock(), self.project, &self.name)?.1)
    }
    pub fn fork(&self, name: &str) -> Result<Self> {
        validate_branch(name)?;
        let mut connection = self.store.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (base, head) = roots(&transaction, self.project, &self.name)?;
        transaction.execute(
            "INSERT OR IGNORE INTO branches(project,name,base,head) VALUES(?1,?2,?3,?4)",
            params![self.project, name, base.0, head.0],
        )?;
        transaction.commit()?;
        let mut workspace = self.clone();
        workspace.name = name.into();
        Ok(workspace)
    }
    pub fn entry(&self, path: &str) -> Result<Entry> {
        let connection = self.store.connection.lock();
        let root = roots(&connection, self.project, &self.name)?.1;
        Ok(lookup(&connection, &root, path)?.entry)
    }
    pub fn read_dir(&self, path: &str) -> Result<Vec<(String, Entry)>> {
        let connection = self.store.connection.lock();
        let root = roots(&connection, self.project, &self.name)?.1;
        let directory = lookup(&connection, &root, path)?;
        if directory.entry.kind != EntryKind::Directory {
            return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
        }
        directory
            .children
            .iter()
            .map(|(name, id)| Ok((name.clone(), Node::load(&connection, id)?.entry)))
            .collect()
    }
    pub fn read(&self, path: &str, offset: u64, length: usize) -> Result<Vec<u8>> {
        let connection = self.store.connection.lock();
        let root = roots(&connection, self.project, &self.name)?.1;
        read_node(
            &connection,
            &lookup(&connection, &root, path)?,
            offset,
            length,
        )
    }
    pub fn read_link(&self, path: &str) -> Result<String> {
        let connection = self.store.connection.lock();
        let root = roots(&connection, self.project, &self.name)?.1;
        lookup(&connection, &root, path)?
            .target
            .ok_or_else(|| Error::Invalid("not a symlink".into()))
    }
    fn mutate<T>(
        &self,
        change: impl FnOnce(&Connection, &ObjectId) -> Result<(ObjectId, T)>,
    ) -> Result<T> {
        let mut connection = self.store.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let root = roots(&transaction, self.project, &self.name)?.1;
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
        let connection = self.store.connection.lock();
        let (_, root) = roots(&connection, self.project, &self.name)?;
        let node = lookup(&connection, &root, path)?;
        if node.entry.kind != EntryKind::Symlink {
            return Ok(None);
        }
        let point = match node.reparse {
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
    pub fn checkpoint(&self, reason: &str) -> Result<ObjectId> {
        let connection = self.store.connection.lock();
        let root = roots(&connection, self.project, &self.name)?.1;
        connection.execute(
            "INSERT INTO checkpoints(project,name,root,reason,created_ms) VALUES(?1,?2,?3,?4,?5)",
            params![
                self.project,
                self.name,
                root.0,
                reason,
                now_ns() / 1_000_000
            ],
        )?;
        Ok(root)
    }
    pub fn changes(&self, offset: usize, limit: usize) -> Result<Vec<Change>> {
        let connection = self.store.connection.lock();
        let (base, head) = roots(&connection, self.project, &self.name)?;
        let before = flatten(&connection, &base)?;
        let after = flatten(&connection, &head)?;
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
    }

    /// Merge is an explicit host effect. Its durable journal records each
    /// started pathname before I/O, and never replays an unfinished effect.
    pub fn merge_to_default(&self, origin: &Directory) -> Result<MergeOutcome> {
        if origin.path() != self.origin || origin.binding() != self.binding {
            return Err(Error::Conflict(
                "merge target directory identity changed".into(),
            ));
        }
        let connection = self.store.connection.lock();
        let (base_root, head_root) = roots(&connection, self.project, &self.name)?;
        let base = flatten(&connection, &base_root)?;
        let ours = flatten(&connection, &head_root)?;
        let target_root = snapshot(&connection, origin, self.project, "")?;
        let theirs = flatten(&connection, &target_root)?;
        let mut outcome = MergeOutcome {
            id: uuid::Uuid::new_v4().to_string(),
            applied: vec![],
            conflicts: vec![],
            uncertain: None,
            error: None,
        };
        let mut desired = BTreeMap::new();
        for path in changed_paths(&base, &ours) {
            let before = base.get(&path);
            let source = ours.get(&path);
            let target = theirs.get(&path);
            if equivalent(source, target) {
                continue;
            }
            if equivalent(before, target) {
                if source.is_none_or(|node| node.entry.kind != EntryKind::Directory)
                    && before.is_some_and(|node| node.entry.kind == EntryKind::Directory)
                    && changed_paths(&base, &theirs)
                        .iter()
                        .any(|child| child.starts_with(&format!("{path}/")))
                {
                    outcome.conflicts.push(path);
                } else {
                    desired.insert(path, source.cloned());
                }
            } else if let (Some(before), Some(source), Some(target)) = (before, source, target) {
                if let Some(merged) = merge_text(&connection, before, source, target)? {
                    desired.insert(path, Some(merged));
                } else {
                    outcome.conflicts.push(path);
                }
            } else {
                outcome.conflicts.push(path);
            }
        }
        connection.execute(
            "INSERT INTO merges VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                outcome.id,
                self.project,
                self.name,
                head_root.0,
                if outcome.conflicts.is_empty() {
                    "prepared"
                } else {
                    "conflict"
                },
                serde_json::to_string(&outcome)?
            ],
        )?;
        if !outcome.conflicts.is_empty() {
            return Ok(outcome);
        }
        // Check the complete write set before changing any user files. The
        // individual I/O operations repeat these checks; a later revocation or
        // host failure is retained as a partial/uncertain merge in the journal.
        let preflight = (|| -> Result<()> {
            for (path, node) in &desired {
                origin.access(path, crate::Access::Write)?;
                if node
                    .as_ref()
                    .is_some_and(|node| node.entry.kind == EntryKind::File)
                {
                    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                    origin.access(parent, crate::Access::Write)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = preflight {
            journal(&connection, &outcome, "refused")?;
            return Err(error);
        }
        let mut paths: Vec<_> = desired.keys().cloned().collect();
        paths.sort_by_key(|path| {
            let deleting = desired[path].is_none();
            (
                !deleting,
                if deleting {
                    usize::MAX - path.matches('/').count()
                } else {
                    path.matches('/').count()
                },
                path.clone(),
            )
        });
        for path in paths {
            outcome.uncertain = Some(path.clone());
            journal(&connection, &outcome, "applying")?;
            let result = apply(
                &connection,
                origin,
                self.project,
                &path,
                theirs.get(&path),
                desired[&path].as_ref(),
            );
            if let Err(error) = result {
                outcome.error = Some(error.to_string());
                journal(&connection, &outcome, "interrupted")?;
                return Ok(outcome);
            }
            outcome.applied.push(path);
            outcome.uncertain = None;
            journal(&connection, &outcome, "applying")?;
        }
        // Publishing a branch does not import concurrent native-directory
        // changes back into its mounted namespace. Its integrated head becomes
        // the next merge base, preserving open handles and snapshot isolation.
        connection.execute("UPDATE branches SET base=?3,revision=revision+1 WHERE project=?1 AND name=?2 AND head=?3",
            params![self.project, self.name, head_root.0])?;
        journal(&connection, &outcome, "completed")?;
        Ok(outcome)
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

fn journal(connection: &Connection, outcome: &MergeOutcome, state: &str) -> Result<()> {
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
    Ok(())
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
        .query_row(
            "SELECT base,head FROM branches WHERE project=?1 AND name=?2",
            params![project, name],
            |row| Ok((ObjectId(row.get(0)?), ObjectId(row.get(1)?))),
        )
        .optional()?
        .ok_or_else(|| missing("workspace branch"))
}

fn snapshot(
    connection: &Connection,
    origin: &Directory,
    project: i64,
    path: &str,
) -> Result<ObjectId> {
    let metadata = origin.metadata(path)?;
    let inode = match connection
        .query_row(
            "SELECT inode FROM source_inodes WHERE project=?1 AND path=?2",
            params![project, path],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .optional()?
    {
        Some(inode) => inode,
        None => {
            let inode = next_inode(connection)?;
            connection.execute(
                "INSERT INTO source_inodes VALUES(?1,?2,?3)",
                params![project, path, inode as i64],
            )?;
            inode
        }
    };
    let kind = if metadata.is_symlink() {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        return Err(Error::Unsupported(format!(
            "special filesystem entry {path}"
        )));
    };
    #[cfg(unix)]
    let mode = {
        use cap_std::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = if metadata.permissions().readonly() {
        0o444
    } else if kind == EntryKind::Directory {
        0o755
    } else {
        0o644
    };
    let modified_ns = metadata
        .modified()?
        .into_std()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos().min(i64::MAX as u128) as i64);
    let mut node = Node {
        entry: Entry {
            inode,
            kind,
            size: 0,
            mode,
            modified_ns,
        },
        blocks: vec![],
        children: BTreeMap::new(),
        target: None,
        #[cfg(windows)]
        reparse: None,
    };
    match kind {
        EntryKind::Directory => {
            for name in origin.names(path)? {
                let child = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                };
                node.children
                    .insert(name, snapshot(connection, origin, project, &child)?);
            }
        }
        EntryKind::File => {
            node.entry.size = origin.read_chunks(path, |chunk| {
                node.blocks.push(put(connection, 0, chunk)?);
                Ok(())
            })?;
        }
        EntryKind::Symlink => {
            #[cfg(not(windows))]
            let target = origin.read_link(path)?;
            #[cfg(windows)]
            let target = {
                let point = origin.read_reparse(path)?;
                let target = point.target()?;
                node.reparse = Some(point);
                target
            };
            node.entry.size = target.len() as u64;
            node.target = Some(target);
        }
    }
    node.save(connection)
}

fn lookup(connection: &Connection, root: &ObjectId, path: &str) -> Result<Node> {
    relative_path(path)?;
    let mut node = Node::load(connection, root)?;
    for name in path.split('/').filter(|name| !name.is_empty()) {
        if node.entry.kind != EntryKind::Directory {
            return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
        }
        node = Node::load(
            connection,
            node.children.get(name).ok_or_else(|| missing(path))?,
        )?;
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

fn merge_text(
    connection: &Connection,
    base: &Node,
    ours: &Node,
    theirs: &Node,
) -> Result<Option<Node>> {
    if [base, ours, theirs]
        .iter()
        .any(|node| node.entry.kind != EntryKind::File)
    {
        return Ok(None);
    }
    if ours.entry.mode != base.entry.mode
        && theirs.entry.mode != base.entry.mode
        && ours.entry.mode != theirs.entry.mode
    {
        return Ok(None);
    }
    let texts = [base, ours, theirs].map(|node| read_node(connection, node, 0, usize::MAX));
    let [base_bytes, our_bytes, their_bytes] = texts;
    let (base_bytes, our_bytes, their_bytes) = (base_bytes?, our_bytes?, their_bytes?);
    if [&base_bytes, &our_bytes, &their_bytes]
        .iter()
        .any(|bytes| bytes.contains(&0))
    {
        return Ok(None);
    }
    let (Ok(base_text), Ok(our_text), Ok(their_text)) = (
        std::str::from_utf8(&base_bytes),
        std::str::from_utf8(&our_bytes),
        std::str::from_utf8(&their_bytes),
    ) else {
        return Ok(None);
    };
    let Ok(merged) = diffy::merge(base_text, our_text, their_text) else {
        return Ok(None);
    };
    let mut node = ours.clone();
    if ours.entry.mode == base.entry.mode {
        node.entry.mode = theirs.entry.mode;
    }
    node.entry.size = merged.len() as u64;
    node.blocks = merged
        .as_bytes()
        .chunks(BLOCK)
        .map(|bytes| put(connection, 0, bytes))
        .collect::<Result<_>>()?;
    Ok(Some(node))
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
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
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
