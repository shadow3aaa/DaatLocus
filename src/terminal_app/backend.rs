//! Bind the terminal library to the existing native, host and administrator
//! process paths. Neither the App nor the library creates an alternative spawn.
use crate::sandbox::{RuntimeSandboxPolicy, SandboxAsyncChild, SandboxProcessOptions};
use daat_locus_bash::{Child, Input, Output, ProcessPolicy};

pub struct TerminalChild {
    child: SandboxAsyncChild,
    executable: std::path::PathBuf,
}

fn with_shell_runtime(
    policy: &RuntimeSandboxPolicy,
    executable: &std::path::Path,
) -> RuntimeSandboxPolicy {
    let mut effective = policy.clone();
    // An embedded runtime is an exact read dependency, never a grant for its
    // installation directory. Native explicit denials still take precedence.
    if !effective
        .filesystem
        .readable_roots
        .iter()
        .any(|path| path == executable)
    {
        effective
            .filesystem
            .readable_roots
            .push(executable.to_owned());
    }
    effective
}

impl ProcessPolicy for RuntimeSandboxPolicy {
    type Child = TerminalChild;
    fn shell_executable(&self) -> std::io::Result<std::path::PathBuf> {
        let executable = std::env::current_exe()?;
        #[cfg(test)]
        {
            // Cargo library-test harnesses do not contain the maintenance
            // executable's early shell entry. Use the explicitly built service.
            let directory = executable
                .parent()
                .and_then(std::path::Path::parent)
                .ok_or_else(|| std::io::Error::other("Missing test binary directory"))?;
            std::fs::canonicalize(
                directory.join(format!("daat-locus{}", std::env::consts::EXE_SUFFIX)),
            )
        }
        #[cfg(not(test))]
        Ok(executable)
    }
    fn spawn(
        &self,
        program: &str,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> std::io::Result<Self::Child> {
        let executable = self.shell_executable()?;
        if std::path::Path::new(program) != executable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Terminal backend requires its current embedded Bash host",
            ));
        }
        let child = SandboxAsyncChild::spawn_shell(
            &with_shell_runtime(self, &executable),
            program,
            args,
            options,
        )?;
        Ok(TerminalChild { child, executable })
    }
}
impl Child for TerminalChild {
    type Policy = RuntimeSandboxPolicy;
    fn ensure_policy(&self, policy: &Self::Policy) -> miette::Result<()> {
        self.child
            .ensure_policy(&with_shell_runtime(policy, &self.executable))
    }
    fn id(&self) -> Option<u32> {
        self.child.id()
    }
    fn take_stdin(&mut self) -> Option<Input> {
        self.child.take_stdin()
    }
    fn take_stdout(&mut self) -> Option<Output> {
        self.child.take_stdout()
    }
    fn take_stderr(&mut self) -> Option<Output> {
        self.child.take_stderr()
    }
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }
    fn start_kill(&mut self) -> std::io::Result<()> {
        self.child.start_kill()
    }
    fn take_denial_report(&mut self) -> crate::sandbox::DenialReport {
        self.child.take_denial_report()
    }
}
