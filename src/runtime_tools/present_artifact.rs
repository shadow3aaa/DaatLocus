//! Runtime tool that presents visual content (image, SVG, HTML, or URL) to the
//! user and surfaces it in the WebUI/TUI activity feed.
//!
//! File-backed artifacts are content-addressed under
//! `<artifacts_dir>/presented/<sha256hex>.<ext>` and exposed to the user through
//! a signed daemon-relative URI. The daemon can serve them (see
//! `src/daemon/mod.rs`) by reusing the free functions exported here.

use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    activity_event::ToolCallActivityEvent,
    context::Context,
    daat_locus_paths::daat_locus_paths_sync,
    dashboard::{
        SessionActivityEvent,
        cells::{ArtifactActivityData, ArtifactKind},
    },
    persistence::{PersistenceFileMode, write_bytes_atomic_sync, write_json_pretty_atomic_sync},
    reasoning::{
        episode::EpisodeActionRecord,
        runtime::{AgentContentPart, AgentToolCall},
    },
    runtime_tools::{
        RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    },
    sandbox::RuntimeSandboxPolicy,
    schema_utils::model_schema_for,
};

/// Directory (under the artifacts root) that holds presented artifacts.
pub(crate) const ARTIFACT_PRESENTED_DIR_NAME: &str = "presented";

const ARTIFACT_SIGNING_SECRET_FILE_NAME: &str = "artifact_signing_secret";
const ARTIFACT_SIGNING_SECRET_LEN: usize = 32;
const ARTIFACT_MANIFEST_FILE_NAME: &str = "manifest.json";
const MAX_ARTIFACT_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ARTIFACT_CONTENT_BYTES: usize = 2 * 1024 * 1024;
const HMAC_BLOCK_SIZE: usize = 64;
const HMAC_SHA256_HEX_LEN: usize = 64;

const ARTIFACT_FILE_EXTENSIONS: &[&str] =
    &["png", "jpeg", "jpg", "gif", "webp", "svg", "html", "htm"];

#[model_schema]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ArtifactKindArg {
    Image,
    Svg,
    Html,
    Url,
}

impl ArtifactKindArg {
    const fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Svg => "svg",
            Self::Html => "html",
            Self::Url => "url",
        }
    }
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PresentArtifactArgs {
    /// Local filesystem path of the file to present. Exactly one of `path`, `content`, or `url` is required. Relative paths resolve from the current execution directory.
    #[serde(default)]
    path: Option<String>,
    /// Inline text content (SVG or HTML only). Exactly one of `path`, `content`, or `url` is required.
    #[serde(default)]
    content: Option<String>,
    /// Target http/https URL to present (for example a local dev server). Exactly one of `path`, `content`, or `url` is required.
    #[serde(default)]
    url: Option<String>,
    /// Artifact kind. Inferred from the source when omitted.
    #[serde(default)]
    kind: Option<ArtifactKindArg>,
    /// Human-readable title. Defaults to the file name or the URL host.
    #[serde(default)]
    title: Option<String>,
    /// Stable logical id. Re-presenting the same id creates a new version.
    #[serde(default)]
    artifact_id: Option<String>,
    /// Optional description shown alongside the artifact.
    #[serde(default)]
    description: Option<String>,
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![Box::new(StaticRuntimeTool::new_with_schema(
        "present_artifact",
        "Present visual content to the user and surface it in the interface. Supports local image files (PNG/JPEG/GIF/WebP), SVG and HTML files or inline text, and http/https URLs. Re-presenting the same `artifact_id` increments the version. Use this whenever the user should see an image, diagram, page, or link.",
        model_schema_for::<PresentArtifactArgs>(),
        summarize_present_artifact_tool,
        render_present_artifact_call_ui,
        execute_present_artifact_runtime_tool,
    ))]
}

fn summarize_present_artifact_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: PresentArtifactArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "present_artifact".to_string(),
        summary: present_artifact_summary(&args),
    })
}

