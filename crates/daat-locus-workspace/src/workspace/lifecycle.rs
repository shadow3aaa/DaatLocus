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
        let connection = self.connection.lock();
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
        let others = reachable(&connection, roots(false)?)?;
        let mut stored_bytes = 0;
        let mut shared = 0;
        for id in &own {
            let size: u64 = connection.query_row(
                "SELECT length(data) FROM objects WHERE id=?1",
                [&id.0],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )?;
            stored_bytes += size;
            if others.contains(id) {
                shared += size;
            }
        }
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
