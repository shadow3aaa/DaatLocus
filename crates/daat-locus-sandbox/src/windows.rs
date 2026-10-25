//! Native MXC/PSEC process isolation. No host DACL mutation or legacy fallback.
use std::{
    collections::BTreeMap,
    io,
    os::windows::{ffi::OsStringExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::atomic::Ordering,
};

use appcontainer_common::base_container_runner::BaseContainerRunner;
use learning_mode_windows::SecurityEnvironmentApi;
use parking_lot::Mutex;
use tokio::io::DuplexStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use wxc_common::{
    cmdline::{CommandLineContext, cmdline_from_argv_for_context},
    filesystem_object::normalize_object_conflicts,
    logger::{Logger, Mode},
    models::{
        BaseProcessUiConfig, CaptureDenialsConfig, ContainerPolicy, ExecutionRequest,
        FallbackPolicy, NetworkAction, NetworkEgressPolicy, NetworkIngressPolicy, NetworkPolicy,
        UiPolicy,
    },
    sandbox_process::{SandboxBackend, SandboxProcess, StdioMode},
};

use super::{
    SandboxPolicy, SandboxProcessOptions, SandboxStdio, policy_paths_with_resolved,
    system_temporary_paths,
};
use crate::denial::{DenialRecord, MAX_CAPTURE_DENIALS, denials_from_capture_document};

#[link(name = "advapi32")]
unsafe extern "system" {}

/// Prefix of the per-spawn directory that owns the captureDenials output file.
const CAPTURE_DIRECTORY_PREFIX: &str = "daat-locus-capture-";
/// Upper bound for one captureDenials JSON document read back from disk.
const MAX_CAPTURE_DOCUMENT_BYTES: u64 = 1024 * 1024;

/// Creates the per-spawn directory for a captureDenials output document.
/// Errors leave capture disabled rather than failing the spawn.
fn prepare_capture_document_path() -> Option<PathBuf> {
    let directory = std::env::temp_dir().join(format!(
        "{CAPTURE_DIRECTORY_PREFIX}{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory.join("denials.json"))
}

/// Bounded read of a captureDenials JSON document written by the runner.
fn read_capture_document(path: &Path) -> Option<(Vec<DenialRecord>, bool)> {
    use std::io::Read;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CAPTURE_DOCUMENT_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(path)
        .ok()?
        .take(MAX_CAPTURE_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CAPTURE_DOCUMENT_BYTES {
        return None;
    }
    Some(denials_from_capture_document(&bytes, MAX_CAPTURE_DENIALS))
}

pub struct WindowsSandboxAsyncChild {
    process: Mutex<Box<dyn SandboxProcess>>,
    stdin: Option<DuplexStream>,
    stdout: Option<DuplexStream>,
    stderr: Option<DuplexStream>,
    status: Option<ExitStatus>,
    shutdown: watch::Sender<()>,
    capture_output: Option<PathBuf>,
    denials: Option<Vec<DenialRecord>>,
}

impl WindowsSandboxAsyncChild {
    pub fn take_stdin(&mut self) -> Option<DuplexStream> {
        self.stdin.take()
    }
    pub fn take_stdout(&mut self) -> Option<DuplexStream> {
        self.stdout.take()
    }
    pub fn take_stderr(&mut self) -> Option<DuplexStream> {
        self.stderr.take()
    }
    pub fn id(&self) -> u32 {
        self.process.lock().id()
    }
    pub fn start_kill(&mut self) -> io::Result<()> {
        self.process.get_mut().kill()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() && self.process.get_mut().try_wait()?.is_some() {
            // The root has exited. SDK wait kills/reaps remaining descendants before
            // releasing PSEC. Output pumps continue draining the taken pipes.
            let code = self.process.get_mut().wait()?;
            self.status = Some(ExitStatus::from_raw(code as u32));
            self.harvest_captured_denials();
        }
        Ok(self.status)
    }

    /// Reads and clears the capture document once this child reached a
    /// terminal state; bounded, best-effort, and local to this spawn.
    fn harvest_captured_denials(&mut self) {
        if self.denials.is_some() {
            return;
        }
        let mut document_path = self
            .process
            .get_mut()
            .output_metadata()
            .and_then(|metadata| metadata.capture_denials.as_ref())
            .map(|capture| PathBuf::from(&capture.output_path));
        if document_path.is_none()
            && let Some(configured) = &self.capture_output
            && let Some(directory) = configured.parent()
            && let Ok(entries) = std::fs::read_dir(directory)
        {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("denials") {
                    document_path = Some(entry.path());
                    break;
                }
            }
        }
        if self.capture_output.is_some() {
            self.denials = Some(
                document_path
                    .as_deref()
                    .and_then(read_capture_document)
                    .map(|(records, _truncated)| records)
                    .unwrap_or_default(),
            );
        }
        self.remove_capture_directory();
    }

    /// Native denial records captured for this child, if any.
    pub fn take_denials(&mut self) -> Vec<DenialRecord> {
        self.denials.take().unwrap_or_default()
    }

    /// Deletes the per-spawn capture directory this child owns, if any.
    fn remove_capture_directory(&self) {
        let Some(directory) = self
            .capture_output
            .as_deref()
            .and_then(|path| path.parent())
        else {
            return;
        };
        let owned = directory
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(CAPTURE_DIRECTORY_PREFIX));
        if owned {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

fn cancelled(options: &SandboxProcessOptions) -> io::Result<()> {
    if options
        .spawn_cancelled
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Acquire))
    {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Process startup cancelled",
        ));
    }
    Ok(())
}

pub fn spawn_restricted_async(
    policy: &SandboxPolicy,
    program: &Path,
    args: Vec<String>,
    options: SandboxProcessOptions,
) -> io::Result<WindowsSandboxAsyncChild> {
    cancelled(&options)?;
    if !BaseContainerRunner::is_process_security_environment_usable() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows process isolation requires usable MXC/PSEC; update Windows to a supported build",
        ));
    }
    // Deny-and-record capture is enabled only when this host can run the
    // native PSEC/V2 learning-mode trace: the request then keeps every
    // denial enforced and only records it. Unsupported hosts keep today's
    // behavior (no capture, no guarded-WPR/UAC fallback).
    let mut capture_output =
        if options.capture_denials && BaseContainerRunner::is_capture_denials_usable() {
            prepare_capture_document_path()
        } else {
            None
        };
    let mut request = build_request(policy, program, args.clone(), &options)?;
    if let Some(capture_document) = capture_output.as_deref() {
        // We call BaseContainerRunner directly rather than MXC's config
        // builder. Starting a trace alone does not enable OS event emission;
        // deny-and-record requires this capability as well. Never enable
        // permissiveLearningMode: collection must preserve every denial.
        request
            .policy
            .capabilities
            .push("learningModeLogging".into());
        request.policy.capture_denials = Some(CaptureDenialsConfig {
            output_path: Some(unicode(capture_document)?),
            ..Default::default()
        });
    }
    if !request.policy.denied_paths.is_empty()
        && !SecurityEnvironmentApi::load()
            .map_err(io::Error::other)?
            .supports_deny_paths()
            .map_err(io::Error::other)?
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "This Windows MXC/PSEC implementation cannot enforce denied paths",
        ));
    }
    cancelled(&options)?;
    let inherited = [options.stdin, options.stdout, options.stderr]
        .iter()
        .all(|mode| *mode == SandboxStdio::Inherit);
    // Mixing a host console reader into asynchronous pipes cannot be cancelled
    // reliably. Terminal and MCP always use piped/null stdin.
    if options.stdin == SandboxStdio::Inherit && !inherited {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "MXC mixed stdio requires piped or null stdin",
        ));
    }
    let mut logger = Logger::new(Mode::Buffer);
    // SDK pipe handles are briefly inheritable. Coordinate with other
    // native spawns so they cannot capture these process-private channels.
    let creation = crate::process_spawn::creation_lock()?;
    cancelled(&options)?;
    let mut process = match BaseContainerRunner::new().spawn(
        &request,
        &mut logger,
        if inherited {
            StdioMode::Inherit
        } else {
            StdioMode::Pipes
        },
    ) {
        Ok(process) => process,
        Err(error) => {
            // A host that advertised capture but cannot complete it must keep
            // today's behavior instead of failing the spawn: retry once with
            // capture disabled when the refusal is capture-specific.
            if capture_output.is_some() && error.error_message.contains("captureDenials") {
                capture_output = None;
                let request = build_request(policy, program, args, &options)?;
                BaseContainerRunner::new()
                    .spawn(
                        &request,
                        &mut logger,
                        if inherited {
                            StdioMode::Inherit
                        } else {
                            StdioMode::Pipes
                        },
                    )
                    .map_err(|error| io::Error::other(error.error_message))?
            } else {
                return Err(io::Error::other(error.error_message));
            }
        }
    };
    drop(creation);
    // Never publish SDK logs: they can contain command/environment secrets.
    cancelled(&options)?; // SDK Drop kills the tree if startup was cancelled.
    let (shutdown, stop) = watch::channel(());
    let stdin = process.take_stdin().and_then(|mut writer| {
        if options.stdin != SandboxStdio::Piped { return None; }
        let (client, mut pump) = tokio::io::duplex(64 * 1024);
        let mut stop = stop.clone();
        tokio::spawn(async move {
            let mut buffer = vec![0;8192];
            loop {
                let count = tokio::select! { _ = stop.changed() => break, result = pump.read(&mut buffer) => match result { Ok(0)|Err(_) => break, Ok(count) => count } };
                // No idle blocking reader thread. A cancelled process closes its
                // native pipe, releasing any currently blocked write as well.
                let bytes = buffer[..count].to_vec();
                match tokio::task::spawn_blocking(move || {
                    io::Write::write_all(&mut writer,&bytes)?;
                    io::Write::flush(&mut writer)?;
                    Ok::<_,io::Error>(writer)
                }).await {
                    Ok(Ok(returned)) => writer = returned,
                    _ => break,
                }
            }
        });
        Some(client)
    });
    let stdout = output_pipe(process.take_stdout(), options.stdout, false);
    let stderr = output_pipe(process.take_stderr(), options.stderr, true);
    Ok(WindowsSandboxAsyncChild {
        process: Mutex::new(process),
        stdin,
        stdout,
        stderr,
        status: None,
        shutdown,
        capture_output,
        denials: None,
    })
}

