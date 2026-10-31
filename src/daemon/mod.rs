//! Reusable service infrastructure retained for the Actor rewrite.
//!
//! This module provides no legacy Manager/Session server or dialog client.

pub mod attachments;
pub mod logs;

pub mod control;
#[cfg(windows)]
pub mod installation;

/// Prefer the installed desktop client; browser access stays available on headless installs.
pub fn open_client(status: &control::Status) -> Result<()> {
    let executable = std::env::current_exe().into_diagnostic()?;
    let client = executable
        .parent()
        .ok_or_else(|| miette!("Executable directory unavailable"))?
        .join(if cfg!(windows) {
            "daat-locus-app.exe"
        } else {
            "daat-locus-app"
        });
    if cfg!(any(target_os = "windows", target_os = "macos")) && client.is_file() {
        let mut command = Command::new(client);
        if let Some(profile) = &status.options.profile {
            command.args(["--profile", profile]);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command.spawn().into_diagnostic()?;
        Ok(())
    } else {
        crate::open_url::open_url(&status.url).into_diagnostic()
    }
}
use crate::daat_locus_paths::daat_locus_paths;
use miette::{IntoDiagnostic, Result, miette};
use std::{
    io::{Read, Seek, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const DAEMONIZE_ENV: &str = "DAAT_LOCUS_DAEMONIZE";
const DAEMON_MAIN_LOG: &str = crate::logging::DAEMON_LOG_FILE_NAME;

pub struct DaemonLock {
    file: Option<std::fs::File>,
}
impl DaemonLock {
    pub async fn acquire() -> Result<Self> {
        Self::acquire_at_path(&daat_locus_paths().await.daemon_lock_file())
    }
    pub fn acquire_at_path(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| miette!("Invalid daemon lock location"))?;
        crate::file_io::ensure_directory(
            &crate::sandbox::RuntimeSandboxPolicy::disabled(),
            parent,
        )?;
        crate::actors::permissions::protect_private_path(parent)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .into_diagnostic()?;
        crate::persistence::set_private_file_permissions(path).into_diagnostic()?;
        file.try_lock()
            .map_err(|_| miette!("Daemon already running for this profile"))?;
        // Public v0.5.0 only creates a JSON PID marker; it never takes an OS
        // file lock. Do not truncate that marker while its recorded PID lives.
        // This is an upgrade fence, not authority to signal the old process.
        let mut previous = Vec::new();
        (&mut file)
            .take(4096)
            .read_to_end(&mut previous)
            .into_diagnostic()?;
        let legacy_pid = serde_json::from_slice::<serde_json::Value>(&previous)
            .ok()
            .filter(|value| {
                value
                    .get("started_at_ms")
                    .is_some_and(serde_json::Value::is_i64)
            })
            .and_then(|value| value.get("pid").and_then(serde_json::Value::as_u64))
            .and_then(|pid| u32::try_from(pid).ok());
        if legacy_pid.is_some_and(|pid| {
            sysinfo::System::new_all()
                .process(sysinfo::Pid::from_u32(pid))
                .is_some()
        }) {
            return Err(miette!(
                "A live PID is recorded by a legacy profile lock; stop the previous Daat Locus version before upgrading"
            ));
        }
        file.set_len(0).into_diagnostic()?;
        file.rewind().into_diagnostic()?;
        writeln!(file, "{}", std::process::id()).into_diagnostic()?;
        file.sync_all().into_diagnostic()?;
        Ok(Self { file: Some(file) })
    }
    pub fn release(&mut self) {
        self.file.take();
    }
}
fn open_stdio_log_pair(
    path: PathBuf,
    marker: &str,
) -> Result<(PathBuf, std::fs::File, std::fs::File)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| miette!("create log dir {} failed: {err}", parent.display()))?;
    }
    {
        let mut marker_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| miette!("open log {} failed: {err}", path.display()))?;
        writeln!(
            marker_file,
            "\n--- {marker} at {} ---",
            chrono::Utc::now().to_rfc3339()
        )
        .map_err(|err| miette!("write log marker {} failed: {err}", path.display()))?;
    }
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| miette!("open stdout log {} failed: {err}", path.display()))?;
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| miette!("open stderr log {} failed: {err}", path.display()))?;
    Ok((path, stdout, stderr))
}

