//! Own bubblewrap's namespace and temporary mount targets until every process dies.
use super::super::{
    RuntimeSandboxPolicy, SandboxProcessOptions, apply_tokio_command_options,
    policy_paths_with_resolved,
};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::MetadataExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{Arc, Mutex, OnceLock, Weak, atomic::Ordering},
    time::{Duration, Instant},
};

type MaskRegistry = Mutex<std::collections::HashMap<PathBuf, Weak<Mask>>>;
fn registry() -> &'static MaskRegistry {
    static MASKS: OnceLock<MaskRegistry> = OnceLock::new();
    MASKS.get_or_init(Mutex::default)
}

struct Mask {
    path: PathBuf,
    _file: File,
    created: std::fs::Metadata,
    parent: File,
    name: std::ffi::CString,
}
impl Drop for Mask {
    fn drop(&mut self) {
        let mut masks = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        masks.remove(&self.path);
        // Preserve a user's replacement, even when another process changes
        // a mountpoint while this sandbox is running.
        let pinned = PathBuf::from(format!("/proc/self/fd/{}", self.parent.as_raw_fd()))
            .join(self.path.file_name().unwrap());
        let Ok(actual) = std::fs::symlink_metadata(pinned) else {
            return;
        };
        let expected = &self.created;
        if !actual.is_file()
            || actual.len() != 0
            || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino())
            || (
                actual.ctime(),
                actual.ctime_nsec(),
                actual.mtime(),
                actual.mtime_nsec(),
            ) != (
                expected.ctime(),
                expected.ctime_nsec(),
                expected.mtime(),
                expected.mtime_nsec(),
            )
        {
            return;
        }
        if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) } < 0 {
            tracing::warn!(error=%io::Error::last_os_error(),"Could not remove a sandbox mount target");
        }
    }
}

pub struct LinuxSandboxAsyncChild {
    child: tokio::process::Child,
    namespace: Option<File>,
    monitor: Option<File>,
    startup_gate: Option<UnixStream>,
    masks: Vec<Arc<Mask>>,
}
impl LinuxSandboxAsyncChild {
    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child.stdin.take()
    }
    pub fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child.stdout.take()
    }
    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child.stderr.take()
    }
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }
    pub fn start_kill(&mut self) -> io::Result<()> {
        terminate_namespace(self.namespace.as_ref())?;
        self.child.start_kill()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            terminate_namespace(self.namespace.as_ref())?;
            wait_namespace(self.namespace.as_ref())?;
            self.namespace.take();
            self.masks.clear();
        }
        Ok(status)
    }
}
impl Drop for LinuxSandboxAsyncChild {
    fn drop(&mut self) {
        let _ = self.start_kill();
        // A pidfd stays attached to the namespace init process across PID reuse.
        // Reap its death before removing mounted placeholders from the host.
        let result = wait_namespace(self.namespace.as_ref())
            .and_then(|()| wait_namespace(self.monitor.as_ref()));
        if let Err(error) = result {
            tracing::error!(%error,"Could not confirm sandbox namespace termination; retaining mount targets");
            for mask in self.masks.drain(..) {
                std::mem::forget(mask);
            }
        }
    }
}
fn terminate_namespace(namespace: Option<&File>) -> io::Result<()> {
    if let Some(file) = namespace {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                file.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }
    Ok(())
}
fn wait_namespace(namespace: Option<&File>) -> io::Result<()> {
    if let Some(file) = namespace {
        let mut fd = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let result = unsafe { libc::poll(&mut fd, 1, 5000) };
            if result > 0 {
                return Ok(());
            }
            if result == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Sandbox namespace did not terminate",
                ));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    Ok(())
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

