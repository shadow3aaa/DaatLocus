use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
    process::Stdio,
};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(not(test), target_os = "windows"))]
mod windows;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WritableRoot {
    pub root: PathBuf,
    pub read_only_subpaths: Vec<PathBuf>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, path: &Path) -> bool {
        path_is_or_descends_resolved(path, &self.root)
            && !self
                .read_only_subpaths
                .iter()
                .any(|subpath| path_is_or_descends_logical_or_resolved(path, subpath))
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
    pub strong_filesystem: StrongFilesystemSandboxMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxSpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SandboxStdio {
    #[default]
    Inherit,
    Null,
    Piped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SandboxProcessOptions {
    pub current_dir: Option<PathBuf>,
    pub stdin: SandboxStdio,
    pub stdout: SandboxStdio,
    pub stderr: SandboxStdio,
}

#[cfg(all(not(test), not(target_os = "windows")))]
pub struct SandboxChild {
    inner: std::process::Child,
}

#[cfg(all(not(test), target_os = "windows"))]
pub struct SandboxChild {
    inner: windows::WindowsSandboxChild,
}

pub struct SandboxAsyncChild {
    inner: tokio::process::Child,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrongFilesystemSandboxMode {
    #[default]
    Off,
    Auto,
    Required,
}

impl SandboxStdio {
    pub(super) fn to_stdio(self) -> Stdio {
        match self {
            Self::Inherit => Stdio::inherit(),
            Self::Null => Stdio::null(),
            Self::Piped => Stdio::piped(),
        }
    }
}

#[cfg(not(test))]
impl SandboxChild {
    pub fn spawn_strong(
        policy: &RuntimeSandboxPolicy,
        program: PathBuf,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> std::io::Result<Self> {
        #[cfg(target_os = "windows")]
        {
            if policy.strong_filesystem != StrongFilesystemSandboxMode::Off
                && filesystem_policy_requires_backend(&policy.filesystem)
            {
                match windows::spawn_restricted(
                    policy,
                    program.clone(),
                    args.clone(),
                    options.clone(),
                ) {
                    Ok(inner) => return Ok(Self { inner }),
                    Err(err)
                        if policy.strong_filesystem == StrongFilesystemSandboxMode::Required =>
                    {
                        return Err(err);
                    }
                    Err(err) => {
                        tracing::warn!(
                            "Windows strong filesystem sandbox requested in auto mode, but restricted spawn failed: {err}"
                        );
                    }
                }
            }
            return windows::spawn_plain(policy, program, args, options)
                .map(|inner| Self { inner });
        }

        #[cfg(not(target_os = "windows"))]
        {
            let spawn_spec = policy
                .strong_command_spawn_spec(program, args)
                .map_err(std::io::Error::other)?;
            let mut command = std::process::Command::new(spawn_spec.program);
            command.args(spawn_spec.args);
            apply_std_command_options(policy, &mut command, options);
            let inner = command.spawn()?;
            Ok(Self { inner })
        }
    }

    pub fn id(&self) -> u32 {
        self.inner.id()
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        self.inner.kill()
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.inner.try_wait()
    }

    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.inner.wait()
    }
}

#[cfg(not(test))]
impl std::fmt::Debug for SandboxChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxChild")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl SandboxAsyncChild {
    pub fn spawn_shell(
        policy: &RuntimeSandboxPolicy,
        program: &str,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> std::io::Result<Self> {
        #[cfg(target_os = "windows")]
        {
            if policy.strong_filesystem != StrongFilesystemSandboxMode::Off
                && filesystem_policy_requires_backend(&policy.filesystem)
            {
                return match policy.strong_filesystem {
                    StrongFilesystemSandboxMode::Required => Err(std::io::Error::other(
                        "Windows strong filesystem sandbox for Terminal is not implemented yet",
                    )),
                    StrongFilesystemSandboxMode::Auto => {
                        tracing::warn!(
                            "Windows strong filesystem sandbox requested for Terminal in auto mode, but Terminal sandbox spawning is not implemented yet"
                        );
                        Ok(())
                    }
                    StrongFilesystemSandboxMode::Off => Ok(()),
                }
                .and_then(|()| Self::spawn_shell_unwrapped(policy, program, args, options));
            }
        }

        Self::spawn_shell_unwrapped(policy, program, args, options)
    }

    fn spawn_shell_unwrapped(
        policy: &RuntimeSandboxPolicy,
        program: &str,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> std::io::Result<Self> {
        let spawn_spec = policy
            .shell_spawn_spec(program, args)
            .map_err(std::io::Error::other)?;
        let mut command = tokio::process::Command::new(spawn_spec.program);
        command.args(spawn_spec.args);
        apply_tokio_command_options(policy, &mut command, options);
        let inner = command.spawn()?;
        Ok(Self { inner })
    }

    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.inner.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.inner.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.inner.stderr.take()
    }

    pub fn start_kill(&mut self) -> std::io::Result<()> {
        self.inner.start_kill()
    }

    pub fn id(&self) -> Option<u32> {
        self.inner.id()
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.inner.try_wait()
    }
}

#[cfg(not(test))]
pub(super) fn apply_std_command_options(
    policy: &RuntimeSandboxPolicy,
    command: &mut std::process::Command,
    options: SandboxProcessOptions,
) {
    strip_protected_env_from_std_command(policy, command);
    if let Some(current_dir) = options.current_dir {
        command.current_dir(current_dir);
    }
    command
        .stdin(options.stdin.to_stdio())
        .stdout(options.stdout.to_stdio())
        .stderr(options.stderr.to_stdio());
}

fn apply_tokio_command_options(
    policy: &RuntimeSandboxPolicy,
    command: &mut tokio::process::Command,
    options: SandboxProcessOptions,
) {
    strip_protected_env_from_tokio_command(policy, command);
    if let Some(current_dir) = options.current_dir {
        command.current_dir(current_dir);
    }
    command
        .stdin(options.stdin.to_stdio())
        .stdout(options.stdout.to_stdio())
        .stderr(options.stderr.to_stdio());
}

#[cfg(not(test))]
fn strip_protected_env_from_std_command(
    policy: &RuntimeSandboxPolicy,
    command: &mut std::process::Command,
) {
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(|name| policy.is_env_var_protected(name))
        {
            command.env_remove(&name);
        }
    }
}

#[cfg(target_os = "windows")]
fn filesystem_policy_requires_backend(policy: &FileSystemSandboxPolicy) -> bool {
    !(policy.full_disk_read
        && policy.full_disk_write
        && policy.readable_roots.is_empty()
        && policy.writable_roots.is_empty()
        && policy.deny_read_paths.is_empty()
        && policy.deny_write_paths.is_empty())
}

fn strip_protected_env_from_tokio_command(
    policy: &RuntimeSandboxPolicy,
    command: &mut tokio::process::Command,
) {
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(|name| policy.is_env_var_protected(name))
        {
            command.env_remove(&name);
        }
    }
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

pub(crate) fn resolve_path_for_check(path: &Path) -> PathBuf {
    let normalized = normalize_path(path);
    if !normalized.is_absolute() {
        return normalized;
    }
    if let Ok(resolved) = std::fs::canonicalize(&normalized) {
        return normalize_path(&resolved);
    }

    let mut existing = normalized.as_path();
    let mut missing_suffix = Vec::<OsString>::new();
    loop {
        if let Ok(resolved) = std::fs::canonicalize(existing) {
            let mut resolved = normalize_path(&resolved);
            for component in missing_suffix.iter().rev() {
                resolved.push(component);
            }
            return normalize_path(&resolved);
        }

        let Some(parent) = existing.parent() else {
            return normalized;
        };
        let Some(file_name) = existing.file_name() else {
            return normalized;
        };
        missing_suffix.push(file_name.to_os_string());
        existing = parent;
    }
}

pub(crate) fn policy_paths_with_resolved(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut expanded = paths
        .iter()
        .flat_map(|path| [normalize_path(path), resolve_path_for_check(path)])
        .collect::<Vec<_>>();
    expanded.sort();
    expanded.dedup();
    expanded
}

fn path_is_or_descends(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn path_is_or_descends_logical_or_resolved(path: &Path, root: &Path) -> bool {
    let normalized = normalize_path(path);
    let normalized_root = normalize_path(root);
    if path_is_or_descends(&normalized, &normalized_root) {
        return true;
    }

    let resolved = resolve_path_for_check(&normalized);
    let resolved_root = resolve_path_for_check(&normalized_root);
    path_is_or_descends(&resolved, &resolved_root)
}

fn path_is_or_descends_resolved(path: &Path, root: &Path) -> bool {
    let resolved = resolve_path_for_check(path);
    let resolved_root = resolve_path_for_check(root);
    path_is_or_descends(&resolved, &resolved_root)
}

fn path_is_denied(path: &Path, denied_roots: &[PathBuf]) -> bool {
    denied_roots
        .iter()
        .any(|denied| path_is_or_descends_logical_or_resolved(path, denied))
}

impl FileSystemSandboxPolicy {
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .deny_read_paths
            .iter()
            .chain(self.deny_write_paths.iter())
            .flat_map(|path| [normalize_path(path), resolve_path_for_check(path)])
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn is_path_readable(&self, path: &Path) -> bool {
        if path_is_denied(path, &self.deny_read_paths) {
            return false;
        }
        if self.full_disk_read {
            return true;
        }
        self.readable_roots
            .iter()
            .any(|root| path_is_or_descends_resolved(path, root))
            || self
                .writable_roots
                .iter()
                .any(|root| root.is_path_writable(path))
    }

    pub fn is_path_writable(&self, path: &Path) -> bool {
        if path_is_denied(path, &self.deny_write_paths) {
            return false;
        }
        if self.full_disk_write {
            return true;
        }
        self.writable_roots
            .iter()
            .any(|root| root.is_path_writable(path))
    }
}

impl RuntimeSandboxPolicy {
    #[cfg(test)]
    pub fn protect_daat_locus_runtime(daat_locus_home: &Path) -> Self {
        Self::protect_daat_locus_runtime_with_options(daat_locus_home, None, Vec::<String>::new())
    }

    #[cfg(test)]
    pub fn protect_daat_locus_runtime_with_options(
        daat_locus_home: &Path,
        daat_locus_source_root: Option<&Path>,
        protected_env_vars: Vec<String>,
    ) -> Self {
        Self::protect_daat_locus_runtime_with_strong_filesystem(
            daat_locus_home,
            daat_locus_source_root,
            protected_env_vars,
            StrongFilesystemSandboxMode::Off,
        )
    }

    pub fn protect_daat_locus_runtime_with_strong_filesystem(
        daat_locus_home: &Path,
        daat_locus_source_root: Option<&Path>,
        protected_env_vars: Vec<String>,
        strong_filesystem: StrongFilesystemSandboxMode,
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
            strong_filesystem,
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

    pub fn strong_command_spawn_spec(
        &self,
        program: PathBuf,
        args: Vec<String>,
    ) -> Result<SandboxSpawnSpec> {
        if self.strong_filesystem == StrongFilesystemSandboxMode::Off {
            return Ok(SandboxSpawnSpec { program, args });
        }

        #[cfg(target_os = "macos")]
        {
            macos::wrap_command(self, program, args)
        }

        #[cfg(target_os = "linux")]
        {
            linux::wrap_command(self, program, args)
        }

        #[cfg(not(target_os = "macos"))]
        #[cfg(not(target_os = "linux"))]
        {
            match self.strong_filesystem {
                StrongFilesystemSandboxMode::Required => Err(miette!(
                    "strong filesystem sandbox is not supported on this platform"
                )),
                StrongFilesystemSandboxMode::Auto | StrongFilesystemSandboxMode::Off => {
                    Ok(SandboxSpawnSpec { program, args })
                }
            }
        }
    }

    pub fn shell_spawn_spec(&self, program: &str, args: Vec<String>) -> Result<SandboxSpawnSpec> {
        #[cfg(target_os = "macos")]
        {
            macos::wrap_shell_command(self, program, args)
        }

        #[cfg(target_os = "linux")]
        {
            linux::wrap_shell_command(self, program, args)
        }

        #[cfg(not(target_os = "macos"))]
        #[cfg(not(target_os = "linux"))]
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

    use super::{
        FileSystemSandboxPolicy, RuntimeSandboxPolicy, SandboxStdio, StrongFilesystemSandboxMode,
        WritableRoot,
    };

    #[test]
    fn sandbox_stdio_variants_are_stable() {
        assert_eq!(SandboxStdio::Inherit, SandboxStdio::default());
        assert_ne!(SandboxStdio::Null, SandboxStdio::Piped);
    }

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

    #[test]
    fn runtime_policy_records_strong_filesystem_mode() {
        let policy = RuntimeSandboxPolicy::protect_daat_locus_runtime_with_strong_filesystem(
            Path::new("/Users/test/.daat-locus"),
            None,
            Vec::<String>::new(),
            StrongFilesystemSandboxMode::Required,
        );

        assert_eq!(
            policy.strong_filesystem,
            StrongFilesystemSandboxMode::Required
        );
    }

    #[test]
    fn strong_command_spawn_spec_is_passthrough_when_disabled() {
        let policy = RuntimeSandboxPolicy::protect_daat_locus_runtime_with_strong_filesystem(
            Path::new("/Users/test/.daat-locus"),
            None,
            Vec::<String>::new(),
            StrongFilesystemSandboxMode::Off,
        );

        let spec = policy
            .strong_command_spawn_spec(Path::new("/bin/echo").to_path_buf(), vec!["ok".into()])
            .expect("spawn spec");

        assert_eq!(spec.program, Path::new("/bin/echo"));
        assert_eq!(spec.args, vec!["ok"]);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_policy_denies_symlink_to_private_home() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let daat_locus_home = tempdir.path().join(".daat-locus");
        let workspace_dir = tempdir.path().join("workspace");
        std::fs::create_dir_all(&daat_locus_home).expect("create protected home");
        std::fs::create_dir_all(&workspace_dir).expect("create workspace");
        std::fs::write(daat_locus_home.join("config.toml"), "secret").expect("write secret");
        let linked_home = workspace_dir.join("linked-home");
        symlink(&daat_locus_home, &linked_home).expect("symlink protected home");

        let policy = RuntimeSandboxPolicy::protect_daat_locus_runtime(&daat_locus_home);

        assert!(!policy.is_path_readable(&linked_home));
        assert!(!policy.is_path_readable(&linked_home.join("config.toml")));
        assert!(!policy.is_path_writable(&linked_home.join("new-state")));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_policy_denies_source_writes_through_symlink() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let daat_locus_home = tempdir.path().join(".daat-locus");
        let source_root = tempdir.path().join("source");
        let workspace_dir = tempdir.path().join("workspace");
        std::fs::create_dir_all(&daat_locus_home).expect("create protected home");
        std::fs::create_dir_all(source_root.join("src")).expect("create source");
        std::fs::write(source_root.join("src/main.rs"), "fn main() {}").expect("write source");
        std::fs::create_dir_all(&workspace_dir).expect("create workspace");
        let linked_source = workspace_dir.join("linked-source");
        symlink(&source_root, &linked_source).expect("symlink source");

        let policy = RuntimeSandboxPolicy::protect_daat_locus_runtime_with_options(
            &daat_locus_home,
            Some(&source_root),
            Vec::<String>::new(),
        );

        assert!(policy.is_path_readable(&linked_source.join("src/main.rs")));
        assert!(!policy.is_path_writable(&linked_source));
        assert!(!policy.is_path_writable(&linked_source.join("src/main.rs")));
        assert!(!policy.is_path_writable(&linked_source.join("new.rs")));
    }

    #[cfg(unix)]
    #[test]
    fn writable_root_does_not_allow_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let workspace_dir = tempdir.path().join("workspace");
        let outside_dir = tempdir.path().join("outside");
        std::fs::create_dir_all(&workspace_dir).expect("create workspace");
        std::fs::create_dir_all(&outside_dir).expect("create outside");
        let linked_outside = workspace_dir.join("outside-link");
        symlink(&outside_dir, &linked_outside).expect("symlink outside");

        let policy = FileSystemSandboxPolicy {
            full_disk_read: false,
            full_disk_write: false,
            readable_roots: Vec::new(),
            writable_roots: vec![WritableRoot {
                root: workspace_dir.clone(),
                read_only_subpaths: Vec::new(),
            }],
            deny_read_paths: Vec::new(),
            deny_write_paths: Vec::new(),
        };

        assert!(policy.is_path_writable(&workspace_dir.join("normal.txt")));
        assert!(!policy.is_path_writable(&linked_outside.join("escaped.txt")));
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
