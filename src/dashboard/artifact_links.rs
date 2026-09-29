//! Helpers that turn agent-presented artifact URIs into openable / displayable
//! targets.
//!
//! File-backed artifacts arrive as self-authorizing daemon-relative URIs such as
//! `/artifacts/<sha>.png?sig=...`.  The TUI derives the daemon base URL the same
//! way the CLI help text does (config file `[daemon] port`, falling back to the
//! default), because the dashboard is only ever wired to a local daemon.

use std::path::Path;

use super::cells::{ArtifactActivityData, ArtifactKind};

/// Derive the local daemon base URL, e.g. `http://localhost:53825`.
pub(super) fn daemon_base_url() -> String {
    format!(
        "http://{}:{}",
        crate::daemon::DAEMON_CLIENT_HOST,
        configured_daemon_port()
    )
}

/// Build an absolute URL for `uri`, resolving daemon-relative paths against
/// `base_url`.  Absolute `http(s)` URIs are returned unchanged; anything else
/// (for example a bare relative path) yields `None`.
pub(super) fn absolute_artifact_uri(uri: &str, base_url: &str) -> Option<String> {
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return Some(uri.to_string());
    }
    if let Some(relative) = uri.strip_prefix('/') {
        let base = base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return None;
        }
        return Some(format!("{base}/{relative}"));
    }
    None
}

/// URI text to show on the artifact card.  Daemon-relative URIs are expanded so
/// the URL can be rendered as an OSC 8 hyperlink.
pub(super) fn artifact_display_uri(cell: &ArtifactActivityData, base_url: &str) -> String {
    absolute_artifact_uri(&cell.uri, base_url).unwrap_or_else(|| cell.uri.trim().to_string())
}

/// Resolve what should be opened for `cell`.
///
/// Image / SVG artifacts open through the system viewer using their absolute
/// local path; HTML and URL artifacts open in the browser via the daemon URL.
pub(super) fn artifact_open_target(cell: &ArtifactActivityData, base_url: &str) -> Option<String> {
    match cell.kind {
        ArtifactKind::Image | ArtifactKind::Svg => cell
            .local_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(ToString::to_string)
            .or_else(|| absolute_artifact_uri(&cell.uri, base_url)),
        ArtifactKind::Html | ArtifactKind::Url => absolute_artifact_uri(&cell.uri, base_url),
    }
}

fn configured_daemon_port() -> u16 {
    let default_port = crate::config::DaemonConfig::default().port;
    configured_daemon_port_from_path(&daat_locus_config_path()).unwrap_or(default_port)
}

fn daat_locus_config_path() -> std::path::PathBuf {
    crate::daat_locus_paths::daat_locus_paths_sync().config_file("config.toml")
}

fn configured_daemon_port_from_path(config_path: &Path) -> Option<u16> {
    let content = std::fs::read_to_string(config_path).ok()?;
    let value = toml::from_str::<toml::Value>(&content).ok()?;
    value
        .get("daemon")
        .and_then(toml::Value::as_table)
        .and_then(|daemon| daemon.get("port"))
        .and_then(toml::Value::as_integer)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://localhost:53825";

    fn cell(kind: ArtifactKind, uri: &str) -> ArtifactActivityData {
        ArtifactActivityData {
            artifact_id: "artifact-1".to_string(),
            version: 1,
            kind,
            title: "Demo".to_string(),
            uri: uri.to_string(),
            mime_type: "application/octet-stream".to_string(),
            local_path: None,
            byte_len: None,
            description: None,
        }
    }

    #[test]
    fn absolute_artifact_uri_resolves_relative_uri_against_base() {
        assert_eq!(
            absolute_artifact_uri("/artifacts/ab.png?sig=1", BASE).as_deref(),
            Some("http://localhost:53825/artifacts/ab.png?sig=1")
        );
        assert_eq!(
            absolute_artifact_uri("/artifacts/ab.png?sig=1", "http://localhost:53825/").as_deref(),
            Some("http://localhost:53825/artifacts/ab.png?sig=1")
        );
    }

    #[test]
    fn absolute_artifact_uri_keeps_absolute_uris_and_rejects_junk() {
        assert_eq!(
            absolute_artifact_uri("https://example.com/x.svg", BASE).as_deref(),
            Some("https://example.com/x.svg")
        );
        assert_eq!(
            absolute_artifact_uri("http://127.0.0.1:8080/index.html", BASE).as_deref(),
            Some("http://127.0.0.1:8080/index.html")
        );
        assert_eq!(absolute_artifact_uri("relative/nope", BASE), None);
        assert_eq!(absolute_artifact_uri("   ", BASE), None);
        assert_eq!(absolute_artifact_uri("/x", "  "), None);
    }

    #[test]
    fn open_target_prefers_local_path_for_images() {
        let mut image = cell(ArtifactKind::Image, "/artifacts/ab.png?sig=1");
        image.local_path = Some("/tmp/ab.png".to_string());
        assert_eq!(
            artifact_open_target(&image, BASE).as_deref(),
            Some("/tmp/ab.png")
        );

        // Without a local path the daemon URL is used instead.
        let remote = cell(ArtifactKind::Svg, "/artifacts/ab.svg?sig=1");
        assert_eq!(
            artifact_open_target(&remote, BASE).as_deref(),
            Some("http://localhost:53825/artifacts/ab.svg?sig=1")
        );
    }

    #[test]
    fn open_target_uses_browser_url_for_html_and_url_kinds() {
        let html = cell(ArtifactKind::Html, "/artifacts/page.html?sig=1");
        assert_eq!(
            artifact_open_target(&html, BASE).as_deref(),
            Some("http://localhost:53825/artifacts/page.html?sig=1")
        );
        let url = cell(ArtifactKind::Url, "http://127.0.0.1:5173/");
        assert_eq!(
            artifact_open_target(&url, BASE).as_deref(),
            Some("http://127.0.0.1:5173/")
        );
    }

    #[test]
    fn display_uri_expands_relative_uri() {
        let html = cell(ArtifactKind::Html, "/artifacts/page.html?sig=1");
        assert_eq!(
            artifact_display_uri(&html, BASE),
            "http://localhost:53825/artifacts/page.html?sig=1"
        );
    }
}
