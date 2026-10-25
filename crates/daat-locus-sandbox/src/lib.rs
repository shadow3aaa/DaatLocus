//! Scoped filesystem policy and native process isolation, independent of any host application.

pub mod capabilities;
mod denial;
#[cfg(windows)]
pub mod process_spawn;
pub use denial::{
    Access, AccessDenied, DenialCoverage, DenialRecord, DenialSource, MAX_CAPTURE_DENIALS,
    denial_coverage, denials_from_capture_document,
};

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
mod contract_tests;
#[cfg(test)]
mod test_support;

use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsString,
    path::{Component, Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::OnceLock,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WritableRoot {
    pub root: PathBuf,
    pub read_only_subpaths: Vec<PathBuf>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, path: &Path) -> bool {
        with_path_resolution_cache(|| {
            path_is_or_descends_resolved(path, &self.root)
                && !self
                    .read_only_subpaths
                    .iter()
                    .any(|subpath| path_is_or_descends_logical_or_resolved(path, subpath))
        })
    }
}

/// Process-runtime temporary directories that never require a sandbox scope.
///
/// Reads and writes inside the platform's shared temporary directories are
/// baseline-authorized: they are not an approval scope and never produce a
/// missing-permission denial. Explicit deny rules still take precedence, and
/// an unrestricted policy is unaffected.
pub fn system_temporary_paths() -> &'static [PathBuf] {
    static CACHE: OnceLock<Vec<PathBuf>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut roots: Vec<PathBuf> = Vec::new();
        push_unique_root(&mut roots, std::env::temp_dir());
        for key in ["TEMP", "TMP", "TMPDIR"] {
            if let Some(value) = std::env::var_os(key) {
                let path = PathBuf::from(value);
                if path.is_absolute() {
                    push_unique_root(&mut roots, path);
                }
            }
        }
        #[cfg(unix)]
        {
            push_unique_root(&mut roots, PathBuf::from("/tmp"));
            push_unique_root(&mut roots, PathBuf::from("/var/tmp"));
        }
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            push_unique_root(&mut roots, PathBuf::from(root).join("Temp"));
        }
        roots
    })
}

fn push_unique_root(roots: &mut Vec<PathBuf>, path: PathBuf) {
    let path = normalize_path(&path);
    if !roots.iter().any(|existing| existing == &path) {
        roots.push(path);
    }
}

