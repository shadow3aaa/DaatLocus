//! Durable review scopes shared by every Actor on this device. App policy and
//! filesystem access are independent; review remains the only grant source.
use miette::{IntoDiagnostic, Result};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub trait ApprovalFingerprint {
    fn approval_fingerprint(&self) -> Result<ApprovalScope>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalScope {
    Filesystem {
        path: PathBuf,
        write: bool,
        recursive: bool,
    },
    App {
        namespace: String,
        policy: String,
        action: String,
    },
}

impl ApprovalScope {
    pub(crate) fn active_filesystem(&self) -> Option<(&std::path::Path, bool)> {
        let Self::Filesystem {
            path,
            write,
            recursive,
        } = self
        else {
            return None;
        };
        if !super::permissions::canonical_path(path).is_ok_and(|current| current == *path)
            || *recursive && path.exists() && !path.is_dir()
            || !recursive && path.is_dir()
        {
            return None;
        }
        Some((path, *write))
    }
    pub fn covers(&self, requested: &Self) -> bool {
        match (self, requested) {
            (
                Self::Filesystem {
                    path,
                    write,
                    recursive,
                },
                Self::Filesystem {
                    path: target,
                    write: target_write,
                    recursive: target_recursive,
                },
            ) => {
                (*write || !target_write)
                    && (path == target && (*recursive || !target_recursive)
                        || *recursive && target.starts_with(path))
            }
            (Self::App { .. }, Self::App { .. }) => self == requested,
            _ => false,
        }
    }

    pub fn key(&self) -> Result<String> {
        Ok(crate::stage::digest(
            &serde_json::to_vec(self).into_diagnostic()?,
        ))
    }

    pub fn filesystem(path: &std::path::Path, write: bool) -> Result<Self> {
        let path = super::permissions::canonical_path(path)?;
        let recursive = path.is_dir() || write && !path.exists();
        Ok(Self::Filesystem {
            path,
            write,
            recursive,
        })
    }
}

/// An App declares when review is necessary, what to evaluate, and the semantic
/// action that may safely reuse that decision. Policy changes invalidate reuse.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppReview {
    pub namespace: String,
    pub criteria: String,
    pub action: Value,
}

impl AppReview {
    pub fn new(namespace: impl Into<String>, criteria: impl Into<String>, action: Value) -> Self {
        Self {
            namespace: namespace.into(),
            criteria: criteria.into(),
            action,
        }
    }
}

impl ApprovalFingerprint for AppReview {
    fn approval_fingerprint(&self) -> Result<ApprovalScope> {
        Ok(ApprovalScope::App {
            namespace: self.namespace.clone(),
            policy: crate::stage::digest(self.criteria.as_bytes()),
            action: crate::stage::digest(&serde_json::to_vec(&self.action).into_diagnostic()?),
        })
    }
}

impl ApprovalFingerprint for ApprovalScope {
    fn approval_fingerprint(&self) -> Result<ApprovalScope> {
        Ok(self.clone())
    }
}

/// Every scope remembered on this device, regardless of which Actor earned it.
pub(crate) fn scopes(tx: &Transaction<'_>, _actor: &str) -> Result<Vec<ApprovalScope>> {
    let records = tx
        .prepare("SELECT record FROM actor_approval_scopes")
        .into_diagnostic()?
        .query_map([], |row| row.get::<_, String>(0))
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?;
    records
        .into_iter()
        .map(|record| serde_json::from_str(&record).into_diagnostic())
        .collect()
}

/// Remember approved scopes for the whole device. The earning Actor is kept as
/// attribution; every Actor on this device reuses the same scope set.
pub(crate) fn remember(
    tx: &Transaction<'_>,
    actor: &str,
    incoming: &[ApprovalScope],
) -> Result<()> {
    ensure_device_consolidation(tx)?;
    let mut saved = filesystem_scopes(tx, actor)?;
    for scope in incoming {
        if matches!(scope, ApprovalScope::App { .. }) {
            let key = scope.key()?;
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM actor_approval_scopes WHERE fingerprint=?1)",
                    params![key],
                    |row| row.get(0),
                )
                .into_diagnostic()?;
            if !exists {
                tx.execute(
                    "INSERT INTO actor_approval_scopes VALUES(?1,?2,?3)",
                    params![actor, key, serde_json::to_string(scope).into_diagnostic()?],
                )
                .into_diagnostic()?;
            }
            continue;
        }
        if saved.iter().any(|grant| grant.covers(scope)) {
            continue;
        }
        for old in saved.iter().filter(|old| scope.covers(old)) {
            tx.execute(
                "DELETE FROM actor_approval_scopes WHERE fingerprint=?1",
                params![old.key()?],
            )
            .into_diagnostic()?;
        }
        saved.retain(|old| !scope.covers(old));
        tx.execute(
            "INSERT INTO actor_approval_scopes VALUES(?1,?2,?3)",
            params![
                actor,
                scope.key()?,
                serde_json::to_string(scope).into_diagnostic()?
            ],
        )
        .into_diagnostic()?;
        saved.push(scope.clone());
    }
    Ok(())
}

