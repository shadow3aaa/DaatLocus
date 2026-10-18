//! Durable, Actor-local review scopes. App policy and filesystem access are independent.
use miette::{IntoDiagnostic, Result};
use rusqlite::{Transaction, params};
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

pub(crate) fn scopes(tx: &Transaction<'_>, actor: &str) -> Result<Vec<ApprovalScope>> {
    let records = tx
        .prepare("SELECT record FROM actor_approval_scopes WHERE actor_id=?1")
        .into_diagnostic()?
        .query_map([actor], |row| row.get::<_, String>(0))
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()?;
    records
        .into_iter()
        .map(|record| serde_json::from_str(&record).into_diagnostic())
        .collect()
}

pub(crate) fn remember(
    tx: &Transaction<'_>,
    actor: &str,
    incoming: &[ApprovalScope],
) -> Result<()> {
    let mut saved = filesystem_scopes(tx, actor)?;
    for scope in incoming {
        if matches!(scope, ApprovalScope::App { .. }) {
            tx.execute("INSERT INTO actor_approval_scopes VALUES(?1,?2,?3) ON CONFLICT(actor_id,fingerprint) DO NOTHING", params![actor,scope.key()?,serde_json::to_string(scope).into_diagnostic()?]).into_diagnostic()?;
            continue;
        }
        if saved.iter().any(|grant| grant.covers(scope)) {
            continue;
        }
        for old in saved.iter().filter(|old| scope.covers(old)) {
            tx.execute(
                "DELETE FROM actor_approval_scopes WHERE actor_id=?1 AND fingerprint=?2",
                params![actor, old.key()?],
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

pub(crate) fn filesystem_scopes(tx: &Transaction<'_>, actor: &str) -> Result<Vec<ApprovalScope>> {
    let records = tx.prepare("SELECT record FROM actor_approval_scopes WHERE actor_id=?1 AND json_extract(record,'$.kind')='filesystem'").into_diagnostic()?
        .query_map([actor], |row| row.get::<_,String>(0)).into_diagnostic()?.collect::<rusqlite::Result<Vec<_>>>().into_diagnostic()?;
    records
        .into_iter()
        .map(|record| serde_json::from_str(&record).into_diagnostic())
        .collect()
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
            let paths = if requested.iter().any(|scope| matches!(scope,ApprovalScope::Filesystem { .. })) {
                filesystem_scopes(tx, actor).map_err(|_| crate::stage::StageError::Invalid("Invalid filesystem approval"))?
            } else { Vec::new() };
            let mut missing = Vec::new();
            for scope in requested {
                let granted = if matches!(scope,ApprovalScope::App { .. }) {
                    let key = scope.key().map_err(|_| crate::stage::StageError::Invalid("Invalid App approval"))?;
                    tx.query_row("SELECT EXISTS(SELECT 1 FROM actor_approval_scopes WHERE actor_id=?1 AND fingerprint=?2)", params![actor,key], |row| row.get(0))?
                } else { paths.iter().any(|saved| saved.covers(&scope)) };
                if !granted { missing.push(scope); }
            }
            Ok(missing)
        }).map_err(|error| miette::miette!("{error}"))
    }
    pub fn approval_scopes(&self, actor: &str) -> Result<Vec<ApprovalScope>> {
        self.stage_transaction(|tx| {
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
    }

    #[test]
    fn action_memory_is_actor_scoped_durable_and_reclaimed_with_actor_data() {
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
        assert_eq!(
            store.approval_scopes("a").unwrap().as_slice(),
            std::slice::from_ref(&scope)
        );
        assert!(store.approval_scopes("b").unwrap().is_empty());
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
        assert!(store.approval_scopes("a").unwrap().is_empty());
    }
}
