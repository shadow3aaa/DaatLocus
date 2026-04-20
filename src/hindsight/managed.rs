//! Managed hindsight daemon lifecycle.
//!
//! When `HindsightConfig::managed = true`, daat-locus spawns and owns the
//! hindsight daemon via `uvx hindsight-embed` instead of relying on an
//! externally-running Docker container.
//!
//! Startup sequence (mirrors @vectorize-io/hindsight-all):
//!   1. `uvx hindsight-embed[@ver] profile create <profile> --merge --port <port> --env K=V …`
//!   2. `uvx hindsight-embed[@ver] daemon --profile <profile> start`
//!   3. Poll `GET /health` until 200 or timeout.
//!
//! Shutdown:
//!   4. `uvx hindsight-embed[@ver] daemon --profile <profile> stop`

use std::time::Duration;

use miette::{Result, miette};
use tokio::process::Command;

use crate::config::HindsightConfig;

/// Ready/health poll configuration.
const HEALTH_POLL_INTERVAL_MS: u64 = 1_000;
const HEALTH_READY_TIMEOUT_MS: u64 = 60_000;
/// Timeout for profile-create and daemon-start commands.
const COMMAND_TIMEOUT_SECS: u64 = 60;

pub struct HindsightManagedServer {
    config: HindsightConfig,
}

impl HindsightManagedServer {
    pub fn new(config: HindsightConfig) -> Self {
        Self { config }
    }

    /// Start the daemon: configure profile then start, then wait for health.
    pub async fn start(&self) -> Result<()> {
        tracing::info!(
            "[hindsight:managed] starting daemon (profile={}, port={})",
            self.config.managed_profile,
            self.config.managed_port,
        );
        self.configure_profile().await?;
        self.start_daemon().await?;
        self.wait_for_ready().await?;
        tracing::info!("[hindsight:managed] daemon ready at {}", self.base_url());
        Ok(())
    }

    /// Stop the daemon gracefully. Never fails — logs and returns.
    pub async fn stop(&self) {
        tracing::info!(
            "[hindsight:managed] stopping daemon (profile={})",
            self.config.managed_profile
        );
        let args = self.base_args("daemon");
        let mut cmd = self.uvx_command();
        cmd.args(&args).args(["stop"]);
        match tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(out)) => {
                if !out.status.success() {
                    tracing::warn!(
                        "[hindsight:managed] daemon stop exited non-zero: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
            }
            Ok(Err(err)) => {
                tracing::warn!("[hindsight:managed] daemon stop spawn error: {err}");
            }
            Err(_) => {
                tracing::warn!("[hindsight:managed] daemon stop timed out");
            }
        }
    }