fn present_artifact_summary(args: &PresentArtifactArgs) -> String {
    if let Some(path) = args.path.as_deref() {
        format!("path={path}")
    } else if let Some(url) = args.url.as_deref() {
        format!("url={url}")
    } else if args.content.is_some() {
        "source=inline".to_string()
    } else {
        "source=unset".to_string()
    }
}

fn render_present_artifact_call_ui(call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
    let args: PresentArtifactArgs = parse_tool_args(call)?;
    let mut lines = vec![present_artifact_summary(&args)];
    if let Some(kind) = args.kind {
        lines.push(format!("kind={}", kind.label()));
    }
    if let Some(title) = args.title.as_deref() {
        lines.push(format!("title={title}"));
    }
    if let Some(artifact_id) = args.artifact_id.as_deref() {
        lines.push(format!("artifact_id={artifact_id}"));
    }
    Ok(ToolCallActivityEvent::app("Present Artifact", lines))
}

fn execute_present_artifact_runtime_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: PresentArtifactArgs = parse_tool_args(call)?;
        validate_artifact_args(&args)?;
        let supports_vision = active_model_supports_vision(context);
        let source = prepare_source_from_args(
            &args,
            Some(&context.execution_cwd),
            Some(&context.sandbox_policy),
        )?;
        let data = present_artifact(&args, source)?;
        Ok(build_tool_result(data, supports_vision))
    })
}

/// Worker-mode entry point. Mirrors `view_image::execute_worker_view_image`:
/// the artifact store is the shared, daemon-served `artifacts_dir`, so worker
/// presentations are reachable in exactly the same worker mode as `view_image`.
pub fn execute_worker_present_artifact(
    execution_cwd: &Path,
    sandbox_policy: &RuntimeSandboxPolicy,
    supports_vision: bool,
    call: &AgentToolCall,
) -> Result<ToolExecutionResult> {
    let args: PresentArtifactArgs = parse_tool_args(call)?;
    validate_artifact_args(&args)?;
    let source = prepare_source_from_args(&args, Some(execution_cwd), Some(sandbox_policy))?;
    let data = present_artifact(&args, source)?;
    Ok(build_tool_result(data, supports_vision))
}

fn active_model_supports_vision(context: &Context) -> bool {
    let model = context.config.main_model_config();
    model.supports_vision.unwrap_or_else(|| {
        crate::model_catalog::catalog_model_capacity(&model.model_id)
            .is_none_or(|capacity| capacity.supports_vision)
    })
}

fn validate_artifact_args(args: &PresentArtifactArgs) -> Result<()> {
    let count = [
        args.path.is_some(),
        args.content.is_some(),
        args.url.is_some(),
    ]
    .iter()
    .filter(|set| **set)
    .count();
    if count != 1 {
        return Err(miette!(
            "present_artifact requires exactly one of `path`, `content`, or `url` (received {count})"
        ));
    }
    if let Some(artifact_id) = args.artifact_id.as_deref() {
        validate_artifact_id(artifact_id)?;
    }
    Ok(())
}

fn validate_artifact_id(artifact_id: &str) -> Result<()> {
    if artifact_id.trim().is_empty() {
        return Err(miette!("`artifact_id` must not be empty"));
    }
    if artifact_id.chars().count() > 128 {
        return Err(miette!("`artifact_id` must be at most 128 characters"));
    }
    if artifact_id.chars().any(char::is_control) {
        return Err(miette!("`artifact_id` must not contain control characters"));
    }
    Ok(())
}

enum PreparedSource {
    File {
        bytes: Vec<u8>,
        kind: ArtifactKind,
        ext: &'static str,
        mime_type: &'static str,
        default_title: String,
    },
    Url {
        url: String,
        default_title: String,
    },
}

