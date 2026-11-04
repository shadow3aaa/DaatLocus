use super::*;
use std::path::Path;

mod merging;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub id: String,
    pub parents: Vec<String>,
    pub message: String,
    pub created_ms: i64,
    pub cursor: i64,
    pub(super) root: ObjectId,
}

/// Select the conflict's path (and subtree) from a branch or immutable version.
/// None deletes the path. Resolve custom content by editing a working fork.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub path: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeOutcome {
    pub base: String,
    pub left: String,
    pub right: String,
    pub branch: Option<String>,
    pub version: Option<Version>,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishOutcome {
    pub id: String,
    pub base: String,
    pub source: String,
    pub target: String,
    pub version: Option<Version>,
    pub applied: Vec<String>,
    pub conflicts: Vec<String>,
    pub uncertain: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileVersion {
    pub entry: Entry,
    pub bytes: Vec<u8>,
    pub target: Option<String>,
}

pub(crate) fn migrate_versions(connection: &mut Connection) -> Result<()> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS versions(
            seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
            project INTEGER NOT NULL REFERENCES projects(id), root TEXT NOT NULL REFERENCES objects(id),
            first_parent TEXT REFERENCES versions(id), second_parent TEXT REFERENCES versions(id),
            message TEXT NOT NULL, created_ms INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS versions_project ON versions(project,seq);
         CREATE TABLE IF NOT EXISTS version_refs(
            project INTEGER NOT NULL REFERENCES projects(id), name TEXT NOT NULL,
            version TEXT NOT NULL REFERENCES versions(id), PRIMARY KEY(project,name));",
    )?;
    let format: i64 =
        transaction.query_row("SELECT version FROM workspace_format", [], |r| r.get(0))?;
    if format == 1 {
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS checkpoints(seq INTEGER PRIMARY KEY, project INTEGER, name TEXT, root TEXT, reason TEXT, created_ms INTEGER);")?;
        let branches: Vec<(i64, String, ObjectId, ObjectId)> = transaction
            .prepare("SELECT project,name,base,head FROM branches ORDER BY project,rowid")?
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    ObjectId(r.get(2)?),
                    ObjectId(r.get(3)?),
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut baselines: BTreeMap<(i64, ObjectId), String> = BTreeMap::new();
        let mut project_roots: BTreeMap<i64, String> = BTreeMap::new();
        for (project, name, base, head) in branches {
            let key = (project, base.clone());
            let baseline = if let Some(version) = baselines.get(&key) {
                version.clone()
            } else {
                // Legacy stores have no ancestry. An empty common base conservatively
                // reports conflicts instead of inventing an ordering of old snapshots.
                let parent = if let Some(id) = project_roots.get(&project) {
                    id.clone()
                } else {
                    let mut empty = Node::load(&transaction, &base)?;
                    empty.children.clear();
                    let root = empty.save(&transaction)?;
                    let id = insert_version(
                        &transaction,
                        project,
                        &root,
                        None,
                        None,
                        "Imported project",
                    )?
                    .id;
                    project_roots.insert(project, id.clone());
                    id
                };
                let version = insert_version(
                    &transaction,
                    project,
                    &base,
                    Some(&parent),
                    None,
                    "Imported baseline",
                )?;
                set_tip(&transaction, project, DEFAULT_BRANCH, &version.id)?;
                baselines.insert(key, version.id.clone());
                version.id
            };
            let checkpoints: Vec<(ObjectId, String)> = transaction
                .prepare(
                    "SELECT root,reason FROM checkpoints WHERE project=?1 AND name=?2 ORDER BY seq",
                )?
                .query_map(params![project, name], |r| {
                    Ok((ObjectId(r.get(0)?), r.get(1)?))
                })?
                .collect::<std::result::Result<_, _>>()?;
            let mut parent = baseline;
            for (root, reason) in checkpoints {
                parent =
                    insert_version(&transaction, project, &root, Some(&parent), None, &reason)?.id;
            }
            let version = if load_version(&transaction, project, &parent)?.root == head {
                parent
            } else {
                insert_version(
                    &transaction,
                    project,
                    &head,
                    Some(&parent),
                    None,
                    "Imported workspace",
                )?
                .id
            };
            set_tip(&transaction, project, &name, &version)?;
        }
        transaction
            .execute_batch("DROP TABLE checkpoints; UPDATE workspace_format SET version=2;")?;
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn project_at(connection: &Connection, path: &Path) -> Result<i64> {
    project_at_optional(connection, path)?.ok_or_else(|| missing("workspace project"))
}

fn project_at_optional(connection: &Connection, path: &Path) -> Result<Option<i64>> {
    Ok(connection
        .query_row(
            "SELECT id FROM projects WHERE path=?1",
            [path
                .to_str()
                .ok_or_else(|| Error::Invalid("project path must be Unicode".into()))?],
            |r| r.get(0),
        )
        .optional()?)
}

fn tip_optional(connection: &Connection, project: i64, name: &str) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT version FROM version_refs WHERE project=?1 AND name=?2",
            params![project, name],
            |r| r.get(0),
        )
        .optional()?)
}

