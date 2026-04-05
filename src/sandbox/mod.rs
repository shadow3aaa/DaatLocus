use std::path::{Component, Path, PathBuf};

use miette::{Result, miette};

#[cfg(target_os = "macos")]
mod macos;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WritableRoot {
    pub root: PathBuf,
    pub read_only_subpaths: Vec<PathBuf>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, path: &Path) -> bool {
        let normalized = normalize_path(path);
        let root = normalize_path(&self.root);
        normalized.starts_with(&root)
            && !self
                .read_only_subpaths
                .iter()
                .map(|subpath| normalize_path(subpath))
                .any(|subpath| normalized == subpath || normalized.starts_with(&subpath))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSystemSandboxPolicy {
    pub full_disk_read: bool,
    pub full_disk_write: bool,
    pub readable_roots: Vec<PathBuf>,
    pub writable_roots: Vec<WritableRoot>,
    pub deny_read_paths: Vec<PathBuf>,
    pub deny_write_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSandboxPolicy {
    pub filesystem: FileSystemSandboxPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxSpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

impl FileSystemSandboxPolicy {
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .deny_read_paths
            .iter()
            .chain(self.deny_write_paths.iter())
            .map(|path| normalize_path(path))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn is_path_readable(&self, path: &Path) -> bool {
        let normalized = normalize_path(path);
        if self
            .deny_read_paths
            .iter()
            .map(|path| normalize_path(path))
            .any(|denied| normalized == denied || normalized.starts_with(&denied))
        {
            return false;
        }
        if self.full_disk_read {
            return true;
        }
        self.readable_roots
            .iter()
            .map(|root| normalize_path(root))
            .any(|root| normalized == root || normalized.starts_with(&root))
            || self.writable_roots.iter().any(|root| root.is_path_writable(&normalized))
    }

    pub fn is_path_writable(&self, path: &Path) -> bool {
        let normalized = normalize_path(path);
        if self
            .deny_write_paths
            .iter()
            .map(|path| normalize_path(path))
            .any(|denied| normalized == denied || normalized.starts_with(&denied))
        {
            return false;
        }
        if self.full_disk_write {
            return true;
        }
        self.writable_roots
            .iter()
            .any(|root| root.is_path_writable(&normalized))
    }
}

impl RuntimeSandboxPolicy {
    pub fn protect_spinova_runtime(
        execution_cwd: &Path,
        spinova_home: &Path,
        executable_dir: Option<&Path>,
    ) -> Self {
        let mut protected_paths = vec![normalize_path(execution_cwd), normalize_path(spinova_home)];
        if let Some(executable_dir) = executable_dir {
            protected_paths.push(normalize_path(executable_dir));
        }
        protected_paths.sort();
        protected_paths.dedup();

        let mut writable_roots = Vec::new();
        let temp_root = std::env::temp_dir();
        if !temp_root.as_os_str().is_empty() {
            writable_roots.push(WritableRoot {
                root: normalize_path(&temp_root),
                read_only_subpaths: Vec::new(),
            });
        }
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            let tmpdir = PathBuf::from(tmpdir);
            if !tmpdir.as_os_str().is_empty() {
                writable_roots.push(WritableRoot {
                    root: normalize_path(&tmpdir),
                    read_only_subpaths: Vec::new(),
                });
            }
        }
        writable_roots.sort_by(|a, b| a.root.cmp(&b.root));
        writable_roots.dedup_by(|left, right| left.root == right.root);

        Self {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: true,
                full_disk_write: false,
                readable_roots: Vec::new(),
                writable_roots,
                deny_read_paths: protected_paths.clone(),
                deny_write_paths: protected_paths,
            },
        }
    }

    pub fn resolve_path(&self, path: &Path, base: Option<&Path>) -> PathBuf {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.unwrap_or_else(|| Path::new("/")).join(path)
        };
        normalize_path(&absolute)
    }

    pub fn protected_paths(&self) -> Vec<PathBuf> {
        self.filesystem.protected_paths()
    }

    pub fn is_path_readable(&self, path: &Path) -> bool {
        self.filesystem.is_path_readable(path)
    }

    pub fn is_path_writable(&self, path: &Path) -> bool {
        self.filesystem.is_path_writable(path)
    }

    pub fn shell_spawn_spec(
        &self,
        program: &str,
        args: Vec<String>,
    ) -> Result<SandboxSpawnSpec> {
        #[cfg(target_os = "macos")]
        {
            return macos::wrap_shell_command(self, program, args);
        }

        #[cfg(not(target_os = "macos"))]
        {
            Ok(SandboxSpawnSpec {
                program: PathBuf::from(program),
                args,
            })
        }
    }

    pub fn ensure_path_readable(&self, path: &Path, label: &str) -> Result<()> {
        let normalized = normalize_path(path);
        if self.is_path_readable(&normalized) {
            Ok(())
        } else {
            Err(miette!(
                "sandbox denies read access to {label}: {}",
                normalized.display()
            ))
        }
    }

    pub fn ensure_path_writable(&self, path: &Path, label: &str) -> Result<()> {
        let normalized = normalize_path(path);
        if self.is_path_writable(&normalized) {
            Ok(())
        } else {
            Err(miette!(
                "sandbox denies write access to {label}: {}",
                normalized.display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::RuntimeSandboxPolicy;

    #[test]
    fn default_runtime_policy_protects_runtime_dirs_and_allows_tmp_write() {
        let execution_cwd = Path::new("/workspace/spinova");
        let spinova_home = Path::new("/Users/test/.spinova");
        let executable_dir = Some(Path::new("/Applications/Spinova.app/Contents/MacOS"));
        let policy = RuntimeSandboxPolicy::protect_spinova_runtime(
            execution_cwd,
            spinova_home,
            executable_dir,
        );
        let writable_tmp = std::env::temp_dir().join("spinova-sandbox-test");

        assert!(!policy.is_path_readable(execution_cwd));
        assert!(!policy.is_path_writable(execution_cwd));
        assert!(!policy.is_path_readable(spinova_home));
        assert!(!policy.is_path_writable(spinova_home));
        assert!(policy.is_path_readable(&writable_tmp));
        assert!(policy.is_path_writable(&writable_tmp));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_spawn_spec_uses_sandbox_exec_on_macos() {
        let policy = RuntimeSandboxPolicy::protect_spinova_runtime(
            Path::new("/workspace/spinova"),
            Path::new("/Users/test/.spinova"),
            Some(Path::new("/Applications/Spinova.app/Contents/MacOS")),
        );
        let spawn_spec = policy
            .shell_spawn_spec("bash", vec!["-lc".to_string(), "pwd".to_string()])
            .expect("macOS sandbox wrapper should render");
        assert_eq!(spawn_spec.program, Path::new("/usr/bin/sandbox-exec"));
        assert!(spawn_spec.args.iter().any(|arg| arg == "--"));
    }
}