fn prepare_source_from_args(
    args: &PresentArtifactArgs,
    execution_cwd: Option<&Path>,
    sandbox_policy: Option<&RuntimeSandboxPolicy>,
) -> Result<PreparedSource> {
    if let Some(requested_path) = args.path.as_deref() {
        let requested_path = requested_path.trim();
        if requested_path.is_empty() {
            return Err(miette!("`path` must not be empty"));
        }
        let resolved = RuntimeSandboxPolicy::resolve_path(Path::new(requested_path), execution_cwd);
        let canonical = resolved.canonicalize().map_err(|err| {
            miette!(
                "unable to resolve artifact path `{}`: {err}",
                resolved.display()
            )
        })?;
        if let Some(policy) = sandbox_policy {
            policy.ensure_path_readable(&canonical, "present_artifact source")?;
        }
        let bytes = read_artifact_source_file(&canonical)?;
        let default_title = file_display_name(&canonical);
        let (kind, mime_type, ext) = classify_file_bytes(&bytes, args.kind)?;
        return Ok(PreparedSource::File {
            bytes,
            kind,
            ext,
            mime_type,
            default_title,
        });
    }

    if let Some(content) = args.content.as_deref() {
        if content.len() > MAX_ARTIFACT_CONTENT_BYTES {
            return Err(miette!(
                "inline `content` is too large: {} bytes exceeds the {} MiB limit",
                content.len(),
                MAX_ARTIFACT_CONTENT_BYTES / 1024 / 1024
            ));
        }
        let kind = infer_inline_kind(content, args.kind)?;
        let (mime_type, ext) = match kind {
            ArtifactKind::Svg => ("image/svg+xml", "svg"),
            _ => ("text/html", "html"),
        };
        return Ok(PreparedSource::File {
            bytes: content.as_bytes().to_vec(),
            kind,
            ext,
            mime_type,
            default_title: format!("inline-{ext}"),
        });
    }

    if let Some(url) = args.url.as_deref() {
        validate_artifact_url(url)?;
        if let Some(kind) = args.kind
            && kind != ArtifactKindArg::Url
        {
            return Err(miette!(
                "a `url` source requires kind=url, not kind={}",
                kind.label()
            ));
        }
        return Ok(PreparedSource::Url {
            url: url.to_string(),
            default_title: url_host(url),
        });
    }

    Err(miette!(
        "present_artifact requires exactly one of `path`, `content`, or `url`"
    ))
}

fn read_artifact_source_file(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path)
        .map_err(|err| miette!("unable to inspect artifact `{}`: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(miette!("artifact path `{}` is not a file", path.display()));
    }
    if metadata.len() > MAX_ARTIFACT_FILE_BYTES {
        return Err(miette!(
            "artifact `{}` is too large: {} bytes exceeds the {} MiB limit",
            path.display(),
            metadata.len(),
            MAX_ARTIFACT_FILE_BYTES / 1024 / 1024
        ));
    }

    let capacity = usize::try_from(metadata.len())
        .unwrap_or(0)
        .min(MAX_ARTIFACT_FILE_BYTES as usize);
    let mut bytes = Vec::with_capacity(capacity);
    fs::File::open(path)
        .map_err(|err| miette!("unable to read artifact `{}`: {err}", path.display()))?
        .take(MAX_ARTIFACT_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| miette!("unable to read artifact `{}`: {err}", path.display()))?;
    if bytes.len() as u64 > MAX_ARTIFACT_FILE_BYTES {
        return Err(miette!(
            "artifact `{}` is too large: more than {} MiB",
            path.display(),
            MAX_ARTIFACT_FILE_BYTES / 1024 / 1024
        ));
    }
    Ok(bytes)
}

fn classify_file_bytes(
    bytes: &[u8],
    requested: Option<ArtifactKindArg>,
) -> Result<(ArtifactKind, &'static str, &'static str)> {
    if let Some((mime_type, ext)) = image_media_type_for_bytes(bytes) {
        expect_requested(requested, ArtifactKindArg::Image)?;
        return Ok((ArtifactKind::Image, mime_type, ext));
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        if text_looks_like_svg(text) {
            expect_requested(requested, ArtifactKindArg::Svg)?;
            return Ok((ArtifactKind::Svg, "image/svg+xml", "svg"));
        }
        if text_looks_like_html(text) {
            expect_requested(requested, ArtifactKindArg::Html)?;
            return Ok((ArtifactKind::Html, "text/html", "html"));
        }
    }
    Err(miette!(
        "unsupported artifact source: unable to determine whether the file is a supported image (PNG/JPEG/GIF/WebP), SVG, or HTML"
    ))
}

