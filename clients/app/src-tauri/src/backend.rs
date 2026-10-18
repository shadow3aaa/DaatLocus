//! Process maintenance goes through the companion CLI's OS-owner IPC, never browser authority.
use clap::Parser;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, Parser)]
#[command(name = "daat-locus-app", version, about = "Daat Locus desktop client.")]
pub struct Options {
    #[arg(long, value_parser = profile)]
    pub profile: Option<String>,
    /// Start in the tray without showing the conversation window.
    #[arg(long)]
    pub background: bool,
}
fn profile(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("Invalid profile name".into());
    }
    Ok(value.into())
}

#[derive(Clone)]
pub struct Backend {
    pub executable: PathBuf,
    pub root: PathBuf,
    options: Options,
}
#[derive(Debug, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub executable: PathBuf,
    pub root: PathBuf,
    pub url: String,
    pub state: String,
}
impl Backend {
    pub fn new(options: Options) -> Result<Self> {
        let client = std::env::current_exe()?;
        let executable = client
            .parent()
            .ok_or("Application directory unavailable")?
            .join(if cfg!(windows) {
                "daat-locus.exe"
            } else {
                "daat-locus"
            });
        let executable = std::fs::canonicalize(executable)?;
        let namespace = std::env::var_os("DAAT_LOCUS_HOME")
            .map(PathBuf::from)
            .unwrap_or(
                std::env::home_dir()
                    .ok_or("Home directory unavailable")?
                    .join(".daat-locus"),
            );
        let namespace = if namespace.is_absolute() {
            namespace
        } else {
            std::env::current_dir()?.join(namespace)
        };
        let root = options.profile.as_ref().map_or_else(
            || namespace.clone(),
            |name| namespace.join("profiles").join(name),
        );
        Ok(Self {
            executable,
            root,
            options,
        })
    }
    async fn command(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(profile) = &self.options.profile {
            command.args(["--profile", profile]);
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        // Upgrading a running build allows 60s for orderly stop and 30s for readiness.
        let output = tokio::time::timeout(Duration::from_secs(100), command.output()).await??;
        if !output.status.success() {
            return Err("Local service operation failed".into());
        }
        if output.stdout.len() > 32 * 1024 {
            return Err("Service response is too large".into());
        }
        Ok(output.stdout)
    }
    pub async fn start(&self) -> Result<Status> {
        self.command(&["start"]).await?;
        self.status()
            .await?
            .ok_or_else(|| "Local service did not start".into())
    }
    pub async fn status(&self) -> Result<Option<Status>> {
        let status: Option<Status> =
            serde_json::from_slice(&self.command(&["status", "--json"]).await?)?;
        if let Some(status) = &status {
            self.validate(status)?;
        }
        Ok(status)
    }
    pub async fn bootstrap(
        &self,
        input: daat_locus_client_protocol::BootstrapRequest,
    ) -> Result<daat_locus_client_protocol::BootstrapReply> {
        use tokio::io::AsyncWriteExt;
        let bytes = serde_json::to_vec(&input)?;
        if bytes.len() > 60 * 1024 {
            return Err("Account request is too large".into());
        }
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("client-bootstrap")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(profile) = &self.options.profile {
            command.args(["--profile", profile]);
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let output = tokio::time::timeout(Duration::from_secs(35), async {
            let mut child = command.spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(&bytes).await?;
            }
            child.wait_with_output().await
        })
        .await??;
        if !output.status.success() || output.stdout.len() > 64 * 1024 {
            return Err("Account bootstrap failed".into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }
    fn validate(&self, status: &Status) -> Result<()> {
        local_origin(&status.url)?;
        if status.pid == 0
            || !matches!(status.state.as_str(), "running" | "stopping")
            || std::fs::canonicalize(&status.root)? != std::fs::canonicalize(&self.root)?
            || std::fs::canonicalize(&status.executable)? != self.executable
        {
            return Err("Service identity does not match this installation".into());
        }
        Ok(())
    }
}
pub fn local_origin(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    let local = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if url.scheme() != "http"
        || !local
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() == Some(0)
    {
        return Err("Expected the local service origin".into());
    }
    Ok(url)
}
pub fn profile_id(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let path = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    hex::encode(Sha256::digest(path.to_string_lossy().as_bytes()))[..32].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_origin_requires_an_exact_loopback_origin() {
        for good in ["http://127.0.0.1:45825", "http://[::1]:45825"] {
            assert!(local_origin(good).is_ok());
        }
        for bad in [
            "http://localhost:45825",
            "https://example.com",
            "http://127.0.0.1.evil.test",
            "http://user@127.0.0.1",
            "http://127.0.0.1/a",
            "http://127.0.0.1?key=x",
            "http://127.0.0.1/#x",
            "file:///a",
            "http://192.168.1.2",
        ] {
            assert!(local_origin(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn profile_arguments_cannot_escape_the_namespace() {
        for value in ["../other", "", "a/b", "a b"] {
            assert!(profile(value).is_err());
        }
        assert!(profile("work-2").is_ok());
    }
}
