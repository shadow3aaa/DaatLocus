use std::path::{Path, PathBuf};

use miette::{Result, miette};

use super::{SandboxPolicy, SandboxSpawnSpec, policy_paths_with_resolved};

const SANDBOX_EXECUTABLE: &str = "/usr/bin/sandbox-exec";
const BASE: &str = include_str!("seatbelt_base_policy.sbpl");
const NETWORK: &str = include_str!("seatbelt_network_policy.sbpl");
const PLATFORM: &str = include_str!("seatbelt_read_only_platform_defaults.sbpl");

#[cfg(test)]
pub fn wrap_shell_command(
    policy: &SandboxPolicy,
    program: &str,
    args: Vec<String>,
) -> Result<SandboxSpawnSpec> {
    wrap_command(policy, PathBuf::from(program), args)
}

#[derive(Default)]
struct Profile {
    rules: String,
    parameters: Vec<String>,
}
impl Profile {
    fn filter(&mut self, path: &Path) -> Result<String> {
        let path = path
            .to_str()
            .filter(|value| !value.contains('\0'))
            .ok_or_else(|| miette!("Seatbelt requires Unicode paths without NUL"))?;
        let key = format!("DL_PATH_{}", self.parameters.len());
        self.parameters.push(format!("-D{key}={path}"));
        // Paths are arguments, never executable SBPL. Literal also covers an
        // absent path itself (including its first creation), not only children.
        Ok(format!(
            "(require-any (literal (param \"{key}\")) (subpath (param \"{key}\")))"
        ))
    }
    fn rule(&mut self, action: &str, operations: &str, path: &Path) -> Result<()> {
        let filter = self.filter(path)?;
        self.rules
            .push_str(&format!("({action} {operations} {filter})\n"));
        Ok(())
    }
}