fn expect_requested(requested: Option<ArtifactKindArg>, actual: ArtifactKindArg) -> Result<()> {
    match requested {
        Some(kind) if kind != actual => Err(miette!(
            "`kind` {} does not match the detected artifact kind {}",
            kind.label(),
            actual.label()
        )),
        _ => Ok(()),
    }
}

fn infer_inline_kind(text: &str, requested: Option<ArtifactKindArg>) -> Result<ArtifactKind> {
    match requested {
        Some(ArtifactKindArg::Svg) => Ok(ArtifactKind::Svg),
        Some(ArtifactKindArg::Html) => Ok(ArtifactKind::Html),
        Some(other) => Err(miette!(
            "inline `content` only supports svg or html artifacts, not kind={}",
            other.label()
        )),
        None => {
            if text_looks_like_svg(text) {
                Ok(ArtifactKind::Svg)
            } else {
                Ok(ArtifactKind::Html)
            }
        }
    }
}

fn image_media_type_for_bytes(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(("image/jpeg", "jpeg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if bytes.get(0..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

fn text_looks_like_svg(text: &str) -> bool {
    let without_bom = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut body = without_bom.trim_start();
    if let Some(prolog) = body.strip_prefix("<?xml") {
        body = match prolog.find("?>") {
            Some(end) => prolog[end + 2..].trim_start(),
            None => body,
        };
    }
    body.get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("<svg"))
}

fn text_looks_like_html(text: &str) -> bool {
    let without_bom = text.strip_prefix('\u{feff}').unwrap_or(text);
    let body = without_bom.trim_start();
    const HTML_PREFIXES: &[&str] = &[
        "<!doctype",
        "<html",
        "<head",
        "<body",
        "<meta",
        "<title",
        "<div",
        "<span",
        "<p",
        "<table",
        "<script",
        "<style",
        "<!--",
    ];
    HTML_PREFIXES.iter().any(|candidate| {
        body.get(..candidate.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(candidate))
    })
}

fn file_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("artifact")
        .to_string()
}

pub(crate) fn validate_artifact_url(url: &str) -> Result<()> {
    if url.trim().is_empty() {
        return Err(miette!("artifact `url` must not be empty"));
    }
    if url.trim() != url {
        return Err(miette!(
            "artifact `url` must not contain surrounding whitespace"
        ));
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| miette!("artifact `url` must start with http:// or https://"))?;
    if rest.is_empty() {
        return Err(miette!("artifact `url` is missing a host"));
    }
    if url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err(miette!(
            "artifact `url` must not contain whitespace or control characters"
        ));
    }
    Ok(())
}

fn url_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "artifact".to_string())
}

fn present_artifact(
    args: &PresentArtifactArgs,
    source: PreparedSource,
) -> Result<ArtifactActivityData> {
    let dir = daat_locus_paths_sync().artifact_dir(ARTIFACT_PRESENTED_DIR_NAME);
    let description = args.description.clone();

    match source {
        PreparedSource::Url { url, default_title } => {
            let artifact_id = resolve_artifact_id(args);
            let title = args.title.clone().unwrap_or(default_title);
            let version =
                update_artifact_manifest(&dir, &artifact_id, None, "text/uri-list", &title)?;
            Ok(ArtifactActivityData {
                artifact_id,
                version,
                kind: ArtifactKind::Url,
                title,
                uri: url,
                mime_type: "text/uri-list".to_string(),
                local_path: None,
                byte_len: None,
                description,
            })
        }
        PreparedSource::File {
            bytes,
            kind,
            ext,
            mime_type,
            default_title,
        } => {
            let artifact_id = resolve_artifact_id(args);
            let title = args.title.clone().unwrap_or(default_title);
            let (file_name, stored_path) = write_artifact_file(&dir, &bytes, ext)?;
            let version =
                update_artifact_manifest(&dir, &artifact_id, Some(&file_name), mime_type, &title)?;
            let secret = artifact_signing_secret();
            let signature = sign_artifact_file_name(&secret, &file_name);
            let uri = format!("/artifacts/{file_name}?sig={signature}");
            let local_path = stored_path
                .canonicalize()
                .unwrap_or(stored_path)
                .display()
                .to_string();
            Ok(ArtifactActivityData {
                artifact_id,
                version,
                kind,
                title,
                uri,
                mime_type: mime_type.to_string(),
                local_path: Some(local_path),
                byte_len: Some(bytes.len() as u64),
                description,
            })
        }
    }
}

