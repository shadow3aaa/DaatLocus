//! Managed hindsight daemon lifecycle.
//!
//! daat-locus always manages the hindsight daemon via `uvx hindsight-embed`
//! (or a locally-downloaded `uv` binary when neither `uvx` nor `uv` is on
//! PATH).  No pre-installed tooling is required.
//!
//! Runner resolution order:
//!   1. `uvx` on PATH  (alias for `uv tool run`)
//!   2. `uv` on PATH
//!   3. `~/.daat-locus/cache/bin/uv[.exe]`  (previously auto-downloaded)
//!   4. Download `uv` latest from GitHub Releases into the cache dir above
//!
//! Startup sequence (mirrors @vectorize-io/hindsight-all):
//!   1. `<runner> hindsight-embed[@ver] profile create <profile> --merge --port <port> --env K=V ...`
//!   2. `<runner> hindsight-embed[@ver] daemon --profile <profile> start`
//!   3. Poll `GET /health` until 200 or deadline.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use miette::{Result, miette};
use tokio::process::Command;

use crate::config::HindsightConfig;
use crate::daat_locus_paths::daat_locus_paths;

// ── Tuning ────────────────────────────────────────────────────────────────────

const HEALTH_POLL_INTERVAL_MS: u64 = 1_000;
const HEALTH_READY_TIMEOUT_MS: u64 = 60_000;
const COMMAND_TIMEOUT_SECS: u64 = 60;
/// `daemon start` blocks until the daemon is fully ready, which on first run
/// requires downloading HuggingFace embedding models (~100 MB). Allow 10 min.
const DAEMON_START_TIMEOUT_SECS: u64 = 600;
/// 5-minute window for the first-run uv download on slow connections.
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;

#[cfg(windows)]
const UV_EXE: &str = "uv.exe";
#[cfg(not(windows))]
const UV_EXE: &str = "uv";

// ── UvInvoker ─────────────────────────────────────────────────────────────────

/// How to invoke `uv tool run <package> <args>`.
enum UvInvoker {
    /// `uvx <package> <args>` — uvx binary on PATH
    Uvx,
    /// `/some/path/to/uv tool run <package> <args>`
    Uv(PathBuf),
}

impl UvInvoker {
    /// Build a `tokio::process::Command` ready to execute `<package> <args>`.
    fn embed_command(&self, package_spec: &str) -> Command {
        match self {
            UvInvoker::Uvx => {
                let mut cmd = Command::new("uvx");
                cmd.arg(package_spec);
                cmd
            }
            UvInvoker::Uv(path) => {
                let mut cmd = Command::new(path);
                cmd.args(["tool", "run", package_spec]);
                cmd
            }
        }
    }
}

// ── HindsightManagedServer ────────────────────────────────────────────────────

pub struct HindsightManagedServer {
    config: HindsightConfig,
    llm_env_vars: Vec<(String, String)>,
}

impl HindsightManagedServer {
    pub fn new(config: HindsightConfig, llm_env_vars: Vec<(String, String)>) -> Self {
        Self {
            config,
            llm_env_vars,
        }
    }

    /// Start the daemon: resolve runner → configure profile → start → wait.
    pub async fn start(&self) -> Result<()> {
        tracing::info!(
            "[hindsight:managed] starting daemon (profile={}, port={})",
            self.config.profile,
            self.config.port,
        );
        let invoker = self.ensure_uv_invoker().await?;
        self.configure_profile(&invoker).await?;
        self.start_daemon(&invoker).await?;
        self.wait_for_ready().await?;
        tracing::info!("[hindsight:managed] daemon ready at {}", self.base_url());
        Ok(())
    }

    /// Stop the daemon gracefully. Never fails — logs and returns.
    pub async fn stop(&self) {
        tracing::info!(
            "[hindsight:managed] stopping daemon (profile={})",
            self.config.profile
        );
        let invoker = match self.ensure_uv_invoker().await {
            Ok(inv) => inv,
            Err(err) => {
                tracing::warn!("[hindsight:managed] stop: could not find uv runner: {err}");
                return;
            }
        };
        let mut cmd = invoker.embed_command(&self.package_spec());
        cmd.args(self.daemon_profile_args()).arg("stop");
        match tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(out)) if !out.status.success() => {
                tracing::warn!(
                    "[hindsight:managed] daemon stop exited non-zero: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(Err(err)) => tracing::warn!("[hindsight:managed] daemon stop spawn error: {err}"),
            Err(_) => tracing::warn!("[hindsight:managed] daemon stop timed out"),
            _ => {}
        }
    }

