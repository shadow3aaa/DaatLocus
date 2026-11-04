//! Native projections are owned by the device, not by an individual Actor App.
//! A mount must outlive all of its users, including background processes.
#[cfg(any(windows, target_os = "linux"))]
mod view;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(not(any(target_os = "linux", windows)))]
use crate::Error;
use crate::{Result, Workspace};
use std::path::Path;

pub(crate) fn directory_path(path: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    return windows::directory_path(path);
    #[cfg(not(windows))]
    path.to_owned()
}

pub(crate) fn logical_path(path: &Path, root: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    return windows::logical_path(path, root);
    #[cfg(not(windows))]
    {
        let _ = root;
        path.to_owned()
    }
}

#[cfg(windows)]
pub fn is_mount_path(path: &Path) -> bool {
    windows::is_mount_path(path)
}

#[cfg(windows)]
pub fn mounted_roots() -> Vec<std::path::PathBuf> {
    windows::mounted_roots()
}

/// A verified host-owned volume root and its kernel path. Use the held handle
/// or this path for I/O so changing the visible mount point cannot redirect it.
#[cfg(windows)]
pub struct MountedDirectory {
    pub directory: std::fs::File,
    pub path: std::path::PathBuf,
}

/// Resolve only a live host-owned mount. The caller must pin its non-reparse
/// ancestors and independently authorize the visible path before using it.
#[cfg(windows)]
pub fn open_mount_directory(path: &Path) -> std::io::Result<Option<MountedDirectory>> {
    windows::open_mount_directory(path)
}

pub fn available() -> Result<bool> {
    #[cfg(target_os = "linux")]
    return linux::available();
    #[cfg(windows)]
    return windows::available();
    #[cfg(not(any(target_os = "linux", windows)))]
    Ok(false)
}

/// Verify native file I/O and symbolic-link traversal using synthetic files.
/// Dependency detection alone cannot establish directory-mount privileges.
/// The host supplies its private infrastructure directory, never a project.
pub fn probe(directory: &Path) -> Result<bool> {
    if !available()? {
        return Ok(false);
    }
    let scratch = tempfile::Builder::new()
        .prefix("probe-")
        .tempdir_in(directory)?;
    let source = scratch.path().join("source");
    std::fs::create_dir(&source)?;
    std::fs::write(source.join("source.txt"), b"original\n")?;
    std::fs::create_dir(source.join("nested"))?;
    std::fs::write(source.join("nested/child.txt"), b"nested bytes\n")?;
    let origin = crate::Directory::open(&source, std::sync::Arc::new(|_, _| Ok(())))?;
    let store = crate::Store::open(&scratch.path().join("store.sqlite3"))?;
    // Seed links in the engine so checking traversal does not require the OS
    // user's privilege to create a native symlink in the source directory.
    let workspace = store.branch(origin.clone(), "probe", crate::DEFAULT_BRANCH)?;
    workspace.symlink("file-link", "source.txt")?;
    workspace.symlink("directory-link", "nested")?;
    let manager = crate::Workspaces::new(store, scratch.path().join("mounts"));
    let branch = manager.select(origin.clone(), "probe", crate::DEFAULT_BRANCH)?;
    let result = (|| -> Result<()> {
        if std::fs::read(branch.directory.join("source.txt"))? != b"original\n" {
            return Err(crate::Error::Corrupt(
                "native projection returned different contents".into(),
            ));
        }
        for (path, expected) in [
            ("file-link", b"original\n".as_slice()),
            ("directory-link/child.txt", b"nested bytes\n".as_slice()),
        ] {
            let contents = std::fs::read(branch.directory.join(path)).map_err(|error| {
                crate::Error::Unsupported(format!(
                    "native projection cannot follow symbolic links ({path}): {error}"
                ))
            })?;
            if contents != expected {
                return Err(crate::Error::Corrupt(
                    "native projection resolved a symbolic link incorrectly".into(),
                ));
            }
        }
        std::fs::write(branch.directory.join("file-link"), b"linked edit\n")?;
        if workspace.read("source.txt", 0, 80)? != b"linked edit\n" {
            return Err(crate::Error::Corrupt(
                "native projection did not write through its symbolic link".into(),
            ));
        }
        std::fs::write(branch.directory.join("created.txt"), b"branch bytes\n")?;
        std::fs::rename(
            branch.directory.join("created.txt"),
            branch.directory.join("renamed.txt"),
        )?;
        if std::fs::read(branch.directory.join("renamed.txt"))? != b"branch bytes\n" {
            return Err(crate::Error::Corrupt(
                "native projection did not retain its write".into(),
            ));
        }
        std::fs::remove_file(branch.directory.join("renamed.txt"))?;
        std::fs::remove_dir_all(branch.directory.join("nested"))?;
        if source.join("created.txt").exists()
            || !source.join("nested/child.txt").exists()
            || std::fs::read(source.join("source.txt"))? != b"original\n"
        {
            return Err(crate::Error::Corrupt(
                "native projection changed its source".into(),
            ));
        }
        Ok(())
    })();
    manager.close();
    drop((manager, workspace, origin));
    result?;
    Ok(true)
}

pub struct Mount {
    directory: std::path::PathBuf,
    #[cfg(target_os = "linux")]
    _session: linux::Session,
    #[cfg(windows)]
    _session: windows::Session,
}

impl Mount {
    pub(crate) fn retire(&mut self) -> Result<()> {
        #[cfg(any(windows, target_os = "linux"))]
        return self._session.retire();
        #[cfg(not(any(windows, target_os = "linux")))]
        Ok(())
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn open(workspace: Workspace, path: &Path) -> Result<Self> {
        #[cfg(target_os = "linux")]
        return linux::mount(workspace, path);
        #[cfg(windows)]
        return windows::mount(workspace, path);
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = (workspace, path);
            Err(Error::Unsupported("native workspace projection".into()))
        }
    }
}