fn resolve_artifact_id(args: &PresentArtifactArgs) -> String {
    args.artifact_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map_or_else(|| Uuid::new_v4().to_string(), str::to_string)
}

fn build_tool_result(data: ArtifactActivityData, supports_vision: bool) -> ToolExecutionResult {
    let kind_label = kind_label(data.kind);
    let summary = format!(
        "presented {kind_label} artifact `{}` (v{})",
        data.title, data.version
    );
    let payload = json!({
        "artifact_id": data.artifact_id,
        "version": data.version,
        "kind": kind_label,
        "uri": data.uri,
        "mime_type": data.mime_type,
        "title": data.title,
        "byte_len": data.byte_len,
        "local_path": data.local_path,
    });
    let model_content = format!(
        "Presented {kind_label} artifact `{}` as version {}. The user can now see it in the WebUI and TUI.\nuri={}",
        data.title, data.version, data.uri
    );

    let mut result = ToolExecutionResult::from_activity_event(
        summary,
        payload,
        Some(SessionActivityEvent::Artifact(data.clone())),
    )
    .with_model_content(model_content);

    if supports_vision
        && data.kind == ArtifactKind::Image
        && let Some(local_path) = data.local_path.clone()
    {
        result = result.with_model_image_part(AgentContentPart::Image {
            path: local_path,
            media_type: data.mime_type.clone(),
            description: Some(format!("presented artifact {}", data.title)),
        });
    }

    result
}

fn kind_label(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Image => "image",
        ArtifactKind::Svg => "svg",
        ArtifactKind::Html => "html",
        ArtifactKind::Url => "url",
    }
}

