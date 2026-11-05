use super::*;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Storage {
    pub store_path: std::path::PathBuf,
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub objects: u64,
    pub stored_bytes: u64,
    pub exclusive_stored_bytes: u64,
    pub shared_stored_bytes: u64,
    pub versions: u64,
    pub branches: u64,
}

impl Store {
    pub(crate) fn rebind(&self, origin: &Directory) -> Result<()> {
        origin.check_binding()?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let project = history::project_at(&transaction, origin.path())?;
        transaction.execute(
            "UPDATE projects SET binding=?2 WHERE id=?1",
            params![project, origin.binding()],
        )?;
        transaction.execute("DELETE FROM source_inodes WHERE project=?1", [project])?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn delete(&self, path: &Path, branch: Option<&str>) -> Result<()> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let project = history::project_at(&transaction, path)?;
        if let Some(branch) = branch {
            validate_branch(branch)?;
            transaction.execute(
                "DELETE FROM projections WHERE project=?1 AND name=?2",
                params![project, branch],
            )?;
            transaction.execute(
                "DELETE FROM version_refs WHERE project=?1 AND name=?2",
                params![project, branch],
            )?;
            transaction.execute(
                "DELETE FROM branches WHERE project=?1 AND name=?2",
                params![project, branch],
            )?;
        } else {
            for table in [
                "projections",
                "version_refs",
                "branches",
                "versions",
                "merges",
                "source_inodes",
            ] {
                transaction.execute(&format!("DELETE FROM {table} WHERE project=?1"), [project])?;
            }
            transaction.execute("DELETE FROM projects WHERE id=?1", [project])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn storage(&self, path: &Path) -> Result<Storage> {
        let mut reader =
            Connection::open_with_flags(self.path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        reader.busy_timeout(std::time::Duration::from_secs(30))?;
        let connection = reader.transaction()?;
        let project = history::project_at(&connection, path)?;
        let roots = |own: bool| -> Result<Vec<ObjectId>> {
            let mut statement = connection.prepare(
                "SELECT base FROM branches WHERE (project=?1)=?2 UNION SELECT head FROM branches WHERE (project=?1)=?2 UNION SELECT root FROM versions WHERE (project=?1)=?2 UNION SELECT head FROM merges WHERE (project=?1)=?2 AND state IN ('prepared','applying','interrupted')"
            )?;
            Ok(statement
                .query_map(params![project, own], |row| Ok(ObjectId(row.get(0)?)))?
                .collect::<std::result::Result<_, _>>()?)
        };
        let own = reachable(&connection, roots(true)?)?;
        let others = if own.is_empty() {
            BTreeMap::new()
        } else {
            reachable(&connection, roots(false)?)?
        };
        let stored_bytes: u64 = own.values().sum();
        let shared: u64 = own
            .iter()
            .filter(|(id, _)| others.contains_key(*id))
            .map(|(_, size)| size)
            .sum();
        let versions = connection.query_row(
            "SELECT COUNT(*) FROM versions WHERE project=?1",
            [project],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )?;
        let branches = connection.query_row(
            "SELECT COUNT(*) FROM branches WHERE project=?1",
            [project],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )?;
        let wal = self.path().with_file_name(format!(
            "{}-wal",
            self.path().file_name().unwrap().to_string_lossy()
        ));
        let wal_bytes = match std::fs::metadata(wal) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        Ok(Storage {
            store_path: self.path().to_owned(),
            database_bytes: std::fs::metadata(self.path())?.len(),
            wal_bytes,
            objects: own.len() as u64,
            stored_bytes,
            exclusive_stored_bytes: stored_bytes - shared,
            shared_stored_bytes: shared,
            versions,
            branches,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::Duration,
    };

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Store) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        std::fs::create_dir(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
        (temp, path, store)
    }

    #[test]
    fn storage_reads_committed_state_while_a_writer_is_active() {
        let (_temp, path, store) = fixture();
        let origin = Directory::open(&path, Arc::new(|_, _| Ok(()))).unwrap();
        store.commit_default(&origin, "Keep").unwrap();
        let mut writer = store.connection.lock();
        let transaction = writer.transaction().unwrap();
        transaction
            .execute_batch("DELETE FROM version_refs; DELETE FROM versions;")
            .unwrap();
        let observer = store.clone();
        let (send, receive) = mpsc::channel();
        let query = std::thread::spawn(move || send.send(observer.storage(&path)).unwrap());
        let result = receive.recv_timeout(Duration::from_secs(5));
        drop(transaction);
        drop(writer);
        query.join().unwrap();
        let stats = result
            .expect("storage waited for the active writer")
            .unwrap();
        assert_eq!(stats.versions, 1);
    }

    #[test]
    fn object_traversal_batches_metadata_and_rejects_missing_or_invalid_blocks() {
        static QUERIES: AtomicUsize = AtomicUsize::new(0);
        let (_temp, path, store) = fixture();
        let origin = Directory::open(&path, Arc::new(|_, _| Ok(()))).unwrap();
        let branch = store.branch(origin, "work", DEFAULT_BRANCH).unwrap();
        branch.create("file", EntryKind::File, 0o644).unwrap();
        let mut bytes = vec![0; BLOCK * 256];
        for (index, block) in bytes.chunks_mut(BLOCK).enumerate() {
            block[..4].copy_from_slice(&(index as u32).to_le_bytes());
        }
        branch.write("file", 0, &bytes).unwrap();
        let connection = store.connection.lock();
        let root = branch.roots(&connection).unwrap().1;
        QUERIES.store(0, Ordering::Relaxed);
        connection.trace_v2(
            rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
            Some(|_| {
                QUERIES.fetch_add(1, Ordering::Relaxed);
            }),
        );
        let live = reachable(&connection, vec![root.clone(), root.clone()]).unwrap();
        let queries = QUERIES.load(Ordering::Relaxed);
        connection.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
        assert_eq!(live.len(), 258);
        let file = Node::load(&connection, &root).unwrap().children["file"].clone();
        let expected: i64 = connection
            .query_row(
                "SELECT SUM(length(data)) FROM objects WHERE domain=0 OR id IN (?1,?2)",
                params![root.0, file.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live.values().sum::<u64>(), expected as u64);
        eprintln!("256-block traversal: {queries} SQL executions");
        assert!(queries < 128, "{queries} SQL executions for 256 blocks");
        let block: String = connection
            .query_row("SELECT id FROM objects WHERE domain=0 LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        connection
            .execute("UPDATE objects SET domain=2 WHERE id=?1", [&block])
            .unwrap();
        assert!(matches!(
            reachable(&connection, vec![root.clone()]),
            Err(Error::Corrupt(_))
        ));
        connection
            .execute("DELETE FROM objects WHERE id=?1", [&block])
            .unwrap();
        assert!(reachable(&connection, vec![root]).is_err());
    }
}
