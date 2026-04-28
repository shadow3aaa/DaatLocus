//! Managed Hindsight daemon lifecycle.
//!
//! Daat Locus does not resolve Python packages at runtime. The Hindsight
//! runtime is supplied as a self-contained sidecar archive, then extracted once
//! into the local cache and executed directly.
//!
//! Sidecar contract:
//!
//! - Input: a pinned Daat Locus GitHub Release sidecar manifest downloaded on
//!   first use.
//! - Archive layout: `bin/hindsight-embed[.exe]` plus all runtime files it
//!   needs, including Python/runtime/native/model assets.
//! - The packaged `hindsight-embed` must not call uv/uvx/pip/network package
//!   installers. It owns profile create/delete/start/stop semantics locally.

use std::{
    fs,
    fs::File,
    io::{Read, Seek},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use miette::{Context as _, IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::{config::HindsightConfig, daat_locus_paths::daat_locus_paths};

// ── Tuning ────────────────────────────────────────────────────────────────────

const HEALTH_POLL_INTERVAL_MS: u64 = 1_000;
const HEALTH_READY_TIMEOUT_MS: u64 = 60_000;
const COMMAND_TIMEOUT_SECS: u64 = 60;
const DAEMON_STOP_TIMEOUT_SECS: u64 = 20;
const DAEMON_START_TIMEOUT_SECS: u64 = 660;
const SIDECAR_METADATA_FILE: &str = "daat-locus-sidecar.json";
const SIDECAR_TARGET: &str = env!("DAAT_LOCUS_BUILD_TARGET");
const SIDECAR_DOWNLOAD_RELEASE_TAG: &str = "hindsight-sidecars-v0.5.5-1";
const SIDECAR_DOWNLOAD_MANIFEST_URL: &str = "https://github.com/shadow3aaa/DaatLocus/releases/download/hindsight-sidecars-v0.5.5-1/manifest.toml";
const SIDECAR_DOWNLOAD_USER_AGENT: &str = concat!("daat-locus/", env!("CARGO_PKG_VERSION"));

#[cfg(all(test, windows))]
const HINDSIGHT_EMBED_EXE: &str = "hindsight-embed.exe";
#[cfg(all(test, not(windows)))]
const HINDSIGHT_EMBED_EXE: &str = "hindsight-embed";

// ── Managed sidecar ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SidecarArchiveKind {
    TarZst,
    TarGz,
    Zip,
}

impl SidecarArchiveKind {
    fn from_manifest(value: &str) -> Result<Self> {
        match value {
            "tar.zst" => Ok(Self::TarZst),
            "tar.gz" => Ok(Self::TarGz),
            "zip" => Ok(Self::Zip),
            "" => Err(miette!("Hindsight sidecar archive kind is missing")),
            other => Err(miette!(
                "Hindsight sidecar archive kind '{other}' is not supported"
            )),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SidecarInstallMetadata {
    target: String,
    archive_kind: SidecarArchiveKind,
    archive_sha256: String,
    entry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    download_release: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SidecarDownloadManifest {
    #[allow(dead_code)]
    schema_version: Option<u32>,
    #[allow(dead_code)]
    release: Option<String>,
    #[allow(dead_code)]
    hindsight_version: Option<String>,
    #[serde(default)]
    sidecar: Vec<SidecarDownloadEntry>,
}

#[derive(Clone, Debug, Deserialize)]
struct SidecarDownloadEntry {
    target: String,
    archive: String,
    archive_kind: String,
    sha256: String,
    entry: String,
    url: String,
    #[allow(dead_code)]
    built_with: Option<String>,
    #[allow(dead_code)]
    hindsight_version: Option<String>,
}

struct HindsightSidecar {
    root: PathBuf,
    executable: PathBuf,
}

impl HindsightSidecar {
    async fn ensure_installed() -> Result<Self> {
        let cache_root = sidecar_cache_root().await;
        if let Some(sidecar) = Self::find_cached_downloaded(&cache_root).await? {
            return Ok(sidecar);
        }
        Self::ensure_downloaded(&cache_root).await
    }

    async fn find_cached_downloaded(cache_root: &Path) -> Result<Option<Self>> {
        let mut entries = match tokio::fs::read_dir(cache_root).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(miette!(
                    "read Hindsight sidecar cache {}: {err}",
                    cache_root.display()
                ));
            }
        };

        let mut candidates = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .into_diagnostic()
            .wrap_err_with(|| format!("scan sidecar cache {}", cache_root.display()))?
        {
            let file_type = entry
                .file_type()
                .await
                .into_diagnostic()
                .wrap_err("read sidecar cache entry type")?;
            if !file_type.is_dir() {
                continue;
            }
            let root = entry.path();
            let metadata_path = root.join(SIDECAR_METADATA_FILE);
            let bytes = match tokio::fs::read(&metadata_path).await {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let metadata = match serde_json::from_slice::<SidecarInstallMetadata>(&bytes) {
                Ok(metadata) => metadata,
                Err(err) => {
                    tracing::warn!(
                        "[hindsight:managed] ignoring invalid sidecar metadata {}: {err}",
                        metadata_path.display()
                    );
                    continue;
                }
            };
            if metadata.target != SIDECAR_TARGET
                || metadata.download_release.as_deref() != Some(SIDECAR_DOWNLOAD_RELEASE_TAG)
            {
                continue;
            }
            if ensure_safe_relative_archive_path(Path::new(&metadata.entry)).is_err()
                || !root.join(Path::new(&metadata.entry)).is_file()
            {
                continue;
            }
            candidates.push((root, metadata.entry));
        }

        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        let Some((root, entry)) = candidates.pop() else {
            return Ok(None);
        };
        tracing::info!(
            "[hindsight:managed] using cached downloaded sidecar at {}",
            root.display()
        );
        Ok(Some(Self::from_root(root, &entry)?))
    }

    async fn ensure_downloaded(cache_root: &Path) -> Result<Self> {
        tracing::info!(
            "[hindsight:managed] no cached sidecar for {}; downloading manifest {}",
            SIDECAR_TARGET,
            SIDECAR_DOWNLOAD_MANIFEST_URL
        );
        let client = reqwest::Client::builder()
            .user_agent(SIDECAR_DOWNLOAD_USER_AGENT)
            .build()
            .map_err(|err| miette!("build Hindsight sidecar download client: {err}"))?;
        let manifest_text = client
            .get(SIDECAR_DOWNLOAD_MANIFEST_URL)
            .send()
            .await
            .map_err(|err| miette!("download Hindsight sidecar manifest: {err}"))?
            .error_for_status()
            .map_err(|err| miette!("download Hindsight sidecar manifest: {err}"))?
            .text()
            .await
            .map_err(|err| miette!("read Hindsight sidecar manifest: {err}"))?;
        let entry = select_sidecar_download_entry(&manifest_text, SIDECAR_TARGET)?;
        let archive_kind = SidecarArchiveKind::from_manifest(&entry.archive_kind)?;
        ensure_safe_relative_archive_path(Path::new(&entry.archive))?;
        ensure_safe_relative_archive_path(Path::new(&entry.entry))?;
        validate_sha256_hex(&entry.sha256)?;

        let short_sha = entry.sha256.get(..16).unwrap_or(&entry.sha256).to_string();
        let install_root = cache_root.join(format!("{}-{short_sha}", entry.target));
        let metadata = SidecarInstallMetadata {
            target: entry.target.clone(),
            archive_kind,
            archive_sha256: entry.sha256.clone(),
            entry: entry.entry.clone(),
            download_release: Some(SIDECAR_DOWNLOAD_RELEASE_TAG.to_string()),
        };

        if sidecar_install_is_valid(&install_root, &metadata).await {
            return Self::from_root(install_root, &metadata.entry);
        }

        let archive_path = download_sidecar_archive(&client, &entry, cache_root).await?;
        let install_result =
            install_downloaded_sidecar(&archive_path, archive_kind, &metadata, &install_root).await;
        let _ = tokio::fs::remove_file(&archive_path).await;
        install_result?;
        Self::from_root(install_root, &metadata.entry)
    }

    fn from_root(root: PathBuf, entry: &str) -> Result<Self> {
        let entry_path = Path::new(entry);
        ensure_safe_relative_archive_path(entry_path)?;
        let executable = root.join(entry_path);
        if !executable.is_file() {
            return Err(miette!(
                "Hindsight sidecar is missing executable {}",
                executable.display()
            ));
        }
        Ok(Self { root, executable })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        configure_sidecar_process_env(&mut command);
        command
    }
}

fn configure_sidecar_process_env(command: &mut Command) {
    command.env("PYTHONUTF8", "1");
    command.env("PYTHONIOENCODING", "utf-8");
}

async fn sidecar_cache_root() -> PathBuf {
    daat_locus_paths()
        .await
        .cache_dir()
        .join("hindsight-sidecars")
}

fn select_sidecar_download_entry(
    manifest_text: &str,
    target: &str,
) -> Result<SidecarDownloadEntry> {
    let manifest: SidecarDownloadManifest = toml::from_str(manifest_text)
        .into_diagnostic()
        .wrap_err("parse Hindsight sidecar download manifest")?;
    let mut matches = manifest
        .sidecar
        .into_iter()
        .filter(|entry| entry.target == target)
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(miette!(
            "Hindsight sidecar download manifest has duplicate entries for target '{target}'"
        ));
    }
    let entry = matches.pop().ok_or_else(|| {
        miette!("Hindsight sidecar download manifest has no entry for target '{target}'")
    })?;
    if entry.url.trim().is_empty() {
        return Err(miette!(
            "Hindsight sidecar download manifest entry for target '{target}' has an empty URL"
        ));
    }
    Ok(entry)
}

fn validate_sha256_hex(value: &str) -> Result<()> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(miette!(
            "Hindsight sidecar sha256 is not a 64-character hex hash"
        ))
    }
}

async fn download_sidecar_archive(
    client: &reqwest::Client,
    entry: &SidecarDownloadEntry,
    cache_root: &Path,
) -> Result<PathBuf> {
    tokio::fs::create_dir_all(cache_root)
        .await
        .into_diagnostic()
        .wrap_err_with(|| format!("create sidecar cache dir {}", cache_root.display()))?;
    let extension = match entry.archive_kind.as_str() {
        "tar.zst" => "tar.zst",
        "tar.gz" => "tar.gz",
        "zip" => "zip",
        other => {
            return Err(miette!(
                "Hindsight sidecar archive kind '{other}' is not supported"
            ));
        }
    };
    let short_sha = entry.sha256.get(..16).unwrap_or(&entry.sha256).to_string();
    let archive_path = cache_root.join(format!(
        "download-{}-{short_sha}.{extension}",
        SIDECAR_TARGET
    ));
    let tmp_path = archive_path.with_extension(format!("tmp-{}", std::process::id()));
    if tmp_path.exists() {
        tokio::fs::remove_file(&tmp_path)
            .await
            .into_diagnostic()
            .wrap_err_with(|| format!("remove stale sidecar archive {}", tmp_path.display()))?;
    }

    tracing::info!(
        "[hindsight:managed] downloading sidecar archive from {}",
        entry.url
    );
    let response = client
        .get(&entry.url)
        .send()
        .await
        .map_err(|err| miette!("download Hindsight sidecar archive: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("download Hindsight sidecar archive: {err}"))?;
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .into_diagnostic()
        .wrap_err_with(|| format!("create sidecar archive {}", tmp_path.display()))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| miette!("read Hindsight sidecar archive stream: {err}"))?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .into_diagnostic()
            .wrap_err_with(|| format!("write sidecar archive {}", tmp_path.display()))?;
        downloaded += chunk.len() as u64;
    }
    file.flush()
        .await
        .into_diagnostic()
        .wrap_err_with(|| format!("flush sidecar archive {}", tmp_path.display()))?;
    drop(file);

    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(&entry.sha256) {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(miette!(
            "downloaded Hindsight sidecar checksum mismatch: expected {}, got {actual}",
            entry.sha256
        ));
    }
    if archive_path.exists() {
        tokio::fs::remove_file(&archive_path)
            .await
            .into_diagnostic()
            .wrap_err_with(|| format!("replace sidecar archive {}", archive_path.display()))?;
    }
    tokio::fs::rename(&tmp_path, &archive_path)
        .await
        .into_diagnostic()
        .wrap_err_with(|| {
            format!(
                "move sidecar archive {} to {}",
                tmp_path.display(),
                archive_path.display()
            )
        })?;
    tracing::info!(
        "[hindsight:managed] downloaded sidecar archive ({} bytes)",
        downloaded
    );
    Ok(archive_path)
}

async fn sidecar_install_is_valid(root: &Path, expected: &SidecarInstallMetadata) -> bool {
    let metadata_path = root.join(SIDECAR_METADATA_FILE);
    let bytes = match tokio::fs::read(&metadata_path).await {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let metadata = match serde_json::from_slice::<SidecarInstallMetadata>(&bytes) {
        Ok(metadata) => metadata,
        Err(err) => {
            tracing::warn!(
                "[hindsight:managed] ignoring invalid sidecar metadata {}: {err}",
                metadata_path.display()
            );
            return false;
        }
    };
    metadata == *expected && root.join(Path::new(&expected.entry)).is_file()
}

async fn install_downloaded_sidecar(
    archive_path: &Path,
    archive_kind: SidecarArchiveKind,
    metadata: &SidecarInstallMetadata,
    install_root: &Path,
) -> Result<()> {
    tracing::info!(
        "[hindsight:managed] installing downloaded sidecar into {}",
        install_root.display()
    );
    install_sidecar_archive(metadata, install_root, |tmp_root| {
        unpack_sidecar_archive_from_file(archive_path, archive_kind, tmp_root)
    })
    .await
}

async fn install_sidecar_archive<F>(
    metadata: &SidecarInstallMetadata,
    install_root: &Path,
    extract: F,
) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    if let Some(parent) = install_root.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .into_diagnostic()
            .wrap_err_with(|| format!("create sidecar cache dir {}", parent.display()))?;
    }
    let tmp_root = install_root.with_extension(format!("tmp-{}", std::process::id()));
    if tmp_root.exists() {
        tokio::fs::remove_dir_all(&tmp_root)
            .await
            .into_diagnostic()
            .wrap_err_with(|| format!("remove stale sidecar temp dir {}", tmp_root.display()))?;
    }
    tokio::fs::create_dir_all(&tmp_root)
        .await
        .into_diagnostic()
        .wrap_err_with(|| format!("create sidecar temp dir {}", tmp_root.display()))?;

    if let Err(err) = extract(&tmp_root) {
        let _ = tokio::fs::remove_dir_all(&tmp_root).await;
        return Err(err);
    }

    let metadata_bytes = serde_json::to_vec_pretty(metadata)
        .into_diagnostic()
        .wrap_err("serialize sidecar metadata")?;
    tokio::fs::write(tmp_root.join(SIDECAR_METADATA_FILE), metadata_bytes)
        .await
        .into_diagnostic()
        .wrap_err("write sidecar metadata")?;

    if install_root.exists() {
        tokio::fs::remove_dir_all(install_root)
            .await
            .into_diagnostic()
            .wrap_err_with(|| format!("replace sidecar install dir {}", install_root.display()))?;
    }
    tokio::fs::rename(&tmp_root, install_root)
        .await
        .into_diagnostic()
        .wrap_err_with(|| {
            format!(
                "move sidecar temp dir {} to {}",
                tmp_root.display(),
                install_root.display()
            )
        })?;
    Ok(())
}

