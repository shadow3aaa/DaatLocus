//! Durable image geometry; observation references never confer native authority.
use crate::{
    actors::ActorSystem,
    image_input::{ImageRegion, ImageResolution, ImageView},
    model_protocol::{AgentContentPart, AgentToolCall},
    resources::{self, Resource},
};
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Observation {
    kind: String,
    pub target: String,
    pub native_state: String,
    pub source: Resource,
    pub view: ImageView,
    original: Value,
}
pub(crate) fn load(
    system: &ActorSystem,
    actor: &str,
    kind: &str,
    target: &str,
    reference: &str,
) -> Result<Observation> {
    let resource = resources::resolve_reference(system, actor, reference).into_diagnostic()?;
    if resource.media_type != "application/vnd.daat.image-view+json"
        || resource.content.size > 256 * 1024
    {
        return Err(miette!("Use the returned image observation reference"));
    }
    let record: Observation =
        serde_json::from_slice(&resources::bytes(system, actor, &resource).into_diagnostic()?)
            .into_diagnostic()?;
    if record.kind != kind || record.target != target {
        return Err(miette!("Image observation belongs to a different target"));
    }
    record.view.validate()?;
    Ok(record)
}

pub(crate) fn publish(
    system: &ActorSystem,
    actor: &str,
    kind: &str,
    original: Value,
    bytes: &[u8],
    region: Option<ImageRegion>,
    resolution: Option<ImageResolution>,
) -> Result<(Value, AgentContentPart)> {
    let target_key = if kind == "browser" {
        "tab_id"
    } else {
        "window_id"
    };
    let target = original[target_key]
        .as_str()
        .ok_or_else(|| miette!("Missing native image target"))?
        .to_owned();
    let native_state = original["state_id"]
        .as_str()
        .ok_or_else(|| miette!("Missing native image reference"))?
        .to_owned();
    let source = resources::publish(system, actor, "gui-source.png", "image/png", bytes)
        .into_diagnostic()?;
    let prepared = crate::image_input::prepare(bytes, region, resolution, true)?;
    if original["width"].as_u64() != Some(u64::from(prepared.view.source_width))
        || original["height"].as_u64() != Some(u64::from(prepared.view.source_height))
    {
        return Err(miette!("Native screenshot geometry mismatch"));
    }
    finish(
        system,
        actor,
        Observation {
            kind: kind.into(),
            target,
            native_state,
            source,
            view: prepared.view,
            original,
        },
        prepared,
    )
}
pub(crate) fn reframe(
    system: &ActorSystem,
    actor: &str,
    kind: &str,
    target: &str,
    reference: &str,
    region: Option<ImageRegion>,
    resolution: Option<ImageResolution>,
) -> Result<(Value, AgentContentPart)> {
    let mut record = load(system, actor, kind, target, reference)?;
    let bytes = resources::bytes(system, actor, &record.source).into_diagnostic()?;
    let prepared = crate::image_input::prepare(&bytes, region, resolution, true)?;
    record.view = prepared.view;
    finish(system, actor, record, prepared)
}
fn finish(
    system: &ActorSystem,
    actor: &str,
    record: Observation,
    prepared: crate::image_input::PreparedImage,
) -> Result<(Value, AgentContentPart)> {
    let reference = resources::publish(
        system,
        actor,
        "image-view.json",
        "application/vnd.daat.image-view+json",
        &serde_json::to_vec(&record).into_diagnostic()?,
    )
    .into_diagnostic()?;
    let image = resources::publish(
        system,
        actor,
        "gui-view",
        prepared.media_type,
        &prepared.bytes,
    )
    .into_diagnostic()?;
    let mut observation = record.original;
    observation["state_id"] = json!(reference.id);
    observation["width"] = json!(record.view.width);
    observation["height"] = json!(record.view.height);
    observation["view"] = json!(record.view);
    observation["source_image"] = json!(record.source.uri(actor));
    observation["screenshot"] = json!(image);
    observation["screenshot_url"] = json!(image.uri(actor));
    // This is already the final provider image. Its descriptor preserves the
    // selected resolution when the history is materialized on any device.
    let identity = ImageView {
        source_width: record.view.width,
        source_height: record.view.height,
        region: ImageRegion {
            x: 0,
            y: 0,
            width: record.view.width,
            height: record.view.height,
        },
        width: record.view.width,
        height: record.view.height,
    };
    Ok((observation,AgentContentPart::ResourceImage { resource:image, view:Some(identity),description:Some("Input coordinates use this image and its state_id. Re-view that state_id to crop the saved source without operating the target.".into()) }))
}