/// Every filesystem scope remembered on this device, regardless of Actor.
pub(crate) fn filesystem_scopes(tx: &Transaction<'_>, _actor: &str) -> Result<Vec<ApprovalScope>> {
    let records = tx.prepare("SELECT record FROM actor_approval_scopes WHERE json_extract(record,'$.kind')='filesystem'").into_diagnostic()?
        .query_map([], |row| row.get::<_,String>(0)).into_diagnostic()?.collect::<rusqlite::Result<Vec<_>>>().into_diagnostic()?;
    records
        .into_iter()
        .map(|record| serde_json::from_str(&record).into_diagnostic())
        .collect()
}

/// Marker that the one-time per-Actor to device-wide scope consolidation ran.
const DEVICE_SCOPE_CONSOLIDATION_KEY: &str = "approval_scopes_shared_v1";

/// Merge legacy per-Actor scope rows into the single device scope set once.
pub(crate) fn ensure_device_consolidation(tx: &Transaction<'_>) -> Result<()> {
    let consolidated: Option<String> = tx
        .query_row(
            "SELECT value FROM stage_public_meta WHERE key=?1",
            params![DEVICE_SCOPE_CONSOLIDATION_KEY],
            |row| row.get(0),
        )
        .optional()
        .into_diagnostic()?;
    if consolidated.is_some() {
        return Ok(());
    }
    consolidate_device_scopes(tx)?;
    tx.execute(
        "INSERT OR REPLACE INTO stage_public_meta VALUES(?1,'1')",
        params![DEVICE_SCOPE_CONSOLIDATION_KEY],
    )
    .into_diagnostic()?;
    Ok(())
}

/// Reduce stored scope rows to one maximal device set: duplicate App
/// fingerprints and filesystem scopes covered by a broader sibling (or an
/// older duplicate) are removed, whichever Actor earned them.
pub(crate) fn consolidate_device_scopes(tx: &Transaction<'_>) -> Result<()> {
    let rows = tx
        .prepare("SELECT rowid, record FROM actor_approval_scopes")
        .into_diagnostic()?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?
        .into_iter()
        .map(|(rowid, record)| Ok((rowid, serde_json::from_str(&record).into_diagnostic()?)))
        .collect::<Result<Vec<(i64, ApprovalScope)>>>()?;
    let mut deletions = Vec::new();
    for (rowid, scope) in &rows {
        let redundant = rows.iter().any(|(other_rowid, other)| {
            other_rowid != rowid
                && other.covers(scope)
                && (!scope.covers(other) || other_rowid < rowid)
        });
        if redundant {
            deletions.push(*rowid);
        }
    }
    for rowid in deletions {
        tx.execute(
            "DELETE FROM actor_approval_scopes WHERE rowid=?1",
            params![rowid],
        )
        .into_diagnostic()?;
    }
    Ok(())
}

pub(crate) fn extend_permissions(
    permissions: &mut super::ActorPermissions,
    scopes: &[ApprovalScope],
) {
    for scope in scopes {
        if let Some((path, write)) = scope.active_filesystem() {
            permissions.readable_roots.push(path.into());
            if write {
                permissions.writable_roots.push(path.into());
            }
        }
    }
    permissions.readable_roots.sort();
    permissions.readable_roots.dedup();
    permissions.writable_roots.sort();
    permissions.writable_roots.dedup();
}