pub fn wrap_command(
    policy: &SandboxPolicy,
    program: PathBuf,
    args: Vec<String>,
) -> Result<SandboxSpawnSpec> {
    super::validate_native_policy_paths(&policy.filesystem)?;
    let fs = &policy.filesystem;
    let mut profile = Profile {
        rules: format!(
            "{BASE}\n{PLATFORM}\n{NETWORK}\n(allow network-outbound)\n(allow network-inbound)\n(allow network-bind (local ip \"*:*\"))\n(allow network-bind (local unix-socket))\n"
        ),
        ..Default::default()
    };
    let read_ops = "file-read* file-map-executable file-test-existence";
    if fs.full_disk_read || fs.full_disk_write {
        profile.rule("allow", read_ops, Path::new("/"))?;
    } else {
        let mut roots = super::runtime_read_roots()
            .map_err(|error| miette!("{error}"))?
            .to_vec();
        roots.extend(fs.readable_roots.clone());
        roots.extend(super::system_temporary_paths().iter().cloned());
        roots.extend(fs.writable_roots.iter().map(|root| root.root.clone()));
        for root in policy_paths_with_resolved(&roots) {
            profile.rule("allow", read_ops, &root)?;
        }
    }
    if fs.full_disk_write {
        profile.rule("allow", "file-write*", Path::new("/"))?;
    }
    let mut writable_roots = fs.writable_roots.clone();
    writable_roots.extend(
        super::system_temporary_paths()
            .iter()
            .map(|root| super::WritableRoot {
                root: root.clone(),
                read_only_subpaths: vec![],
            }),
    );
    for root in &writable_roots {
        for root in policy_paths_with_resolved(std::slice::from_ref(&root.root)) {
            profile.rule("allow", "file-write*", &root)?;
            profile.filter(&root)?;
            // Keep authority anchors from being unlinked/replaced while a
            // sandboxed process (or one of its descendants) is running.
            let literal_key = profile.parameters.len() - 1;
            profile.rules.push_str(&format!(
                "(deny file-write-unlink (literal (param \"DL_PATH_{literal_key}\")))\n"
            ));
        }
        for path in policy_paths_with_resolved(&root.read_only_subpaths) {
            profile.rule("deny", "file-write*", &path)?;
        }
    }
    if fs.deny_all_writes {
        profile.rules.push_str("(deny file-write*)\n");
    }
    let deny_read = &fs.deny_read_paths;
    let deny_write = &fs.deny_write_paths;
    for path in policy_paths_with_resolved(deny_read) {
        profile.rule("deny", read_ops, &path)?;
        let key = profile.parameters.len() - 1;
        profile.rules.push_str(&format!(
            "(deny network-outbound (remote unix-socket (subpath (param \"DL_PATH_{key}\"))))\n(deny network-bind (local unix-socket (subpath (param \"DL_PATH_{key}\"))))\n"
        ));
        // Native file writes can reveal private contents and replace protected
        // state; read denial is also a write boundary for native programs.
        profile.rule("deny", "file-write*", &path)?;
    }
    for path in policy_paths_with_resolved(deny_write) {
        profile.rule("deny", "file-write*", &path)?;
    }
    let anchors = fs
        .writable_roots
        .iter()
        .map(|root| &root.root)
        .chain(deny_read.iter())
        .chain(deny_write.iter())
        .chain(
            fs.writable_roots
                .iter()
                .flat_map(|root| &root.read_only_subpaths),
        );
    let mut ancestors = std::collections::BTreeSet::new();
    for path in anchors {
        for path in policy_paths_with_resolved(std::slice::from_ref(path)) {
            ancestors.extend(path.ancestors().skip(1).map(Path::to_owned));
        }
    }
    for path in ancestors {
        profile.filter(&path)?;
        let key = profile.parameters.len() - 1;
        profile.rules.push_str(&format!("(deny file-write-unlink (require-all (vnode-type DIRECTORY) (literal (param \"DL_PATH_{key}\"))))\n"));
    }
    profile.rules.push_str("(deny mach-lookup (xpc-service-name-prefix \"\"))\n(deny system-fcntl (fcntl-command 80 110))\n");
    let program = program
        .to_str()
        .ok_or_else(|| miette!("Seatbelt requires a Unicode executable path"))?;
    let mut sandbox_args = profile.parameters;
    sandbox_args.extend([
        "-p".to_owned(),
        profile.rules,
        "--".to_owned(),
        program.to_owned(),
    ]);
    sandbox_args.extend(args);
    Ok(SandboxSpawnSpec {
        program: PathBuf::from(SANDBOX_EXECUTABLE),
        args: sandbox_args,
    })
}

pub(super) struct MacosSandboxAsyncChild {
    child: tokio::process::Child,
}

fn finish_exited_group_cleanup(pid: u32, result: std::io::Result<()>) -> std::io::Result<()> {
    let Err(error) = result else {
        return Ok(());
    };
    if error.raw_os_error() == Some(libc::EPERM) {
        // XNU's killpg1 excludes zombies, and returns EPERM if no signalable
        // members remain. The caller has observed this leader with WNOWAIT,
        // so it still reserves its PID/group ID. Accept only a complete group
        // snapshot containing that zombie alone: it cannot fork after the
        // snapshot. A second slot detects any other member or truncated list;
        // query failures and genuinely denied live descendants stay errors.
        // PROC_PGRP_ONLY is defined in Apple's sys/proc_info.h.
        const PROC_PGRP_ONLY: u32 = 2;
        let mut members = [0 as libc::pid_t; 2];
        let bytes = unsafe {
            libc::proc_listpids(
                PROC_PGRP_ONLY,
                pid,
                members.as_mut_ptr().cast(),
                std::mem::size_of_val(&members) as libc::c_int,
            )
        };
        if bytes == std::mem::size_of::<libc::pid_t>() as libc::c_int
            && members[0] == pid as libc::pid_t
        {
            return Ok(());
        }
    }
    Err(error)
}

impl MacosSandboxAsyncChild {
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
    pub fn start_kill(&mut self) -> std::io::Result<()> {
        if let Some(pid) = self.child.id() {
            let result = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        self.child.start_kill()
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if let Some(pid) = self.child.id() {
            // Observe without reaping first: the zombie reserves its PID while
            // we terminate the group, preventing signals to a reused group ID.
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WNOHANG | libc::WEXITED | libc::WNOWAIT,
                )
            };
            if result < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if info.si_pid == 0 {
                return Ok(None);
            }
            finish_exited_group_cleanup(pid, self.start_kill())?;
        }
        self.child.try_wait()
    }
}
impl Drop for MacosSandboxAsyncChild {
    fn drop(&mut self) {
        let _ = self.start_kill();
    }
}

