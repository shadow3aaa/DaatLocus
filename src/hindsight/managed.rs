//! Managed hindsight daemon lifecycle.
//!
//! daat-locus always manages the hindsight daemon via `uvx hindsight-embed`
//! (or a locally-downloaded `uv` binary when neither `uvx` nor `uv` is on
//! PATH).  No pre-installed tooling is required.
//!
//! Runner resolution order:
//!   1. `uvx` on PATH  (alias for `uv tool run`)
//!   2. `uv` on PATH
//!   3. Verified `~/.daat-locus/cache/bin/uv[.exe]` matching the pinned release
//!   4. Download the pinned `uv` release from GitHub Releases into the cache dir above
//!
//! Startup sequence (mirrors @vectorize-io/hindsight-all):
//!   1. `<runner> hindsight-embed[@ver] profile delete <profile>` best-effort, then
//!      `<runner> hindsight-embed[@ver] profile create <profile> --port <port> --env K=V ...`
//!      for non-secret profile values only.
//!   2. `<runner> hindsight-embed[@ver] daemon --profile <profile> start`
//!      with LLM credentials supplied through the daemon start environment.
//!   3. Poll `GET /health` until 200 or deadline.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::{
    config::HindsightConfig,
    daat_locus_paths::daat_locus_paths,
    persistence::{PersistenceFileMode, write_bytes_atomic},
};

// ── Tuning ────────────────────────────────────────────────────────────────────

const HEALTH_POLL_INTERVAL_MS: u64 = 1_000;
const HEALTH_READY_TIMEOUT_MS: u64 = 60_000;
const COMMAND_TIMEOUT_SECS: u64 = 60;
/// `daemon start` blocks until the daemon is fully ready, which on first run
/// requires downloading HuggingFace embedding models (~100 MB). Allow 10 min.
const DAEMON_START_TIMEOUT_SECS: u64 = 600;
/// 5-minute window for the first-run uv download on slow connections.
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;
const PINNED_UV_VERSION: &str = "0.11.7";

#[cfg(windows)]
const UV_EXE: &str = "uv.exe";
#[cfg(not(windows))]
const UV_EXE: &str = "uv";

