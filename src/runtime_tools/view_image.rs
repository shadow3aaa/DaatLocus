use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    activity_event::ToolCallActivityEvent,
    context::Context,
    daat_locus_paths::{DaatLocusPaths, daat_locus_paths_sync},
    reasoning::{
        episode::EpisodeActionRecord,
        runtime::{AgentContentPart, AgentToolCall},
    },
    runtime_tools::{
        RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    },
    schema_utils::model_schema_for,
};

const MAX_VIEW_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewImageArgs {
    /// Local filesystem path to an image file. Relative paths are resolved from the current execution directory.
    path: String,
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![Box::new(
        StaticRuntimeTool::new_with_schema_and_availability(
            "view_image",
            "Read a local image file and attach it to the next model request for visual inspection. Use only for images already available on disk.",
            model_schema_for::<ViewImageArgs>(),
            active_model_supports_vision,
            summarize_view_image_tool,
            render_view_image_call_ui,
            execute_view_image_runtime_tool,
        ),
    )]
}

fn summarize_view_image_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ViewImageArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "view_image".to_string(),
        summary: format!("path={}", args.path),
    })
}

fn render_view_image_call_ui(call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
    let args: ViewImageArgs = parse_tool_args(call)?;
    Ok(ToolCallActivityEvent::app(
        "View Image",
        vec![format!("path={}", args.path)],
    ))
}

fn execute_view_image_runtime_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        if !active_model_supports_vision(context) {
            return Err(miette!(
                "view_image is not available because the active model does not support image inputs"
            ));
        }

        let args: ViewImageArgs = parse_tool_args(call)?;
        let requested_path = args.path;
        let resolved = context
            .sandbox_policy
            .resolve_path(Path::new(&requested_path), Some(&context.execution_cwd));
        let canonical_path = resolved
            .canonicalize()
            .map_err(|err| miette!("unable to resolve image `{}`: {err}", resolved.display()))?;
        context
            .sandbox_policy
            .ensure_path_readable(&canonical_path, "view_image target")?;

        let bytes = read_view_image_bytes(&canonical_path)?;
        let media_type = media_type_for_image_bytes(&bytes).ok_or_else(|| {
            miette!(
                "unsupported image type for `{}`; supported formats are PNG, JPEG, WebP, and GIF",
                canonical_path.display()
            )
        })?;
        let snapshot_path = snapshot_viewed_image(context, &bytes, media_type)?;

        Ok(ToolExecutionResult::from_activity_event(
            format!("loaded image {requested_path}"),
            json!({
                "path": requested_path,
                "resolved_path": canonical_path.display().to_string(),
                "media_type": media_type,
                "bytes": bytes.len(),
            }),
            None,
        )
        .with_model_content(format!(
            "Attached local image `{}` ({media_type}, {} bytes) for visual inspection.",
            canonical_path.display(),
            bytes.len()
        ))
        .with_model_image_part(AgentContentPart::Image {
            path: snapshot_path.display().to_string(),
            media_type: media_type.to_string(),
            description: Some(format!("local image {requested_path}")),
        }))
    })
}

fn active_model_supports_vision(context: &Context) -> bool {
    let model = context.config.main_model_config();
    model.supports_vision.unwrap_or_else(|| {
        crate::model_catalog::catalog_model_capacity(&model.model_id)
            .map(|capacity| capacity.supports_vision)
            .unwrap_or(true)
    })
}

fn read_view_image_bytes(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path)
        .map_err(|err| miette!("unable to inspect image `{}`: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(miette!("image path `{}` is not a file", path.display()));
    }
    if metadata.len() > MAX_VIEW_IMAGE_BYTES {
        return Err(miette!(
            "image `{}` is too large: {} bytes exceeds the {} MiB limit",
            path.display(),
            metadata.len(),
            MAX_VIEW_IMAGE_BYTES / 1024 / 1024
        ));
    }

    let capacity = usize::try_from(metadata.len()).unwrap_or(MAX_VIEW_IMAGE_BYTES as usize);
    let mut bytes = Vec::with_capacity(capacity);
    fs::File::open(path)
        .map_err(|err| miette!("unable to read image `{}`: {err}", path.display()))?
        .take(MAX_VIEW_IMAGE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| miette!("unable to read image `{}`: {err}", path.display()))?;
    if bytes.len() as u64 > MAX_VIEW_IMAGE_BYTES {
        return Err(miette!(
            "image `{}` is too large: more than {} MiB",
            path.display(),
            MAX_VIEW_IMAGE_BYTES / 1024 / 1024
        ));
    }
    Ok(bytes)
}

fn snapshot_viewed_image(context: &Context, bytes: &[u8], media_type: &str) -> Result<PathBuf> {
    let paths = context
        .session_id
        .as_deref()
        .map(DaatLocusPaths::for_session)
        .unwrap_or_else(daat_locus_paths_sync);
    let dir = paths.state_dir().join("viewed_images");
    fs::create_dir_all(&dir)
        .map_err(|err| miette!("unable to create viewed-image cache: {err}"))?;

    let digest = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = dir.join(format!(
        "view-{digest}.{}",
        image_extension_for_media_type(media_type)
    ));
    fs::write(&path, bytes)
        .map_err(|err| miette!("unable to store viewed image `{}`: {err}", path.display()))?;
    Ok(path)
}

fn media_type_for_image_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.get(0..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn image_extension_for_media_type(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "image",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_image_signatures() {
        assert_eq!(
            media_type_for_image_bytes(b"\x89PNG\r\n\x1a\nfixture"),
            Some("image/png")
        );
        assert_eq!(
            media_type_for_image_bytes(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(media_type_for_image_bytes(b"GIF89a"), Some("image/gif"));
        assert_eq!(
            media_type_for_image_bytes(b"RIFFxxxxWEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(media_type_for_image_bytes(b"not an image"), None);
    }
}