pub fn spawn_detached_daemon_process(
    namespace: &Path,
    root: &Path,
    options: &control::LaunchOptions,
) -> Result<()> {
    crate::file_io::ensure_directory(&crate::sandbox::RuntimeSandboxPolicy::disabled(), root)?;
    crate::actors::permissions::protect_private_path(root)?;
    let paths = crate::daat_locus_paths::DaatLocusPaths::from_root(root.to_path_buf());
    let (log_path, stdout_log, stderr_log) =
        open_stdio_log_pair(paths.logs_file(DAEMON_MAIN_LOG), "daemon process starting")?;
    let mut command = std::process::Command::new(std::env::current_exe().into_diagnostic()?);
    command
        .arg("serve")
        .arg("--listen")
        .arg(options.listen.to_string())
        .arg("--workspace")
        .arg(&options.workspace);
    if let Some(profile) = &options.profile {
        command.arg("--profile").arg(profile);
    }
    // Pass the original namespace, never a root which has already applied --profile.
    command
        .env("DAAT_LOCUS_HOME", namespace)
        .env_remove("DAAT_LOCUS_PROFILE_NAME");
    command
        .stdin(Stdio::null())
        .stdout(stdout_log)
        .stderr(stderr_log)
        .env(DAEMONIZE_ENV, "1");
    apply_detached_daemon_creation_flags(&mut command);
    #[cfg(windows)]
    let _creation = crate::process_spawn::creation_lock().into_diagnostic()?;
    #[cfg(windows)]
    let _stdio = DetachedStdio::new().into_diagnostic()?;
    command.spawn().into_diagnostic()?;
    let _ = log_path;
    Ok(())
}

#[cfg(windows)]
struct DetachedStdio(Vec<(windows_sys::Win32::Foundation::HANDLE, u32)>);
#[cfg(windows)]
impl DetachedStdio {
    fn new() -> std::io::Result<Self> {
        use windows_sys::Win32::{
            Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation},
            System::Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
        };
        let mut guard = Self(Vec::new());
        for kind in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            unsafe {
                let handle = GetStdHandle(kind);
                let mut flags = 0;
                if GetHandleInformation(handle, &mut flags) == 0 {
                    continue;
                }
                if flags & HANDLE_FLAG_INHERIT != 0 {
                    if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) == 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    guard.0.push((handle, flags));
                }
            }
        }
        Ok(guard)
    }
}
#[cfg(windows)]
impl Drop for DetachedStdio {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        for &(handle, flags) in &self.0 {
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags & HANDLE_FLAG_INHERIT);
            }
        }
    }
}

#[cfg(windows)]
fn apply_detached_daemon_creation_flags(command: &mut std::process::Command) {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    crate::process_spawn::apply_no_window_with_flags(
        command,
        DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
    );
}

#[cfg(not(windows))]
fn apply_detached_daemon_creation_flags(_command: &mut std::process::Command) {}

pub fn daemonize_current_process_if_requested() {
    if std::env::var_os(DAEMONIZE_ENV).is_none() {
        return;
    }
    // This marker is only for the top-level daemon child. If it survives in the
    // daemon environment, later helper processes will daemonize themselves
    // before running their actual subcommand.
    unsafe {
        std::env::remove_var(DAEMONIZE_ENV);
    }
    #[cfg(unix)]
    daemonize_current_process().expect("daemon process should detach successfully");
    #[cfg(not(unix))]
    daemonize_current_process();
}

#[cfg(unix)]
fn daemonize_current_process() -> Result<()> {
    // Run before Tokio starts: daemonizing forks, so doing it after a
    // multi-thread runtime exists would duplicate only the calling thread.
    unsafe {
        fork_parent_exit("first daemon fork")?;
        if libc::setsid() == -1 {
            return Err(miette!(
                "daemonize setsid failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        fork_parent_exit("second daemon fork")?;
    }
    Ok(())
}

#[cfg(unix)]
unsafe fn fork_parent_exit(label: &str) -> Result<()> {
    match unsafe { libc::fork() } {
        -1 => Err(miette!(
            "{label} failed: {}",
            std::io::Error::last_os_error()
        )),
        0 => Ok(()),
        _ => unsafe { libc::_exit(0) },
    }
}

#[cfg(not(unix))]
const fn daemonize_current_process() {}

#[cfg(test)]
mod lock_tests {
    use super::DaemonLock;

    #[test]
    fn active_legacy_pid_lock_is_not_replaced_by_a_native_file_lock() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.lock");
        // Public v0.5.0 uses a create-new JSON PID marker, not an OS file lock.
        let previous = serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(), "started_at_ms": 1
        }))
        .unwrap();
        std::fs::write(&path, &previous).unwrap();
        assert!(
            DaemonLock::acquire_at_path(&path).is_err(),
            "A live legacy runtime must retain its profile lock"
        );
        assert_eq!(std::fs::read(&path).unwrap(), previous);
    }

    #[test]
    fn retired_legacy_pid_recovers_without_weakening_native_exclusivity() {
        let mut process = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = process.id();
        assert!(process.wait().unwrap().success());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.lock");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"pid":pid,"started_at_ms":1})).unwrap(),
        )
        .unwrap();
        let mut lock = DaemonLock::acquire_at_path(&path).unwrap();
        assert!(DaemonLock::acquire_at_path(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );
        lock.release();
        assert!(DaemonLock::acquire_at_path(&path).is_ok());
    }
}