fn unpack_sidecar_archive_from_file(
    archive_path: &Path,
    archive_kind: SidecarArchiveKind,
    target_dir: &Path,
) -> Result<()> {
    let file = File::open(archive_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("open sidecar archive {}", archive_path.display()))?;
    match archive_kind {
        SidecarArchiveKind::TarZst => unpack_tar_zst(file, target_dir),
        SidecarArchiveKind::TarGz => unpack_tar_gz(file, target_dir),
        SidecarArchiveKind::Zip => unpack_zip(file, target_dir),
    }
}

fn unpack_tar_zst<R: Read>(reader: R, target_dir: &Path) -> Result<()> {
    let decoder = zstd::stream::read::Decoder::new(reader)
        .into_diagnostic()
        .wrap_err("read sidecar tar.zst archive")?;
    unpack_tar(decoder, target_dir)
}

fn unpack_tar_gz<R: Read>(reader: R, target_dir: &Path) -> Result<()> {
    let decoder = GzDecoder::new(reader);
    unpack_tar(decoder, target_dir)
}

fn unpack_tar<R: std::io::Read>(reader: R, target_dir: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive
        .entries()
        .into_diagnostic()
        .wrap_err("read sidecar tar archive entries")?
    {
        let mut entry = entry.into_diagnostic().wrap_err("read sidecar tar entry")?;
        let path = entry
            .path()
            .into_diagnostic()
            .wrap_err("read sidecar tar entry path")?
            .into_owned();
        ensure_safe_relative_archive_path(&path)?;
        let out_path = target_dir.join(path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .into_diagnostic()
                .wrap_err_with(|| format!("create sidecar tar parent {}", parent.display()))?;
        }
        entry
            .unpack(out_path)
            .into_diagnostic()
            .wrap_err("unpack sidecar tar entry")?;
    }
    Ok(())
}

