//! Terminal sessions own their shell processes and bounded output. The host
//! supplies process authority; this crate never substitutes unrestricted spawns.
use daat_locus_sandbox::{DenialReport, SandboxProcessOptions};
use std::{io, path::PathBuf, process::ExitStatus};

mod manager;
pub mod process;
pub mod shell;
mod wait_mode;
pub use manager::{
    TerminalExecCommandRequest, TerminalManager, TerminalSessionState, TerminalToolResult,
    tokenize_shell_words, truncate_terminal_output_tail,
};
pub use wait_mode::TerminalWaitMode;

pub type Input = Box<dyn tokio::io::AsyncWrite + Unpin + Send + Sync>;
pub type Output = Box<dyn tokio::io::AsyncRead + Unpin + Send + Sync>;

/// Implemented by the embedding host's effective sandbox/Sudo policy.
pub trait ProcessPolicy: Clone + Send + Sync + 'static {
    type Child: Child<Policy = Self>;
    /// The embedding executable containing the early Bash subprocess entry.
    fn shell_executable(&self) -> io::Result<PathBuf> {
        std::env::current_exe()
    }
    fn spawn(
        &self,
        program: &str,
        args: Vec<String>,
        options: SandboxProcessOptions,
    ) -> io::Result<Self::Child>;
}

/// The sandbox retains responsibility for enforcing authority and killing the
/// entire process tree, including on drop and after its root exits.
pub trait Child: Send + 'static {
    type Policy: ProcessPolicy<Child = Self>;
    fn ensure_policy(&self, policy: &Self::Policy) -> miette::Result<()>;
    fn id(&self) -> Option<u32>;
    fn take_stdin(&mut self) -> Option<Input>;
    fn take_stdout(&mut self) -> Option<Output>;
    fn take_stderr(&mut self) -> Option<Output>;
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    fn start_kill(&mut self) -> io::Result<()>;
    fn take_denial_report(&mut self) -> DenialReport;
}

pub(crate) fn shell_invocation(
    path: &std::path::Path,
    command: &str,
) -> io::Result<(String, Vec<String>)> {
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Shell path is not Unicode"))?;
    Ok((
        path.to_owned(),
        vec!["--internal-bash".into(), "-c".into(), command.into()],
    ))
}

pub const ENVIRONMENT: [(&str, &str); 11] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("DAAT_LOCUS_CI", "1"),
    ("PYTHONIOENCODING", "utf-8"),
];