impl super::store::ActorStore {
    pub(crate) fn filesystem_approval_scopes(&self, actor: &str) -> Result<Vec<ApprovalScope>> {
        self.stage_transaction(|tx| {
            ensure_device_consolidation(tx)
                .map_err(|_| crate::stage::StageError::Invalid("Invalid device approval"))?;
            filesystem_scopes(tx, actor)
                .map_err(|_| crate::stage::StageError::Invalid("Invalid filesystem approval"))
        })
        .map_err(|error| miette::miette!("{error}"))
    }
    pub(crate) fn unapproved_scopes(
        &self,
        actor: &str,
        requested: Vec<ApprovalScope>,
    ) -> Result<Vec<ApprovalScope>> {
        self.stage_transaction(|tx| {
            ensure_device_consolidation(tx)
                .map_err(|_| crate::stage::StageError::Invalid("Invalid device approval"))?;
            let paths = if requested
                .iter()
                .any(|scope| matches!(scope, ApprovalScope::Filesystem { .. }))
            {
                filesystem_scopes(tx, actor)
                    .map_err(|_| crate::stage::StageError::Invalid("Invalid filesystem approval"))?
            } else {
                Vec::new()
            };
            let mut missing = Vec::new();
            for scope in requested {
                let granted = if matches!(scope, ApprovalScope::App { .. }) {
                    let key = scope
                        .key()
                        .map_err(|_| crate::stage::StageError::Invalid("Invalid App approval"))?;
                    tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM actor_approval_scopes WHERE fingerprint=?1)",
                        params![key],
                        |row| row.get(0),
                    )?
                } else {
                    paths.iter().any(|saved| saved.covers(&scope))
                };
                if !granted {
                    missing.push(scope);
                }
            }
            Ok(missing)
        })
        .map_err(|error| miette::miette!("{error}"))
    }
    pub fn approval_scopes(&self, actor: &str) -> Result<Vec<ApprovalScope>> {
        self.stage_transaction(|tx| {
            ensure_device_consolidation(tx)
                .map_err(|_| crate::stage::StageError::Invalid("Invalid device approval"))?;
            scopes(tx, actor)
                .map_err(|_| crate::stage::StageError::Invalid("Invalid remembered approval"))
        })
        .map_err(|error| miette::miette!("{error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::{
        ActorConfig, ActorPermissions, ActorRecord, ActorStatus, store::ActorStore,
    };

    fn actor(store: &ActorStore, id: &str, workspace: &std::path::Path) {
        store
            .save_actor(&ActorRecord {
                remote_read_only: false,
                id: id.into(),
                parent_id: None,
                depth: 0,
                config: ActorConfig {
                    name: id.into(),
                    workspace: workspace.into(),
                    apps: vec!["actors".into()],
                    model: None,
                    instructions: String::new(),
                    permissions: ActorPermissions::workspace(workspace.into()),
                },
                context: Default::default(),
                status: ActorStatus::Idle,
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .unwrap();
    }

    fn approve(store: &ActorStore, actor: &str, scopes: &[ApprovalScope]) {
        store
            .stage_transaction(|tx| {
                remember(tx, actor, scopes).unwrap();
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn directory_scopes_merge_descendants_without_granting_siblings_or_upgrading_reads() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let nested = project.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("source.txt");
        std::fs::write(&file, "source").unwrap();
        let store = ActorStore::open(&dir.path().join("store.db")).unwrap();
        actor(&store, "a", dir.path());
        let file_read = ApprovalScope::filesystem(&file, false).unwrap();
        approve(
            &store,
            "a",
            &[
                file_read.clone(),
                ApprovalScope::filesystem(&nested, false).unwrap(),
            ],
        );
        assert_eq!(store.approval_scopes("a").unwrap().len(), 1);
        let project_read = ApprovalScope::filesystem(&project, false).unwrap();
        approve(&store, "a", std::slice::from_ref(&project_read));
        assert_eq!(
            store.approval_scopes("a").unwrap().as_slice(),
            std::slice::from_ref(&project_read)
        );
        assert!(project_read.covers(&file_read));
        assert!(!project_read.covers(&ApprovalScope::filesystem(&file, true).unwrap()));
        assert!(!project_read.covers(&ApprovalScope::Filesystem {
            path: dir.path().join("project-other/file.txt"),
            write: false,
            recursive: false
        }));
        let project_write = ApprovalScope::filesystem(&project, true).unwrap();
        approve(&store, "a", std::slice::from_ref(&project_write));
        assert_eq!(
            store.approval_scopes("a").unwrap().as_slice(),
            std::slice::from_ref(&project_write)
        );
        assert!(
            project_write.covers(&ApprovalScope::Filesystem {
                path: super::super::permissions::canonical_path(&nested.join("new/deep/file.txt"))
                    .unwrap(),
                write: true,
                recursive: false
            })
        );
        // Scope merging also crosses Actors: a nested request through another
        // Actor is covered by the existing parent grant and adds no new row.
        approve(
            &store,
            "b",
            &[ApprovalScope::filesystem(&nested, false).unwrap()],
        );
        assert_eq!(
            store.approval_scopes("a").unwrap().as_slice(),
            std::slice::from_ref(&project_write)
        );
        assert_eq!(
            store.approval_scopes("b").unwrap().as_slice(),
            std::slice::from_ref(&project_write)
        );
    }

    #[test]
    fn device_scope_covers_future_children_without_a_new_review() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let nested = project.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let store = ActorStore::open(&dir.path().join("store.db")).unwrap();
        actor(&store, "a", dir.path());
        actor(&store, "b", dir.path());
        let parent_write = ApprovalScope::filesystem(&project, true).unwrap();
        approve(&store, "a", std::slice::from_ref(&parent_write));
        // A path that does not exist yet under the approved parent, requested by
        // a different Actor, is already covered and needs no further review.
        let future = ApprovalScope::Filesystem {
            path: super::super::permissions::canonical_path(&nested.join("new/deep/result.txt"))
                .unwrap(),
            write: true,
            recursive: false,
        };
        assert!(
            store
                .unapproved_scopes("b", vec![future])
                .unwrap()
                .is_empty()
        );
        // Siblings outside the granted subtree still require review.
        let sibling =
            ApprovalScope::filesystem(&dir.path().join("other/deep/result.txt"), true).unwrap();
        assert_eq!(
            store.unapproved_scopes("b", vec![sibling]).unwrap().len(),
            1
        );
    }

    #[test]
    fn legacy_scope_rows_consolidate_once_into_one_device_set() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let nested = project.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let store = ActorStore::open(&dir.path().join("store.db")).unwrap();
        actor(&store, "a", dir.path());
        actor(&store, "b", dir.path());
        let child = ApprovalScope::filesystem(&nested, false).unwrap();
        let parent = ApprovalScope::filesystem(&project, false).unwrap();
        let app = AppReview::new("mail/send/v1", "policy", serde_json::json!({"x": 1}))
            .approval_fingerprint()
            .unwrap();
        // Seed legacy per-Actor rows: a parent, nested children from two Actors,
        // and a duplicated App fingerprint.
        store
            .stage_transaction(|tx| {
                let insert = |owner: &str, scope: &ApprovalScope| {
                    tx.execute(
                        "INSERT INTO actor_approval_scopes VALUES(?1,?2,?3)",
                        rusqlite::params![
                            owner,
                            scope.key().unwrap(),
                            serde_json::to_string(scope).unwrap()
                        ],
                    )
                    .unwrap();
                };
                insert("a", &parent);
                insert("b", &child);
                insert("a", &child);
                insert("a", &app);
                insert("b", &app);
                Ok(())
            })
            .unwrap();
        // The first scope read migrates the legacy rows into one device set.
        let scopes = store.approval_scopes("b").unwrap();
        assert_eq!(scopes.len(), 2);
        assert!(scopes.contains(&parent));
        assert!(scopes.contains(&app));
        let total: i64 = store
            .stage_transaction(|tx| {
                Ok(tx
                    .query_row("SELECT COUNT(*) FROM actor_approval_scopes", [], |row| {
                        row.get(0)
                    })
                    .unwrap())
            })
            .unwrap();
        assert_eq!(total, 2);
        let marked: i64 = store
            .stage_transaction(|tx| {
                Ok(tx
                    .query_row(
                        "SELECT COUNT(*) FROM stage_public_meta WHERE key='approval_scopes_shared_v1'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap())
            })
            .unwrap();
        assert_eq!(marked, 1);
        // Consolidation is one-shot: later reads leave the reduced set alone.
        assert_eq!(store.approval_scopes("a").unwrap().len(), 2);
    }

    #[test]
    fn action_memory_is_device_shared_durable_and_reclaimed_with_actor_data() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("store.db");
        let store = ActorStore::open(&database).unwrap();
        actor(&store, "a", dir.path());
        actor(&store, "b", dir.path());
        let review = AppReview::new(
            "mail/send/v1",
            "Only send the requested message to the exact recipient.",
            serde_json::json!({"recipient":"alice","text":"hello"}),
        );
        let scope = review.approval_fingerprint().unwrap();
        approve(&store, "a", std::slice::from_ref(&scope));
        drop(store);
        let store = ActorStore::open(&database).unwrap();
        // Every Actor on this device reuses one scope set; the earning Actor
        // only stays as attribution on the row.
        assert_eq!(
            store.approval_scopes("a").unwrap().as_slice(),
            std::slice::from_ref(&scope)
        );
        assert_eq!(
            store.approval_scopes("b").unwrap().as_slice(),
            std::slice::from_ref(&scope)
        );
        let changed_target = AppReview::new(
            &review.namespace,
            &review.criteria,
            serde_json::json!({"recipient":"bob","text":"hello"}),
        )
        .approval_fingerprint()
        .unwrap();
        let changed_policy = AppReview::new(
            &review.namespace,
            "A revised policy requires another decision.",
            review.action.clone(),
        )
        .approval_fingerprint()
        .unwrap();
        assert!(!scope.covers(&changed_target));
        assert!(!scope.covers(&changed_policy));
        store
            .stage_transaction(|tx| {
                tx.execute("DELETE FROM actors WHERE id='a'", [])?;
                Ok(())
            })
            .unwrap();
        actor(&store, "a", dir.path());
        // Attribution rows cascade with their Actor: deleting the recording
        // Actor drops the scope from the device set until review grants it again.
        assert!(store.approval_scopes("a").unwrap().is_empty());
        assert!(store.approval_scopes("b").unwrap().is_empty());
    }
}