impl Drop for WindowsSandboxAsyncChild {
    fn drop(&mut self) {
        let _ = self.process.get_mut().kill();
        let _ = self.shutdown.send(());
        self.remove_capture_directory();
    }
}

fn output_pipe(
    reader: Option<Box<dyn io::Read + Send>>,
    mode: SandboxStdio,
    stderr: bool,
) -> Option<DuplexStream> {
    let mut reader = reader?;
    let (client, mut pump) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        loop {
            let read = tokio::task::spawn_blocking(move || {
                let mut buffer = vec![0; 8192];
                let count = reader.read(&mut buffer)?;
                buffer.truncate(count);
                Ok::<_, io::Error>((reader, buffer))
            });
            // Taken output streams outlive the process handle. Terminating the
            // SDK job closes native writers; pending bytes must still drain to
            // the consumer instead of being cancelled with stdin.
            let result = read.await;
            let (returned, bytes) = match result {
                Ok(Ok(result)) => result,
                _ => break,
            };
            reader = returned;
            if bytes.is_empty() {
                break;
            }
            if mode == SandboxStdio::Piped {
                if pump.write_all(&bytes).await.is_err() {
                    break;
                }
            } else if mode == SandboxStdio::Inherit {
                let result = tokio::task::spawn_blocking(move || {
                    if stderr {
                        io::Write::write_all(&mut io::stderr(), &bytes)
                    } else {
                        io::Write::write_all(&mut io::stdout(), &bytes)
                    }
                })
                .await;
                if !matches!(result, Ok(Ok(()))) {
                    break;
                }
            }
        }
    });
    (mode == SandboxStdio::Piped).then_some(client)
}