fn prepare_masks(policy: &RuntimeSandboxPolicy) -> io::Result<Vec<Arc<Mask>>> {
    let fs = &policy.filesystem;
    let mut protections = fs.deny_read_paths.clone();
    protections.extend(fs.deny_write_paths.clone());
    protections.extend(
        fs.writable_roots
            .iter()
            .flat_map(|root| root.read_only_subpaths.clone()),
    );
    let read_denies = policy_paths_with_resolved(&fs.deny_read_paths);
    let mut masks = Vec::new();
    for path in policy_paths_with_resolved(&protections) {
        if read_denies
            .iter()
            .any(|parent| parent != &path && path.starts_with(parent) && parent.exists())
        {
            continue;
        }
        let mut prefix = PathBuf::new();
        for component in path.components() {
            prefix.push(component);
            loop {
                let mut entries = registry()
                    .lock()
                    .map_err(|_| io::Error::other("Sandbox mask registry poisoned"))?;
                if let Some(existing) = entries.get(&prefix) {
                    if let Some(existing) = existing.upgrade() {
                        masks.push(existing);
                        break;
                    }
                    // The last owner is removing this inode under the same
                    // lock. Let its Drop finish before observing the pathname.
                    drop(entries);
                    std::thread::yield_now();
                    continue;
                }
                match std::fs::symlink_metadata(&prefix) {
                    Ok(metadata) if metadata.is_file() => {
                        // A different service can own an absent-path target.
                        // Refuse that cross-process conflict instead of letting
                        // its cleanup invalidate this sandbox's mount boundary.
                        use std::os::unix::fs::OpenOptionsExt;
                        let file = std::fs::OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                            .open(&prefix)?;
                        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) }
                            < 0
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::WouldBlock,
                                "Protected mount target is busy in another service",
                            ));
                        }
                        break;
                    }
                    Ok(_) => {
                        drop(entries);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        // Pin only the first absent component, never walk a tree.
                        let parent = pin_directory(
                            prefix
                                .parent()
                                .ok_or_else(|| io::Error::other("Invalid mask path"))?,
                        )?;
                        use std::os::unix::ffi::OsStrExt;
                        let name = std::ffi::CString::new(prefix.file_name().unwrap().as_bytes())
                            .map_err(io::Error::other)?;
                        if prefix == super::super::unix_control_directory() {
                            // This persistent infrastructure directory must stay
                            // a directory if the maintenance server binds later.
                            let result =
                                unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
                            if result < 0
                                && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                            {
                                return Err(io::Error::last_os_error());
                            }
                            let descriptor = unsafe {
                                libc::openat(
                                    parent.as_raw_fd(),
                                    name.as_ptr(),
                                    libc::O_PATH
                                        | libc::O_DIRECTORY
                                        | libc::O_NOFOLLOW
                                        | libc::O_CLOEXEC,
                                )
                            };
                            if descriptor < 0 {
                                return Err(io::Error::last_os_error());
                            }
                            let directory = unsafe { File::from_raw_fd(descriptor) };
                            if directory.metadata()?.uid() != unsafe { libc::geteuid() } {
                                return Err(io::Error::new(
                                    io::ErrorKind::PermissionDenied,
                                    "Control directory has a different OS owner",
                                ));
                            }
                            break;
                        }
                        let descriptor = unsafe {
                            libc::openat(
                                parent.as_raw_fd(),
                                name.as_ptr(),
                                libc::O_CREAT
                                    | libc::O_EXCL
                                    | libc::O_WRONLY
                                    | libc::O_NOFOLLOW
                                    | libc::O_CLOEXEC,
                                0o600,
                            )
                        };
                        if descriptor >= 0 {
                            let file = unsafe { File::from_raw_fd(descriptor) };
                            if unsafe {
                                libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
                            } < 0
                            {
                                return Err(io::Error::last_os_error());
                            }
                            let mask = Arc::new(Mask {
                                path: prefix.clone(),
                                created: file.metadata()?,
                                _file: file,
                                parent,
                                name,
                            });
                            entries.insert(prefix.clone(), Arc::downgrade(&mask));
                            masks.push(mask);
                        } else {
                            let error = io::Error::last_os_error();
                            if error.kind() == io::ErrorKind::AlreadyExists {
                                drop(entries);
                                continue;
                            }
                            return Err(error);
                        }
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }
            if masks
                .last()
                .is_some_and(|mask| prefix.starts_with(&mask.path))
                || prefix.is_file()
            {
                break;
            }
        }
    }
    Ok(masks)
}