    /// One-shot health probe (used to detect already-running daemon).
    pub async fn check_health(&self) -> bool {
        reqwest::Client::new()
            .get(format!("{}/health", self.base_url()))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.config.port)
    }

    fn package_spec(&self) -> String {
        let ver = self.config.embed_version.trim();
        if ver.is_empty() {
            "hindsight-embed".to_string()
        } else {
            format!("hindsight-embed@{ver}")
        }
    }

    /// `["daemon", "--profile", "<name>"]`
    fn daemon_profile_args(&self) -> Vec<String> {
        vec![
            "daemon".to_string(),
            "--profile".to_string(),
            self.config.profile.clone(),
        ]
    }

    async fn configure_profile(&self, invoker: &UvInvoker) -> Result<()> {
        tracing::info!(
            "[hindsight:managed] configuring profile '{}'",
            self.config.profile
        );
        let mut cmd = invoker.embed_command(&self.package_spec());
        cmd.args([
            "profile",
            "create",
            &self.config.profile,
            "--merge",
            "--port",
            &self.config.port.to_string(),
        ]);
        for (k, v) in self.profile_env_vars() {
            cmd.args(["--env", &format!("{k}={v}")]);
        }
        self.run_command(cmd, "profile.create").await
    }

    async fn start_daemon(&self, invoker: &UvInvoker) -> Result<()> {
        tracing::info!("[hindsight:managed] starting daemon process");
        let mut cmd = invoker.embed_command(&self.package_spec());
        cmd.args(self.daemon_profile_args()).arg("start");
        // Use a long timeout: first run downloads HuggingFace models (~100 MB).
        self.run_command_with_timeout(cmd, "daemon.start", DAEMON_START_TIMEOUT_SECS)
            .await
    }

    fn profile_env_vars(&self) -> Vec<(String, String)> {
        let mut vars = self.llm_env_vars.clone();
        // Use a daat-locus-specific pg0 instance so the database does not
        // collide with other apps that also use hindsight-embed.
        // Data lands at ~/.pg0/instances/daat-locus/data/
        vars.push((
            "HINDSIGHT_API_DATABASE_URL".into(),
            "pg0://daat-locus".into(),
        ));
        // macOS: local embedding/reranker models crash without CPU-only mode
        // (Metal/Accelerate incompatibility with hindsight's bundled ONNX runtime).
        if cfg!(target_os = "macos") {
            vars.push((
                "HINDSIGHT_API_EMBEDDINGS_LOCAL_FORCE_CPU".into(),
                "1".into(),
            ));
            vars.push(("HINDSIGHT_API_RERANKER_LOCAL_FORCE_CPU".into(), "1".into()));
        }
        vars
    }

    async fn run_command(&self, cmd: Command, label: &str) -> Result<()> {
        self.run_command_with_timeout(cmd, label, COMMAND_TIMEOUT_SECS)
            .await
    }

    async fn run_command_with_timeout(
        &self,
        mut cmd: Command,
        label: &str,
        timeout_secs: u64,
    ) -> Result<()> {
        // kill_on_drop(false): `daemon start` spawns a background OS process and
        // exits 0 — we must not kill the Command handle after it returns.
        cmd.kill_on_drop(false);
        let result = tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()).await;
        match result {
            Err(_) => Err(miette!(
                "[hindsight:managed] {label} timed out after {timeout_secs}s"
            )),
            Ok(Err(err)) => Err(miette!("[hindsight:managed] {label} spawn failed: {err}")),
            Ok(Ok(out)) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                for line in stdout.lines().chain(stderr.lines()) {
                    let t = line.trim();
                    if !t.is_empty() {
                        tracing::debug!("[hindsight:{label}] {t}");
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
                        truncate_error(&detail)
                    ))
                }
            }
        }
    }

    async fn wait_for_ready(&self) -> Result<()> {
        let url = format!("{}/health", self.base_url());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(HEALTH_POLL_INTERVAL_MS))
            .build()
            .map_err(|e| miette!("build health client: {e}"))?;
        let deadline = std::time::Instant::now() + Duration::from_millis(HEALTH_READY_TIMEOUT_MS);
        tracing::info!("[hindsight:managed] waiting for daemon at {url}");
        let mut attempt = 0u32;
        while std::time::Instant::now() < deadline {
            attempt += 1;
            if let Ok(r) = client.get(&url).send().await {
                if r.status().is_success() {
                    tracing::debug!("[hindsight:managed] health check passed (attempt {attempt})");
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(HEALTH_POLL_INTERVAL_MS)).await;
        }
        Err(miette!(
            "[hindsight:managed] daemon at {url} did not become ready within {HEALTH_READY_TIMEOUT_MS}ms"
        ))
    }

    /// Find or download a usable `uv` runner.
    async fn ensure_uv_invoker(&self) -> Result<UvInvoker> {
        // 1. uvx on PATH
        if probe_binary("uvx") {
            tracing::debug!("[hindsight:managed] using uvx from PATH");
            return Ok(UvInvoker::Uvx);
        }
        // 2. uv on PATH
        if probe_binary("uv") {
            tracing::debug!("[hindsight:managed] using uv from PATH");
            return Ok(UvInvoker::Uv(PathBuf::from("uv")));
        }
        // 3. Previously downloaded binary
        let cache_bin = daat_locus_paths()
            .await
            .cache_dir()
            .join("bin")
            .join(UV_EXE);
        if cache_bin.exists() {
            tracing::debug!(
                "[hindsight:managed] using cached uv at {}",
                cache_bin.display()
            );
            return Ok(UvInvoker::Uv(cache_bin));
        }
        // 4. Download
        tracing::info!(
            "[hindsight:managed] uv/uvx not found on PATH — downloading uv automatically"
        );
        download_uv(&cache_bin).await?;
        Ok(UvInvoker::Uv(cache_bin))
    }
}