const UV_CACHE_METADATA_FILE: &str = "uv.install.json";

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

    /// Stop the daemon gracefully.
    pub async fn stop(&self) -> Result<()> {
        if !self.check_health().await {
            return Ok(());
        }
        tracing::info!(
            "[hindsight:managed] stopping daemon (profile={})",
            self.config.profile,
        );
        let invoker = self.ensure_uv_invoker().await?;
        let mut cmd = invoker.embed_command(&self.package_spec());
        cmd.args(self.daemon_profile_args()).arg("stop");
        self.run_command(cmd, "daemon.stop").await
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
        self.delete_profile_if_exists(invoker).await?;
        let mut cmd = invoker.embed_command(&self.package_spec());
        cmd.args([
            "profile",
            "create",
            &self.config.profile,
            "--port",
            &self.config.port.to_string(),
        ]);
        for (k, v) in self.profile_env_vars() {
            cmd.args(["--env", &format!("{k}={v}")]);
        }
        self.run_command(cmd, "profile.create").await
    }

    async fn delete_profile_if_exists(&self, invoker: &UvInvoker) -> Result<()> {
        let mut cmd = invoker.embed_command(&self.package_spec());
        cmd.args(["profile", "delete", &self.config.profile]);
        match self.run_command(cmd, "profile.delete").await {
            Ok(()) => Ok(()),
            Err(err) if err.to_string().contains("does not exist") => Ok(()),
            Err(err) => Err(err),
        }
    }

    async fn start_daemon(&self, invoker: &UvInvoker) -> Result<()> {
        tracing::info!("[hindsight:managed] starting daemon process");
        let mut cmd = invoker.embed_command(&self.package_spec());
        cmd.args(self.daemon_profile_args()).arg("start");
        for (k, v) in self.daemon_env_vars() {
            cmd.env(k, v);
        }
        // Use a long timeout: first run downloads HuggingFace models (~100 MB).
        self.run_command_with_timeout(cmd, "daemon.start", DAEMON_START_TIMEOUT_SECS)
            .await
    }

    fn profile_env_vars(&self) -> Vec<(String, String)> {
        let mut vars = Vec::new();
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

    fn daemon_env_vars(&self) -> Vec<(String, String)> {
        let mut vars = self.profile_env_vars();
        vars.extend(self.llm_env_vars.clone());
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
            if let Ok(r) = client.get(&url).send().await
                && r.status().is_success()
            {
                tracing::debug!("[hindsight:managed] health check passed (attempt {attempt})");
                return Ok(());
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
        // 3. Previously downloaded binary, if it matches the pinned release.
        let cache_dir = daat_locus_paths().await.cache_dir().join("bin");
        let cache_bin = cache_dir.join(UV_EXE);
        let cache_metadata = cache_dir.join(UV_CACHE_METADATA_FILE);
        let spec = uv_download_spec().ok_or_else(|| {
            miette!(
                "[hindsight:managed] unsupported platform ({}/{}): cannot auto-download uv. \
                 Install uv manually from https://docs.astral.sh/uv/ and ensure it is on PATH.",
                std::env::consts::OS,
                std::env::consts::ARCH,
            )
        })?;
        if cached_uv_is_valid(&cache_bin, &cache_metadata, spec).await {
            tracing::debug!(
                "[hindsight:managed] using verified cached uv {} at {}",
                PINNED_UV_VERSION,
                cache_bin.display()
            );
            return Ok(UvInvoker::Uv(cache_bin));
        }

        // 4. Download the pinned release.
        tracing::info!(
            "[hindsight:managed] uv/uvx not found on PATH — downloading pinned uv {} automatically",
            PINNED_UV_VERSION,
        );
        download_uv(&cache_bin, &cache_metadata, spec).await?;
        Ok(UvInvoker::Uv(cache_bin))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_env_vars_do_not_persist_llm_secrets() {
        let server = HindsightManagedServer::new(
            HindsightConfig::default(),
            vec![
                (
                    "HINDSIGHT_API_LLM_API_KEY".to_string(),
                    "secret-value".to_string(),
                ),
                (
                    "HINDSIGHT_API_LLM_MODEL".to_string(),
                    "gpt-test".to_string(),
                ),
            ],
        );

        let profile_vars = server.profile_env_vars();
        assert!(
            !profile_vars
                .iter()
                .any(|(key, _)| key == "HINDSIGHT_API_LLM_API_KEY")
        );

        let daemon_vars = server.daemon_env_vars();
        assert!(
            daemon_vars
                .iter()
                .any(|(key, value)| key == "HINDSIGHT_API_LLM_API_KEY" && value == "secret-value")
        );
    }

    #[test]
    fn uv_download_url_uses_pinned_release() {
        if let Some(spec) = uv_download_spec() {
            let url = spec.download_url();
            assert!(url.contains(&format!("/download/{PINNED_UV_VERSION}/")));
            assert!(!url.contains("/latest/"));
            assert_eq!(spec.archive_sha256.len(), 64);
        }
    }

    #[test]
    fn verify_sha256_rejects_mismatched_download() {
        let expected = sha256_hex(b"expected");
        verify_sha256("test", b"expected", &expected).expect("matching hash");

        let err =
            verify_sha256("test", b"tampered", &expected).expect_err("mismatched hash should fail");
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[tokio::test]
    async fn cached_uv_requires_matching_metadata_and_binary_hash() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let uv_path = tempdir.path().join(UV_EXE);
        let metadata_path = tempdir.path().join(UV_CACHE_METADATA_FILE);
        let spec = UvDownloadSpec {
            asset_name: "uv-test.tar.gz",
            archive_kind: UvArchiveKind::TarGz,
            archive_sha256: "archive-hash",
        };
        tokio::fs::write(&uv_path, b"uv-binary")
            .await
            .expect("write uv");
        let metadata = CachedUvMetadata::from_install(spec, sha256_hex(b"uv-binary"));
        tokio::fs::write(
            &metadata_path,
            serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
        )
        .await
        .expect("write metadata");

        assert!(
            cached_uv_is_valid(&uv_path, &metadata_path, spec).await,
            "matching metadata and binary hash should be valid"
        );

        tokio::fs::write(&uv_path, b"tampered")
            .await
            .expect("tamper uv");
        assert!(
            !cached_uv_is_valid(&uv_path, &metadata_path, spec).await,
            "tampered cached uv should be invalid before execution"
        );
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UvArchiveKind {
    TarGz,
    Zip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UvDownloadSpec {
    asset_name: &'static str,
    archive_kind: UvArchiveKind,
    archive_sha256: &'static str,
}

impl UvDownloadSpec {
    fn download_url(self) -> String {
        format!(
            "https://github.com/astral-sh/uv/releases/download/{}/{}",
            PINNED_UV_VERSION, self.asset_name
        )
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct CachedUvMetadata {
    version: String,
    asset_name: String,
    archive_sha256: String,
    binary_sha256: String,
}

impl CachedUvMetadata {
    fn from_install(spec: UvDownloadSpec, binary_sha256: String) -> Self {
        Self {
            version: PINNED_UV_VERSION.to_string(),
            asset_name: spec.asset_name.to_string(),
            archive_sha256: spec.archive_sha256.to_string(),
            binary_sha256,
        }
    }

    fn matches_install(&self, spec: UvDownloadSpec, binary_sha256: &str) -> bool {
        self.version == PINNED_UV_VERSION
            && self.asset_name == spec.asset_name
            && self.archive_sha256 == spec.archive_sha256
            && self.binary_sha256 == binary_sha256
    }
}

/// Pinned GitHub release asset and SHA256 for uv on the current platform.
/// Hashes come from the pinned release's `sha256.sum` asset.
fn uv_download_spec() -> Option<UvDownloadSpec> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(UvDownloadSpec {
            asset_name: "uv-aarch64-apple-darwin.tar.gz",
            archive_kind: UvArchiveKind::TarGz,
            archive_sha256: "66e37d91f839e12481d7b932a1eccbfe732560f42c1cfb89faddfa2454534ba8",
        }),
        ("macos", "x86_64") => Some(UvDownloadSpec {
            asset_name: "uv-x86_64-apple-darwin.tar.gz",
            archive_kind: UvArchiveKind::TarGz,
            archive_sha256: "0a4bc8fcde4974ea3560be21772aeecab600a6f43fa6e58169f9fa7b3b71d302",
        }),
        ("linux", "x86_64") => Some(UvDownloadSpec {
            asset_name: "uv-x86_64-unknown-linux-musl.tar.gz",
            archive_kind: UvArchiveKind::TarGz,
            archive_sha256: "64ddb5f1087649e3f75aa50d139aa4f36ddde728a5295a141e0fa9697bfb7b0f",
        }),
        ("linux", "aarch64") => Some(UvDownloadSpec {
            asset_name: "uv-aarch64-unknown-linux-musl.tar.gz",
            archive_kind: UvArchiveKind::TarGz,
            archive_sha256: "46647dc16cbb7d6700f762fdd7a67d220abe18570914732bc310adc91308d272",
        }),
        ("windows", "x86_64") => Some(UvDownloadSpec {
            asset_name: "uv-x86_64-pc-windows-msvc.zip",
            archive_kind: UvArchiveKind::Zip,
            archive_sha256: "fe0c7815acf4fc45f8a5eff58ed3cf7ae2e15c3cf1dceadbd10c816ec1690cc1",
        }),
        ("windows", "aarch64") => Some(UvDownloadSpec {
            asset_name: "uv-aarch64-pc-windows-msvc.zip",
            archive_kind: UvArchiveKind::Zip,
            archive_sha256: "1387e1c94e15196351196b79fce4c1e6f4b30f19cdaaf9ff85fbd6b046018aa2",
        }),
        _ => None,
    }
}

async fn cached_uv_is_valid(uv_path: &Path, metadata_path: &Path, spec: UvDownloadSpec) -> bool {
    let metadata = match tokio::fs::read(metadata_path).await {
        Ok(bytes) => match serde_json::from_slice::<CachedUvMetadata>(&bytes) {
            Ok(metadata) => metadata,
            Err(err) => {
                tracing::warn!(
                    "[hindsight:managed] ignoring invalid uv cache metadata {}: {err}",
                    metadata_path.display()
                );
                return false;
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
        Err(err) => {
            tracing::warn!(
                "[hindsight:managed] failed to read uv cache metadata {}: {err}",
                metadata_path.display()
            );
            return false;
        }
    };

    let bytes = match tokio::fs::read(uv_path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
        Err(err) => {
            tracing::warn!(
                "[hindsight:managed] failed to read cached uv binary {}: {err}",
                uv_path.display()
            );
            return false;
        }
    };
    let binary_sha256 = sha256_hex(&bytes);
    if metadata.matches_install(spec, &binary_sha256) {
        true
    } else {
        tracing::warn!(
            "[hindsight:managed] cached uv at {} does not match pinned uv {}; redownloading",
            uv_path.display(),
            PINNED_UV_VERSION
        );
        false
    }
}

async fn download_uv(dest: &Path, metadata_path: &Path, spec: UvDownloadSpec) -> Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| miette!("create uv cache dir: {e}"))?;
    }

    let url = spec.download_url();
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
    verify_sha256("uv download archive", &archive_bytes, spec.archive_sha256)?;

    tracing::info!(
        "[hindsight:managed] extracting uv binary ({:.1} MiB compressed)",
        archive_bytes.len() as f64 / 1_048_576.0
    );

    let uv_bytes = match spec.archive_kind {
        UvArchiveKind::Zip => extract_uv_from_zip(&archive_bytes)?,
        UvArchiveKind::TarGz => extract_uv_from_targz(&archive_bytes)?,
    };
    let binary_sha256 = sha256_hex(&uv_bytes);

    #[cfg(windows)]
    {
        let _ = tokio::fs::remove_file(dest).await;
        let _ = tokio::fs::remove_file(metadata_path).await;
    }

    write_bytes_atomic(
        dest.to_path_buf(),
        uv_bytes.clone(),
        PersistenceFileMode::Default,
    )
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

    let metadata = CachedUvMetadata::from_install(spec, binary_sha256);
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|e| miette!("serialize uv cache metadata: {e}"))?;
    write_bytes_atomic(
        metadata_path.to_path_buf(),
        metadata_bytes,
        PersistenceFileMode::Default,
    )
    .await
    .map_err(|e| {
        miette!(
            "write uv cache metadata to {}: {e}",
            metadata_path.display()
        )
    })?;

    tracing::info!(
        "[hindsight:managed] uv {} installed at {} ({:.1} MiB)",
        PINNED_UV_VERSION,
        dest.display(),
        uv_bytes.len() as f64 / 1_048_576.0
    );
    Ok(())
}

fn verify_sha256(label: &str, bytes: &[u8], expected: &str) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(miette!(
            "[hindsight:managed] {label} checksum mismatch: expected sha256:{expected}, got sha256:{actual}"
        ))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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
