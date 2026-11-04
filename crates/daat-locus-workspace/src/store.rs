use crate::{Error, Result, missing};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};

/// Hashes include an object domain; a file block cannot impersonate a tree node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObjectId(pub(crate) String);

impl ObjectId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Statistics {
    pub objects: u64,
    pub stored_bytes: u64,
    pub uncompressed_bytes: u64,
    pub branches: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub removed_objects: u64,
    pub removed_bytes: u64,
}

#[derive(Clone)]
pub struct Store {
    pub(crate) connection: Arc<Mutex<Connection>>,
    path: Arc<std::path::PathBuf>,
}

impl Store {
    /// The caller establishes owner-only permissions on the containing directory
    /// before opening storage, just as for its other private persistence.
    pub fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(30))?;
        connection.execute_batch(
            "PRAGMA auto_vacuum=INCREMENTAL;
             PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS workspace_format(version INTEGER PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS objects(
                id TEXT PRIMARY KEY, domain INTEGER NOT NULL,
                size INTEGER NOT NULL, data BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS projects(
                id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                binding TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS branches(
                project INTEGER NOT NULL REFERENCES projects(id),
                name TEXT NOT NULL, base TEXT NOT NULL REFERENCES objects(id),
                head TEXT NOT NULL REFERENCES objects(id), revision INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(project,name));
             DROP INDEX IF EXISTS project_binding;
             CREATE TABLE IF NOT EXISTS source_inodes(
                project INTEGER NOT NULL REFERENCES projects(id), path TEXT NOT NULL,
                inode INTEGER NOT NULL UNIQUE, PRIMARY KEY(project,path));
             CREATE TABLE IF NOT EXISTS projections(
                project INTEGER NOT NULL, name TEXT NOT NULL, directory TEXT NOT NULL UNIQUE,
                PRIMARY KEY(project,name), FOREIGN KEY(project,name) REFERENCES branches(project,name));
             CREATE TABLE IF NOT EXISTS merges(
                id TEXT PRIMARY KEY, project INTEGER NOT NULL, branch TEXT NOT NULL,
                head TEXT NOT NULL, state TEXT NOT NULL, report TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS native_files(
                id TEXT PRIMARY KEY, root TEXT NOT NULL REFERENCES objects(id));
             CREATE TABLE IF NOT EXISTS workspace_sequence(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1), inode INTEGER NOT NULL);
             INSERT OR IGNORE INTO workspace_sequence VALUES(1,1);",
        )?;
        let generation: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('branches') WHERE name='generation')",
            [],
            |row| row.get(0),
        )?;
        if !generation {
            connection.execute_batch(
                "ALTER TABLE branches ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        let version: Option<u32> = connection
            .query_row("SELECT version FROM workspace_format", [], |row| row.get(0))
            .optional()?;
        match version {
            None => {
                connection.execute("INSERT INTO workspace_format VALUES(1)", [])?;
            }
            Some(1) => {}
            Some(2) => {}
            Some(other) => return Err(Error::Unsupported(format!("storage format {other}"))),
        }
        crate::workspace::migrate_versions(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: Arc::new(path.canonicalize()?),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Only the exclusive host may call this during crash recovery, before it
    /// restores any projections. Handles from the former host no longer exist.
    pub fn recover_native_files(&self) -> Result<()> {
        self.connection
            .lock()
            .execute("DELETE FROM native_files", [])?;
        Ok(())
    }

    pub fn statistics(&self) -> Result<Statistics> {
        let connection = self.connection.lock();
        let (objects, stored_bytes, uncompressed_bytes) = connection.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(data)),0),COALESCE(SUM(size),0) FROM objects",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                ))
            },
        )?;
        let branches = connection.query_row("SELECT COUNT(*) FROM branches", [], |row| {
            Ok(row.get::<_, i64>(0)? as u64)
        })?;
        Ok(Statistics {
            objects,
            stored_bytes,
            uncompressed_bytes,
            branches,
        })
    }
}

pub(crate) fn put(connection: &Connection, domain: u8, bytes: &[u8]) -> Result<ObjectId> {
    let mut digest = Sha256::new();
    digest.update([domain]);
    digest.update(bytes);
    let id = ObjectId(hex::encode(digest.finalize()));
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM objects WHERE id=?1)",
        [&id.0],
        |row| row.get(0),
    )?;
    if !exists {
        let data = zstd::bulk::compress(bytes, 3)?;
        connection.execute(
            "INSERT OR IGNORE INTO objects VALUES(?1,?2,?3,?4)",
            params![id.0, domain, bytes.len() as i64, data],
        )?;
    }
    Ok(id)
}

pub(crate) fn get(connection: &Connection, domain: u8, id: &ObjectId) -> Result<Vec<u8>> {
    let (actual_domain, size, data): (u8, usize, Vec<u8>) = connection
        .query_row(
            "SELECT domain,size,data FROM objects WHERE id=?1",
            [&id.0],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as usize, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| missing(format!("object {}", id.0)))?;
    if domain != actual_domain {
        return Err(Error::Corrupt(id.0.clone()));
    }
    let bytes = zstd::bulk::decompress(&data, size)?;
    let mut digest = Sha256::new();
    digest.update([domain]);
    digest.update(&bytes);
    if bytes.len() != size || hex::encode(digest.finalize()) != id.0 {
        return Err(Error::Corrupt(id.0.clone()));
    }
    Ok(bytes)
}

pub(crate) fn next_inode(connection: &Connection) -> Result<u64> {
    Ok(connection.query_row(
        "UPDATE workspace_sequence SET inode=inode+1 WHERE singleton=1 RETURNING inode",
        [],
        |row| Ok(row.get::<_, i64>(0)? as u64),
    )?)
}