fn unicode(path: &Path) -> io::Result<String> {
    path.to_str()
        .filter(|value| !value.contains('\0'))
        .map(str::to_owned)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "MXC requires Unicode paths without NUL",
            )
        })
}

fn paths(paths: &[PathBuf]) -> io::Result<Vec<String>> {
    policy_paths_with_resolved(paths)
        .iter()
        .map(|path| unicode(path))
        .collect()
}

fn volumes() -> io::Result<Vec<PathBuf>> {
    let drives = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    if drives == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((0..26)
        .filter(|bit| drives & (1 << bit) != 0)
        .map(|bit| PathBuf::from(format!("{}:\\", (b'A' + bit) as char)))
        .collect())
}

fn expand_volumes(roots: Vec<String>) -> io::Result<Vec<String>> {
    let mut result = Vec::new();
    for root in roots {
        let path = Path::new(&root);
        if path.parent().is_none() {
            let entries = match std::fs::read_dir(path) {
                Ok(entries) => entries,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) || matches!(error.raw_os_error(), Some(21 | 53 | 55 | 1201 | 1167)) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            // A volume grant covers only its root object in PSEC. Expand one
            // level, never recursively enumerate the workspace.
            for entry in entries {
                let path = entry?.path();
                if wxc_common::filesystem_object::compare_existing_filesystem_objects(&path, &path)
                    == wxc_common::filesystem_object::ExistingObjectComparison::Same
                {
                    result.push(unicode(&path)?);
                }
            }
        }
        result.push(root);
    }
    result.sort_by_key(|path| path.to_lowercase());
    result.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    Ok(result)
}

fn known_folder(folder: &windows::core::GUID) -> io::Result<PathBuf> {
    use windows::Win32::{
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
        UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    };
    let status = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if status.is_err() && status.0 != -2147417850 {
        return Err(io::Error::other(format!(
            "COM initialization failed: {status}"
        )));
    }
    struct Com(bool);
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }
    let _com = Com(status.is_ok());
    let path =
        unsafe { SHGetKnownFolderPath(folder, KF_FLAG_DEFAULT, None) }.map_err(io::Error::other)?;
    let value = unsafe { path.to_string() }.map_err(io::Error::other);
    unsafe {
        CoTaskMemFree(Some(path.0.cast()));
    }
    Ok(PathBuf::from(value?))
}

