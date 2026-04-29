use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
    pin::Pin,
    process::Stdio,
    task::{Context, Poll},
};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
#[cfg_attr(test, allow(dead_code))]
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
    inner: SandboxAsyncChildInner,
}

enum SandboxAsyncChildInner {
    Tokio(tokio::process::Child),
    #[cfg(target_os = "windows")]
    Windows(windows::WindowsSandboxAsyncChild),
}

pub struct SandboxChildStdin {
    inner: SandboxChildStdinInner,
}

enum SandboxChildStdinInner {
    Tokio(tokio::process::ChildStdin),
    #[cfg(target_os = "windows")]
    File(tokio::fs::File),
}

pub struct SandboxChildStdout {
    inner: SandboxChildStdoutInner,
}

enum SandboxChildStdoutInner {
    Tokio(tokio::process::ChildStdout),
    #[cfg(target_os = "windows")]
    File(tokio::fs::File),
}

pub struct SandboxChildStderr {
    inner: SandboxChildStderrInner,
}

enum SandboxChildStderrInner {
    Tokio(tokio::process::ChildStderr),
    #[cfg(target_os = "windows")]
    File(tokio::fs::File),
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
            windows::spawn_plain(policy, program, args, options).map(|inner| Self { inner })
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
                let spawn_spec = policy
                    .shell_spawn_spec(program, args.clone())
                    .map_err(std::io::Error::other)?;
                match windows::spawn_restricted_async(
                    policy,
                    spawn_spec.program,
                    spawn_spec.args,
                    options.clone(),
                ) {
                    Ok(inner) => {
                        return Ok(Self {
                            inner: SandboxAsyncChildInner::Windows(inner),
                        });
                    }
                    Err(err)
                        if policy.strong_filesystem == StrongFilesystemSandboxMode::Required =>
                    {
                        return Err(err);
                    }
                    Err(err) => {
                        tracing::warn!(
                            "Windows strong filesystem sandbox requested for Terminal in auto mode, but restricted spawn failed: {err}"
                        );
                    }
                }
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
        Ok(Self {
            inner: SandboxAsyncChildInner::Tokio(inner),
        })
    }

    pub fn take_stdin(&mut self) -> Option<SandboxChildStdin> {
        match &mut self.inner {
            SandboxAsyncChildInner::Tokio(child) => child.stdin.take().map(SandboxChildStdin::from),
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => {
                child.take_stdin().map(SandboxChildStdin::from_file)
            }
        }
    }

    pub fn take_stdout(&mut self) -> Option<SandboxChildStdout> {
        match &mut self.inner {
            SandboxAsyncChildInner::Tokio(child) => {
                child.stdout.take().map(SandboxChildStdout::from)
            }
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => {
                child.take_stdout().map(SandboxChildStdout::from_file)
            }
        }
    }

    pub fn take_stderr(&mut self) -> Option<SandboxChildStderr> {
        match &mut self.inner {
            SandboxAsyncChildInner::Tokio(child) => {
                child.stderr.take().map(SandboxChildStderr::from)
            }
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => {
                child.take_stderr().map(SandboxChildStderr::from_file)
            }
        }
    }

    pub fn start_kill(&mut self) -> std::io::Result<()> {
        match &mut self.inner {
            SandboxAsyncChildInner::Tokio(child) => child.start_kill(),
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => child.start_kill(),
        }
    }

    pub fn id(&self) -> Option<u32> {
        match &self.inner {
            SandboxAsyncChildInner::Tokio(child) => child.id(),
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => Some(child.id()),
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match &mut self.inner {
            SandboxAsyncChildInner::Tokio(child) => child.try_wait(),
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => child.try_wait(),
        }
    }
}

impl From<tokio::process::ChildStdin> for SandboxChildStdin {
    fn from(stdin: tokio::process::ChildStdin) -> Self {
        Self {
            inner: SandboxChildStdinInner::Tokio(stdin),
        }
    }
}

impl From<tokio::process::ChildStdout> for SandboxChildStdout {
    fn from(stdout: tokio::process::ChildStdout) -> Self {
        Self {
            inner: SandboxChildStdoutInner::Tokio(stdout),
        }
    }
}

