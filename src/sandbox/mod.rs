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
    pub protected_env_vars: Vec<String>,
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
        Self::protect_daat_locus_runtime_with_options(daat_locus_home, None, Vec::<String>::new())
    }

    pub fn protect_daat_locus_runtime_with_options(
        daat_locus_home: &Path,
        daat_locus_source_root: Option<&Path>,
        protected_env_vars: Vec<String>,
    ) -> Self {
        let protected_runtime_paths = vec![normalize_path(daat_locus_home)];
        let mut deny_write_paths = protected_runtime_paths.clone();
        if let Some(source_root) = daat_locus_source_root {
            deny_write_paths.push(normalize_path(source_root));
        }
        deny_write_paths.sort();
        deny_write_paths.dedup();

        let mut protected_env_vars = protected_env_vars
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        protected_env_vars.sort_by_key(|name| name.to_ascii_uppercase());
        protected_env_vars.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

        Self {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: true,
                full_disk_write: true,
                readable_roots: Vec::new(),
                writable_roots: Vec::new(),
                deny_read_paths: protected_runtime_paths,
                deny_write_paths,
            },
            protected_env_vars,
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

    pub fn protected_env_vars(&self) -> &[String] {
        &self.protected_env_vars
    }

    pub fn is_env_var_protected(&self, name: &str) -> bool {
        Self::is_env_var_protected_by_list(name, &self.protected_env_vars)
    }

    pub fn is_env_var_protected_by_list(name: &str, protected_env_vars: &[String]) -> bool {
        let normalized = name.trim();
        if normalized.is_empty() {
            return false;
        }
        if protected_env_vars
            .iter()
            .any(|protected| protected.eq_ignore_ascii_case(normalized))
        {
            return true;
        }
        is_sensitive_env_var_name(normalized)
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

fn is_sensitive_env_var_name(name: &str) -> bool {
    const SENSITIVE_MARKERS: &[&str] = &[
        "API_KEY",
        "ACCESS_KEY",
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE_KEY",
    ];
    let upper = name.to_ascii_uppercase();
    SENSITIVE_MARKERS
        .iter()
        .any(|marker| upper.contains(marker))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::RuntimeSandboxPolicy;

    #[test]
    fn default_runtime_policy_protects_private_home_without_locking_machine() {
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

    #[test]
    fn runtime_policy_protects_source_writes_and_secret_env_names() {
        let source_root = Path::new("/workspace/daat-locus");
        let workspace_dir = Path::new("/Users/test/daat-locus-workspace");
        let daat_locus_home = Path::new("/Users/test/.daat-locus");
        let policy = RuntimeSandboxPolicy::protect_daat_locus_runtime_with_options(
            daat_locus_home,
            Some(source_root),
            vec![
                "CUSTOM_PROVIDER_TOKEN".to_string(),
                "custom_provider_token".to_string(),
            ],
        );

        assert!(policy.is_path_readable(source_root));
        assert!(!policy.is_path_writable(source_root));
        assert!(!policy.is_path_writable(&source_root.join("src/main.rs")));
        assert!(policy.is_path_readable(workspace_dir));
        assert!(policy.is_path_writable(workspace_dir));
        assert!(!policy.is_path_readable(daat_locus_home));
        assert!(!policy.is_path_writable(daat_locus_home));

        assert!(policy.is_env_var_protected("CUSTOM_PROVIDER_TOKEN"));
        assert!(policy.is_env_var_protected("OPENAI_API_KEY"));
        assert!(policy.is_env_var_protected("aws_secret_access_key"));
        assert!(!policy.is_env_var_protected("PATH"));
        assert!(!policy.is_env_var_protected("SSH_AUTH_SOCK"));
        assert_eq!(policy.protected_env_vars, vec!["CUSTOM_PROVIDER_TOKEN"]);
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