// ── Binary detection & download ───────────────────────────────────────────────

/// Returns `true` if `name` resolves on PATH and responds to `--version`.
fn probe_binary(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// GitHub "latest release" asset URL for uv on the current platform.
/// Uses the `/latest/download/` redirect so no explicit version pin is needed.
fn uv_download_url() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-aarch64-apple-darwin.tar.gz",
        ),
        ("macos", "x86_64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-apple-darwin.tar.gz",
        ),
        ("linux", "x86_64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-unknown-linux-musl.tar.gz",
        ),
        ("linux", "aarch64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-aarch64-unknown-linux-musl.tar.gz",
        ),
        ("windows", "x86_64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-pc-windows-msvc.zip",
        ),
        ("windows", "aarch64") => Some(
            "https://github.com/astral-sh/uv/releases/latest/download/uv-aarch64-pc-windows-msvc.zip",
        ),
        _ => None,
    }
}

async fn download_uv(dest: &Path) -> Result<()> {
    let url = uv_download_url().ok_or_else(|| {
        miette!(
            "[hindsight:managed] unsupported platform ({}/{}): cannot auto-download uv. \
             Install uv manually from https://docs.astral.sh/uv/ and ensure it is on PATH.",
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    })?;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| miette!("create uv cache dir: {e}"))?;
    }

    tracing::info!("[hindsight:managed] downloading uv from {url}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .build()
        .map_err(|e| miette!("build download client: {e}"))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| miette!("download uv: {e}"))?;

    if !response.status().is_success() {
        return Err(miette!(
            "[hindsight:managed] uv download failed: HTTP {}",
            response.status()
        ));
    }

    let archive_bytes = response
        .bytes()
        .await
        .map_err(|e| miette!("read uv download body: {e}"))?;

    tracing::info!(
        "[hindsight:managed] extracting uv binary ({:.1} MiB compressed)",
        archive_bytes.len() as f64 / 1_048_576.0
    );

    let uv_bytes = if url.ends_with(".zip") {
        extract_uv_from_zip(&archive_bytes)?
    } else {
        extract_uv_from_targz(&archive_bytes)?
    };

    tokio::fs::write(dest, &uv_bytes)
        .await
        .map_err(|e| miette!("write uv binary to {}: {e}", dest.display()))?;

    // Mark executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = tokio::fs::metadata(dest)
            .await
            .map_err(|e| miette!("stat uv binary: {e}"))?;
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(dest, perms)
            .await
            .map_err(|e| miette!("chmod uv binary: {e}"))?;
    }

    tracing::info!(
        "[hindsight:managed] uv installed at {} ({:.1} MiB)",
        dest.display(),
        uv_bytes.len() as f64 / 1_048_576.0
    );
    Ok(())
}

// ── Archive extraction ────────────────────────────────────────────────────────

fn extract_uv_from_targz(bytes: &[u8]) -> Result<Vec<u8>> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let gz = GzDecoder::new(bytes);
    let mut archive = Archive::new(gz);
    for entry in archive
        .entries()
        .map_err(|e| miette!("read tar entries: {e}"))?
    {
        let mut entry = entry.map_err(|e| miette!("read tar entry: {e}"))?;
        let path = entry.path().map_err(|e| miette!("tar entry path: {e}"))?;
        // Match the binary regardless of whether it's at root or inside a
        // subdirectory (uv releases have used both layouts).
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if filename == "uv" {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| miette!("read uv from tar: {e}"))?;
            return Ok(buf);
        }
    }
    Err(miette!(
        "uv binary not found in tar.gz archive (expected entry named 'uv')"
    ))
}

fn extract_uv_from_zip(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Cursor;
    use zip::ZipArchive;

    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| miette!("open zip archive: {e}"))?;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| miette!("read zip entry {i}: {e}"))?;
        let filename = Path::new(file.name())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if filename == "uv.exe" {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|e| miette!("read uv.exe from zip: {e}"))?;
            return Ok(buf);
        }
    }
    Err(miette!(
        "uv.exe not found in zip archive (expected entry named 'uv.exe')"
    ))
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn truncate_error(text: &str) -> String {
    const MAX: usize = 400;
    if text.len() <= MAX {
        text.to_string()
    } else {
        format!("{}…", &text[..MAX])
    }
}