fn write_artifact_file(dir: &Path, bytes: &[u8], ext: &str) -> Result<(String, PathBuf)> {
    fs::create_dir_all(dir)
        .map_err(|err| miette!("unable to create artifact store `{}`: {err}", dir.display()))?;
    let digest = hex::encode(Sha256::digest(bytes));
    let file_name = format!("{digest}.{ext}");
    let path = dir.join(&file_name);
    if !path.exists() {
        write_bytes_atomic_sync(&path, bytes, PersistenceFileMode::Default)
            .map_err(|err| miette!("unable to store artifact `{}`: {err}", path.display()))?;
    }
    Ok((file_name, path))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ArtifactManifest {
    #[serde(default)]
    artifacts: BTreeMap<String, ArtifactManifestEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ArtifactManifestEntry {
    latest_version: u32,
    versions: Vec<ArtifactVersionRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ArtifactVersionRecord {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_name: Option<String>,
    mime_type: String,
    title: String,
}

fn update_artifact_manifest(
    dir: &Path,
    artifact_id: &str,
    file_name: Option<&str>,
    mime_type: &str,
    title: &str,
) -> Result<u32> {
    fs::create_dir_all(dir)
        .map_err(|err| miette!("unable to create artifact store `{}`: {err}", dir.display()))?;
    let path = dir.join(ARTIFACT_MANIFEST_FILE_NAME);
    let mut manifest = load_artifact_manifest(&path)?;
    let entry = manifest
        .artifacts
        .entry(artifact_id.to_string())
        .or_default();
    let version = entry.latest_version.saturating_add(1).max(1);
    entry.latest_version = version;
    entry.versions.push(ArtifactVersionRecord {
        version,
        file_name: file_name.map(str::to_string),
        mime_type: mime_type.to_string(),
        title: title.to_string(),
    });
    write_json_pretty_atomic_sync(&path, &manifest, PersistenceFileMode::Default).map_err(
        |err| {
            miette!(
                "unable to write artifact manifest `{}`: {err}",
                path.display()
            )
        },
    )?;
    Ok(version)
}

fn load_artifact_manifest(path: &Path) -> Result<ArtifactManifest> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|err| miette!("invalid artifact manifest `{}`: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(ArtifactManifest::default()),
        Err(err) => Err(miette!(
            "unable to read artifact manifest `{}`: {err}",
            path.display()
        )),
    }
}

/// Load the persistent HMAC secret, creating a fresh random one on first use.
///
/// The secret is never rotated implicitly so previously emitted artifact links
/// keep working across daemon restarts.
pub(crate) fn artifact_signing_secret() -> Vec<u8> {
    static CREATE_LOCK: Mutex<()> = Mutex::new(());
    let path = daat_locus_paths_sync()
        .state_dir()
        .join(ARTIFACT_SIGNING_SECRET_FILE_NAME);
    if let Some(secret) = read_signing_secret(&path) {
        return secret;
    }
    let _guard = CREATE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(secret) = read_signing_secret(&path) {
        return secret;
    }
    let secret = generate_signing_secret();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if write_bytes_atomic_sync(&path, &secret, PersistenceFileMode::Private).is_err()
        && let Some(existing) = read_signing_secret(&path)
    {
        return existing;
    }
    secret
}

fn read_signing_secret(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    (bytes.len() == ARTIFACT_SIGNING_SECRET_LEN).then_some(bytes)
}

fn generate_signing_secret() -> Vec<u8> {
    let mut secret = Vec::with_capacity(ARTIFACT_SIGNING_SECRET_LEN);
    while secret.len() < ARTIFACT_SIGNING_SECRET_LEN {
        secret.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    secret.truncate(ARTIFACT_SIGNING_SECRET_LEN);
    secret
}

/// HMAC-SHA256 over the exact artifact file name. The result is lowercase hex.
pub(crate) fn sign_artifact_file_name(secret: &[u8], file_name: &str) -> String {
    hex::encode(hmac_sha256(secret, file_name.as_bytes()))
}

/// Constant-time verification of an artifact capability signature.
pub(crate) fn verify_artifact_signature(secret: &[u8], file_name: &str, sig: &str) -> bool {
    if sig.len() != HMAC_SHA256_HEX_LEN {
        return false;
    }
    let expected = sign_artifact_file_name(secret, file_name);
    constant_time_eq(expected.as_bytes(), sig.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut normalized_key = [0u8; HMAC_BLOCK_SIZE];
    if key.len() > HMAC_BLOCK_SIZE {
        let digest = Sha256::digest(key);
        normalized_key[..32].copy_from_slice(digest.as_slice());
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; HMAC_BLOCK_SIZE];
    let mut outer_pad = [0x5cu8; HMAC_BLOCK_SIZE];
    for index in 0..HMAC_BLOCK_SIZE {
        inner_pad[index] ^= normalized_key[index];
        outer_pad[index] ^= normalized_key[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    let outer_digest = outer.finalize();

    let mut result = [0u8; 32];
    result.copy_from_slice(outer_digest.as_slice());
    result
}

/// Strict validation of a stored artifact file name: 64 lowercase hex chars, a
/// single `.`, then one of the supported extensions. No separators, no `..`.
pub(crate) fn is_valid_artifact_file_name(file_name: &str) -> bool {
    if file_name.contains(['/', '\\']) {
        return false;
    }
    let Some((stem, ext)) = file_name.rsplit_once('.') else {
        return false;
    };
    if !ARTIFACT_FILE_EXTENSIONS.contains(&ext) {
        return false;
    }
    stem.len() == 64
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX64: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn signer_secret() -> Vec<u8> {
        b"unit-test-signing-secret-key".to_vec()
    }

    #[test]
    fn hmac_sha256_matches_rfc4231_style_vector() {
        let digest = hmac_sha256(b"key", b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            hex::encode(digest),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn hmac_construction_is_stable() {
        let secret = signer_secret();
        let first = sign_artifact_file_name(&secret, &format!("{HEX64}.png"));
        let second = sign_artifact_file_name(&secret, &format!("{HEX64}.png"));
        assert_eq!(first, second);
        assert_eq!(first.len(), HMAC_SHA256_HEX_LEN);
    }

    #[test]
    fn signature_verification_rejects_tampering_and_wrong_inputs() {
        let secret = signer_secret();
        let file_name = format!("{HEX64}.png");
        let sig = sign_artifact_file_name(&secret, &file_name);
        assert!(verify_artifact_signature(&secret, &file_name, &sig));

        let mut tampered = sig.clone();
        tampered.replace_range(0..1, if sig.starts_with('0') { "1" } else { "0" });
        assert!(!verify_artifact_signature(&secret, &file_name, &tampered));

        assert!(!verify_artifact_signature(
            &secret,
            &format!("{HEX64}.svg"),
            &sig
        ));
        assert!(!verify_artifact_signature(
            &secret,
            &format!("{}.png", "f".repeat(64)),
            &sig
        ));
        assert!(!verify_artifact_signature(
            &secret,
            &file_name,
            "not-a-signature"
        ));
    }

    #[test]
    fn file_name_validation_accepts_content_addresses_and_rejects_traversal() {
        assert!(is_valid_artifact_file_name(&format!("{HEX64}.png")));
        assert!(is_valid_artifact_file_name(&format!("{HEX64}.html")));
        assert!(is_valid_artifact_file_name(&format!("{HEX64}.jpeg")));

        assert!(!is_valid_artifact_file_name("../secret.png"));
        assert!(!is_valid_artifact_file_name(&format!("..\\{HEX64}.png")));
        assert!(!is_valid_artifact_file_name(&format!("sub/{HEX64}.png")));
        assert!(!is_valid_artifact_file_name(&format!("{HEX64}.exe")));
        assert!(!is_valid_artifact_file_name(&format!("{HEX64}.png.txt")));
        assert!(!is_valid_artifact_file_name(&format!(
            "{}.png",
            "A".repeat(64)
        )));
        assert!(!is_valid_artifact_file_name(&format!(
            "{}.png",
            "0".repeat(63)
        )));
        assert!(!is_valid_artifact_file_name("manifest.json"));
    }

    #[test]
    fn url_host_uses_parsed_url_host() {
        assert_eq!(url_host("https://example.com/path?q=1"), "example.com");
        assert_eq!(url_host("http://user:pw@127.0.0.1:8080/index"), "127.0.0.1");
        assert_eq!(url_host("https://[::1]/artifact"), "[::1]");
        assert_eq!(url_host("not a url"), "artifact");
    }

    #[test]
    fn kind_inference_sniffs_magic_bytes_and_text() {
        let png = b"\x89PNG\r\n\x1a\nrest-of-image";
        assert_eq!(
            classify_file_bytes(png, None).unwrap(),
            (ArtifactKind::Image, "image/png", "png")
        );
        assert_eq!(
            classify_file_bytes(&[0xff, 0xd8, 0xff, 0xe0], None).unwrap(),
            (ArtifactKind::Image, "image/jpeg", "jpeg")
        );
        assert_eq!(
            classify_file_bytes(b"GIF89a.....", None).unwrap(),
            (ArtifactKind::Image, "image/gif", "gif")
        );
        assert_eq!(
            classify_file_bytes(b"RIFFxxxxWEBPVP8 ", None).unwrap(),
            (ArtifactKind::Image, "image/webp", "webp")
        );

        assert_eq!(
            classify_file_bytes(b"\xef\xbb\xbf  <?xml version=\"1.0\"?><svg></svg>", None).unwrap(),
            (ArtifactKind::Svg, "image/svg+xml", "svg")
        );
        assert_eq!(
            classify_file_bytes(b"<!DOCTYPE html><html></html>", None).unwrap(),
            (ArtifactKind::Html, "text/html", "html")
        );
        assert!(classify_file_bytes(b"not visually renderable", None).is_err());

        assert!(classify_file_bytes(png, Some(ArtifactKindArg::Svg)).is_err());
        assert!(classify_file_bytes(png, Some(ArtifactKindArg::Image)).is_ok());
    }

    #[test]
    fn inline_kind_inference_defaults_to_html() {
        assert_eq!(
            infer_inline_kind("<svg></svg>", None).unwrap(),
            ArtifactKind::Svg
        );
        assert_eq!(
            infer_inline_kind("<div>hi</div>", None).unwrap(),
            ArtifactKind::Html
        );
        assert_eq!(
            infer_inline_kind("plain", Some(ArtifactKindArg::Html)).unwrap(),
            ArtifactKind::Html
        );
        assert!(infer_inline_kind("plain", Some(ArtifactKindArg::Image)).is_err());
    }

    #[test]
    fn source_selection_requires_exactly_one_source() {
        let base =
            |path: Option<&str>, content: Option<&str>, url: Option<&str>| PresentArtifactArgs {
                path: path.map(str::to_string),
                content: content.map(str::to_string),
                url: url.map(str::to_string),
                kind: None,
                title: None,
                artifact_id: None,
                description: None,
            };

        assert!(validate_artifact_args(&base(Some("a.png"), None, None)).is_ok());
        assert!(validate_artifact_args(&base(None, None, None)).is_err());
        assert!(validate_artifact_args(&base(Some("a.png"), Some("x"), None)).is_err());
        assert!(validate_artifact_args(&base(None, Some("x"), Some("http://x"))).is_err());
    }

    #[test]
    fn url_validation_accepts_http_and_rejects_others() {
        assert!(validate_artifact_url("https://example.com/page").is_ok());
        assert!(validate_artifact_url("http://127.0.0.1:5173/").is_ok());
        assert!(validate_artifact_url("file:///etc/passwd").is_err());
        assert!(validate_artifact_url("javascript:alert(1)").is_err());
        assert!(validate_artifact_url(" https://example.com").is_err());
        assert!(validate_artifact_url("https://exa mple.com").is_err());
    }

    #[test]
    fn version_bump_through_manifest_increments_per_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join(ARTIFACT_PRESENTED_DIR_NAME);

        let first = update_artifact_manifest(&dir, "chart", Some("a.png"), "image/png", "A")
            .expect("first version");
        let second = update_artifact_manifest(&dir, "chart", Some("b.png"), "image/png", "B")
            .expect("second version");
        let other =
            update_artifact_manifest(&dir, "other", None, "text/uri-list", "U").expect("fresh id");

        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(other, 1);

        let manifest =
            load_artifact_manifest(&dir.join(ARTIFACT_MANIFEST_FILE_NAME)).expect("load manifest");
        let chart = manifest.artifacts.get("chart").expect("chart entry");
        assert_eq!(chart.latest_version, 2);
        assert_eq!(chart.versions.len(), 2);
        assert_eq!(chart.versions[0].file_name.as_deref(), Some("a.png"));
        assert_eq!(chart.versions[1].file_name.as_deref(), Some("b.png"));
        assert_eq!(manifest.artifacts.get("other").unwrap().latest_version, 1);
    }

    #[test]
    fn serialized_artifact_activity_data_uses_snake_case_kind() {
        let data = ArtifactActivityData {
            artifact_id: "demo-chart".to_string(),
            version: 2,
            kind: ArtifactKind::Image,
            title: "chart.png".to_string(),
            uri: format!("/artifacts/{HEX64}.png?sig=deadbeef"),
            mime_type: "image/png".to_string(),
            local_path: Some("C:/data/artifacts/presented/chart.png".to_string()),
            byte_len: Some(1024),
            description: None,
        };
        let encoded = serde_json::to_string(&data).expect("serialize");
        assert!(encoded.contains("\"kind\":\"image\""));
        assert!(encoded.contains("\"artifact_id\":\"demo-chart\""));
        assert!(!encoded.contains("description"));
        let decoded: ArtifactActivityData = serde_json::from_str(&encoded).expect("roundtrip");
        assert_eq!(decoded, data);
    }
}
