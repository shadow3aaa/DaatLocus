use std::io::Cursor;

use crate::daat_locus_paths::daat_locus_paths;
use miette::{Result, miette};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ChromeForTestingManifest {
    channels: std::collections::BTreeMap<String, ChromeForTestingChannel>,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingChannel {
    version: String,
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

fn browser_runtime_platform() -> Result<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok("mac-arm64");
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Ok("mac-x64");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok("linux64");
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok("win64");
    }
    #[cfg(all(target_os = "windows", target_arch = "x86"))]
    {
        return Ok("win32");
    }

    #[allow(unreachable_code)]
    Err(miette!("unsupported browser runtime platform"))
}

/// Download and install the browser runtime when it is missing. Called automatically during daemon startup.
pub(crate) async fn maybe_setup_browser_runtime() {
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

    let archive_bytes = reqwest::get(&download.url)
        .await
        .map_err(|err| miette!("failed to download browser runtime: {err}"))?
        .error_for_status()
        .map_err(|err| miette!("failed to download browser runtime: {err}"))?
        .bytes()
        .await
        .map_err(|err| miette!("failed to read browser runtime archive: {err}"))?;

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

        let reader = Cursor::new(archive_bytes.to_vec());
        let mut archive = zip::ZipArchive::new(reader)
            .map_err(|err| miette!("failed to open browser runtime archive: {err}"))?;

        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .map_err(|err| miette!("failed to read browser runtime archive entry: {err}"))?;
            let enclosed = file
                .enclosed_name()
                .ok_or_else(|| miette!("browser runtime archive contained unsafe path"))?
                .to_path_buf();
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
    tokio::fs::write(&version_file, format!("{}\n", stable.version))
        .await
        .map_err(|err| miette!("failed to write browser runtime version file: {err}"))?;

    tracing::info!(
        "[browser-runtime] installed successfully (version={} executable={})",
        stable.version,
        executable_path.display()
    );
    Ok(())
}
