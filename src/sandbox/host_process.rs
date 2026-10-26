//! Explicit host execution still owns and cleans up its descendant processes.
use daat_locus_sandbox::{SandboxProcessOptions, SandboxStdio};
use std::{io, path::Path, process::ExitStatus};

pub(crate) struct HostChild {
    pub child: tokio::process::Child,
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
    #[cfg(unix)]
    killed: bool,
}
fn stdio(mode: SandboxStdio) -> std::process::Stdio {
    match mode {
        SandboxStdio::Inherit => std::process::Stdio::inherit(),
        SandboxStdio::Null => std::process::Stdio::null(),
        SandboxStdio::Piped => std::process::Stdio::piped(),
    }
}
impl HostChild {
    pub fn spawn(
        program: &str,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> io::Result<Self> {
        if options
            .spawn_cancelled
            .as_ref()
            .is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Process startup cancelled",
            ));
        }
        let mut command = tokio::process::Command::new(Path::new(program));
        command
            .args(args)
            .envs(options.environment)
            .kill_on_drop(true)
            .stdin(stdio(options.stdin))
            .stdout(stdio(options.stdout))
            .stderr(stdio(options.stderr));
        if let Some(cwd) = options.current_dir {
            command.current_dir(cwd);
        }
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(0x08000004); // CREATE_NO_WINDOW | CREATE_SUSPENDED
        #[cfg(windows)]
        let _creation = crate::process_spawn::creation_lock()?;
        let mut child = command.spawn()?;
        #[cfg(windows)]
        let job = match windows_job(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.start_kill();
                return Err(error);
            }
        };
        #[cfg(unix)]
        let _ = &mut child;
        let mut owned = Self {
            child,
            #[cfg(windows)]
            _job: job,
            #[cfg(unix)]
            killed: false,
        };
        if options
            .spawn_cancelled
            .as_ref()
            .is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Acquire))
        {
            owned.start_kill()?;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Process startup cancelled",
            ));
        }
        Ok(owned)
    }
    pub fn start_kill(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            if unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(
                    self._job.as_raw_handle(),
                    1,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(unix)]
        if !self.killed
            && let Some(pid) = self.child.id()
        {
            if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            self.killed = true;
        }
        Ok(())
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WNOHANG | libc::WEXITED | libc::WNOWAIT,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let exited = unsafe { info.si_pid() } != 0;
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let exited = info.si_pid != 0;
            if !exited {
                return Ok(None);
            }
            let cleanup = self.start_kill();
            #[cfg(target_os = "macos")]
            if cleanup
                .as_ref()
                .is_err_and(|error| error.raw_os_error() == Some(libc::EPERM))
            {
                let mut members = [0 as libc::pid_t; 2];
                let bytes = unsafe {
                    libc::proc_listpids(
                        2,
                        pid,
                        members.as_mut_ptr().cast(),
                        std::mem::size_of_val(&members) as i32,
                    )
                };
                if bytes == std::mem::size_of::<libc::pid_t>() as i32
                    && members[0] == pid as libc::pid_t
                {
                    self.killed = true;
                    return self.child.try_wait();
                }
            }
            cleanup?;
        }
        let status = self.child.try_wait()?;
        #[cfg(windows)]
        if status.is_some() {
            self.start_kill()?;
        }
        Ok(status)
    }
}
impl Drop for HostChild {
    fn drop(&mut self) {
        let _ = self.start_kill();
    }
}

#[cfg(windows)]
fn windows_job(child: &tokio::process::Child) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(handle);
        let mut limits = std::mem::zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) == 0
            || AssignProcessToJobObject(
                job.as_raw_handle(),
                child
                    .raw_handle()
                    .ok_or_else(|| io::Error::other("Process handle unavailable"))?,
            ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = OwnedHandle::from_raw_handle(snapshot);
        let mut entry = std::mem::zeroed::<THREADENTRY32>();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut valid = Thread32First(snapshot.as_raw_handle(), &mut entry);
        while valid != 0 {
            if Some(entry.th32OwnerProcessID) == child.id() {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let thread = OwnedHandle::from_raw_handle(thread);
                if ResumeThread(thread.as_raw_handle()) == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(job);
            }
            valid = Thread32Next(snapshot.as_raw_handle(), &mut entry);
        }
        Err(io::Error::other("Suspended process has no initial thread"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    #[tokio::test]
    async fn host_process_tree_is_cleaned_after_drop_and_parent_exit() {
        for parent_exits in [false, true] {
            #[cfg(windows)]
            let (program, args) = (
                "powershell.exe",
                vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    format!(
                        "$child=Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 120' -WindowStyle Hidden -PassThru; [Console]::Out.WriteLine($child.Id); {}",
                        if parent_exits {
                            "exit 0"
                        } else {
                            "Start-Sleep -Seconds 120"
                        }
                    ),
                ],
            );
            #[cfg(unix)]
            let (program, args) = (
                "/bin/sh",
                vec![
                    "-c".into(),
                    format!(
                        "sleep 120 & echo $!; {}",
                        if parent_exits { "exit 0" } else { "wait" }
                    ),
                ],
            );
            let mut child = HostChild::spawn(
                program,
                args,
                SandboxProcessOptions {
                    stdout: SandboxStdio::Piped,
                    stderr: SandboxStdio::Null,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut output = tokio::io::BufReader::new(child.child.stdout.take().unwrap());
            let mut line = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(15),
                output.read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            let descendant: u32 = line.trim().parse().unwrap();
            if parent_exits {
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while child.try_wait().unwrap().is_none() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            drop(child);
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let mut processes = sysinfo::System::new();
                    let pid = sysinfo::Pid::from_u32(descendant);
                    processes.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
                    if processes
                        .process(pid)
                        .is_none_or(|process| process.status() == sysinfo::ProcessStatus::Zombie)
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("host descendants must not survive their owning process");
        }
    }
}
