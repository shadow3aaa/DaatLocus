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
        BaseProcessUiConfig, ContainerPolicy, ExecutionRequest, FallbackPolicy, NetworkAction,
        NetworkEgressPolicy, NetworkIngressPolicy, NetworkPolicy, UiPolicy,
    },
    sandbox_process::{SandboxBackend, SandboxProcess, StdioMode},
};

use super::{SandboxPolicy, SandboxProcessOptions, SandboxStdio, policy_paths_with_resolved};

#[link(name = "advapi32")]
unsafe extern "system" {}

pub struct WindowsSandboxAsyncChild {
    process: Mutex<Box<dyn SandboxProcess>>,
    stdin: Option<DuplexStream>,
    stdout: Option<DuplexStream>,
    stderr: Option<DuplexStream>,
    status: Option<ExitStatus>,
    shutdown: watch::Sender<()>,
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
        }
        Ok(self.status)
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
    let request = build_request(policy, program, args, &options)?;
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
    let mut process = BaseContainerRunner::new()
        .spawn(
            &request,
            &mut logger,
            if inherited {
                StdioMode::Inherit
            } else {
                StdioMode::Pipes
            },
        )
        .map_err(|error| io::Error::other(error.error_message))?;
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
    })
}

impl Drop for WindowsSandboxAsyncChild {
    fn drop(&mut self) {
        let _ = self.process.get_mut().kill();
        let _ = self.shutdown.send(());
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

fn platform_roots() -> io::Result<Vec<PathBuf>> {
    use windows::Win32::UI::Shell::{
        FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86,
    };
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
    for folder in [
        FOLDERID_ProgramFiles,
        FOLDERID_ProgramFilesX86,
        FOLDERID_ProgramData,
    ] {
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
    let mut read = platform_roots()?;
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
    async fn mxc_enforces_scopes_and_future_denies_with_streaming_stdio() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
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
