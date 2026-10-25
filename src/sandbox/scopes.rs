//! Device-local filesystem authority. Actor identity is never a grant key.
use miette::{IntoDiagnostic, Result};
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Exact filesystem scope proposed at an actual boundary or by an explicit application.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SandboxScopeRequest {
    pub readable_roots: Vec<PathBuf>,
    pub writable_roots: Vec<PathBuf>,
}

impl SandboxScopeRequest {
    pub fn canonicalized(mut self) -> Result<Self> {
        for roots in [&mut self.readable_roots, &mut self.writable_roots] {
            *roots = roots
                .iter()
                .map(|path| crate::actors::permissions::canonical_path(path))
                .collect::<Result<_>>()?;
            roots.sort();
            roots.dedup();
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxScope {
    pub path: PathBuf,
    pub write: bool,
    pub recursive: bool,
}

impl SandboxScope {
    pub fn filesystem(path: &Path, write: bool) -> Result<Self> {
        let path = crate::actors::permissions::canonical_path(path)?;
        let recursive = path.is_dir() || write && !path.exists();
        Ok(Self {
            path,
            write,
            recursive,
        })
    }

    pub(crate) fn active_filesystem(&self) -> Option<(&Path, bool)> {
        if !crate::actors::permissions::canonical_path(&self.path)
            .is_ok_and(|current| current == self.path)
            || !self.recursive && self.path.is_dir()
        {
            return None;
        }
        Some((&self.path, self.write))
    }

    pub fn covers(&self, target: &Self) -> bool {
        (self.write || !target.write)
            && (self.path == target.path && (self.recursive || !target.recursive)
                || self.recursive && target.path.starts_with(&self.path))
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        self.path == other.path
            || self.recursive && other.path.starts_with(&self.path)
            || other.recursive && self.path.starts_with(&other.path)
    }
}

pub(crate) fn scopes(tx: &Transaction<'_>) -> Result<Vec<SandboxScope>> {
    let mut statement = tx
        .prepare(
            "SELECT path, write, recursive FROM sandbox_scopes ORDER BY path, write, recursive",
        )
        .into_diagnostic()?;
    statement
        .query_map([], |row| {
            Ok(SandboxScope {
                path: PathBuf::from(row.get::<_, String>(0)?),
                write: row.get(1)?,
                recursive: row.get(2)?,
            })
        })
        .into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()
}

pub(crate) fn remember(tx: &Transaction<'_>, incoming: &[SandboxScope]) -> Result<()> {
    let mut saved = scopes(tx)?;
    for scope in incoming {
        if saved.iter().any(|old| old.covers(scope)) {
            continue;
        }
        for old in saved.iter().filter(|old| scope.covers(old)) {
            tx.execute(
                "DELETE FROM sandbox_scopes WHERE path=?1 AND write=?2 AND recursive=?3",
                params![
                    old.path
                        .to_str()
                        .ok_or_else(|| miette::miette!("Invalid sandbox path encoding"))?,
                    old.write,
                    old.recursive
                ],
            )
            .into_diagnostic()?;
        }
        saved.retain(|old| !scope.covers(old));
        tx.execute(
            "INSERT INTO sandbox_scopes(path,write,recursive) VALUES(?1,?2,?3)",
            params![
                scope
                    .path
                    .to_str()
                    .ok_or_else(|| miette::miette!("Invalid sandbox path encoding"))?,
                scope.write,
                scope.recursive
            ],
        )
        .into_diagnostic()?;
        saved.push(scope.clone());
    }
    Ok(())
}

// Windows legacy records may use DOS spelling while canonical paths use the
// verbatim prefix. Accept only that lexical equivalence; following a changed
// symlink must never rebind an old grant to its new target.
fn restore_legacy_binding(mut scope: SandboxScope) -> SandboxScope {
    if let Ok(current) = crate::actors::permissions::canonical_path(&scope.path)
        && dunce::simplified(&current) == dunce::simplified(&scope.path)
    {
        scope.path = current;
    }
    scope
}

pub(crate) fn initialize(tx: &Transaction<'_>) -> Result<()> {
    let actor_column: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('approvals') WHERE name='actor_id')",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    if actor_column {
        tx.execute_batch("CREATE TABLE device_scope_applications_migration(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,record TEXT NOT NULL);
            INSERT INTO device_scope_applications_migration(seq,id,message_id,record) SELECT seq,id,message_id,json_remove(record,'$.actor_id') FROM approvals;
            DROP TABLE approvals;
            ALTER TABLE device_scope_applications_migration RENAME TO approvals;
            CREATE INDEX approval_message ON approvals(message_id,seq);").into_diagnostic()?;
    }
    tx.execute_batch("CREATE TABLE IF NOT EXISTS sandbox_scopes(path TEXT NOT NULL,write INTEGER NOT NULL,recursive INTEGER NOT NULL,PRIMARY KEY(path,write,recursive));").into_diagnostic()?;
    let legacy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='actor_approval_scopes')", [], |row| row.get(0)).into_diagnostic()?;
    if legacy {
        let records = tx.prepare("SELECT record FROM actor_approval_scopes WHERE json_extract(record,'$.kind')='filesystem'").into_diagnostic()?
            .query_map([], |row| row.get::<_,String>(0)).into_diagnostic()?
            .collect::<rusqlite::Result<Vec<_>>>().into_diagnostic()?;
        let scopes = records
            .into_iter()
            .map(|record| {
                serde_json::from_str(&record)
                    .map(restore_legacy_binding)
                    .into_diagnostic()
            })
            .collect::<Result<Vec<SandboxScope>>>()?;
        remember(tx, &scopes)?;
        tx.execute_batch("DROP TABLE actor_approval_scopes;")
            .into_diagnostic()?;
    }
    let configured = tx.prepare("SELECT value FROM actors, json_each(actors.record,'$.config.permissions.readable_roots') UNION SELECT value FROM actors, json_each(actors.record,'$.config.permissions.writable_roots')").into_diagnostic()?
        .query_map([], |row| row.get::<_,String>(0)).into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>().into_diagnostic()?;
    let writable = tx.prepare("SELECT value FROM actors, json_each(actors.record,'$.config.permissions.writable_roots')").into_diagnostic()?
        .query_map([], |row| row.get::<_,String>(0)).into_diagnostic()?
        .collect::<rusqlite::Result<Vec<_>>>().into_diagnostic()?;
    let configured = configured
        .into_iter()
        .map(|value| SandboxScope {
            write: writable.contains(&value),
            path: value.into(),
            recursive: true,
        })
        .map(restore_legacy_binding)
        .collect::<Vec<_>>();
    remember(tx, &configured)?;
    tx.execute("UPDATE actors SET record=json_remove(record,'$.config.permissions') WHERE json_type(record,'$.config.permissions') IS NOT NULL", []).into_diagnostic()?;
    tx.execute("UPDATE messages SET record=json_remove(json_set(record,'$.remote_authority.origin_actor',json_extract(record,'$.remote_authority.grant_actor')),'$.remote_authority.grant_actor') WHERE json_type(record,'$.remote_authority.grant_actor') IS NOT NULL", []).into_diagnostic()?;
    tx.execute("UPDATE messages SET record=json_set(record,'$.remote_authority.read_only',json(CASE WHEN COALESCE(json_extract(record,'$.delegation_grant.read_only'),json_extract(record,'$.delegation_grant.permissions.read_only'),1) THEN 'true' ELSE 'false' END)) WHERE json_type(record,'$.remote_authority')='object' AND json_type(record,'$.remote_authority.read_only') IS NULL", []).into_diagnostic()?;
    tx.execute("UPDATE messages SET record=json_remove(record,'$.delegation_grant') WHERE json_type(record,'$.delegation_grant') IS NOT NULL", []).into_diagnostic()?;
    tx.execute("UPDATE approvals SET record=json_set(record,'$.decision','cancelled','$.reason','Sandbox permission model changed; the original operation was not replayed') WHERE json_type(record,'$.mode') IS NOT NULL AND json_extract(record,'$.decision') IN ('pending','application_required')", []).into_diagnostic()?;
    tx.execute("UPDATE approvals SET record=json_set(json_remove(record,'$.mode','$.review','$.actor_id','$.requested.operation_details'),'$.scopes',json(COALESCE((SELECT json_group_array(json_remove(value,'$.kind')) FROM json_each(approvals.record,'$.scopes') WHERE json_extract(value,'$.kind')='filesystem' OR (json_extract(value,'$.kind') IS NULL AND json_type(value,'$.path') IS NOT NULL)),'[]'))) WHERE json_type(record,'$.mode') IS NOT NULL OR json_type(record,'$.review') IS NOT NULL OR json_type(record,'$.actor_id') IS NOT NULL OR json_type(record,'$.requested.operation_details') IS NOT NULL", []).into_diagnostic()?;
    tx.execute_batch(
        "DROP TABLE IF EXISTS account_approval_refs; DROP TABLE IF EXISTS account_approval_sync;",
    )
    .into_diagnostic()?;
    Ok(())
}

pub(crate) fn policy(scopes: &[SandboxScope], read_only: bool) -> super::RuntimeSandboxPolicy {
    let mut filesystem = super::FileSystemSandboxPolicy {
        full_disk_read: false,
        deny_all_writes: read_only,
        full_disk_write: false,
        readable_roots: vec![],
        writable_roots: vec![],
        deny_read_paths: vec![],
        deny_write_paths: vec![],
        bound_roots: vec![],
    };
    for scope in scopes {
        if let Some((path, write)) = scope.active_filesystem() {
            filesystem.bound_roots.push(path.into());
            filesystem.readable_roots.push(path.into());
            if write && !read_only {
                filesystem.writable_roots.push(super::WritableRoot {
                    root: path.into(),
                    read_only_subpaths: vec![],
                });
            }
        }
    }
    super::RuntimeSandboxPolicy {
        builtin_resources: vec![],
        native: super::SandboxPolicy {
            filesystem,
            strong_filesystem: super::StrongFilesystemSandboxMode::Required,
        },
    }
}

impl crate::actors::store::ActorStore {
    pub(crate) fn initialize_device_sandbox(&self, workspace: &Path) -> Result<()> {
        let scope = SandboxScope::filesystem(workspace, true)?;
        self.stage_transaction(|tx| {
            let initialized: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_public_meta WHERE key='sandbox_workspace_initialized')", [], |row| row.get(0))?;
            if !initialized {
                remember(tx, &[scope]).map_err(|_| crate::stage::StageError::Invalid("Invalid initial device sandbox"))?;
                tx.execute("INSERT INTO stage_public_meta VALUES('sandbox_workspace_initialized','1')", [])?;
            }
            Ok(())
        }).map_err(|error| miette::miette!("{error}"))
    }
    pub fn sandbox_scopes(&self) -> Result<Vec<SandboxScope>> {
        self.stage_transaction(|tx| {
            scopes(tx)
                .map_err(|_| crate::stage::StageError::Invalid("Invalid device sandbox scope"))
        })
        .map_err(|error| miette::miette!("{error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newly_created_file_keeps_its_remembered_write_scope() {
        let directory = crate::sandbox::test_support::scoped_directory();
        let file = directory.path().join("new-file.txt");
        let scope = SandboxScope::filesystem(&file, true).unwrap();
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE sandbox_scopes(path TEXT NOT NULL,write INTEGER NOT NULL,recursive INTEGER NOT NULL,PRIMARY KEY(path,write,recursive));").unwrap();
        let tx = db.transaction().unwrap();
        remember(&tx, &[scope]).unwrap();
        let before = policy(&scopes(&tx).unwrap(), false);
        before
            .ensure_path_writable(&file, "new native file target")
            .unwrap();
        // Native in-place creation does not use the Files atomic writer's
        // sibling temporary file. Simulate that effect, then reload memory.
        std::fs::write(&file, "remember this file").unwrap();
        let saved = scopes(&tx).unwrap();
        assert_eq!(saved.len(), 1);
        let after = policy(&saved, false);
        let read = crate::file_io::read_file(
            directory.path(),
            &after,
            &crate::file_io::ReadFileArgs {
                path: file.to_string_lossy().into_owned(),
                start_line: None,
                line_count: None,
            },
        )
        .expect("creation must not revoke the scope just approved for this file");
        assert_eq!(read.content, "remember this file");
        assert!(after.is_path_writable(&file));
        assert!(!after.is_path_readable(&directory.path().join("unapproved-sibling.txt")));
    }

    #[cfg(windows)]
    #[test]
    fn legacy_dos_paths_retain_effective_access_without_rebinding_other_paths() {
        let directory = crate::sandbox::test_support::scoped_directory();
        let canonical = crate::actors::permissions::canonical_path(directory.path()).unwrap();
        let legacy = SandboxScope {
            path: dunce::simplified(&canonical).to_path_buf(),
            write: true,
            recursive: true,
        };
        let restored = restore_legacy_binding(legacy);
        assert_eq!(restored.path, canonical);
        assert!(restored.active_filesystem().is_some());
        let policy = policy(&[restored], false);
        assert!(policy.is_path_writable(&directory.path().join("future.txt")));
        assert!(!policy.is_path_writable(&directory.path().parent().unwrap().join("sibling.txt")));
    }

    #[test]
    fn migration_merges_device_scopes_and_discards_operation_fingerprints() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE actors(id TEXT PRIMARY KEY, record TEXT NOT NULL DEFAULT '{}'); INSERT INTO actors(id) VALUES('a'),('b'); CREATE TABLE messages(record TEXT NOT NULL); CREATE TABLE approvals(record TEXT NOT NULL); CREATE TABLE actor_approval_scopes(actor_id TEXT, fingerprint TEXT, record TEXT);").unwrap();
        let parent = SandboxScope {
            path: crate::actors::permissions::canonical_path(
                &std::env::temp_dir().join("device-scope-fixture"),
            )
            .unwrap(),
            write: true,
            recursive: true,
        };
        let child = SandboxScope {
            path: parent.path.join("file.txt"),
            write: false,
            recursive: false,
        };
        for (actor, scope) in [("a", &parent), ("b", &child)] {
            let mut record = serde_json::to_value(scope).unwrap();
            record["kind"] = "filesystem".into();
            db.execute(
                "INSERT INTO actor_approval_scopes VALUES(?1,?1,?2)",
                params![actor, record.to_string()],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO actor_approval_scopes VALUES('a','old-action',?1)",
            [r#"{"kind":"app","namespace":"fixture","policy":"old","action":"old"}"#],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        initialize(&tx).unwrap();
        assert_eq!(scopes(&tx).unwrap(), vec![parent.clone()]);
        tx.execute("DELETE FROM actors", []).unwrap();
        assert_eq!(scopes(&tx).unwrap(), vec![parent]);
        assert!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='actor_approval_scopes')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
        );
        tx.commit().unwrap();
    }

    #[test]
    fn directory_writes_cover_future_descendants_but_not_siblings() {
        let parent = SandboxScope {
            path: std::env::temp_dir().join("device-scope-fixture"),
            write: true,
            recursive: true,
        };
        assert!(parent.covers(&SandboxScope {
            path: parent.path.join("future/nested/file"),
            write: true,
            recursive: false
        }));
        assert!(!parent.covers(&SandboxScope {
            path: parent.path.with_file_name("sibling"),
            write: false,
            recursive: true
        }));
        assert!(
            !SandboxScope {
                write: false,
                ..parent.clone()
            }
            .covers(&parent)
        );
    }
}