pub(super) fn spawn_restricted_async(
    policy: &SandboxPolicy,
    program: PathBuf,
    args: Vec<String>,
    options: super::SandboxProcessOptions,
) -> std::io::Result<MacosSandboxAsyncChild> {
    let spawn = wrap_command(policy, program, args).map_err(std::io::Error::other)?;
    let cancelled = options.spawn_cancelled.clone();
    let mut command = tokio::process::Command::new(spawn.program);
    command.args(spawn.args).process_group(0).kill_on_drop(true);
    super::apply_tokio_command_options(&mut command, options);
    if cancelled
        .as_ref()
        .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "Process startup cancelled",
        ));
    }
    let child = MacosSandboxAsyncChild {
        child: command.spawn()?,
    };
    if cancelled
        .as_ref()
        .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "Process startup cancelled",
        ));
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FileSystemSandboxPolicy, SandboxAsyncChild, SandboxProcessOptions, SandboxStdio,
        StrongFilesystemSandboxMode, WritableRoot,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    fn policy(root: &Path) -> SandboxPolicy {
        SandboxPolicy {
            filesystem: FileSystemSandboxPolicy {
                full_disk_read: false,
                deny_all_writes: false,
                full_disk_write: false,
                readable_roots: vec![root.to_owned()],
                writable_roots: vec![WritableRoot {
                    root: root.to_owned(),
                    read_only_subpaths: vec![root.join(".git")],
                }],
                deny_read_paths: vec![
                    root.join("private"),
                    root.join("future"),
                    root.join("ordinary/secret"),
                ],
                deny_write_paths: vec![],
                bound_roots: vec![std::fs::canonicalize(root).unwrap()],
            },
            strong_filesystem: StrongFilesystemSandboxMode::Required,
        }
    }
    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
    }
    #[test]
    fn policy_paths_are_parameters_and_cannot_be_seatbelt_code() {
        let temp = tempfile::Builder::new()
            .prefix("dl-\"(allow default)\n")
            .tempdir()
            .unwrap();
        let spec = wrap_command(
            &policy(temp.path()),
            PathBuf::from("/bin/sh"),
            vec!["-c".into(), "true".into()],
        )
        .unwrap();
        let profile = &spec.args[spec.args.iter().position(|arg| arg == "-p").unwrap() + 1];
        assert!(!profile.contains(temp.path().to_str().unwrap()));
        let parameters = spec
            .args
            .iter()
            .filter(|arg| arg.starts_with("-DDL_PATH_"))
            .map(|arg| arg.split_once('=').unwrap().1)
            .collect::<Vec<_>>();
        // /var is a symlink to /private/var on macOS. Both exact spellings
        // must survive as data; suffix matching can select the resolved one.
        for path in [
            temp.path().to_owned(),
            std::fs::canonicalize(temp.path()).unwrap(),
        ] {
            assert!(parameters.contains(&path.to_str().unwrap()));
        }
    }
    #[tokio::test]
    async fn actor_scope_runs_shell_and_enforces_future_and_ancestor_denies() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        let temp = tempfile::Builder::new()
            .prefix("dl quoted \" (root)\n")
            .tempdir()
            .unwrap();
        let root = temp.path().join("work");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("private")).unwrap();
        std::fs::write(root.join("private/secret"), "private").unwrap();
        std::fs::create_dir(root.join("ordinary")).unwrap();
        std::fs::write(root.join("ordinary/secret"), "private").unwrap();
        std::fs::write(root.join("input"), "input").unwrap();
        std::fs::write(temp.path().join("outside"), "outside").unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), root.join("alias")).unwrap();
        let script = format!(
            r#"set -eu
[ "$(cat input)" = input ]
[ -c /dev/null ]
printf probe >/dev/null
if cat alias >/dev/null 2>&1; then exit 10; fi
if cat private/secret >/dev/null 2>&1; then exit 11; fi
if printf bad 2>/dev/null > .git; then exit 12; fi
if mv ordinary moved 2>/dev/null; then exit 13; fi
printf allowed > ordinary/allowed
read command
if cat future >/dev/null 2>&1; then exit 14; fi
printf '%s' "$command" > {}
"#,
            quote(&root.join("output"))
        );
        let mut child = SandboxAsyncChild::spawn_shell(
            &policy(&root),
            "/bin/sh",
            vec!["-c".into(), script],
            SandboxProcessOptions {
                current_dir: Some(root.clone()),
                stdin: SandboxStdio::Piped,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(root.join("future"), "new private").unwrap();
        child
            .take_stdin()
            .unwrap()
            .write_all(b"persisted\n")
            .await
            .unwrap();
        let mut out = child.take_stdout().unwrap();
        let mut err = child.take_stderr().unwrap();
        let (out, err, status) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::try_join!(
                async {
                    let mut text = String::new();
                    out.read_to_string(&mut text).await?;
                    Ok::<_, std::io::Error>(text)
                },
                async {
                    let mut text = String::new();
                    err.read_to_string(&mut text).await?;
                    Ok::<_, std::io::Error>(text)
                },
                async {
                    loop {
                        if let Some(status) = child.try_wait()? {
                            return Ok::<_, std::io::Error>(status);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            )
        })
        .await
        .expect("Seatbelt probe timed out")
        .unwrap();
        assert!(status.success(), "{status};out={out};err={err}");
        assert_eq!(
            std::fs::read_to_string(root.join("output")).unwrap(),
            "persisted"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("ordinary/allowed")).unwrap(),
            "allowed"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("future")).unwrap(),
            "new private"
        );
        assert!(!root.join(".git").exists());
        assert!(!root.join("moved").exists());
    }
    #[tokio::test]
    async fn root_exit_and_drop_close_background_process_pipes() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
        use tokio::io::{AsyncBufReadExt, BufReader};
        for (cancel, background) in [(false, false), (false, true), (true, true)] {
            let temp = tempfile::tempdir().unwrap();
            let command = if cancel {
                "sleep 300 & printf 'ready\n'; read command"
            } else if background {
                "sleep 300 & printf 'ready\n'"
            } else {
                "printf 'ready\n'; exit 7"
            };
            let mut child = SandboxAsyncChild::spawn_shell(
                &policy(temp.path()),
                "/bin/sh",
                vec!["-c".into(), command.into()],
                SandboxProcessOptions {
                    current_dir: Some(temp.path().to_owned()),
                    stdin: SandboxStdio::Piped,
                    stdout: SandboxStdio::Piped,
                    stderr: SandboxStdio::Null,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut out = BufReader::new(child.take_stdout().unwrap());
            let mut line = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), out.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(line.trim(), "ready");
            if cancel {
                drop(child);
            } else {
                // Observe a zombie deterministically, without allowing Tokio
                // to reap it before the group-cleanup path is exercised.
                let pid = child.id().unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
                        assert_eq!(
                            unsafe {
                                libc::waitid(
                                    libc::P_PID,
                                    pid,
                                    &mut info,
                                    libc::WNOHANG | libc::WEXITED | libc::WNOWAIT,
                                )
                            },
                            0
                        );
                        if info.si_pid != 0 {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                // An actual live descendant must prevent EPERM recovery.
                // Other failures must propagate even for a zombie-only group.
                let errno = if background { libc::EPERM } else { libc::EIO };
                let error =
                    finish_exited_group_cleanup(pid, Err(std::io::Error::from_raw_os_error(errno)))
                        .unwrap_err();
                assert_eq!(error.raw_os_error(), Some(errno));
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
                assert_eq!(status.code(), Some(if background { 0 } else { 7 }));
            }
            let mut tail = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                out.read_to_string(&mut tail),
            )
            .await
            .expect("background process retained its output pipe")
            .unwrap();
        }
    }
}
