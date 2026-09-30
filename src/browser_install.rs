use std::io::Cursor;

use crate::{
    daat_locus_paths::daat_locus_paths,
    persistence::{PersistenceFileMode, write_bytes_atomic},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use md5::{Digest as _, Md5};
use miette::{Result, miette};
use reqwest::header::{CONTENT_LENGTH, ETAG, HeaderMap};
use serde::{Deserialize, Serialize};

const BROWSER_RUNTIME_METADATA_FILE: &str = "INSTALL.json";

#[derive(Debug, Deserialize)]
struct ChromeForTestingManifest {
    channels: std::collections::BTreeMap<String, ChromeForTestingChannel>,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingChannel {
    version: String,
    revision: String,
    downloads: ChromeForTestingDownloads,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingDownloads {
    chrome: Vec<ChromeForTestingDownload>,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingDownload {
    platform: String,
    url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BrowserArchiveIntegrity {
    content_length: Option<u64>,
    md5_base64: Option<String>,
    etag: Option<String>,
}

#[derive(Debug, Serialize)]
struct BrowserRuntimeInstallMetadata {
    version: String,
    revision: String,
    platform: String,
    url: String,
    archive_bytes: usize,
    archive_md5_base64: String,
    etag: Option<String>,
}

fn browser_runtime_platform() -> Result<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("mac-arm64")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("mac-x64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("linux64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("win64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86")) {
        Ok("win32")
    } else {
        Err(miette!("unsupported browser runtime platform"))
    }
}

/// Download and install the browser runtime when it is missing. Called automatically during daemon startup.
pub async fn maybe_setup_browser_runtime() {
    let paths = daat_locus_paths().await;
    if paths.browser_executable_path().exists() {
        return;
    }
    tracing::info!("[browser-runtime] executable not found, starting auto-install...");
    if let Err(err) = run_browser_runtime_setup().await {
        tracing::warn!("[browser-runtime] auto-install failed: {err:?}");
    }
}

async fn run_browser_runtime_setup() -> Result<()> {
    const MANIFEST_URL: &str = "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

    let platform = browser_runtime_platform()?;
    let paths = daat_locus_paths().await;
    let runtime_dir = paths.browser_runtime_dir();
    let executable_path = paths.browser_executable_path();

    tracing::info!(
        "[browser-runtime] downloading for platform `{platform}` into {}",
        runtime_dir.display()
    );

    let manifest = reqwest::get(MANIFEST_URL)
        .await
        .map_err(|err| miette!("failed to fetch Chrome for Testing manifest: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("failed to fetch Chrome for Testing manifest: {err}"))?
        .json::<ChromeForTestingManifest>()
        .await
        .map_err(|err| miette!("failed to decode Chrome for Testing manifest: {err}"))?;

    let stable = manifest
        .channels
        .get("Stable")
        .ok_or_else(|| miette!("Chrome for Testing manifest missing Stable channel"))?;
    let download = stable
        .downloads
        .chrome
        .iter()
        .find(|entry| entry.platform == platform)
        .ok_or_else(|| {
            miette!("Chrome for Testing has no chrome download for platform `{platform}`")
        })?;

    tracing::info!(
        "[browser-runtime] downloading Chrome for Testing {} from {}",
        stable.version,
        download.url
    );

    let response = reqwest::get(&download.url)
        .await
        .map_err(|err| miette!("failed to download browser runtime: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("failed to download browser runtime: {err}"))?;
    let integrity = BrowserArchiveIntegrity::from_headers(response.headers())?;
    let archive_bytes = response
        .bytes()
        .await
        .map_err(|err| miette!("failed to read browser runtime archive: {err}"))?;
    verify_browser_archive_integrity(&archive_bytes, &integrity)?;
    let archive_byte_len = archive_bytes.len();
    let archive_md5_base64 = browser_archive_md5_base64(&archive_bytes);
    let archive_bytes_for_extract = archive_bytes.to_vec();

    let runtime_dir_for_extract = runtime_dir.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if runtime_dir_for_extract.exists() {
            std::fs::remove_dir_all(&runtime_dir_for_extract).map_err(|err| {
                miette!(
                    "failed to clear existing browser runtime {}: {err}",
                    runtime_dir_for_extract.display()
                )
            })?;
        }
        std::fs::create_dir_all(&runtime_dir_for_extract).map_err(|err| {
            miette!(
                "failed to create browser runtime dir {}: {err}",
                runtime_dir_for_extract.display()
            )
        })?;

        let reader = Cursor::new(archive_bytes_for_extract);
        let mut archive = zip::ZipArchive::new(reader)
            .map_err(|err| miette!("failed to open browser runtime archive: {err}"))?;

        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .map_err(|err| miette!("failed to read browser runtime archive entry: {err}"))?;
            let enclosed = file
                .enclosed_name()
                .ok_or_else(|| miette!("browser runtime archive contained unsafe path"))?
                .clone();
            let destination = runtime_dir_for_extract.join(enclosed);
            if file.name().ends_with('/') {
                std::fs::create_dir_all(&destination).map_err(|err| {
                    miette!(
                        "failed to create extracted dir {}: {err}",
                        destination.display()
                    )
                })?;
                continue;
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    miette!(
                        "failed to create extracted parent dir {}: {err}",
                        parent.display()
                    )
                })?;
            }
            let mut output = std::fs::File::create(&destination).map_err(|err| {
                miette!(
                    "failed to create extracted file {}: {err}",
                    destination.display()
                )
            })?;
            std::io::copy(&mut file, &mut output).map_err(|err| {
                miette!(
                    "failed to extract browser runtime file {}: {err}",
                    destination.display()
                )
            })?;

            #[cfg(unix)]
            if let Some(mode) = file.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode));
            }
        }

        Ok(())
    })
    .await
    .map_err(|err| miette!("browser runtime extraction task failed: {err}"))??;

    if !executable_path.exists() {
        return Err(miette!(
            "browser runtime installed but executable not found at {}",
            executable_path.display()
        ));
    }

    let version_file = runtime_dir.join("VERSION");
    write_bytes_atomic(
        version_file,
        format!("{}\n", stable.version).into_bytes(),
        PersistenceFileMode::Default,
    )
    .await
    .map_err(|err| miette!("failed to write browser runtime version file: {err}"))?;
    let metadata = BrowserRuntimeInstallMetadata {
        version: stable.version.clone(),
        revision: stable.revision.clone(),
        platform: platform.to_string(),
        url: download.url.clone(),
        archive_bytes: archive_byte_len,
        archive_md5_base64,
        etag: integrity.etag,
    };
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|err| miette!("failed to serialize browser runtime metadata: {err}"))?;
    write_bytes_atomic(
        runtime_dir.join(BROWSER_RUNTIME_METADATA_FILE),
        metadata_bytes,
        PersistenceFileMode::Default,
    )
    .await
    .map_err(|err| miette!("failed to write browser runtime metadata file: {err}"))?;

    tracing::info!(
        "[browser-runtime] installed successfully (version={} revision={} executable={})",
        stable.version,
        stable.revision,
        executable_path.display()
    );
    Ok(())
}

