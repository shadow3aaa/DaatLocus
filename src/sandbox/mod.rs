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
            || self
                .writable_roots
                .iter()
                .any(|root| root.is_path_writable(&normalized))
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
    pub fn protect_daat_locus_runtime(daat_locus_home: &Path) -> Self {
        let protected_paths = vec![normalize_path(daat_locus_home)];
        Self {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: true,
                full_disk_write: true,
                readable_roots: Vec::new(),
                writable_roots: Vec::new(),
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

    pub fn shell_spawn_spec(&self, program: &str, args: Vec<String>) -> Result<SandboxSpawnSpec> {
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
    fn default_runtime_policy_only_protects_daat_locus_home() {
        let repo_dir = Path::new("/workspace/daat-locus");
        let workspace_dir = Path::new("/Users/test/daat-locus-workspace");
        let daat_locus_home = Path::new("/Users/test/.daat-locus");
        let policy = RuntimeSandboxPolicy::protect_daat_locus_runtime(daat_locus_home);
        let writable_tmp = std::env::temp_dir().join("daat-locus-sandbox-test");

        assert!(!policy.is_path_readable(daat_locus_home));
        assert!(!policy.is_path_writable(daat_locus_home));
        assert!(policy.is_path_readable(repo_dir));
        assert!(policy.is_path_writable(repo_dir));
        assert!(policy.is_path_readable(workspace_dir));
        assert!(policy.is_path_writable(workspace_dir));
        assert!(policy.is_path_readable(&writable_tmp));
        assert!(policy.is_path_writable(&writable_tmp));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_spawn_spec_uses_sandbox_exec_on_macos() {
        let policy =
            RuntimeSandboxPolicy::protect_daat_locus_runtime(Path::new("/Users/test/.daat-locus"));
        let spawn_spec = policy
            .shell_spawn_spec("bash", vec!["-lc".to_string(), "pwd".to_string()])
            .expect("macOS sandbox wrapper should render");
        assert_eq!(spawn_spec.program, Path::new("/usr/bin/sandbox-exec"));
        assert!(spawn_spec.args.iter().any(|arg| arg == "--"));
    }
}