pub(super) fn set_tip(connection: &Connection, project: i64, name: &str, id: &str) -> Result<()> {
    connection.execute("INSERT INTO version_refs VALUES(?1,?2,?3) ON CONFLICT(project,name) DO UPDATE SET version=excluded.version", params![project,name,id])?;
    Ok(())
}

fn load_version(connection: &Connection, project: i64, id: &str) -> Result<Version> {
    connection.query_row(
        "SELECT id,root,first_parent,second_parent,message,created_ms,seq FROM versions WHERE project=?1 AND id=?2",
        params![project,id], |r| Ok(Version {
            id:r.get(0)?, root:ObjectId(r.get(1)?),
            parents:[r.get::<_,Option<String>>(2)?,r.get(3)?].into_iter().flatten().collect(),
            message:r.get(4)?, created_ms:r.get(5)?, cursor:r.get(6)?,
        }),
    ).optional()?.ok_or_else(|| missing("workspace version in this project"))
}

fn insert_version(
    connection: &Connection,
    project: i64,
    root: &ObjectId,
    first: Option<&str>,
    second: Option<&str>,
    message: &str,
) -> Result<Version> {
    let id = format!("v-{}", uuid::Uuid::new_v4());
    let second = second.filter(|second| Some(*second) != first);
    connection.execute("INSERT INTO versions(id,project,root,first_parent,second_parent,message,created_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![id,project,root.0,first,second,message,now_ns()/1_000_000])?;
    load_version(connection, project, &id)
}

fn trees_equal(connection: &Connection, left: &ObjectId, right: &ObjectId) -> Result<bool> {
    if left == right {
        return Ok(true);
    }
    let left_node = Node::load(connection, left)?;
    let right_node = Node::load(connection, right)?;
    Ok(left_node.same_contents(&right_node)
        && changed_paths(&flatten(connection, left)?, &flatten(connection, right)?).is_empty())
}

pub(super) fn capture_default(
    connection: &Connection,
    origin: &Directory,
    project: i64,
    message: &str,
) -> Result<Version> {
    let root = snapshot(connection, origin, project, "")?;
    let previous = tip_optional(connection, project, DEFAULT_BRANCH)?;
    if let Some(id) = &previous {
        let version = load_version(connection, project, id)?;
        if trees_equal(connection, &root, &version.root)? {
            return Ok(version);
        }
    }
    let version = insert_version(
        connection,
        project,
        &root,
        previous.as_deref(),
        None,
        message,
    )?;
    set_tip(connection, project, DEFAULT_BRANCH, &version.id)?;
    Ok(version)
}

pub(super) fn save_branch(
    connection: &Connection,
    project: i64,
    name: &str,
    message: &str,
) -> Result<Version> {
    let previous =
        tip_optional(connection, project, name)?.ok_or_else(|| missing("workspace branch"))?;
    let version = load_version(connection, project, &previous)?;
    let head = roots(connection, project, name)?.1;
    if trees_equal(connection, &head, &version.root)? {
        return Ok(version);
    }
    let version = insert_version(connection, project, &head, Some(&previous), None, message)?;
    set_tip(connection, project, name, &version.id)?;
    Ok(version)
}

fn resolve_saved(
    connection: &Connection,
    project: i64,
    reference: &str,
    message: &str,
) -> Result<Version> {
    if is_version_id(reference) {
        load_version(connection, project, reference)
    } else if reference == DEFAULT_BRANCH {
        let id = tip_optional(connection, project, reference)?
            .ok_or_else(|| missing("captured native directory"))?;
        load_version(connection, project, &id)
    } else {
        save_branch(connection, project, reference, message)
    }
}

fn resolve_root(connection: &Connection, project: i64, reference: &str) -> Result<ObjectId> {
    if is_version_id(reference) {
        return Ok(load_version(connection, project, reference)?.root);
    }
    if reference == DEFAULT_BRANCH {
        let id = tip_optional(connection, project, reference)?
            .ok_or_else(|| missing("captured native directory"))?;
        return Ok(load_version(connection, project, &id)?.root);
    }
    Ok(roots(connection, project, reference)?.1)
}

pub(super) fn is_version_id(reference: &str) -> bool {
    reference
        .strip_prefix("v-")
        .is_some_and(|id| uuid::Uuid::try_parse(id).is_ok())
}

fn insert_branch(
    connection: &Connection,
    project: i64,
    name: &str,
    version: &Version,
) -> Result<()> {
    validate_branch(name)?;
    if tip_optional(connection, project, name)?.is_some() {
        return Err(Error::Conflict(format!(
            "workspace branch already exists: {name}"
        )));
    }
    connection.execute(
        "INSERT INTO branches(project,name,base,head,generation) VALUES(?1,?2,?3,?3,?4)",
        params![
            project,
            name,
            version.root.0,
            next_inode(connection)? as i64
        ],
    )?;
    set_tip(connection, project, name, &version.id)
}

impl Workspace {
    pub fn commit(&self, message: &str) -> Result<Version> {
        let mut connection = self.store.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        self.roots(&transaction)?;
        let version = save_branch(&transaction, self.project, &self.name, message)?;
        transaction.commit()?;
        Ok(version)
    }

    pub fn publish(&self, origin: &Directory) -> Result<PublishOutcome> {
        if origin.path() != self.origin || origin.binding() != self.binding {
            return Err(Error::Conflict(
                "publish target directory identity changed".into(),
            ));
        }
        self.store
            .publish_checked(origin, &self.name, None, &[], Some(self))
    }
}

impl Store {
    pub fn commit_default(&self, origin: &Directory, message: &str) -> Result<Version> {
        if self.path().starts_with(origin.path()) {
            return Err(Error::Invalid(
                "a snapshot source cannot contain its content store".into(),
            ));
        }
        let mut connection = self.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let project = project(&transaction, origin)?;
        let version = capture_default(&transaction, origin, project, message)?;
        transaction.commit()?;
        Ok(version)
    }

    /// Fork captures a dirty source once; the new branch never shares mutable state.
    pub fn fork_at(&self, path: &Path, name: &str, reference: &str) -> Result<Workspace> {
        validate_branch(name)?;
        let mut connection = self.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let project = project_at(&transaction, path)?;
        let version = resolve_saved(&transaction, project, reference, "Fork workspace")?;
        insert_branch(&transaction, project, name, &version)?;
        transaction.commit()?;
        drop(connection);
        self.open_recorded_branch(path, name)
    }

    /// History walks both parents. The sequence cursor stays stable as new versions arrive.
    pub fn history(
        &self,
        path: &Path,
        reference: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Version>> {
        let connection = self.connection.lock();
        let Some(project) = project_at_optional(&connection, path)? else {
            return if reference == DEFAULT_BRANCH {
                Ok(vec![])
            } else {
                Err(missing("workspace reference"))
            };
        };
        let id = if is_version_id(reference) {
            reference.to_owned()
        } else {
            match tip_optional(&connection, project, reference)? {
                Some(id) => id,
                None if reference == DEFAULT_BRANCH => return Ok(vec![]),
                None => return Err(missing("workspace reference")),
            }
        };
        load_version(&connection, project, &id)?;
        let mut query = connection.prepare(
            "WITH RECURSIVE ancestors(id) AS (
                SELECT ?2 UNION SELECT v.first_parent FROM versions v JOIN ancestors a ON a.id=v.id WHERE v.project=?1 AND v.first_parent IS NOT NULL
                UNION SELECT v.second_parent FROM versions v JOIN ancestors a ON a.id=v.id WHERE v.project=?1 AND v.second_parent IS NOT NULL)
             SELECT v.id FROM versions v JOIN ancestors a ON a.id=v.id WHERE v.project=?1 AND (?3 IS NULL OR v.seq<?3) ORDER BY v.seq DESC LIMIT ?4"
        )?;
        let ids = query
            .query_map(params![project, id, before, limit.min(201) as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| load_version(&connection, project, id))
            .collect()
    }

    /// Default denotes the last explicit native capture; live branches denote their current tree.
    pub fn diff(
        &self,
        path: &Path,
        left: &str,
        right: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Change>> {
        let connection = self.connection.lock();
        let project = project_at(&connection, path)?;
        let before = flatten(&connection, &resolve_root(&connection, project, left)?)?;
        let next = flatten(&connection, &resolve_root(&connection, project, right)?)?;
        Ok(changed_paths(&before, &next)
            .into_iter()
            .filter(|path| after.is_none_or(|cursor| path.as_str() > cursor))
            .take(limit.min(201))
            .map(|path| Change {
                kind: if !before.contains_key(&path) {
                    ChangeKind::Added
                } else if !next.contains_key(&path) {
                    ChangeKind::Deleted
                } else {
                    ChangeKind::Modified
                },
                entry: next.get(&path).map(|node| node.entry.clone()),
                path,
            })
            .collect())
    }

    pub fn read_version(
        &self,
        path: &Path,
        reference: &str,
        file: &str,
        offset: u64,
        length: usize,
    ) -> Result<FileVersion> {
        let connection = self.connection.lock();
        let project = project_at(&connection, path)?;
        let node = lookup(
            &connection,
            &resolve_root(&connection, project, reference)?,
            file,
        )?;
        let bytes = if node.entry.kind == EntryKind::File {
            read_node(&connection, &node, offset, length.min(32 * 1024))?
        } else {
            vec![]
        };
        Ok(FileVersion {
            entry: node.entry,
            bytes,
            target: node.target,
        })
    }

    /// Merge captures both inputs and writes only a fresh result branch.
    pub fn merge(
        &self,
        path: &Path,
        left: &str,
        right: &str,
        name: &str,
        message: &str,
        resolutions: &[Resolution],
    ) -> Result<MergeOutcome> {
        validate_branch(name)?;
        if !resolutions.is_empty() && (!is_version_id(left) || !is_version_id(right)) {
            return Err(Error::Invalid(
                "resolve conflicts against the fixed left and right version IDs".into(),
            ));
        }
        let mut connection = self.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let project = project_at(&transaction, path)?;
        if tip_optional(&transaction, project, name)?.is_some() {
            return Err(Error::Conflict(format!(
                "workspace branch already exists: {name}"
            )));
        }
        let left = resolve_saved(&transaction, project, left, "Capture merge source")?;
        let right = resolve_saved(&transaction, project, right, "Capture merge source")?;
        let base = merging::merge_base(&transaction, project, &left, &right)?;
        let (root, conflicts) =
            merging::merge_trees(&transaction, project, &base, &left, &right, resolutions)?;
        let version = if conflicts.is_empty() {
            let version = if base.id == left.id && trees_equal(&transaction, &root, &right.root)? {
                right.clone()
            } else if base.id == right.id && trees_equal(&transaction, &root, &left.root)? {
                left.clone()
            } else {
                insert_version(
                    &transaction,
                    project,
                    &root,
                    Some(&left.id),
                    Some(&right.id),
                    message,
                )?
            };
            insert_branch(&transaction, project, name, &version)?;
            Some(version)
        } else {
            None
        };
        let result = MergeOutcome {
            base: base.id,
            left: left.id,
            right: right.id,
            branch: version.as_ref().map(|_| name.to_owned()),
            version,
            conflicts,
        };
        transaction.commit()?;
        Ok(result)
    }

    /// Native publication is journaled per pathname, and interrupted effects are never replayed.
    pub fn publish(
        &self,
        origin: &Directory,
        reference: &str,
        expected_target: Option<&str>,
        resolutions: &[Resolution],
    ) -> Result<PublishOutcome> {
        self.publish_checked(origin, reference, expected_target, resolutions, None)
    }

    fn publish_checked(
        &self,
        origin: &Directory,
        reference: &str,
        expected_target: Option<&str>,
        resolutions: &[Resolution],
        workspace: Option<&Workspace>,
    ) -> Result<PublishOutcome> {
        if !resolutions.is_empty() && (expected_target.is_none() || !is_version_id(reference)) {
            return Err(Error::Invalid(
                "resolve publication conflicts using the fixed source and target version IDs"
                    .into(),
            ));
        }
        let mut connection = self.connection.lock();
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(workspace) = workspace {
            workspace.roots(&transaction)?;
        }
        let project = project(&transaction, origin)?;
        let source = resolve_saved(
            &transaction,
            project,
            reference,
            "Capture published workspace",
        )?;
        let target = capture_default(&transaction, origin, project, "Capture native edits")?;
        if let Some(expected) = expected_target {
            let expected = load_version(&transaction, project, expected)?;
            if !trees_equal(&transaction, &expected.root, &target.root)? {
                return Err(Error::Conflict("native target changed after the conflict; inspect a fresh publication before resolving".into()));
            }
        }
        let base = merging::merge_base(&transaction, project, &source, &target)?;
        let (root, conflicts) =
            merging::merge_trees(&transaction, project, &base, &source, &target, resolutions)?;
        let integrated = base.id == source.id;
        let mut outcome = PublishOutcome {
            id: uuid::Uuid::new_v4().to_string(),
            base: base.id,
            source: source.id.clone(),
            target: target.id.clone(),
            version: None,
            applied: vec![],
            conflicts,
            uncertain: None,
            error: None,
        };
        transaction.execute(
            "INSERT INTO merges VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                outcome.id,
                project,
                reference,
                root.0,
                if outcome.conflicts.is_empty() {
                    "prepared"
                } else {
                    "conflict"
                },
                serde_json::to_string(&outcome)?
            ],
        )?;
        transaction.commit()?;
        if !outcome.conflicts.is_empty() {
            return Ok(outcome);
        }
        let before = flatten(&connection, &target.root)?;
        let after = flatten(&connection, &root)?;
        let mut paths = changed_paths(&before, &after);
        let preflight = (|| -> Result<()> {
            for path in &paths {
                origin.access(path, crate::Access::Write)?;
                if after
                    .get(path)
                    .is_some_and(|node| node.entry.kind == EntryKind::File)
                {
                    origin.access(
                        path.rsplit_once('/').map_or("", |(parent, _)| parent),
                        crate::Access::Write,
                    )?;
                }
            }
            Ok(())
        })();
        if let Err(error) = preflight {
            journal(&connection, &outcome, "refused")?;
            return Err(error);
        }
        paths.sort_by_key(|path| {
            let deleting = !after.contains_key(path);
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
            if let Err(error) = apply(
                &connection,
                origin,
                project,
                &path,
                before.get(&path),
                after.get(&path),
            ) {
                outcome.error = Some(error.to_string());
                journal(&connection, &outcome, "interrupted")?;
                return Ok(outcome);
            }
            outcome.applied.push(path);
            outcome.uncertain = None;
            journal(&connection, &outcome, "applying")?;
        }
        // Capture unrelated concurrent native edits too, then verify the published write set.
        let finish = (|| -> Result<Version> {
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let actual = snapshot(&transaction, origin, project, "")?;
            let actual_entries = flatten(&transaction, &actual)?;
            for path in &outcome.applied {
                if !equivalent(actual_entries.get(path), after.get(path)) {
                    return Err(Error::Conflict(format!(
                        "native file changed during publication: {path}"
                    )));
                }
            }
            let version = if integrated && trees_equal(&transaction, &actual, &target.root)? {
                target.clone()
            } else {
                insert_version(
                    &transaction,
                    project,
                    &actual,
                    Some(&target.id),
                    Some(&source.id),
                    "Publish workspace",
                )?
            };
            set_tip(&transaction, project, DEFAULT_BRANCH, &version.id)?;
            transaction.execute("UPDATE branches SET base=?3,revision=revision+1 WHERE project=?1 AND name=?2 AND head=?3",params![project,reference,source.root.0])?;
            transaction.commit()?;
            Ok(version)
        })();
        match finish {
            Ok(version) => {
                outcome.version = Some(version);
                journal(&connection, &outcome, "completed")?;
            }
            Err(error) => {
                outcome.error = Some(error.to_string());
                journal(&connection, &outcome, "interrupted")?;
            }
        }
        Ok(outcome)
    }
}