/// Map every point in one call using its immutable image, leaving native state
/// validation, authority, receiver selection and uncertain-effect handling intact.
pub(crate) fn map_call(
    system: &ActorSystem,
    actor: &str,
    kind: &str,
    target_key: &str,
    call: &AgentToolCall,
    integer: bool,
) -> Result<(AgentToolCall, Option<(u32, u32)>)> {
    let mut call = call.clone();
    let Some(reference) = call.arguments["state_id"].as_str() else {
        return Ok((call, None));
    };
    let target = call.arguments[target_key]
        .as_str()
        .ok_or_else(|| miette!("Missing GUI target"))?;
    let record = load(system, actor, kind, target, reference)
        .map_err(|error| miette!("Invalid state_id: {error}"))?;
    let map = |point: &mut Value, xkey: &str, ykey: &str| -> Result<()> {
        if point.get(xkey).is_none() && point.get(ykey).is_none() {
            return Ok(());
        }
        let x = point[xkey]
            .as_f64()
            .ok_or_else(|| miette!("Invalid image point"))?;
        let y = point[ykey]
            .as_f64()
            .ok_or_else(|| miette!("Invalid image point"))?;
        let (x, y) = record.view.source_point(x, y)?;
        point[xkey] = if integer {
            json!(x.floor() as i64)
        } else {
            json!(x)
        };
        point[ykey] = if integer {
            json!(y.floor() as i64)
        } else {
            json!(y)
        };
        Ok(())
    };
    if matches!(
        call.name.as_str(),
        "click" | "scroll" | "drag" | "move_pointer"
    ) {
        map(&mut call.arguments, "x", "y")?;
        map(&mut call.arguments, "to_x", "to_y")?;
        if let Some(points) = call.arguments.get_mut("path").and_then(Value::as_array_mut) {
            for point in points {
                map(point, "x", "y")?;
            }
        }
    }
    call.arguments["state_id"] = json!(record.native_state);
    Ok((
        call,
        Some((record.view.source_width, record.view.source_height)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn saved_crops_map_each_reference_and_keep_actor_and_target_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let system = ActorSystem::open(
            temp.path().join("state"),
            temp.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        system.seed_test_actor("reader");
        system.seed_test_actor("other");
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            3840,
            2160,
            image::Rgb([255, 0, 0]),
        ))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
        let native_state = "b".repeat(32);
        let original = json!({"tab_id":"tab","state_id":native_state,"width":3840,"height":2160});
        let (overview, _) = publish(
            &system,
            "reader",
            "browser",
            original,
            bytes.get_ref(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            (overview["width"].as_u64(), overview["height"].as_u64()),
            (Some(1024), Some(576))
        );
        let reference = overview["state_id"].as_str().unwrap();
        let (crop, _) = reframe(
            &system,
            "reader",
            "browser",
            "tab",
            reference,
            Some(ImageRegion {
                x: 2000,
                y: 800,
                width: 600,
                height: 400,
            }),
            None,
        )
        .unwrap();
        let click = |reference: &str, x: f64, y: f64| AgentToolCall {
            id: "click".into(),
            name: "click".into(),
            arguments: json!({"tab_id":"tab","state_id":reference,"x":x,"y":y}),
        };
        let cropped = click(crop["state_id"].as_str().unwrap(), 300.0, 200.0);
        let (mapped, size) =
            map_call(&system, "reader", "browser", "tab_id", &cropped, false).unwrap();
        assert_eq!(mapped.arguments["x"], 2300.0);
        assert_eq!(mapped.arguments["y"], 1000.0);
        assert_eq!(mapped.arguments["state_id"], native_state);
        assert_eq!(size, Some((3840, 2160)));
        let (_, high_image) = reframe(
            &system,
            "reader",
            "browser",
            "tab",
            reference,
            None,
            Some(ImageResolution {
                width: 1568,
                height: 1568,
                max_pixels: 1_150_000,
            }),
        )
        .unwrap();
        let request = crate::model_protocol::AgentTurnRequest {
            messages: vec![crate::model_protocol::AgentMessage::user_content(
                crate::model_protocol::AgentContent::multimodal("Inspect", vec![high_image]),
            )],
            tools: vec![],
        };
        let request = resources::model_request(&system, "reader", request, true).unwrap();
        let crate::model_protocol::AgentMessage::User { content } = &request.messages[0] else {
            panic!("Expected image")
        };
        let AgentContentPart::ImageData { data_base64, .. } = &content.parts()[0] else {
            panic!("Expected image data")
        };
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (1365, 768));
        let old = click(reference, 512.0, 288.0);
        let (mapped, _) = map_call(&system, "reader", "browser", "tab_id", &old, false).unwrap();
        assert_eq!(mapped.arguments["x"], 1920.0);
        assert_eq!(mapped.arguments["y"], 1080.0);
        assert!(map_call(&system, "other", "browser", "tab_id", &old, false).is_err());
        assert!(load(&system, "reader", "browser", "different", reference).is_err());
        assert!(load(&system, "reader", "computer", "tab", reference).is_err());
        let reference = reference.to_owned();
        drop(system);
        let reopened = ActorSystem::open(
            temp.path().join("state"),
            temp.path().join("workspace"),
            Default::default(),
        )
        .await
        .unwrap();
        assert!(load(&reopened, "reader", "browser", "tab", &reference).is_ok());
    }
}