impl From<tokio::process::ChildStderr> for SandboxChildStderr {
    fn from(stderr: tokio::process::ChildStderr) -> Self {
        Self {
            inner: SandboxChildStderrInner::Tokio(stderr),
        }
    }
}

impl SandboxChildStdin {
    #[cfg(target_os = "windows")]
    fn from_file(file: tokio::fs::File) -> Self {
        Self {
            inner: SandboxChildStdinInner::File(file),
        }
    }
}

impl SandboxChildStdout {
    #[cfg(target_os = "windows")]
    fn from_file(file: tokio::fs::File) -> Self {
        Self {
            inner: SandboxChildStdoutInner::File(file),
        }
    }
}

impl SandboxChildStderr {
    #[cfg(target_os = "windows")]
    fn from_file(file: tokio::fs::File) -> Self {
        Self {
            inner: SandboxChildStderrInner::File(file),
        }
    }
}

impl AsyncWrite for SandboxChildStdin {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut self.inner {
            SandboxChildStdinInner::Tokio(stdin) => Pin::new(stdin).poll_write(cx, buf),
            #[cfg(target_os = "windows")]
            SandboxChildStdinInner::File(file) => Pin::new(file).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.inner {
            SandboxChildStdinInner::Tokio(stdin) => Pin::new(stdin).poll_flush(cx),
            #[cfg(target_os = "windows")]
            SandboxChildStdinInner::File(file) => Pin::new(file).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.inner {
            SandboxChildStdinInner::Tokio(stdin) => Pin::new(stdin).poll_shutdown(cx),
            #[cfg(target_os = "windows")]
            SandboxChildStdinInner::File(file) => Pin::new(file).poll_shutdown(cx),
        }
    }
}

impl AsyncRead for SandboxChildStdout {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut self.inner {
            SandboxChildStdoutInner::Tokio(stdout) => Pin::new(stdout).poll_read(cx, buf),
            #[cfg(target_os = "windows")]
            SandboxChildStdoutInner::File(file) => Pin::new(file).poll_read(cx, buf),
        }
    }
}

impl AsyncRead for SandboxChildStderr {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut self.inner {
            SandboxChildStderrInner::Tokio(stderr) => Pin::new(stderr).poll_read(cx, buf),
            #[cfg(target_os = "windows")]
            SandboxChildStderrInner::File(file) => Pin::new(file).poll_read(cx, buf),
        }
    }
}

#[cfg(any(not(test), all(test, target_os = "windows")))]
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

#[cfg(any(not(test), all(test, target_os = "windows")))]
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

pub(super) fn path_is_denied(path: &Path, denied_roots: &[PathBuf]) -> bool {
    denied_roots
        .iter()
        .any(|denied| path_is_or_descends_logical_or_resolved(path, denied))
}

impl FileSystemSandboxPolicy {
    pub fn unrestricted() -> Self {
        Self {
            full_disk_read: true,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: Vec::new(),
            deny_write_paths: Vec::new(),
        }
    }