fn pin_directory(path: &Path) -> io::Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::fs::canonicalize(path)?;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x04,
    }; // RESOLVE_NO_SYMLINKS
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            name.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(descriptor as i32) })
}

pub fn spawn_restricted_async(
    policy: &RuntimeSandboxPolicy,
    program: &Path,
    args: Vec<String>,
    options: SandboxProcessOptions,
) -> io::Result<LinuxSandboxAsyncChild> {
    cancelled(&options)?;
    super::validate_paths(&policy.filesystem).map_err(io::Error::other)?;
    // Resolve prerequisites before any temporary filesystem changes.
    let bwrap = super::find_bwrap_in_path().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Linux filesystem sandbox requires bubblewrap",
        )
    })?;
    // Fail before creating any workload on kernels without stable process handles.
    drop(pidfd(std::process::id() as i32)?);
    let mut policy = policy.clone();
    if let Some(scope) = policy.filesystem.private_delivery.take() {
        policy.filesystem.deny_read_paths.push(scope.root.clone());
        policy.filesystem.deny_write_paths.push(scope.root);
    }
    let policy = &policy;
    let masks = prepare_masks(policy)?;
    let mut filesystem = policy.filesystem.clone();
    let boundary = |path: &mut PathBuf| {
        if let Some(mask) = masks.iter().find(|mask| path.starts_with(&mask.path)) {
            *path = mask.path.clone();
        }
    };
    filesystem.deny_read_paths.iter_mut().for_each(boundary);
    filesystem.deny_write_paths.iter_mut().for_each(boundary);
    filesystem
        .writable_roots
        .iter_mut()
        .flat_map(|root| &mut root.read_only_subpaths)
        .for_each(boundary);
    let mut command_argv = vec![
        program
            .to_str()
            .ok_or_else(|| io::Error::other("Linux sandbox requires Unicode executable paths"))?
            .to_owned(),
    ];
    command_argv.extend(args);
    let (mut info, info_child) = UnixStream::pair()?;
    info.set_read_timeout(Some(Duration::from_millis(100)))?;
    let filter = seccomp_filter()?;
    let (gate_parent, gate_child) = UnixStream::pair()?;
    let mut command = tokio::process::Command::new(bwrap);
    command.args([
        "--block-fd",
        &gate_child.as_raw_fd().to_string(),
        "--info-fd",
        &info_child.as_raw_fd().to_string(),
        "--seccomp",
        &filter.as_raw_fd().to_string(),
    ]);
    command.args(super::create_bwrap_args(&filesystem, command_argv));
    let spawn_options = options.clone();
    if !policy.filesystem.full_disk_read && !policy.filesystem.full_disk_write {
        command.env("TMPDIR", "/tmp");
    }
    apply_tokio_command_options(&mut command, options);
    command.kill_on_drop(true);
    // Do not change parent descriptor flags in this multithreaded service.
    // Bubblewrap consumes both descriptors and closes them before workload exec.
    unsafe {
        command.pre_exec(move || {
            for fd in [
                info_child.as_raw_fd(),
                filter.as_raw_fd(),
                gate_child.as_raw_fd(),
            ] {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    cancelled(&spawn_options)?;
    let child = command.spawn()?;
    drop(command);
    let mut child = LinuxSandboxAsyncChild {
        child,
        namespace: None,
        monitor: None,
        startup_gate: Some(gate_parent),
        masks,
    };
    child.monitor = pidfd(
        child
            .child
            .id()
            .ok_or_else(|| io::Error::other("Missing bubblewrap PID"))? as i32,
    )?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut bytes = Vec::new();
    loop {
        cancelled(&spawn_options)?;
        let mut buffer = [0; 1024];
        match info.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.len() > 16 * 1024 {
                    return Err(io::Error::other("Oversized bubblewrap startup status"));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Bubblewrap startup timed out",
            ));
        }
    }
    let info: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
        io::Error::other(
            "Bubblewrap did not establish isolation; check bubblewrap and user namespace support",
        )
    })?;
    let pid = info
        .get("child-pid")
        .and_then(|pid| pid.as_i64())
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::other("Bubblewrap did not report its namespace process"))?;
    child.namespace = pidfd(pid)?;
    cancelled(&spawn_options)?;
    child.startup_gate.as_mut().unwrap().write_all(&[1])?;
    child.startup_gate.take();
    Ok(child)
}