impl BrowserArchiveIntegrity {
    fn from_headers(headers: &HeaderMap) -> Result<Self> {
        let content_length = headers
            .get(CONTENT_LENGTH)
            .map(|value| {
                value
                    .to_str()
                    .map_err(|err| miette!("browser runtime content-length is invalid: {err}"))?
                    .parse::<u64>()
                    .map_err(|err| miette!("browser runtime content-length is invalid: {err}"))
            })
            .transpose()?;
        let content_length = content_length.ok_or_else(|| {
            miette!("browser runtime download response did not include content-length")
        })?;

        let mut integrity = Self {
            content_length: Some(content_length),
            md5_base64: None,
            etag: headers
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_etag),
        };

        for value in headers.get_all("x-goog-hash") {
            let Ok(value) = value.to_str() else {
                continue;
            };
            if let Some(hash) = goog_hash_value(value, "md5") {
                integrity.md5_base64 = Some(hash);
            }
        }

        if integrity.md5_base64.is_none() {
            return Err(miette!(
                "browser runtime download response did not include x-goog-hash md5"
            ));
        }
        Ok(integrity)
    }
}

/// Parse an HTTP entity-tag: optional `W/`, then one quoted-string.
///
/// Backslash-escaped quotes inside the quotes are unescaped. Values that are
/// not a single quoted-string (including ones that only look trimmed of
/// surrounding quotes) are rejected.
fn parse_etag(raw: &str) -> Option<String> {
    let rest = raw.trim().strip_prefix("W/").unwrap_or(raw.trim());
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.push(chars.next()?);
        } else if ch == '"' {
            return None;
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Select `wanted` from a comma-separated list of `name=value` pairs.
///
/// Matching is on the trimmed key, not a substring of another key.
fn goog_hash_value(header: &str, wanted: &str) -> Option<String> {
    let mut found = None;
    for part in header.split(',') {
        let Some((name, hash)) = part.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(wanted) {
            let hash = hash.trim();
            if !hash.is_empty() {
                found = Some(hash.to_string());
            }
        }
    }
    found
}

fn verify_browser_archive_integrity(
    archive_bytes: &[u8],
    integrity: &BrowserArchiveIntegrity,
) -> Result<()> {
    if let Some(content_length) = integrity.content_length
        && archive_bytes.len() as u64 != content_length
    {
        return Err(miette!(
            "browser runtime archive length mismatch: expected {content_length}, got {}",
            archive_bytes.len()
        ));
    }

    let expected_md5 = integrity
        .md5_base64
        .as_deref()
        .ok_or_else(|| miette!("browser runtime archive missing expected md5"))?;
    let actual_md5 = browser_archive_md5_base64(archive_bytes);
    if actual_md5 != expected_md5 {
        return Err(miette!(
            "browser runtime archive md5 mismatch: expected {expected_md5}, got {actual_md5}"
        ));
    }

    Ok(())
}

fn browser_archive_md5_base64(archive_bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(archive_bytes);
    BASE64_STANDARD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    #[test]
    fn parses_gcs_integrity_headers_and_verifies_archive() {
        let archive = b"browser";
        let md5 = browser_archive_md5_base64(archive);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("7"));
        headers.insert(ETAG, HeaderValue::from_static("\"etag-value\""));
        headers.append(
            "x-goog-hash",
            HeaderValue::from_str(&format!("crc32c=AAAAAA==,md5={md5}")).expect("hash header"),
        );

        let integrity =
            BrowserArchiveIntegrity::from_headers(&headers).expect("parse integrity headers");

        assert_eq!(integrity.content_length, Some(7));
        assert_eq!(integrity.md5_base64.as_deref(), Some(md5.as_str()));
        assert_eq!(integrity.etag.as_deref(), Some("etag-value"));
        verify_browser_archive_integrity(archive, &integrity).expect("verify archive");
    }

    #[test]
    fn rejects_browser_archive_length_mismatch() {
        let archive = b"browser";
        let integrity = BrowserArchiveIntegrity {
            content_length: Some(8),
            md5_base64: Some(browser_archive_md5_base64(archive)),
            etag: None,
        };

        let err = verify_browser_archive_integrity(archive, &integrity)
            .expect_err("length mismatch should fail");

        assert!(err.to_string().contains("length mismatch"));
    }

    #[test]
    fn rejects_browser_archive_md5_mismatch() {
        let integrity = BrowserArchiveIntegrity {
            content_length: Some(7),
            md5_base64: Some(browser_archive_md5_base64(b"tampered")),
            etag: None,
        };

        let err = verify_browser_archive_integrity(b"browser", &integrity)
            .expect_err("md5 mismatch should fail");

        assert!(err.to_string().contains("md5 mismatch"));
    }

    #[test]
    fn requires_browser_archive_md5_header() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("7"));

        let err = BrowserArchiveIntegrity::from_headers(&headers)
            .expect_err("missing md5 header should fail");

        assert!(err.to_string().contains("x-goog-hash md5"));
    }

    #[test]
    fn etag_parses_one_quoted_string_and_rejects_loose_quotes() {
        assert_eq!(parse_etag("\"etag-value\"").as_deref(), Some("etag-value"));
        assert_eq!(
            parse_etag("W/\"weak-tag\"").as_deref(),
            Some("weak-tag")
        );
        assert_eq!(
            parse_etag("\"quote\\\"inside\"").as_deref(),
            Some("quote\"inside")
        );
        assert_eq!(parse_etag("etag-value"), None);
        assert_eq!(parse_etag("\"a\"\"b\""), None);
        assert_eq!(parse_etag("\"only-leading"), None);
        assert_eq!(parse_etag(""), None);
    }

    #[test]
    fn goog_hash_selects_md5_by_key_not_substring() {
        assert_eq!(
            goog_hash_value("crc32c=AAAAAA==,md5=abc123==", "md5").as_deref(),
            Some("abc123==")
        );
        assert_eq!(
            goog_hash_value("crc32c=AAAAAA==, md5 = abc123== ", "md5").as_deref(),
            Some("abc123==")
        );
        assert_eq!(goog_hash_value("notmd5=abc123==", "md5"), None);
        assert_eq!(goog_hash_value("md5-prefix=abc123==", "md5"), None);
        assert_eq!(goog_hash_value("md5abc=abc123==", "md5"), None);
        assert_eq!(goog_hash_value("crc32c=AAAAAA==", "md5"), None);
        assert_eq!(goog_hash_value("not,a=pair", "md5"), None);
        assert_eq!(
            goog_hash_value("garbage,md5=abc123==", "md5").as_deref(),
            Some("abc123==")
        );
    }
}