pub(super) fn platform_roots() -> io::Result<Vec<PathBuf>> {
    use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86};
    let mut buffer = vec![0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    let mut roots = vec![PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..length],
    ))];
    for folder in [FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86] {
        roots.push(known_folder(&folder)?);
    }
    Ok(roots)
}

fn build_request(
    policy: &SandboxPolicy,
    program: &Path,
    args: Vec<String>,
    options: &SandboxProcessOptions,
) -> io::Result<ExecutionRequest> {
    let fs = &policy.filesystem;
    let mut read = super::runtime_read_roots()?.to_vec();
    read.extend(fs.readable_roots.clone());
    let mut write = fs
        .writable_roots
        .iter()
        .map(|root| root.root.clone())
        .collect::<Vec<_>>();
    let mut deny = fs.deny_read_paths.clone();
    if fs.full_disk_read || fs.full_disk_write {
        read.extend(volumes()?);
    }
    if fs.full_disk_write {
        write.extend(volumes()?);
    }
    // The platform's shared temporary directories are baseline-authorized:
    // sandboxed children always receive them without any approval scope.
    read.extend(system_temporary_paths().iter().cloned());
    write.extend(system_temporary_paths().iter().cloned());
    if fs.deny_all_writes {
        write.clear();
    }
    let mut readonly = fs.deny_write_paths.clone();
    readonly.extend(
        fs.writable_roots
            .iter()
            .flat_map(|root| root.read_only_subpaths.clone()),
    );
    // A read-only restriction must never grant an otherwise unreadable subtree.
    let carveouts = paths(&readonly)?;
    for path in readonly {
        if fs.is_path_readable(&path) {
            read.push(path);
        } else {
            deny.push(path);
        }
    }
    let mut container = ContainerPolicy {
        capabilities: vec!["registryRead".into()],
        readwrite_paths: expand_volumes(paths(&write)?)?,
        readonly_paths: expand_volumes(paths(&read)?)?,
        denied_paths: paths(&deny)?,
        fallback: FallbackPolicy {
            allow_dacl_mutation: false,
        },
        default_network_policy: NetworkPolicy::Allow,
        allow_local_network: true,
        network_egress: Some(NetworkEgressPolicy {
            default: NetworkAction::Allow,
            ..Default::default()
        }),
        network_ingress: Some(NetworkIngressPolicy {
            default: NetworkAction::Allow,
            host_loopback: NetworkAction::Allow,
        }),
        network_specified: true,
        network_mode_specified: true,
        ui: UiPolicy {
            disable: false,
            ..Default::default()
        },
        base_process_ui: BaseProcessUiConfig {
            isolation: "desktop".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    container
        .readwrite_paths
        .retain(|path| !carveouts.iter().any(|r| r.eq_ignore_ascii_case(path)));
    container.readonly_paths.retain(|path| {
        !container
            .readwrite_paths
            .iter()
            .any(|w| w.eq_ignore_ascii_case(path))
    });
    // Reconcile object aliases, including hardlinks, rather than trusting strings.
    if let Some(normalized) = normalize_object_conflicts(&container, &mut Logger::new(Mode::Buffer))
        .map_err(io::Error::other)?
    {
        container = normalized;
    }
    let mut env = process_environment(std::env::vars_os(), &options.environment)?;
    // SDK resolves native NT namespaces through these variables. Bind them to
    // OS-known directories rather than caller-controlled environment values.
    let windows = platform_roots()?.remove(0);
    env.insert(
        "LOCALAPPDATA".into(),
        unicode(&known_folder(
            &windows::Win32::UI::Shell::FOLDERID_LocalAppData,
        )?)?,
    );
    env.insert("SYSTEMROOT".into(), unicode(&windows)?);
    env.insert("WINDIR".into(), unicode(&windows)?);
    env.insert(
        "SYSTEMDRIVE".into(),
        unicode(&windows)?.chars().take(2).collect(),
    );
    if let Some(temp) = system_temporary_paths().first() {
        env.insert("TEMP".into(), unicode(temp)?);
        env.insert("TMP".into(), unicode(temp)?);
    }
    let cwd = options
        .current_dir
        .clone()
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)?;
    if !cwd.is_absolute() {
        return Err(io::Error::other(
            "MXC requires an absolute working directory",
        ));
    }
    // PSEC's grants use the canonical object spelling. PowerShell otherwise
    // retains an 8.3 cwd (common in Windows service/CI profiles) and rejects
    // ordinary commands in an otherwise authorized workspace.
    let cwd = dunce::canonicalize(cwd)?;
    let mut argv = vec![unicode(program)?];
    argv.extend(args);
    Ok(ExecutionRequest {
        script_code: cmdline_from_argv_for_context(&argv, CommandLineContext::WindowsCreateProcess)
            .map_err(io::Error::other)?,
        working_directory: unicode(&cwd)?,
        env: env
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        policy: container,
        ..Default::default()
    })
}

fn process_environment(
    inherited: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    overrides: &[(String, String)],
) -> io::Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for (key, value) in inherited {
        let key = key
            .into_string()
            .map_err(|_| io::Error::other("MXC requires Unicode environment names"))?;
        let value = value
            .into_string()
            .map_err(|_| io::Error::other("MXC requires Unicode environment values"))?;
        env.insert(key.to_uppercase(), value);
    }
    for (key, value) in overrides {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            return Err(io::Error::other("Invalid process environment"));
        }
        env.insert(key.to_uppercase(), value.clone());
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSystemSandboxPolicy, StrongFilesystemSandboxMode, WritableRoot};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn process_environment_inherits_names_and_preserves_values() {
        let inherited = [
            ("Path", r"C:\Tool One\bin;C:\Tool Two\bin;C:\Tool One\bin"),
            ("PATHEXT", ".CMD;.EXE;.BAT"),
            ("CARGO_HOME", r"C:\Build Cache\cargo"),
            ("RUSTUP_HOME", r"C:\Build Cache\rustup"),
            ("custom_SETTING", "Keep Case and spaces"),
            ("Override_OPTION", "inherited option"),
            ("APP_TOKEN", "synthetic token fixture"),
            ("APP_SECRET", "synthetic secret fixture"),
        ];
        let env = process_environment(
            inherited
                .into_iter()
                .map(|(name, value)| (name.into(), value.into())),
            &[("override_option".into(), "explicit override".into())],
        )
        .unwrap();
        assert_eq!(
            env["PATH"],
            r"C:\Tool One\bin;C:\Tool Two\bin;C:\Tool One\bin"
        );
        assert_eq!(env["PATHEXT"], ".CMD;.EXE;.BAT");
        assert_eq!(env["CARGO_HOME"], r"C:\Build Cache\cargo");
        assert_eq!(env["RUSTUP_HOME"], r"C:\Build Cache\rustup");
        assert_eq!(env["CUSTOM_SETTING"], "Keep Case and spaces");
        assert_eq!(env["OVERRIDE_OPTION"], "explicit override");
        assert_eq!(env["APP_TOKEN"], "synthetic token fixture");
        assert_eq!(env["APP_SECRET"], "synthetic secret fixture");
        assert!(!env.contains_key("custom_SETTING"));
    }

    fn policy(root: &Path) -> SandboxPolicy {
        SandboxPolicy {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: false,
                deny_all_writes: false,
                full_disk_write: false,
                readable_roots: vec![root.to_owned()],
                writable_roots: vec![WritableRoot {
                    root: root.to_owned(),
                    read_only_subpaths: vec![],
                }],
                deny_read_paths: vec![],
                deny_write_paths: vec![],
                bound_roots: vec![],
            },
            strong_filesystem: StrongFilesystemSandboxMode::Required,
        }
    }
    fn quote(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "''"))
    }
    fn launch(
        policy: &SandboxPolicy,
        root: &Path,
        script: String,
        stdin: SandboxStdio,
    ) -> WindowsSandboxAsyncChild {
        let shell = platform_roots()
            .unwrap()
            .remove(0)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        spawn_restricted_async(
            policy,
            &shell,
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script,
            ],
            SandboxProcessOptions {
                current_dir: Some(root.to_owned()),
                stdin,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
                ..Default::default()
            },
        )
        .unwrap()
    }
    async fn collect(mut child: WindowsSandboxAsyncChild) -> (ExitStatus, String, String) {
        let mut out = child.take_stdout().unwrap();
        let mut err = child.take_stderr().unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::try_join!(
                async {
                    let mut value = String::new();
                    out.read_to_string(&mut value).await?;
                    Ok::<_, io::Error>(value)
                },
                async {
                    let mut value = String::new();
                    err.read_to_string(&mut value).await?;
                    Ok::<_, io::Error>(value)
                },
                async {
                    loop {
                        if let Some(status) = child.try_wait()? {
                            return Ok::<_, io::Error>(status);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            )
        })
        .await
        .expect("MXC process timed out")
        .unwrap();
        (result.2, result.0, result.1)
    }

    #[test]
    fn cancelled_startup_does_not_probe_or_launch() {
        let temp = tempfile::tempdir().unwrap();
        let result = spawn_restricted_async(
            &policy(temp.path()),
            Path::new("missing.exe"),
            vec![],
            SandboxProcessOptions {
                spawn_cancelled: Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                    true,
                ))),
                ..Default::default()
            },
        );
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::Interrupted));
    }

    #[tokio::test]
    async fn path_is_preserved_but_command_discovery_requires_tool_directory_access() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        let temp = crate::test_support::scoped_directory();
        let root = temp.path().join("work");
        let tools = temp.path().join("tools");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&tools).unwrap();
        std::fs::write(
            tools.join("dl-path-fixture.cmd"),
            "@echo off\r\necho fixture-ran\r\n",
        )
        .unwrap();
        let shell = platform_roots()
            .unwrap()
            .remove(0)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let path = format!("{};{}", tools.display(), shell.parent().unwrap().display());
        for allow_tools in [false, true] {
            let mut policy = policy(&root);
            if allow_tools {
                policy.filesystem.readable_roots.push(tools.clone());
            }
            let script = format!(
                "$ErrorActionPreference='Stop'; if ($env:PATH -cne {}) {{ exit 10 }}; if (Get-Command dl-path-fixture.cmd -ErrorAction SilentlyContinue) {{ [Console]::WriteLine('resolved') }} else {{ [Console]::WriteLine('unresolved') }}",
                quote(Path::new(&path)),
            );
            let child = spawn_restricted_async(
                &policy,
                &shell,
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    script,
                ],
                SandboxProcessOptions {
                    current_dir: Some(root.clone()),
                    environment: vec![("PATH".into(), path.clone())],
                    stdin: SandboxStdio::Null,
                    stdout: SandboxStdio::Piped,
                    stderr: SandboxStdio::Piped,
                    ..Default::default()
                },
            )
            .unwrap();
            let (status, out, err) = collect(child).await;
            assert!(status.success(), "{status}; stdout={out}; stderr={err}");
            assert_eq!(
                out.trim(),
                if allow_tools {
                    "resolved"
                } else {
                    "unresolved"
                }
            );
        }
    }

    #[tokio::test]
    async fn mxc_enforces_scopes_and_future_denies_with_streaming_stdio() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        let temp = crate::test_support::scoped_directory();
        let root = temp.path().join("work");
        std::fs::create_dir(&root).unwrap();
        let inside = root.join("inside.txt");
        let outside = temp.path().join("outside.txt");
        let secret = root.join("private.txt");
        let readonly = root.join("readonly.txt");
        let future = root.join("future").join("secret.txt");
        let output = root.join("output.txt");
        for (path, value) in [
            (&inside, "inside"),
            (&outside, "outside"),
            (&secret, "secret"),
            (&readonly, "fixed"),
        ] {
            std::fs::write(path, value).unwrap();
        }
        let before = [&root, &inside, &secret, &readonly].map(|path| dacl(path));
        let mut policy = policy(&root);
        policy.filesystem.deny_read_paths =
            vec![secret.clone(), future.parent().unwrap().to_owned()];
        policy.filesystem.deny_write_paths = policy.filesystem.deny_read_paths.clone();
        policy.filesystem.writable_roots[0]
            .read_only_subpaths
            .push(readonly.clone());
        let script = format!(
            r#"$ErrorActionPreference='Stop'; if([IO.File]::ReadAllText({}) -ne 'inside'){{exit 10}}; [IO.File]::WriteAllText({},'persisted'); [Console]::WriteLine('ready'); $null=[Console]::ReadLine(); foreach($p in @({},{},{})){{$ok=$false;try{{$null=[IO.File]::ReadAllText($p);$ok=$true}}catch{{}};if($ok){{exit 20}}}}; $ok=$false;try{{[IO.File]::WriteAllText({},'bad');$ok=$true}}catch{{}};if($ok){{exit 30}}; for($i=0;$i -lt 6000;$i++){{[Console]::Out.Write('abcdefghij');[Console]::Error.Write('0123456789')}}"#,
            quote(&inside),
            quote(&output),
            quote(&outside),
            quote(&secret),
            quote(&future),
            quote(&readonly)
        );
        let mut child = launch(&policy, &root, script, SandboxStdio::Piped);
        // Data created by the host after policy installation must stay denied.
        std::fs::create_dir(future.parent().unwrap()).unwrap();
        std::fs::write(&future, "new secret").unwrap();
        let mut stdin = child.take_stdin().unwrap();
        stdin.write_all(b"go\n").await.unwrap();
        drop(stdin);
        let (status, out, err) = collect(child).await;
        assert!(status.success(), "{status}; stdout={out}; stderr={err}");
        assert_eq!(out.trim_end().len(), 60007); // ready + newline + 60k bytes
        assert_eq!(err.len(), 60000);
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "persisted");
        assert_eq!(std::fs::read_to_string(&readonly).unwrap(), "fixed");
        assert_eq!(std::fs::read_to_string(&future).unwrap(), "new secret");
        assert_eq!(
            [&root, &inside, &secret, &readonly].map(|path| dacl(path)),
            before,
            "sandbox changed host DACLs"
        );
    }
    #[tokio::test]
    async fn dropping_mxc_kills_descendants_with_open_pipes() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        use tokio::io::{AsyncBufReadExt, BufReader};
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };
        let temp = tempfile::tempdir().unwrap();
        let shell = platform_roots()
            .unwrap()
            .remove(0)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let script = format!(
            "$p=Start-Process -FilePath {} -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 300' -WindowStyle Hidden -PassThru; [Console]::WriteLine($p.Id); $null=[Console]::ReadLine()",
            quote(&shell)
        );
        let mut child = launch(
            &policy(temp.path()),
            temp.path(),
            script,
            SandboxStdio::Piped,
        );
        let stdin = child.take_stdin().unwrap();
        let mut output = BufReader::new(child.take_stdout().unwrap());
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            output.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let pid = line.trim().parse::<u32>().expect("descendant PID");
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!handle.is_null());
        drop(child);
        let result = unsafe { WaitForSingleObject(handle, 5000) };
        unsafe {
            CloseHandle(handle);
        }
        assert_eq!(result, WAIT_OBJECT_0, "sandbox descendant survived Drop");
        // Retained client pipes must not leave blocking I/O threads alive after
        // process shutdown, including an idle stdin and an undrained stdout.
        drop(stdin);
        drop(output);
    }

    #[tokio::test]
    async fn auto_mode_never_downgrades_isolation() {
        let temp = tempfile::tempdir().unwrap();
        let supported = matches!(
            crate::capabilities::native_support().expect("native sandbox capability probe failed"),
            crate::capabilities::NativeSupport::Supported
        );
        let mut policy = policy(temp.path());
        policy.strong_filesystem = StrongFilesystemSandboxMode::Auto;
        policy.filesystem.deny_read_paths = vec![temp.path().join("private")];
        policy.filesystem.deny_write_paths = policy.filesystem.deny_read_paths.clone();
        let result = crate::SandboxAsyncChild::spawn_shell(
            &policy,
            "cmd.exe",
            vec!["/d".into(), "/c".into(), "echo isolated".into()],
            SandboxProcessOptions {
                current_dir: Some(temp.path().to_owned()),
                stdin: SandboxStdio::Null,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Null,
                ..Default::default()
            },
        );
        if supported {
            let mut child = result.unwrap();
            let mut out = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                child.take_stdout().unwrap().read_to_string(&mut out),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(out.trim(), "isolated");
            let status = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        break status;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(status.success());
        } else {
            let error = result
                .err()
                .expect("unsupported host must refuse restrictive Auto execution");
            // The backend preserves typed support-query failures as Other.
            // Accept only the same explicit absence codes as the probe, never
            // an unrelated permission/configuration/launch error.
            let unsupported_query = error.get_ref().is_some_and(|cause| {
                matches!(
                    cause.downcast_ref::<learning_mode_windows::LearningModeError>(),
                    Some(learning_mode_windows::LearningModeError::HResultCall { code, .. })
                        if matches!(*code as u32, 0x8000_4001 | 0x8007_0032 | 0x8007_0078)
                )
            });
            assert!(
                error.kind() == io::ErrorKind::Unsupported || unsupported_query,
                "unexpected failure on unsupported host: {error}"
            );
        }
    }

    fn dacl(path: &Path) -> Vec<u8> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
                DACL_SECURITY_INFORMATION,
            },
        };
        let name = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut descriptor = std::ptr::null_mut();
        let mut acl = std::ptr::null_mut();
        let result = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut acl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(result, 0);
        let bytes = if acl.is_null() {
            vec![]
        } else {
            unsafe {
                std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize).to_vec()
            }
        };
        unsafe {
            LocalFree(descriptor);
        }
        bytes
    }
    #[test]
    fn unavailable_volumes_do_not_invalidate_an_available_scope() {
        let temp = tempfile::tempdir().unwrap();
        let unavailable = format!(r"\\?\Volume{{{}}}\", uuid::Uuid::new_v4());
        let root = temp.path().to_str().unwrap().to_owned();
        let expanded = expand_volumes(vec![root.clone(), unavailable.clone()]).unwrap();
        assert!(expanded.contains(&root));
        assert!(!expanded.contains(&unavailable));
    }
    #[tokio::test]
    async fn exited_process_output_survives_dropping_the_process_handle() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let mut child = launch(
            &policy(temp.path()),
            temp.path(),
            "[Console]::Out.Write(('x' * 65537))".into(),
            SandboxStdio::Null,
        );
        let mut stdout = child.take_stdout().unwrap();
        // Leave the consumer idle until exit. The last output chunk must remain
        // drainable even while the bounded bridge is applying backpressure.
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("root could not finish its buffered output");
        assert!(status.success());
        drop(child);
        tokio::task::yield_now().await;
        let mut bytes = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stdout.read_to_end(&mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(bytes.len(), 65537);
        assert!(bytes.iter().all(|byte| *byte == b'x'));
    }
}