fn pidfd(pid: i32) -> io::Result<Option<File>> {
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if descriptor >= 0 {
        return Ok(Some(unsafe { File::from_raw_fd(descriptor as i32) }));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(None);
    }
    Err(error)
}

fn seccomp_filter() -> io::Result<File> {
    let arch = if cfg!(target_arch = "x86_64") {
        Some(0xc000003e)
    } else if cfg!(target_arch = "aarch64") {
        Some(0xc00000b7)
    } else {
        None
    }
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux process isolation needs a reviewed syscall ABI for this architecture",
        )
    })?;
    // Classic BPF over struct seccomp_data: nr@0, arch@4, args[0]@16.
    let stmt = |code, k| libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    };
    let jump = |k, jt, jf| libc::sock_filter {
        code: 0x15,
        jt,
        jf,
        k,
    };
    let deny = stmt(0x06, 0x00050000 | libc::EPERM as u32);
    let mut instructions = vec![
        stmt(0x20, 4),
        jump(arch, 1, 0),
        stmt(0x06, 0x80000000),
        stmt(0x20, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    instructions.extend([
        libc::sock_filter {
            code: 0x35,
            jt: 0,
            jf: 1,
            k: 0x40000000,
        },
        stmt(0x06, 0x80000000),
    ]);
    for syscall in [
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_open_by_handle_at,
        libc::SYS_bpf,
        libc::SYS_userfaultfd,
        libc::SYS_ptrace,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_setns,
        libc::SYS_unshare,
    ] {
        instructions.push(jump(syscall as u32, 0, 1));
        instructions.push(deny);
    }
    instructions.extend([
        jump(libc::SYS_socket as u32, 0, 3),
        stmt(0x20, 16),
        jump(libc::AF_VSOCK as u32, 0, 1),
        deny,
        stmt(0x06, 0x7fff0000),
    ]);
    let name = c"daat-sandbox-seccomp";
    let descriptor =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    for item in instructions {
        file.write_all(&item.code.to_ne_bytes())?;
        file.write_all(&[item.jt, item.jf])?;
        file.write_all(&item.k.to_ne_bytes())?;
    }
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let result = unsafe {
        libc::fcntl(
            file.as_raw_fd(),
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{
        FileSystemSandboxPolicy, SandboxStdio, StrongFilesystemSandboxMode, WritableRoot,
    };
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    fn policy(root: &Path) -> RuntimeSandboxPolicy {
        RuntimeSandboxPolicy {
            builtin_resources: Vec::new(),
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: false,
                full_disk_write: false,
                readable_roots: vec![root.to_owned()],
                writable_roots: vec![WritableRoot {
                    root: root.to_owned(),
                    read_only_subpaths: vec![],
                }],
                deny_read_paths: vec![crate::sandbox::unix_control_directory()],
                deny_write_paths: vec![crate::sandbox::unix_control_directory()],
                private_delivery: None,
                bound_roots: vec![root.to_owned()],
            },
            strong_filesystem: StrongFilesystemSandboxMode::Required,
        }
    }
    fn spawn(policy: &RuntimeSandboxPolicy, root: &Path, script: &str) -> LinuxSandboxAsyncChild {
        spawn_restricted_async(
            policy,
            Path::new("/bin/sh"),
            vec!["-c".into(), script.into()],
            SandboxProcessOptions {
                current_dir: Some(root.to_owned()),
                stdin: SandboxStdio::Piped,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
                ..Default::default()
            },
        )
        .unwrap()
    }
    async fn collect(mut child: LinuxSandboxAsyncChild) -> (ExitStatus, String, String) {
        let mut out = child.take_stdout().unwrap();
        let mut err = child.take_stderr().unwrap();
        let (out, err, status) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::try_join!(
                async {
                    let mut text = String::new();
                    out.read_to_string(&mut text).await?;
                    Ok::<_, io::Error>(text)
                },
                async {
                    let mut text = String::new();
                    err.read_to_string(&mut text).await?;
                    Ok::<_, io::Error>(text)
                },
                async {
                    loop {
                        if let Some(status) = child.try_wait()? {
                            return Ok::<_, io::Error>(status);
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            )
        })
        .await
        .expect("Linux sandbox timed out")
        .unwrap();
        (status, out, err)
    }
    #[tokio::test]
    #[ignore = "requires bubblewrap, user namespaces and Linux pidfd/openat2"]
    async fn actor_scope_runs_shell_and_preserves_missing_denies() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work");
        std::fs::create_dir(&root).unwrap();
        let private = root.join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::write(private.join("secret"), "secret").unwrap();
        std::fs::write(root.join("input"), "input").unwrap();
        std::fs::write(temp.path().join("outside"), "outside").unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), root.join("link")).unwrap();
        let mut policy = policy(&root);
        policy.filesystem.deny_read_paths = vec![
            private.clone(),
            root.join("future/secret"),
            root.join("future-file"),
        ];
        policy.filesystem.deny_write_paths = policy.filesystem.deny_read_paths.clone();
        policy.filesystem.writable_roots[0].read_only_subpaths = vec![root.join(".git")];
        let script = r#"set -eu
[ "$(cat input)" = input ]
[ -c /dev/null ]
printf device >/dev/null
scratch=$(mktemp /tmp/dl-sandbox.XXXXXXXX)
printf scratch > "$scratch"
[ "$(cat "$scratch")" = scratch ]
rm "$scratch"
if cat link >/dev/null 2>&1; then exit 10; fi
if cat private/secret >/dev/null 2>&1; then exit 11; fi
if printf bad 2>/dev/null > private/output; then exit 12; fi
if mkdir -p future/deeper 2>/dev/null; then exit 13; fi
if printf bad 2>/dev/null > .git; then exit 14; fi
printf ready\n
read command
[ "$(cat future-file)" != "host-secret" ]
printf '%s' "$command" > output
"#;
        let mut child = spawn(&policy, &root, script);
        std::fs::write(root.join("future-file"), "host-secret").unwrap();
        child
            .take_stdin()
            .unwrap()
            .write_all(b"persisted\n")
            .await
            .unwrap();
        let (status, out, err) = collect(child).await;
        assert!(status.success(), "{status};out={out};err={err}");
        assert_eq!(
            std::fs::read_to_string(root.join("output")).unwrap(),
            "persisted"
        );
        assert_eq!(
            std::fs::read_to_string(private.join("secret")).unwrap(),
            "secret"
        );
        assert!(!root.join("future").exists());
        assert!(!root.join(".git").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("future-file")).unwrap(),
            "host-secret"
        );
    }
    #[tokio::test]
    #[ignore = "requires bubblewrap, user namespaces and Linux pidfd/openat2"]
    async fn dropping_process_kills_descendants_before_removing_masks() {
        let temp = tempfile::tempdir().unwrap();
        let mut policy = policy(temp.path());
        policy.filesystem.deny_read_paths = vec![temp.path().join("missing")];
        let mut child = spawn(&policy, temp.path(), "sleep 300 & echo ready; read command");
        let mut output = BufReader::new(child.take_stdout().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line.trim(), "ready");
        let namespace = child.namespace.as_ref().unwrap().try_clone().unwrap();
        drop(child);
        wait_namespace(Some(&namespace)).unwrap();
        assert!(!temp.path().join("missing").exists());
        let mut rest = String::new();
        tokio::time::timeout(Duration::from_secs(5), output.read_to_string(&mut rest))
            .await
            .unwrap()
            .unwrap();
    }
    #[test]
    fn protected_writable_symlink_is_rejected_before_launch() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(temp.path(), root.join("alias")).unwrap();
        let mut policy = policy(&root);
        policy.filesystem.deny_read_paths = vec![root.join("alias/private")];
        let result = spawn_restricted_async(
            &policy,
            Path::new("/bin/sh"),
            vec![],
            SandboxProcessOptions::default(),
        );
        assert!(result.is_err());
        assert!(!temp.path().join("private").exists());
    }
    #[tokio::test]
    #[ignore = "requires bubblewrap, user namespaces, pidfd/openat2 and rustc"]
    async fn native_filter_blocks_kernel_bypasses_and_keeps_ip_sockets() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("probe.rs");
        let executable = temp.path().join("probe");
        std::fs::write(&source,r#"
unsafe extern "C" {fn syscall(number:i64,...)->i64;fn socket(domain:i32,kind:i32,protocol:i32)->i32;fn close(fd:i32)->i32;fn prctl(option:i32,...)->i32;}
fn main() {
    assert_eq!(unsafe{prctl(39,0,0,0,0)},1);
    #[cfg(target_arch="x86_64")] let ptrace=101;
    #[cfg(target_arch="aarch64")] let ptrace=117;
    assert_eq!(unsafe{syscall(ptrace,0,0,0,0)},-1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(1));
    assert_eq!(unsafe{syscall(425,1,0)},-1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(1));
    assert_eq!(unsafe{socket(40,1,0)},-1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(1));
    let fd=unsafe{socket(2,1,0)};assert!(fd>=0);unsafe{close(fd);}
}
"#).unwrap();
        let output =
            std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .output()
                .expect("native seccomp test requires rustc");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let child = spawn(&policy(temp.path()), temp.path(), "exec ./probe");
        assert!(
            crate::sandbox::unix_control_directory().is_dir(),
            "maintenance startup must not inherit a file mountpoint"
        );
        let (status, out, err) = collect(child).await;
        assert!(status.success(), "{status};out={out};err={err}");
    }

    #[tokio::test]
    #[ignore = "requires bubblewrap, user namespaces and Linux pidfd/openat2"]
    async fn concurrent_sandboxes_keep_missing_masks_until_the_last_exit() {
        let temp = tempfile::tempdir().unwrap();
        let mut policy = policy(temp.path());
        policy.filesystem.deny_read_paths = vec![temp.path().join("missing")];
        let first = spawn(&policy, temp.path(), "read command");
        let mut second = spawn(
            &policy,
            temp.path(),
            "read command; value=$(cat missing 2>/dev/null || true); [ \"$value\" != host-secret ] && printf denied",
        );
        drop(first);
        std::fs::write(temp.path().join("missing"), "host-secret").unwrap();
        second
            .take_stdin()
            .unwrap()
            .write_all(b"go\n")
            .await
            .unwrap();
        let (status, out, err) = collect(second).await;
        assert!(status.success(), "{status};out={out};err={err}");
        assert_eq!(out, "denied");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("missing")).unwrap(),
            "host-secret"
        );
    }
    #[tokio::test]
    #[ignore = "requires bubblewrap, user namespaces and Linux pidfd/openat2"]
    async fn sandbox_tcp_socket_cannot_claim_local_owner_authority() {
        let temp = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let script = format!(
            "exec 3<>/dev/tcp/127.0.0.1/{}; read command",
            address.port()
        );
        let mut child = spawn_restricted_async(
            &policy(temp.path()),
            Path::new("/bin/bash"),
            vec!["-c".into(), script],
            SandboxProcessOptions {
                current_dir: Some(temp.path().to_owned()),
                stdin: SandboxStdio::Piped,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
                ..Default::default()
            },
        )
        .unwrap();
        let (_socket, peer) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !crate::accounts::local_owner_connection(peer, Some(address)),
            "sandbox UID was mistaken for unrestricted OS-owner authority"
        );
        child
            .take_stdin()
            .unwrap()
            .write_all(b"finish\n")
            .await
            .unwrap();
        let (status, out, err) = collect(child).await;
        assert!(status.success(), "{status};out={out};err={err}");
    }
}