    pub fn is_unrestricted(&self) -> bool {
        self.full_disk_read
            && self.full_disk_write
            && self.readable_roots.is_empty()
            && self.writable_roots.is_empty()
            && self.deny_read_paths.is_empty()
            && self.deny_write_paths.is_empty()
    }

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
    pub fn disabled() -> Self {
        Self {
            filesystem: FileSystemSandboxPolicy::unrestricted(),
            protected_env_vars: Vec::new(),
            strong_filesystem: StrongFilesystemSandboxMode::Off,
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.filesystem.is_unrestricted()
            && self.protected_env_vars.is_empty()
            && self.strong_filesystem == StrongFilesystemSandboxMode::Off
    }

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
        if self.is_disabled() {
            return false;
        }
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

    #[cfg(any(not(target_os = "windows"), test))]
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
        if self.is_disabled() {
            return Ok(SandboxSpawnSpec {
                program: PathBuf::from(program),
                args,
            });
        }

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
    use std::{path::Path, process::ExitStatus, time::Duration};

    use super::{
        FileSystemSandboxPolicy, RuntimeSandboxPolicy, SandboxAsyncChild, SandboxProcessOptions,
        SandboxStdio, StrongFilesystemSandboxMode, WritableRoot,
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

    #[test]
    fn disabled_runtime_policy_removes_all_sandbox_restrictions() {
        let policy = RuntimeSandboxPolicy::disabled();

        assert!(policy.is_disabled());
        assert_eq!(policy.strong_filesystem, StrongFilesystemSandboxMode::Off);
        assert!(policy.protected_env_vars().is_empty());
        assert!(policy.protected_paths().is_empty());
        assert!(policy.is_path_readable(Path::new("/home/user/.daat-locus/config.toml")));
        assert!(policy.is_path_writable(Path::new("/repo/src/main.rs")));
        assert!(!policy.is_env_var_protected("OPENAI_API_KEY"));

        let shell_spec = policy
            .shell_spawn_spec("/bin/sh", vec!["-lc".to_string(), "echo ok".to_string()])
            .expect("shell spawn spec");
        assert_eq!(shell_spec.program, Path::new("/bin/sh"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[tokio::test]
    async fn strong_backend_denies_protected_runtime_paths_when_available() {
        use tokio::io::AsyncReadExt;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let protected_home = tempdir.path().join(".daat-locus");
        let protected_config = protected_home.join("config");
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&protected_config).expect("create protected config");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let secret_file = protected_config.join("secret.txt");
        let protected_write_file = protected_home.join("created-by-sandbox.txt");
        let public_file = workspace.join("public.txt");
        let workspace_output = workspace.join("output.txt");
        std::fs::write(&secret_file, "secret").expect("write protected secret");
        std::fs::write(&public_file, "public").expect("write public file");

        let policy = RuntimeSandboxPolicy {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: true,
                full_disk_write: false,
                readable_roots: Vec::new(),
                writable_roots: vec![WritableRoot {
                    root: workspace.clone(),
                    read_only_subpaths: Vec::new(),
                }],
                deny_read_paths: vec![protected_home.clone()],
                deny_write_paths: vec![protected_home.clone()],
            },
            protected_env_vars: Vec::new(),
            strong_filesystem: StrongFilesystemSandboxMode::Required,
        };
        let (program, args) = conformance_probe_command(
            &secret_file,
            &protected_write_file,
            &public_file,
            &workspace_output,
        );
        let options = SandboxProcessOptions {
            current_dir: Some(workspace.clone()),
            stdin: SandboxStdio::Null,
            stdout: SandboxStdio::Piped,
            stderr: SandboxStdio::Piped,
        };

        let mut child = match SandboxAsyncChild::spawn_shell(&policy, &program, args, options) {
            Ok(child) => child,
            Err(err) if strong_backend_is_unavailable(&err) => {
                eprintln!("skipping strong sandbox conformance test: {err}");
                return;
            }
            Err(err) => panic!("failed to spawn conformance probe: {err}"),
        };
        let mut stdout = child.take_stdout();
        let mut stderr = child.take_stderr();
        let status = wait_for_child(&mut child).await.expect("wait for probe");

        let mut stdout_text = String::new();
        if let Some(stdout) = stdout.as_mut() {
            stdout
                .read_to_string(&mut stdout_text)
                .await
                .expect("read stdout");
        }
        let mut stderr_text = String::new();
        if let Some(stderr) = stderr.as_mut() {
            stderr
                .read_to_string(&mut stderr_text)
                .await
                .expect("read stderr");
        }

        assert!(
            status.success(),
            "conformance probe failed with {status}; stdout={stdout_text:?}; stderr={stderr_text:?}"
        );
        assert!(
            !protected_write_file.exists(),
            "sandboxed child wrote into protected runtime path"
        );
        assert_eq!(
            std::fs::read_to_string(&workspace_output).expect("read workspace output"),
            "ok"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    async fn wait_for_child(child: &mut SandboxAsyncChild) -> std::io::Result<ExitStatus> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.start_kill();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "sandbox conformance probe timed out",
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[cfg(target_os = "linux")]
    fn strong_backend_is_unavailable(error: &std::io::Error) -> bool {
        error
            .to_string()
            .contains("Linux filesystem sandbox requires `bwrap` on PATH")
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn strong_backend_is_unavailable(_error: &std::io::Error) -> bool {
        false
    }

    #[cfg(unix)]
    fn conformance_probe_command(
        secret_file: &Path,
        protected_write_file: &Path,
        public_file: &Path,
        workspace_output: &Path,
    ) -> (String, Vec<String>) {
        let script = format!(
            r#"secret={secret}
protected_write={protected_write}
public_file={public_file}
workspace_output={workspace_output}
read_denied=0
cat "$secret" >/dev/null 2>&1 || read_denied=1
write_denied=0
printf bad > "$protected_write" 2>/dev/null || write_denied=1
public_ok=0
[ "$(cat "$public_file")" = public ] && public_ok=1
write_ok=0
printf ok > "$workspace_output" 2>/dev/null && write_ok=1
if [ "$read_denied" = 1 ] && [ "$write_denied" = 1 ] && [ "$public_ok" = 1 ] && [ "$write_ok" = 1 ]; then
    exit 0
fi
printf 'read_denied=%s write_denied=%s public_ok=%s write_ok=%s\n' "$read_denied" "$write_denied" "$public_ok" "$write_ok"
exit 42
"#,
            secret = sh_quote(secret_file),
            protected_write = sh_quote(protected_write_file),
            public_file = sh_quote(public_file),
            workspace_output = sh_quote(workspace_output),
        );
        ("/bin/sh".to_string(), vec!["-c".to_string(), script])
    }

    #[cfg(unix)]
    fn sh_quote(path: &Path) -> String {
        let value = path.to_string_lossy();
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    #[cfg(target_os = "windows")]
    fn conformance_probe_command(
        secret_file: &Path,
        protected_write_file: &Path,
        public_file: &Path,
        workspace_output: &Path,
    ) -> (String, Vec<String>) {
        let system_root = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
        let powershell = system_root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let script = [
            format!("$secret = {}", ps_quote(secret_file)),
            format!("$protectedWrite = {}", ps_quote(protected_write_file)),
            format!("$publicFile = {}", ps_quote(public_file)),
            format!("$workspaceOutput = {}", ps_quote(workspace_output)),
            "$readDenied = $false".to_string(),
            "try { Get-Content -Raw -LiteralPath $secret -ErrorAction Stop | Out-Null } catch { $readDenied = $true }".to_string(),
            "$writeDenied = $false".to_string(),
            "try { Set-Content -LiteralPath $protectedWrite -Value 'bad' -NoNewline -ErrorAction Stop } catch { $writeDenied = $true }".to_string(),
            "$publicOk = $false".to_string(),
            "try { $publicOk = (Get-Content -Raw -LiteralPath $publicFile -ErrorAction Stop) -eq 'public' } catch { }".to_string(),
            "$writeOk = $false".to_string(),
            "try { Set-Content -LiteralPath $workspaceOutput -Value 'ok' -NoNewline -ErrorAction Stop; $writeOk = $true } catch { }".to_string(),
            "if ($readDenied -and $writeDenied -and $publicOk -and $writeOk) { exit 0 }".to_string(),
            "Write-Output \"readDenied=$readDenied writeDenied=$writeDenied publicOk=$publicOk writeOk=$writeOk\"".to_string(),
            "exit 42".to_string(),
        ]
        .join("; ");
        (
            powershell.to_string_lossy().into_owned(),
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-Command".to_string(),
                script,
            ],
        )
    }

    #[cfg(target_os = "windows")]
    fn ps_quote(path: &Path) -> String {
        let value = path.to_string_lossy();
        format!("'{}'", value.replace('\'', "''"))
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

        let policy = super::FileSystemSandboxPolicy {
            full_disk_read: false,
            full_disk_write: false,
            readable_roots: Vec::new(),
            writable_roots: vec![super::WritableRoot {
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
