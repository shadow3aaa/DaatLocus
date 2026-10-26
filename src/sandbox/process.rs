//! Device process execution: native isolation or an explicitly reviewed host context.
use super::*;
use std::{io, process::ExitStatus};

pub type SandboxChildStdin = Box<dyn tokio::io::AsyncWrite + Unpin + Send + Sync>;
pub type SandboxChildStdout = Box<dyn tokio::io::AsyncRead + Unpin + Send + Sync>;
pub type SandboxChildStderr = Box<dyn tokio::io::AsyncRead + Unpin + Send + Sync>;

enum Child {
    Native(daat_locus_sandbox::SandboxAsyncChild),
    Host(crate::host_process::HostChild),
    Administrator(crate::sudo_host::BrokerChild),
}
pub struct SandboxAsyncChild {
    policy: RuntimeSandboxPolicy,
    child: Child,
}
impl SandboxAsyncChild {
    pub fn spawn_shell(
        policy: &RuntimeSandboxPolicy,
        program: &str,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> io::Result<Self> {
        let host = policy.is_unrestricted();
        let child = if let Some(administrator) = &policy.administrator {
            if !host {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Administrator execution cannot masquerade as sandbox enforcement",
                ));
            }
            Child::Administrator(crate::sudo_host::BrokerChild::spawn(
                administrator.clone(),
                program,
                args,
                options,
            )?)
        } else if host {
            Child::Host(crate::host_process::HostChild::spawn(
                program, args, options,
            )?)
        } else {
            Child::Native(daat_locus_sandbox::SandboxAsyncChild::spawn_shell(
                &policy.native,
                program,
                args,
                options,
            )?)
        };
        Ok(Self {
            policy: policy.clone(),
            child,
        })
    }
    pub fn ensure_policy(&self, current: &RuntimeSandboxPolicy) -> miette::Result<()> {
        self.policy.filesystem.ensure_root_bindings()?;
        current.filesystem.ensure_root_bindings()?;
        if self.policy.native != current.native
            || self.policy.administrator != current.administrator
        {
            return Err(miette::miette!(
                "The process uses a different execution policy. Explicitly restart it to change sandbox or administrator authority; no command was replayed."
            ));
        }
        Ok(())
    }
    pub fn id(&self) -> Option<u32> {
        match &self.child {
            Child::Native(child) => child.id(),
            Child::Host(child) => child.child.id(),
            Child::Administrator(child) => child.id(),
        }
    }
    pub fn take_stdin(&mut self) -> Option<SandboxChildStdin> {
        match &mut self.child {
            Child::Native(child) => child
                .take_stdin()
                .map(|stream| Box::new(stream) as SandboxChildStdin),
            Child::Host(child) => child
                .child
                .stdin
                .take()
                .map(|stream| Box::new(stream) as SandboxChildStdin),
            Child::Administrator(child) => child
                .take_stdin()
                .map(|stream| Box::new(stream) as SandboxChildStdin),
        }
    }
    pub fn take_stdout(&mut self) -> Option<SandboxChildStdout> {
        match &mut self.child {
            Child::Native(child) => child
                .take_stdout()
                .map(|stream| Box::new(stream) as SandboxChildStdout),
            Child::Host(child) => child
                .child
                .stdout
                .take()
                .map(|stream| Box::new(stream) as SandboxChildStdout),
            Child::Administrator(child) => child
                .take_stdout()
                .map(|stream| Box::new(stream) as SandboxChildStdout),
        }
    }
    pub fn take_stderr(&mut self) -> Option<SandboxChildStderr> {
        match &mut self.child {
            Child::Native(child) => child
                .take_stderr()
                .map(|stream| Box::new(stream) as SandboxChildStderr),
            Child::Host(child) => child
                .child
                .stderr
                .take()
                .map(|stream| Box::new(stream) as SandboxChildStderr),
            Child::Administrator(child) => child
                .take_stderr()
                .map(|stream| Box::new(stream) as SandboxChildStderr),
        }
    }
    pub fn take_denial_report(&mut self) -> DenialReport {
        match &mut self.child {
            Child::Native(child) => child.take_denial_report(),
            Child::Host(_) | Child::Administrator(_) => {
                DenialReport::new(NativeBackend::Unrestricted, CaptureStatus::NotRequested)
            }
        }
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match &mut self.child {
            Child::Native(child) => child.try_wait(),
            Child::Host(child) => child.try_wait(),
            Child::Administrator(child) => child.try_wait(),
        }
    }
    pub fn start_kill(&mut self) -> io::Result<()> {
        match &mut self.child {
            Child::Native(child) => child.start_kill(),
            Child::Host(child) => child.start_kill(),
            Child::Administrator(child) => child.start_kill(),
        }
    }
}