fn path_is_system_temporary(path: &Path) -> bool {
    system_temporary_paths().iter().any(|root| {
        path_is_or_descends_resolved(path, root)
            || path_is_or_descends_logical_or_resolved(path, root)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSystemSandboxPolicy {
    pub full_disk_read: bool,
    pub full_disk_write: bool,
    pub readable_roots: Vec<PathBuf>,
    pub writable_roots: Vec<WritableRoot>,
    pub deny_read_paths: Vec<PathBuf>,
    pub deny_write_paths: Vec<PathBuf>,
    /// Canonical roots captured when authority was granted. Replacing one with
    /// a symlink must not silently authorize the symlink's new destination.
    pub bound_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxPolicy {
    pub filesystem: FileSystemSandboxPolicy,
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

#[derive(Clone, Debug, Default)]
pub struct SandboxProcessOptions {
    /// Process-specific overrides applied to the inherited environment.
    pub environment: Vec<(String, String)>,
    pub current_dir: Option<PathBuf>,
    pub stdin: SandboxStdio,
    pub stdout: SandboxStdio,
    pub stderr: SandboxStdio,
    /// A cancelled startup must never launch a process after slow native preparation.
    pub spawn_cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Enable native deny-and-record capture for this child where the backend
    /// supports it (Windows MXC PSEC/V2 learning mode). Capture keeps every
    /// denial enforced and only records it; unsupported hosts ignore the
    /// request and run with the current behavior.
    pub capture_denials: bool,
}

impl PartialEq for SandboxProcessOptions {
    fn eq(&self, other: &Self) -> bool {
        self.current_dir == other.current_dir
            && self.environment == other.environment
            && self.stdin == other.stdin
            && self.stdout == other.stdout
            && self.stderr == other.stderr
            && self.spawn_cancelled.as_ref().map(std::sync::Arc::as_ptr)
                == other.spawn_cancelled.as_ref().map(std::sync::Arc::as_ptr)
            && self.capture_denials == other.capture_denials
    }
}
impl Eq for SandboxProcessOptions {}

pub struct SandboxAsyncChild {
    inner: SandboxAsyncChildInner,
}

enum SandboxAsyncChildInner {
    #[cfg(target_os = "macos")]
    Macos(macos::MacosSandboxAsyncChild),
    #[cfg(target_os = "linux")]
    Linux(linux::LinuxSandboxAsyncChild),
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
    Pipe(tokio::io::DuplexStream),
}

pub struct SandboxChildStdout {
    inner: SandboxChildStdoutInner,
}

enum SandboxChildStdoutInner {
    Tokio(tokio::process::ChildStdout),
    #[cfg(target_os = "windows")]
    Pipe(tokio::io::DuplexStream),
}

pub struct SandboxChildStderr {
    inner: SandboxChildStderrInner,
}

enum SandboxChildStderrInner {
    Tokio(tokio::process::ChildStderr),
    #[cfg(target_os = "windows")]
    Pipe(tokio::io::DuplexStream),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrongFilesystemSandboxMode {
    #[default]
    Off,
    Auto,
    Required,
}

impl SandboxStdio {
    pub(crate) fn to_stdio(self) -> Stdio {
        match self {
            Self::Inherit => Stdio::inherit(),
            Self::Null => Stdio::null(),
            Self::Piped => Stdio::piped(),
        }
    }
}

impl SandboxAsyncChild {
    pub fn spawn_shell(
        policy: &SandboxPolicy,
        program: &str,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> std::io::Result<Self> {
        policy
            .filesystem
            .ensure_root_bindings()
            .map_err(std::io::Error::other)?;
        #[cfg(target_os = "windows")]
        {
            if policy.strong_filesystem != StrongFilesystemSandboxMode::Off
                && filesystem_policy_requires_backend(&policy.filesystem)
            {
                let spawn_spec = policy.shell_spawn_spec(program, args.clone());
                let inner = windows::spawn_restricted_async(
                    policy,
                    &spawn_spec.program,
                    spawn_spec.args,
                    options,
                )?;
                return Ok(Self {
                    inner: SandboxAsyncChildInner::Windows(inner),
                });
            }
        }

        #[cfg(target_os = "linux")]
        if policy.strong_filesystem != StrongFilesystemSandboxMode::Off
            && linux::requires_backend(&policy.filesystem)
        {
            let inner = linux::spawn_restricted_async(policy, Path::new(program), args, options)?;
            return Ok(Self {
                inner: SandboxAsyncChildInner::Linux(inner),
            });
        }
        #[cfg(target_os = "macos")]
        if policy.strong_filesystem != StrongFilesystemSandboxMode::Off {
            let inner =
                macos::spawn_restricted_async(policy, PathBuf::from(program), args, options)?;
            return Ok(Self {
                inner: SandboxAsyncChildInner::Macos(inner),
            });
        }
        Self::spawn_shell_unwrapped(policy, program, args, options)
    }

    fn spawn_shell_unwrapped(
        policy: &SandboxPolicy,
        program: &str,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> std::io::Result<Self> {
        #[cfg(target_os = "windows")]
        let spawn_spec = policy.shell_spawn_spec(program, args);
        #[cfg(not(target_os = "windows"))]
        let spawn_spec = policy
            .strong_command_spawn_spec(PathBuf::from(program), args)
            .map_err(std::io::Error::other)?;
        let mut command = tokio::process::Command::new(spawn_spec.program);
        command.args(spawn_spec.args);
        let spawn_cancelled = options.spawn_cancelled.clone();
        apply_tokio_command_options(&mut command, options);
        #[cfg(windows)]
        let _creation = crate::process_spawn::creation_lock()?;
        if spawn_cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Process startup cancelled",
            ));
        }
        let inner = command.spawn()?;
        Ok(Self {
            inner: SandboxAsyncChildInner::Tokio(inner),
        })
    }

    pub fn take_stdin(&mut self) -> Option<SandboxChildStdin> {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            SandboxAsyncChildInner::Macos(child) => child.take_stdin().map(SandboxChildStdin::from),
            #[cfg(target_os = "linux")]
            SandboxAsyncChildInner::Linux(child) => child.take_stdin().map(SandboxChildStdin::from),
            SandboxAsyncChildInner::Tokio(child) => child.stdin.take().map(SandboxChildStdin::from),
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => {
                child.take_stdin().map(SandboxChildStdin::from_pipe)
            }
        }
    }

    pub fn take_stdout(&mut self) -> Option<SandboxChildStdout> {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            SandboxAsyncChildInner::Macos(child) => {
                child.take_stdout().map(SandboxChildStdout::from)
            }
            #[cfg(target_os = "linux")]
            SandboxAsyncChildInner::Linux(child) => {
                child.take_stdout().map(SandboxChildStdout::from)
            }
            SandboxAsyncChildInner::Tokio(child) => {
                child.stdout.take().map(SandboxChildStdout::from)
            }
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => {
                child.take_stdout().map(SandboxChildStdout::from_pipe)
            }
        }
    }

    pub fn take_stderr(&mut self) -> Option<SandboxChildStderr> {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            SandboxAsyncChildInner::Macos(child) => {
                child.take_stderr().map(SandboxChildStderr::from)
            }
            #[cfg(target_os = "linux")]
            SandboxAsyncChildInner::Linux(child) => {
                child.take_stderr().map(SandboxChildStderr::from)
            }
            SandboxAsyncChildInner::Tokio(child) => {
                child.stderr.take().map(SandboxChildStderr::from)
            }
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => {
                child.take_stderr().map(SandboxChildStderr::from_pipe)
            }
        }
    }

    /// Native denial records captured for a finished sandboxed child.
    ///
    /// Empty unless capture was requested, the backend produced records, and
    /// the child reached a terminal state. Records are bounded and only name
    /// filesystem read/write targets that the sandbox actually denied.
    pub fn take_denials(&mut self) -> Vec<denial::DenialRecord> {
        match &mut self.inner {
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => child.take_denials(),
            _ => Vec::new(),
        }
    }

    pub fn start_kill(&mut self) -> std::io::Result<()> {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            SandboxAsyncChildInner::Macos(child) => child.start_kill(),
            #[cfg(target_os = "linux")]
            SandboxAsyncChildInner::Linux(child) => child.start_kill(),
            SandboxAsyncChildInner::Tokio(child) => child.start_kill(),
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => child.start_kill(),
        }
    }

    pub fn id(&self) -> Option<u32> {
        match &self.inner {
            #[cfg(target_os = "macos")]
            SandboxAsyncChildInner::Macos(child) => child.id(),
            #[cfg(target_os = "linux")]
            SandboxAsyncChildInner::Linux(child) => child.id(),
            SandboxAsyncChildInner::Tokio(child) => child.id(),
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => Some(child.id()),
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match &mut self.inner {
            #[cfg(target_os = "macos")]
            SandboxAsyncChildInner::Macos(child) => child.try_wait(),
            #[cfg(target_os = "linux")]
            SandboxAsyncChildInner::Linux(child) => child.try_wait(),
            SandboxAsyncChildInner::Tokio(child) => child.try_wait(),
            #[cfg(target_os = "windows")]
            SandboxAsyncChildInner::Windows(child) => child.try_wait(),
        }
    }
}

impl Drop for SandboxAsyncChild {
    fn drop(&mut self) {
        let _ = self.start_kill();
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
    const fn from_pipe(file: tokio::io::DuplexStream) -> Self {
        Self {
            inner: SandboxChildStdinInner::Pipe(file),
        }
    }
}

impl SandboxChildStdout {
    #[cfg(target_os = "windows")]
    const fn from_pipe(file: tokio::io::DuplexStream) -> Self {
        Self {
            inner: SandboxChildStdoutInner::Pipe(file),
        }
    }
}

impl SandboxChildStderr {
    #[cfg(target_os = "windows")]
    const fn from_pipe(file: tokio::io::DuplexStream) -> Self {
        Self {
            inner: SandboxChildStderrInner::Pipe(file),
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
            SandboxChildStdinInner::Pipe(file) => Pin::new(file).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.inner {
            SandboxChildStdinInner::Tokio(stdin) => Pin::new(stdin).poll_flush(cx),
            #[cfg(target_os = "windows")]
            SandboxChildStdinInner::Pipe(file) => Pin::new(file).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.inner {
            SandboxChildStdinInner::Tokio(stdin) => Pin::new(stdin).poll_shutdown(cx),
            #[cfg(target_os = "windows")]
            SandboxChildStdinInner::Pipe(file) => Pin::new(file).poll_shutdown(cx),
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
            SandboxChildStdoutInner::Pipe(file) => Pin::new(file).poll_read(cx, buf),
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
            SandboxChildStderrInner::Pipe(file) => Pin::new(file).poll_read(cx, buf),
        }
    }
}

fn apply_tokio_command_options(
    command: &mut tokio::process::Command,
    options: SandboxProcessOptions,
) {
    command.envs(options.environment);
    if let Some(current_dir) = options.current_dir {
        command.current_dir(current_dir);
    }
    command
        .stdin(options.stdin.to_stdio())
        .stdout(options.stdout.to_stdio())
        .stderr(options.stderr.to_stdio());
}

#[cfg(target_os = "windows")]
const fn filesystem_policy_requires_backend(policy: &FileSystemSandboxPolicy) -> bool {
    !(policy.full_disk_read
        && policy.full_disk_write
        && policy.readable_roots.is_empty()
        && policy.writable_roots.is_empty()
        && policy.deny_read_paths.is_empty()
        && policy.deny_write_paths.is_empty())
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

pub fn resolve_path_for_check(path: &Path) -> PathBuf {
    if let Some(cached) = PATH_RESOLUTION_CACHE.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|cache| cache.get(path).cloned())
    }) {
        return cached;
    }

    let resolved = resolve_path_for_check_uncached(path);
    PATH_RESOLUTION_CACHE.with(|slot| {
        if let Some(cache) = slot.borrow_mut().as_mut() {
            cache.insert(path.to_path_buf(), resolved.clone());
        }
    });
    resolved
}

fn resolve_path_for_check_uncached(path: &Path) -> PathBuf {
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

thread_local! {
    static PATH_RESOLUTION_CACHE: RefCell<Option<HashMap<PathBuf, PathBuf>>> =
        const { RefCell::new(None) };
}

/// Cache canonicalized paths for one policy check. The cache is cleared before
/// this returns, so a replaced path cannot be reused by a later check.
fn with_path_resolution_cache<T>(check: impl FnOnce() -> T) -> T {
    let installed = PATH_RESOLUTION_CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(HashMap::new());
            true
        } else {
            false
        }
    });
    let result = check();
    if installed {
        PATH_RESOLUTION_CACHE.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
    result
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_native_policy_paths(policy: &FileSystemSandboxPolicy) -> Result<()> {
    for path in policy
        .readable_roots
        .iter()
        .chain(policy.writable_roots.iter().map(|root| &root.root))
        .chain(policy.deny_read_paths.iter())
        .chain(policy.deny_write_paths.iter())
        .chain(
            policy
                .writable_roots
                .iter()
                .flat_map(|root| &root.read_only_subpaths),
        )
    {
        if !path.is_absolute() || path.to_str().is_none() {
            return Err(miette!(
                "Native sandbox requires absolute Unicode policy paths"
            ));
        }
    }
    // A protected spelling cannot be rebound by replacing a writable symlink.
    for path in policy
        .deny_read_paths
        .iter()
        .chain(&policy.deny_write_paths)
        .chain(
            policy
                .writable_roots
                .iter()
                .flat_map(|root| &root.read_only_subpaths),
        )
    {
        for ancestor in path.ancestors() {
            if std::fs::symlink_metadata(ancestor).is_ok_and(|md| md.is_symlink())
                && ancestor
                    .parent()
                    .is_some_and(|parent| policy.is_path_writable(parent))
            {
                return Err(miette!(
                    "Cannot protect a path through a writable symlink: {}",
                    ancestor.display()
                ));
            }
        }
    }
    Ok(())
}

pub fn policy_paths_with_resolved(paths: &[PathBuf]) -> Vec<PathBuf> {
    with_path_resolution_cache(|| {
        let mut expanded = paths
            .iter()
            .flat_map(|path| [normalize_path(path), resolve_path_for_check(path)])
            .collect::<Vec<_>>();
        expanded.sort();
        expanded.dedup();
        expanded
    })
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

pub fn path_is_denied(path: &Path, denied_roots: &[PathBuf]) -> bool {
    denied_roots
        .iter()
        .any(|denied| path_is_or_descends_logical_or_resolved(path, denied))
}

impl FileSystemSandboxPolicy {
    pub const fn unrestricted() -> Self {
        Self {
            full_disk_read: true,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: Vec::new(),
            deny_write_paths: Vec::new(),
            bound_roots: Vec::new(),
        }
    }

    pub const fn is_unrestricted(&self) -> bool {
        self.full_disk_read
            && self.full_disk_write
            && self.readable_roots.is_empty()
            && self.writable_roots.is_empty()
            && self.deny_read_paths.is_empty()
            && self.deny_write_paths.is_empty()
            && self.bound_roots.is_empty()
    }

    pub fn ensure_root_bindings(&self) -> Result<()> {
        if self
            .bound_roots
            .iter()
            .any(|root| resolve_path_for_check(root) != normalize_path(root))
        {
            return Err(miette!(
                "An authorized filesystem root was replaced; reconfigure its access before continuing"
            ));
        }
        Ok(())
    }

    pub fn is_path_readable(&self, path: &Path) -> bool {
        with_path_resolution_cache(|| {
            if self.ensure_root_bindings().is_err() {
                return false;
            }
            if path_is_denied(path, &self.deny_read_paths) {
                return false;
            }
            if path_is_system_temporary(path) {
                return true;
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
        })
    }

    pub fn is_path_writable(&self, path: &Path) -> bool {
        with_path_resolution_cache(|| {
            if self.ensure_root_bindings().is_err() {
                return false;
            }
            if path_is_denied(path, &self.deny_write_paths) {
                return false;
            }
            if path_is_system_temporary(path) {
                return true;
            }
            if self.full_disk_write {
                return true;
            }
            self.writable_roots
                .iter()
                .any(|root| root.is_path_writable(path))
        })
    }
}

impl SandboxPolicy {
    pub const fn disabled() -> Self {
        Self {
            filesystem: FileSystemSandboxPolicy::unrestricted(),

            strong_filesystem: StrongFilesystemSandboxMode::Off,
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.filesystem.is_unrestricted()
            && self.strong_filesystem == StrongFilesystemSandboxMode::Off
    }

    pub fn resolve_path(path: &Path, base: Option<&Path>) -> PathBuf {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.unwrap_or_else(|| Path::new("/")).join(path)
        };
        normalize_path(&absolute)
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
        self.filesystem.ensure_root_bindings()?;
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
                StrongFilesystemSandboxMode::Auto if self.filesystem.is_unrestricted() => {
                    Ok(SandboxSpawnSpec { program, args })
                }
                StrongFilesystemSandboxMode::Required | StrongFilesystemSandboxMode::Auto => Err(
                    miette!("strong filesystem sandbox is not supported on this platform"),
                ),
                StrongFilesystemSandboxMode::Off => Ok(SandboxSpawnSpec { program, args }),
            }
        }
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn shell_spawn_spec(&self, program: &str, args: Vec<String>) -> SandboxSpawnSpec {
        if self.is_disabled() {
            return SandboxSpawnSpec {
                program: PathBuf::from(program),
                args,
            };
        }

        #[cfg(target_os = "macos")]
        {
            macos::wrap_shell_command(self, program, args)
                .expect("macOS shell sandbox command should be renderable")
        }

        #[cfg(target_os = "linux")]
        {
            linux::wrap_shell_command(self, program, args)
                .expect("Linux shell sandbox command should be renderable")
        }

        #[cfg(not(target_os = "macos"))]
        #[cfg(not(target_os = "linux"))]
        {
            SandboxSpawnSpec {
                program: PathBuf::from(program),
                args,
            }
        }
    }

    pub fn ensure_path_readable(&self, path: &Path, label: &str) -> Result<()> {
        self.filesystem.ensure_root_bindings()?;
        let normalized = normalize_path(path);
        if self.is_path_readable(&normalized) {
            Ok(())
        } else {
            self.filesystem.ensure_root_bindings()?;
            Err(AccessDenied::new(Access::Read, normalized, label).into())
        }
    }

    pub fn ensure_path_writable(&self, path: &Path, label: &str) -> Result<()> {
        self.filesystem.ensure_root_bindings()?;
        let normalized = normalize_path(path);
        if self.is_path_writable(&normalized) {
            Ok(())
        } else {
            self.filesystem.ensure_root_bindings()?;
            Err(AccessDenied::new(Access::Write, normalized, label).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        process::ExitStatus,
        time::Duration,
    };

    use super::{
        FileSystemSandboxPolicy, SandboxAsyncChild, SandboxPolicy, SandboxProcessOptions,
        SandboxStdio, StrongFilesystemSandboxMode, WritableRoot,
    };

    #[cfg(unix)]
    #[test]
    fn replacing_an_authorized_root_cannot_rebind_its_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("private.txt"), "outside data").unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let mut policy = FileSystemSandboxPolicy::unrestricted();
        policy.full_disk_read = false;
        policy.full_disk_write = false;
        policy.readable_roots = vec![root.clone()];
        policy.writable_roots = vec![WritableRoot {
            root: root.clone(),
            read_only_subpaths: vec![],
        }];
        policy.bound_roots = vec![root.clone()];
        assert!(policy.is_path_readable(&root));
        std::fs::rename(&root, directory.path().join("original-project")).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        assert!(policy.ensure_root_bindings().is_err());
        assert!(!policy.is_path_readable(&root.join("private.txt")));
        assert!(!policy.is_path_writable(&root.join("private.txt")));
    }

    #[test]
    fn strong_command_spawn_spec_is_passthrough_when_disabled() {
        let policy = SandboxPolicy::disabled();

        let spec = policy
            .strong_command_spawn_spec(Path::new("/bin/echo").to_path_buf(), vec!["ok".into()])
            .expect("spawn spec");

        assert_eq!(spec.program, Path::new("/bin/echo"));
        assert_eq!(spec.args, vec!["ok"]);
    }

    #[test]
    fn disabled_runtime_policy_removes_all_sandbox_restrictions() {
        let policy = SandboxPolicy::disabled();

        assert!(policy.is_disabled());
        assert_eq!(policy.strong_filesystem, StrongFilesystemSandboxMode::Off);
        assert!(policy.is_path_readable(Path::new("/home/user/.daat-locus/config.toml")));
        assert!(policy.is_path_writable(Path::new("/repo/src/main.rs")));

        let shell_spec =
            policy.shell_spawn_spec("/bin/sh", vec!["-lc".to_string(), "echo ok".to_string()]);
        assert_eq!(shell_spec.program, Path::new("/bin/sh"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[tokio::test]
    async fn strong_backend_denies_protected_runtime_paths() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        use tokio::io::AsyncReadExt;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let protected_home = tempdir.path().join(".daat-locus");
        let protected_config = protected_home.join("config");
        let workspace = tempdir.path().join("workspace");
        std::fs::create_dir_all(&protected_config).expect("create protected config");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let secret_file = protected_config.join("secret.txt");
        let protected_write_files = [
            protected_home.join("created-by-sandbox.txt"),
            protected_config.join("created-by-sandbox.txt"),
        ];
        let public_file = workspace.join("public.txt");
        let workspace_output = workspace.join("output.txt");
        std::fs::write(&secret_file, "secret").expect("write protected secret");
        std::fs::write(&public_file, "public").expect("write public file");

        let policy = SandboxPolicy {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: true,
                full_disk_write: false,
                readable_roots: Vec::new(),
                writable_roots: vec![WritableRoot {
                    root: workspace.clone(),
                    read_only_subpaths: Vec::new(),
                }],
                // Include redundant, unsorted directory and file boundaries:
                // native setup must handle them before starting the command.
                deny_read_paths: vec![
                    protected_config.clone(),
                    protected_home.clone(),
                    secret_file.clone(),
                ],
                deny_write_paths: vec![protected_config, protected_home, secret_file.clone()],
                bound_roots: Vec::new(),
            },
            strong_filesystem: StrongFilesystemSandboxMode::Required,
        };
        let mut write_probes = protected_write_files.to_vec();
        write_probes.push(secret_file.clone());
        let (program, args) =
            conformance_probe_command(&secret_file, &write_probes, &public_file, &workspace_output);
        let options = SandboxProcessOptions {
            environment: Vec::new(),
            current_dir: Some(workspace.clone()),
            stdin: SandboxStdio::Null,
            stdout: SandboxStdio::Piped,
            stderr: SandboxStdio::Piped,
            spawn_cancelled: None,
            capture_denials: false,
        };

        let mut child = SandboxAsyncChild::spawn_shell(&policy, &program, args, options)
            .expect("required native sandbox must be available for this conformance test");
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
        for path in protected_write_files {
            assert!(
                !path.exists(),
                "sandboxed child wrote into {}",
                path.display()
            );
        }
        assert_eq!(std::fs::read_to_string(&secret_file).unwrap(), "secret");
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

    #[cfg(unix)]
    fn conformance_probe_command(
        secret_file: &Path,
        protected_write_files: &[PathBuf],
        public_file: &Path,
        workspace_output: &Path,
    ) -> (String, Vec<String>) {
        let protected_writes = protected_write_files
            .iter()
            .map(|path| sh_quote(path))
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!(
            r#"secret={secret}
public_file={public_file}
workspace_output={workspace_output}
read_denied=0
[ "$(cat "$secret" 2>/dev/null)" != secret ] && read_denied=1
write_denied=1
for protected_write in {protected_writes}; do
    if printf bad 2>/dev/null > "$protected_write"; then write_denied=0; fi
done
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
        protected_write_files: &[PathBuf],
        public_file: &Path,
        workspace_output: &Path,
    ) -> (String, Vec<String>) {
        let system_root = std::env::var_os("SystemRoot").map_or_else(
            || std::path::PathBuf::from(r"C:\Windows"),
            std::path::PathBuf::from,
        );
        let powershell = system_root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let script = [
            format!("$secret = {}", ps_quote(secret_file)),
            format!("$publicFile = {}", ps_quote(public_file)),
            format!("$workspaceOutput = {}", ps_quote(workspace_output)),
            "$readDenied = $false".to_string(),
            "try { $readDenied = (Get-Content -Raw -LiteralPath $secret -ErrorAction Stop) -ne 'secret' } catch { $readDenied = $true }".to_string(),
            "$writeDenied = $true".to_string(),
            format!("foreach ($protectedWrite in @({})) {{ try {{ Set-Content -LiteralPath $protectedWrite -Value 'bad' -NoNewline -ErrorAction Stop; $writeDenied = $false }} catch {{ }} }}", protected_write_files.iter().map(|path| ps_quote(path)).collect::<Vec<_>>().join(",")),
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
    fn explicit_denial_covers_symlink_targets() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let daat_locus_home = tempdir.path().join(".daat-locus");
        let workspace_dir = tempdir.path().join("workspace");
        std::fs::create_dir_all(&daat_locus_home).expect("create protected home");
        std::fs::create_dir_all(&workspace_dir).expect("create workspace");
        std::fs::write(daat_locus_home.join("config.toml"), "secret").expect("write secret");
        let linked_home = workspace_dir.join("linked-home");
        symlink(&daat_locus_home, &linked_home).expect("symlink protected home");

        let mut policy = SandboxPolicy::disabled();
        policy
            .filesystem
            .deny_read_paths
            .push(daat_locus_home.clone());
        policy
            .filesystem
            .deny_write_paths
            .push(daat_locus_home.clone());

        assert!(!policy.is_path_readable(&linked_home));
        assert!(!policy.is_path_readable(&linked_home.join("config.toml")));
        assert!(!policy.is_path_writable(&linked_home.join("new-state")));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_write_denial_covers_symlink_targets() {
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

        let mut policy = SandboxPolicy::disabled();
        policy.filesystem.deny_write_paths.push(source_root.clone());

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
            bound_roots: Vec::new(),
        };

        assert!(policy.is_path_writable(&workspace_dir.join("normal.txt")));
        assert!(!policy.is_path_writable(&linked_outside.join("escaped.txt")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_spawn_spec_uses_sandbox_exec_on_macos() {
        let policy = SandboxPolicy {
            strong_filesystem: StrongFilesystemSandboxMode::Required,
            ..SandboxPolicy::disabled()
        };
        let spawn_spec =
            policy.shell_spawn_spec("bash", vec!["-lc".to_string(), "pwd".to_string()]);
        assert_eq!(spawn_spec.program, Path::new("/usr/bin/sandbox-exec"));
        assert!(spawn_spec.args.iter().any(|arg| arg == "--"));
    }
}

#[cfg(test)]
mod system_temp_tests {
    use super::*;

    #[test]
    fn system_temporary_paths_never_require_a_scope() {
        let temp = std::env::temp_dir();
        let mut policy = SandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        policy.filesystem.readable_roots = vec![];
        policy.filesystem.writable_roots = vec![];
        let sandbox = policy.clone();
        assert!(sandbox.is_path_readable(&temp.join("daat-scope-check.txt")));
        assert!(sandbox.is_path_writable(&temp.join("daat-scope-check.txt")));
        assert!(sandbox.ensure_path_readable(&temp, "temp").is_ok());
        assert!(sandbox.ensure_path_writable(&temp, "temp").is_ok());
        let mut denied = sandbox.clone();
        denied.filesystem.deny_write_paths = vec![temp.clone()];
        assert!(!denied.is_path_writable(&temp.join("daat-scope-check.txt")));
        let outside = sandbox.clone();
        assert!(!outside.is_path_writable(&temp.parent().unwrap().join("daat-outside-check")));
    }
}
