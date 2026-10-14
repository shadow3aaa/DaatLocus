//! Explicit OpenSSH forwarding to an already running remote Stage; no remote shell command.
use crate::stage::{Result, StageError};
use serde::{Deserialize, Serialize};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    process::{Child, ChildStdin, ChildStdout, Command},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshTarget {
    pub host: String,
    pub user: Option<String>,
    pub ssh_port: u16,
    pub stage_port: u16,
}
impl SshTarget {
    pub fn arguments(&self) -> Result<Vec<String>> {
        if self.host.is_empty()
            || self.host.len() > 253
            || self.host.starts_with('-')
            || !self
                .host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_:[]".contains(&byte))
            || self.user.as_ref().is_some_and(|user| {
                user.is_empty()
                    || user.len() > 64
                    || user.starts_with('-')
                    || !user
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            })
            || self.ssh_port == 0
            || self.stage_port == 0
        {
            return Err(StageError::Invalid("Invalid SSH target"));
        }
        let destination = self
            .user
            .as_ref()
            .map_or_else(|| self.host.clone(), |user| format!("{user}@{}", self.host));
        Ok(vec![
            "-T".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            "UpdateHostKeys=no".into(),
            "-o".into(),
            "ExitOnForwardFailure=yes".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-p".into(),
            self.ssh_port.to_string(),
            "-W".into(),
            format!("127.0.0.1:{}", self.stage_port),
            "--".into(),
            destination,
        ])
    }
}
pub struct SshStream {
    reader: ChildStdout,
    writer: ChildStdin,
    _child: Child,
}
/// Owner configuration selects the destination. This never installs/starts remote DL.
pub fn open(target: &SshTarget) -> Result<SshStream> {
    let mut command = Command::new("ssh");
    command
        .args(target.arguments()?)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    #[cfg(windows)]
    let _creation = crate::process_spawn::creation_lock()?;
    let mut child = command.spawn()?;
    let writer = child
        .stdin
        .take()
        .ok_or(StageError::Unavailable("SSH input unavailable"))?;
    let reader = child
        .stdout
        .take()
        .ok_or(StageError::Unavailable("SSH output unavailable"))?;
    Ok(SshStream {
        reader,
        writer,
        _child: child,
    })
}
impl AsyncRead for SshStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().reader).poll_read(context, buffer)
    }
}
impl AsyncWrite for SshStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().writer).poll_write(context, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_flush(context)
    }
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_shutdown(context)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_forwarding_keeps_host_verification_and_rejects_option_injection() {
        let mut target = SshTarget {
            host: "device.example".into(),
            user: Some("owner".into()),
            ssh_port: 22,
            stage_port: 45827,
        };
        let args = target.arguments().unwrap();
        assert!(args.iter().any(|arg| arg == "StrictHostKeyChecking=yes"));
        assert!(args.iter().any(|arg| arg == "UpdateHostKeys=no"));
        assert!(args.iter().any(|arg| arg == "127.0.0.1:45827"));
        assert_eq!(args.last().unwrap(), "owner@device.example");
        target.host = "-oProxyCommand=run".into();
        assert!(target.arguments().is_err());
        target.host = "host; command".into();
        assert!(target.arguments().is_err());
    }
}