fn unpack_zip<R: Read + Seek>(reader: R, target_dir: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(reader)
        .into_diagnostic()
        .wrap_err("read sidecar zip archive")?;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .into_diagnostic()
            .wrap_err("read sidecar zip entry")?;
        let Some(path) = file.enclosed_name() else {
            return Err(miette!(
                "sidecar zip entry '{}' is not a safe relative path",
                file.name()
            ));
        };
        ensure_safe_relative_archive_path(&path)?;
        let out_path = target_dir.join(path);
        if file.is_dir() {
            fs::create_dir_all(&out_path)
                .into_diagnostic()
                .wrap_err_with(|| format!("create sidecar zip dir {}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .into_diagnostic()
                .wrap_err_with(|| format!("create sidecar zip parent {}", parent.display()))?;
        }
        let mut out = fs::File::create(&out_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("create sidecar zip file {}", out_path.display()))?;
        std::io::copy(&mut file, &mut out)
            .into_diagnostic()
            .wrap_err_with(|| format!("write sidecar zip file {}", out_path.display()))?;
        #[cfg(unix)]
        if let Some(mode) = file.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out_path, fs::Permissions::from_mode(mode))
                .into_diagnostic()
                .wrap_err_with(|| format!("set sidecar zip mode {}", out_path.display()))?;
        }
    }
    Ok(())
}