#[cfg(test)]
mod capture_denial_tests {
    use crate::denial::Access;
    use crate::{
        SandboxAsyncChild, SandboxPolicy, SandboxProcessOptions, SandboxStdio,
        StrongFilesystemSandboxMode, WritableRoot,
    };
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    fn powershell_program() -> String {
        use std::os::windows::ffi::OsStringExt;
        let mut buffer = vec![0u16; 32768];
        let length = unsafe {
            windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        } as usize;
        assert!(length > 0 && length < buffer.len());
        PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]))
            .join("System32/WindowsPowerShell/v1.0/powershell.exe")
            .to_string_lossy()
            .into_owned()
    }

    fn narrow_policy(root: &Path) -> SandboxPolicy {
        let mut policy = SandboxPolicy::disabled();
        policy.strong_filesystem = StrongFilesystemSandboxMode::Required;
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        policy.filesystem.readable_roots = vec![root.to_path_buf()];
        policy.filesystem.writable_roots = vec![WritableRoot {
            root: root.to_path_buf(),
            read_only_subpaths: vec![],
        }];
        policy
    }

    async fn run_powershell(
        script: &str,
        capture: bool,
    ) -> (bool, String, Vec<crate::denial::DenialRecord>) {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().canonicalize().unwrap();
        let mut child = SandboxAsyncChild::spawn_shell(
            &narrow_policy(&root),
            &powershell_program(),
            vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script.into(),
            ],
            SandboxProcessOptions {
                capture_denials: capture,
                current_dir: Some(root.clone()),
                stdin: SandboxStdio::Null,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
                ..Default::default()
            },
        )
        .expect("required native sandbox must start the capture probe");
        let mut stdout = child.take_stdout().unwrap();
        let mut stderr = child.take_stderr().unwrap();
        let (out, err, status) = tokio::time::timeout(Duration::from_secs(60), async {
            tokio::try_join!(
                async {
                    let mut text = Vec::new();
                    stdout.read_to_end(&mut text).await?;
                    Ok::<_, std::io::Error>(String::from_utf8_lossy(&text).into_owned())
                },
                async {
                    let mut text = Vec::new();
                    stderr.read_to_end(&mut text).await?;
                    Ok::<_, std::io::Error>(String::from_utf8_lossy(&text).into_owned())
                },
                async {
                    loop {
                        if let Some(status) = child.try_wait()? {
                            return Ok::<_, std::io::Error>(status);
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            )
        })
        .await
        .expect("capture probe timed out")
        .unwrap();
        let denials = child.take_denials();
        let success = status.success();
        (success, format!("{out}{err}"), denials)
    }

    #[tokio::test]
    async fn capture_denials_records_denied_reads() {
        use appcontainer_common::base_container_runner::BaseContainerRunner;
        if !BaseContainerRunner::is_capture_denials_usable() {
            return;
        }
        let directory = crate::test_support::scoped_directory();
        let secret = directory.path().join("daat-capture-probe.txt");
        std::fs::write(&secret, "capture probe secret").unwrap();
        let script = format!(
            "try {{ $null=[IO.File]::ReadAllText('{}'); exit 73 }} catch {{exit 0}}",
            secret.to_string_lossy().replace('\'', "''")
        );
        let (success, output, denials) = run_powershell(&script, true).await;
        assert!(success, "capture must preserve the read denial: {output}");
        assert!(
            denials.iter().any(|record| {
                record.access == Access::Read
                    && record
                        .path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains("daat-capture-probe")
            }),
            "expected a native read denial for the probe file: {denials:?}"
        );
    }

    #[tokio::test]
    async fn system_temp_is_writable_without_a_grant() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        let script = "$p = Join-Path $env:TEMP ('daat-temp-probe-' + [Guid]::NewGuid().ToString('N') + '.txt'); \
[IO.File]::WriteAllText($p, 'ok'); \
$ok = [IO.File]::ReadAllText($p) -eq 'ok'; \
[IO.File]::Delete($p); \
if ($ok) { 'temp-ok' } else { throw 'temp probe failed' }";
        let (success, output, _) = run_powershell(script, false).await;
        assert!(success, "output={output}");
        assert!(output.contains("temp-ok"), "output={output}");
    }
}
