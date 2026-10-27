use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

/// Own the real executable entries until every process using this PATH exits.
/// A fresh private directory avoids cross-version caches and shared mutable tools.
pub struct CommandPath {
    directory: tempfile::TempDir,
}

impl CommandPath {
    pub fn new(host: &Path) -> io::Result<Self> {
        let host = std::fs::canonicalize(host)?;
        let directory = tempfile::Builder::new()
            .prefix("daat-locus-tools-")
            .tempdir()?;
        #[cfg(windows)]
        let mut backing = host.clone();
        for name in crate::names() {
            let entry = directory
                .path()
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            #[cfg(unix)]
            std::os::unix::fs::symlink(&host, &entry)?;
            #[cfg(windows)]
            if let Err(error) = std::fs::hard_link(&backing, &entry) {
                // The OS temporary directory may be on another volume. Copy
                // once, then hard-link every remaining applet to that copy.
                if backing != host {
                    return Err(error);
                }
                backing = directory.path().join(".host.exe");
                std::fs::copy(&host, &backing)?;
                std::fs::hard_link(&backing, &entry)?;
            }
        }
        std::fs::write(
            directory.path().join("LICENSE-uutils"),
            include_bytes!("../LICENSE-uutils"),
        )?;
        std::fs::write(
            directory.path().join("LICENSE-ripgrep"),
            include_bytes!("../vendor/ripgrep/LICENSE-MIT"),
        )?;
        Ok(Self { directory })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    pub fn prepend_to(&self, inherited: Option<&OsStr>) -> io::Result<OsString> {
        let mut paths = vec![PathBuf::from(self.path())];
        if let Some(inherited) = inherited {
            paths.extend(std::env::split_paths(inherited));
        }
        std::env::join_paths(paths)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
    }
}
