use crate::{DEFAULT_BRANCH, Directory, Error, Result, Store, Workspace, native};
use parking_lot::Mutex;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub path: PathBuf,
    pub branch: String,
    pub directory: PathBuf,
}

/// One device/profile owns projections for every Actor using it. Releasing an
/// Actor App never drops a mount; the host explicitly closes this manager after
/// it has stopped the native processes using the projections.
pub struct Workspaces {
    store: Store,
    mount_root: PathBuf,
    mount_directory: Mutex<Option<Directory>>,
    mounts: Mutex<HashMap<String, native::Mount>>,
}

impl Workspaces {
    pub fn new(store: Store, mount_root: PathBuf) -> Self {
        Self {
            store,
            mount_root,
            mount_directory: Mutex::new(None),
            mounts: Mutex::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Infrastructure paths, used by a host to avoid re-entering its own
    /// projection while authorizing I/O against the original native directory.
    pub fn projected_directories(&self) -> Result<Vec<PathBuf>> {
        let connection = self.store.connection.lock();
        let mut statement = connection.prepare("SELECT directory FROM projections")?;
        Ok(statement
            .query_map([], |row| {
                Ok(native::directory_path(Path::new(&row.get::<_, String>(0)?)))
            })?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// Default is the original directory. Existing named branches are reopened
    /// without importing again; no selection implicitly writes to default.
    pub fn select(&self, origin: Directory, branch: &str, from: &str) -> Result<Location> {
        if branch == DEFAULT_BRANCH {
            return Ok(Location {
                path: origin.path().to_owned(),
                branch: branch.into(),
                directory: origin.path().to_owned(),
            });
        }
        if self.store.path().starts_with(origin.path()) {
            return Err(Error::Invalid(
                "a branch source cannot contain its own content store".into(),
            ));
        }
        if !native::available()? {
            return Err(Error::Unsupported(
                "native workspace projection is unavailable".into(),
            ));
        }
        if self.projection_root()?.starts_with(origin.path()) {
            return Err(Error::Invalid(
                "a branch source cannot contain its own projections".into(),
            ));
        }
        let workspace = self.store.branch(origin.clone(), branch, from)?;
        origin.access("", crate::Access::Read)?;
        self.mount(workspace)
    }

    /// Reopen or fork an immutable branch without holding the original native
    /// directory open. None means that a new snapshot of default is required.
    pub fn select_recorded(
        &self,
        path: &Path,
        branch: &str,
        from: &str,
    ) -> Result<Option<Location>> {
        if branch == DEFAULT_BRANCH {
            return Ok(None);
        }
        if !native::available()? {
            return Err(Error::Unsupported(
                "native workspace projection is unavailable".into(),
            ));
        }
        let workspace = match self.store.open_recorded_branch(path, branch) {
            Ok(workspace) => workspace,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if from == DEFAULT_BRANCH {
                    return Ok(None);
                }
                self.store.open_recorded_branch(path, from)?.fork(branch)?
            }
            Err(error) => return Err(error),
        };
        self.mount(workspace).map(Some)
    }

    fn mount(&self, workspace: Workspace) -> Result<Location> {
        let key = workspace.key();
        let directory = self
            .projection_root()?
            .join(hex::encode(Sha256::digest(key.as_bytes())));
        let location = Location {
            path: workspace.origin().to_owned(),
            branch: workspace.name().into(),
            directory: directory.clone(),
        };
        let mut mounts = self.mounts.lock();
        if let Some(mount) = mounts.get(&key) {
            return Ok(Location {
                directory: mount.directory().to_owned(),
                ..location
            });
        }
        // The mount container is host-owned infrastructure, never supplied by a
        // model. Reject links and nonempty targets rather than masking user data.
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => {
                if std::fs::read_dir(&directory)?.next().is_some() {
                    return Err(Error::Conflict("workspace mount point is not empty".into()));
                }
            }
            Ok(_) => return Err(Error::Conflict("workspace mount point was replaced".into())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&directory)?
            }
            Err(error) => return Err(error.into()),
        }
        let mount = native::Mount::open(workspace.clone(), &directory)?;
        self.store.connection.lock().execute(
            "INSERT INTO projections(project,name,directory) VALUES(?1,?2,?3) ON CONFLICT(project,name) DO UPDATE SET directory=excluded.directory",
            params![workspace.project_id(), workspace.name(), directory.to_str().ok_or_else(|| Error::Invalid("mount directory must be Unicode".into()))?],
        )?;
        let location = Location {
            directory: mount.directory().to_owned(),
            ..location
        };
        mounts.insert(key, mount);
        Ok(location)
    }

    fn projection_root(&self) -> Result<PathBuf> {
        let mut directory = self.mount_directory.lock();
        if directory.is_none() {
            std::fs::create_dir_all(&self.mount_root)?;
            *directory = Some(Directory::open(
                &self.mount_root,
                std::sync::Arc::new(|_, _| Ok(())),
            )?);
        }
        let directory = directory.as_ref().expect("initialized mount container");
        directory.check_binding()?;
        Ok(directory.path().to_owned())
    }

    /// Resolve persisted execution directories even before a service restart has
    /// remounted their parent projection. Ordinary directories need no record.
    pub fn locate(&self, directory: &Path) -> Result<Location> {
        let lookup = native::logical_path(directory, &self.projection_root()?);
        let connection = self.store.connection.lock();
        // Windows canonicalization adds a trailing separator at a mounted
        // volume's root. Path components compare equally, but SQLite TEXT keys
        // do not; normalize separators before looking up persisted projections.
        let normalized: PathBuf = lookup.components().collect();
        for parent in normalized.ancestors() {
            let Some(parent_text) = parent.to_str() else {
                continue;
            };
            let found: Option<(String, String)> = connection.query_row(
                "SELECT projects.path,projections.name FROM projections JOIN projects ON projects.id=projections.project WHERE directory=?1", [parent_text],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((path, branch)) = found {
                return Ok(Location {
                    path: path.into(),
                    branch,
                    directory: directory.to_owned(),
                });
            }
        }
        Ok(Location {
            path: directory.to_owned(),
            branch: DEFAULT_BRANCH.into(),
            directory: directory.to_owned(),
        })
    }

    pub fn restore(&self, directory: &Path) -> Result<Location> {
        let location = self.locate(directory)?;
        if location.branch == DEFAULT_BRANCH {
            return Ok(location);
        }
        let workspace = self
            .store
            .open_recorded_branch(&location.path, &location.branch)?;
        let root: String = self.store.connection.lock().query_row(
            "SELECT projections.directory FROM projections JOIN projects ON projects.id=projections.project WHERE projects.path=?1 AND projections.name=?2",
            params![location.path.to_str(), location.branch], |row| row.get(0),
        )?;
        let lookup = native::logical_path(directory, &self.projection_root()?);
        let relative = lookup
            .strip_prefix(root)
            .map_err(|_| Error::Conflict("persisted mount location changed".into()))?;
        let mounted = self.mount(workspace)?;
        Ok(Location {
            directory: mounted.directory.join(relative),
            ..location
        })
    }

    pub fn close(&self) {
        let mounts = std::mem::take(&mut *self.mounts.lock());
        drop(mounts);
    }
}