fn ensure_safe_relative_archive_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(miette!(
            "sidecar archive entry '{}' is not relative",
            path.display()
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(miette!(
                    "sidecar archive entry '{}' escapes install directory",
                    path.display()
                ));
            }
        }
    }
    Ok(())
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

    /// Start the daemon: prepare sidecar → configure profile → start → wait.
    pub async fn start(&self) -> Result<()> {
        tracing::info!(
            "[hindsight:managed] starting daemon from managed sidecar (profile={}, port={})",
            self.config.profile,
            self.config.port,
        );
        let sidecar = HindsightSidecar::ensure_installed().await?;
        self.configure_profile(&sidecar).await?;
        if let Err(err) = self.start_daemon(&sidecar).await {
            tracing::warn!(
                "[hindsight:managed] daemon start failed; attempting best-effort stop: {err:?}"
            );
            let _ = self
                .stop_with_sidecar(&sidecar, "daemon.stop_after_start_failure")
                .await;
            return Err(err);
        }
        if let Err(err) = self.wait_for_ready().await {
            tracing::warn!(
                "[hindsight:managed] daemon did not become ready; attempting best-effort stop: {err:?}"
            );
            let _ = self
                .stop_with_sidecar(&sidecar, "daemon.stop_after_ready_timeout")
                .await;
            return Err(err);
        }
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
        let sidecar = HindsightSidecar::ensure_installed().await?;
        self.stop_with_sidecar(&sidecar, "daemon.stop").await
    }

    /// Stop the daemon even when its health endpoint is unhealthy or wedged.
    pub async fn force_stop(&self) -> Result<()> {
        tracing::info!(
            "[hindsight:managed] force stopping daemon (profile={})",
            self.config.profile,
        );
        let sidecar = HindsightSidecar::ensure_installed().await?;
        self.stop_with_sidecar(&sidecar, "daemon.force_stop").await
    }

    pub async fn force_restart(&self) -> Result<()> {
        if let Err(err) = self.force_stop().await {
            tracing::warn!("[hindsight:managed] force stop failed before restart: {err:?}");
        }
        self.start().await
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

    /// `["daemon", "--profile", "<name>"]`
    fn daemon_profile_args(&self) -> Vec<String> {
        vec![
            "daemon".to_string(),
            "--profile".to_string(),
            self.config.profile.clone(),
        ]
    }

    async fn configure_profile(&self, sidecar: &HindsightSidecar) -> Result<()> {
        tracing::info!(
            sidecar_root = %sidecar.root.display(),
            "[hindsight:managed] configuring profile '{}'",
            self.config.profile
        );
        self.delete_profile_if_exists(sidecar).await?;
        let mut cmd = sidecar.command();
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

    async fn delete_profile_if_exists(&self, sidecar: &HindsightSidecar) -> Result<()> {
        let mut cmd = sidecar.command();
        cmd.args(["profile", "delete", &self.config.profile]);
        match self.run_command(cmd, "profile.delete").await {
            Ok(()) => Ok(()),
            Err(err) if err.to_string().contains("does not exist") => Ok(()),
            Err(err) => Err(err),
        }
    }

    async fn start_daemon(&self, sidecar: &HindsightSidecar) -> Result<()> {
        tracing::info!("[hindsight:managed] starting daemon process");
        let mut cmd = sidecar.command();
        cmd.args(self.daemon_profile_args()).arg("start");
        for (k, v) in self.daemon_env_vars() {
            cmd.env(k, v);
        }
        self.run_command_with_timeout(cmd, "daemon.start", DAEMON_START_TIMEOUT_SECS)
            .await
    }

    async fn stop_with_sidecar(&self, sidecar: &HindsightSidecar, label: &str) -> Result<()> {
        let mut cmd = sidecar.command();
        cmd.args(self.daemon_profile_args()).arg("stop");
        self.run_command_with_timeout(cmd, label, DAEMON_STOP_TIMEOUT_SECS)
            .await
    }

    fn profile_env_vars(&self) -> Vec<(String, String)> {
        let mut vars = Vec::new();
        // Use a daat-locus-specific pg0 instance so the database does not
        // collide with other apps that also use Hindsight.
        vars.push((
            "HINDSIGHT_API_DATABASE_URL".into(),
            "pg0://daat-locus".into(),
        ));
        // macOS: local embedding/reranker models crash without CPU-only mode
        // (Metal/Accelerate incompatibility with Hindsight's bundled ONNX runtime).
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
        cmd.kill_on_drop(true);
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
}

fn truncate_error(text: &str) -> String {
    const MAX_CHARS: usize = 2_000;
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        trimmed.to_string()
    } else {
        format!("{}…", trimmed.chars().take(MAX_CHARS).collect::<String>())
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
    fn truncate_error_handles_multibyte_characters() {
        let text = format!("{}{}", "a".repeat(1_999), "─error");
        let truncated = truncate_error(&text);
        assert!(truncated.ends_with('…'));
        assert!(truncated.contains('─'));
    }

    #[test]
    fn archive_paths_must_not_escape_install_dir() {
        ensure_safe_relative_archive_path(Path::new("bin/hindsight-embed"))
            .expect("normal relative path");
        ensure_safe_relative_archive_path(Path::new("../escape"))
            .expect_err("parent path should be rejected");
        ensure_safe_relative_archive_path(Path::new("/absolute"))
            .expect_err("absolute path should be rejected");
    }

    #[test]
    fn selects_download_manifest_entry_for_target() {
        let manifest = r#"
schema_version = 1
release = "hindsight-sidecars-v0.5.5-1"
hindsight_version = "0.5.5"

[[sidecar]]
target = "x86_64-unknown-linux-gnu"
archive = "x86_64-unknown-linux-gnu.tar.zst"
archive_kind = "tar.zst"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
entry = "bin/hindsight-embed"
url = "https://github.com/shadow3aaa/DaatLocus/releases/download/hindsight-sidecars-v0.5.5-1/x86_64-unknown-linux-gnu.tar.zst"
"#;

        let entry = select_sidecar_download_entry(manifest, "x86_64-unknown-linux-gnu")
            .expect("manifest entry");

        assert_eq!(entry.archive_kind, "tar.zst");
        assert_eq!(entry.entry, "bin/hindsight-embed");
        validate_sha256_hex(&entry.sha256).expect("valid sha256");
    }

    #[test]
    fn rejects_duplicate_download_manifest_entries() {
        let manifest = r#"
[[sidecar]]
target = "x86_64-unknown-linux-gnu"
archive = "one.tar.zst"
archive_kind = "tar.zst"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
entry = "bin/hindsight-embed"
url = "https://example.invalid/one.tar.zst"

[[sidecar]]
target = "x86_64-unknown-linux-gnu"
archive = "two.tar.zst"
archive_kind = "tar.zst"
sha256 = "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
entry = "bin/hindsight-embed"
url = "https://example.invalid/two.tar.zst"
"#;

        let err = select_sidecar_download_entry(manifest, "x86_64-unknown-linux-gnu")
            .expect_err("duplicate target should fail");

        assert!(err.to_string().contains("duplicate entries"));
    }

    #[test]
    fn tar_gz_sidecar_archive_extracts_executable_layout() {
        let mut archive_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let content = b"#!/bin/sh\n";
            let mut header = tar::Header::new_gnu();
            header
                .set_path(format!("bin/{HINDSIGHT_EMBED_EXE}"))
                .unwrap();
            header.set_size(content.len() as u64);
            header.set_cksum();
            builder.append(&header, &content[..]).unwrap();
            builder.finish().unwrap();
        }

        let tempdir = tempfile::tempdir().expect("tempdir");
        let archive_path = tempdir.path().join("sidecar.tar.gz");
        let extract_dir = tempdir.path().join("extract");
        std::fs::write(&archive_path, archive_bytes).expect("write archive");
        std::fs::create_dir_all(&extract_dir).expect("create extract dir");
        unpack_sidecar_archive_from_file(&archive_path, SidecarArchiveKind::TarGz, &extract_dir)
            .expect("extract sidecar");
        assert!(extract_dir.join("bin").join(HINDSIGHT_EMBED_EXE).is_file());
    }

    #[test]
    fn tar_zst_sidecar_archive_extracts_executable_layout() {
        let mut archive_bytes = Vec::new();
        {
            let encoder =
                zstd::stream::write::Encoder::new(&mut archive_bytes, 19).expect("zstd encoder");
            let mut builder = tar::Builder::new(encoder);
            let content = b"#!/bin/sh\n";
            let mut header = tar::Header::new_gnu();
            header
                .set_path(format!("bin/{HINDSIGHT_EMBED_EXE}"))
                .unwrap();
            header.set_size(content.len() as u64);
            header.set_cksum();
            builder.append(&header, &content[..]).unwrap();
            let encoder = builder.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let tempdir = tempfile::tempdir().expect("tempdir");
        let archive_path = tempdir.path().join("sidecar.tar.zst");
        let extract_dir = tempdir.path().join("extract");
        std::fs::write(&archive_path, archive_bytes).expect("write archive");
        std::fs::create_dir_all(&extract_dir).expect("create extract dir");
        unpack_sidecar_archive_from_file(&archive_path, SidecarArchiveKind::TarZst, &extract_dir)
            .expect("extract sidecar");
        assert!(extract_dir.join("bin").join(HINDSIGHT_EMBED_EXE).is_file());
    }
}