    /// Quick one-shot health probe.
    pub async fn check_health(&self) -> bool {
        let url = format!("{}/health", self.base_url());
        match reqwest::Client::new()
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    // -------------------------------------------------------------------------
    // Internal
    // -------------------------------------------------------------------------

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.config.managed_port)
    }

    /// Build `uvx hindsight-embed[@ver]` command with common environment.
    fn uvx_command(&self) -> Command {
        let mut cmd = Command::new("uvx");
        let package = if self.config.managed_embed_version.trim().is_empty() {
            "hindsight-embed".to_string()
        } else {
            format!(
                "hindsight-embed@{}",
                self.config.managed_embed_version.trim()
            )
        };
        cmd.arg(package);
        cmd
    }

    /// Build `["daemon", "--profile", <name>]` (or `["profile", "--profile", <name>]` etc.)
    /// Actually: `hindsight-embed [-p PROFILE] <command>` — profile flag comes BEFORE the command.
    fn base_args(&self, subcommand: &str) -> Vec<String> {
        vec![
            "--profile".to_string(),
            self.config.managed_profile.clone(),
            subcommand.to_string(),
        ]
    }

    async fn configure_profile(&self) -> Result<()> {
        tracing::info!(
            "[hindsight:managed] configuring profile '{}'",
            self.config.managed_profile
        );
        let mut cmd = self.uvx_command();
        // hindsight-embed profile create <name> --merge --port <port> [--env K=V ...]
        cmd.args([
            "profile",
            "create",
            &self.config.managed_profile,
            "--merge",
            "--port",
            &self.config.managed_port.to_string(),
        ]);

        // Forward LLM config as --env flags.
        for (k, v) in self.profile_env_vars() {
            cmd.args(["--env", &format!("{k}={v}")]);
        }

        self.run_command_with_timeout(cmd, "profile.create").await
    }

    async fn start_daemon(&self) -> Result<()> {
        tracing::info!("[hindsight:managed] starting daemon process");
        let mut cmd = self.uvx_command();
        cmd.args(self.base_args("daemon")).arg("start");
        self.run_command_with_timeout(cmd, "daemon.start").await
    }

    /// Returns the env vars to persist into the profile (LLM config + macOS workarounds).
    fn profile_env_vars(&self) -> Vec<(String, String)> {
        let llm = &self.config.managed_llm;
        let mut vars = vec![
            ("HINDSIGHT_API_LLM_PROVIDER".into(), llm.provider.clone()),
            ("HINDSIGHT_API_LLM_API_KEY".into(), llm.api_key.clone()),
            ("HINDSIGHT_API_LLM_MODEL".into(), llm.model.clone()),
        ];
        if !llm.base_url.trim().is_empty() {
            vars.push((
                "HINDSIGHT_API_LLM_BASE_URL".into(),
                llm.base_url.trim().to_string(),
            ));
        }
        // macOS: local embedding/reranker models require CPU-only mode to avoid
        // Metal/Accelerate compatibility crashes (same workaround as hindsight-all).
        if cfg!(target_os = "macos") {
            vars.push((
                "HINDSIGHT_API_EMBEDDINGS_LOCAL_FORCE_CPU".into(),
                "1".into(),
            ));
            vars.push(("HINDSIGHT_API_RERANKER_LOCAL_FORCE_CPU".into(), "1".into()));
        }
        vars
    }

    async fn run_command_with_timeout(&self, mut cmd: Command, label: &str) -> Result<()> {
        cmd.kill_on_drop(false); // daemon start exits 0 once background process is running
        let result =
            tokio::time::timeout(Duration::from_secs(COMMAND_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Err(_) => Err(miette!(
                "[hindsight:managed] {label} timed out after {COMMAND_TIMEOUT_SECS}s"
            )),
            Ok(Err(err)) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    Err(miette!(
                        "[hindsight:managed] {label}: 'uvx' not found — install uv (https://docs.astral.sh/uv/)"
                    ))
                } else {
                    Err(miette!("[hindsight:managed] {label} spawn failed: {err}"))
                }
            }
            Ok(Ok(out)) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stdout = String::from_utf8_lossy(&out.stdout);
                for line in stdout.lines().chain(stderr.lines()) {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        tracing::debug!("[hindsight:{label}] {trimmed}");
                    }
                }
                if out.status.success() {
                    Ok(())
                } else {
                    let detail = if !stderr.trim().is_empty() {
                        stderr.trim().to_string()
                    } else {
                        stdout.trim().to_string()
                    };
                    Err(miette!(
                        "[hindsight:managed] {label} failed (exit {:?}): {}",
                        out.status.code(),
                        truncate_error_detail(&detail)
                    ))
                }
            }
        }
    }

    async fn wait_for_ready(&self) -> Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_millis(HEALTH_READY_TIMEOUT_MS);
        let url = format!("{}/health", self.base_url());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(HEALTH_POLL_INTERVAL_MS))
            .build()
            .map_err(|e| miette!("build health client: {e}"))?;

        tracing::info!("[hindsight:managed] waiting for daemon at {url}");
        let mut attempt = 0u32;
        while std::time::Instant::now() < deadline {
            attempt += 1;
            match client.get(&url).send().await {
                Ok(r) if r.status().is_success() => {
                    tracing::debug!("[hindsight:managed] health check passed (attempt {attempt})");
                    return Ok(());
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(HEALTH_POLL_INTERVAL_MS)).await;
        }
        Err(miette!(
            "[hindsight:managed] daemon at {url} did not become ready within {}ms",
            HEALTH_READY_TIMEOUT_MS
        ))
    }
}

fn truncate_error_detail(text: &str) -> String {
    const MAX: usize = 400;
    if text.len() <= MAX {
        text.to_string()
    } else {
        format!("{}…", &text[..MAX])
    }
}
