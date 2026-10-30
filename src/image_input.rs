//! Bounded model views of immutable source images. Source limits are independent of output limits.
use daat_locus_macros::model_schema;
use image::{DynamicImage, ImageDecoder, ImageReader, imageops::FilterType};
use miette::{IntoDiagnostic, Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Cursor;

pub const MAX_EDGE: u32 = 1568;
pub const MAX_PIXELS: u32 = 1_150_000;
pub const MAX_SOURCE_PIXELS: u64 = 64_000_000;
pub const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ENCODED_BYTES: usize = 3_000_000;

#[model_schema]
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImageRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[model_schema]
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImageResolution {
    pub width: u32,
    pub height: u32,
    pub max_pixels: u32,
}
impl Default for ImageResolution {
    fn default() -> Self {
        Self {
            width: 1024,
            height: 1024,
            max_pixels: 786_432,
        }
    }
}
impl ImageResolution {
    pub fn validate(self) -> Result<Self> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_EDGE
            || self.height > MAX_EDGE
            || self.max_pixels == 0
            || self.max_pixels > MAX_PIXELS
        {
            return Err(miette!(
                "Image resolution must have positive dimensions <= 1568 and max_pixels <= 1150000"
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImageView {
    pub source_width: u32,
    pub source_height: u32,
    pub region: ImageRegion,
    pub width: u32,
    pub height: u32,
}
impl ImageView {
    pub fn plan(
        width: u32,
        height: u32,
        region: Option<ImageRegion>,
        resolution: Option<ImageResolution>,
        gui: bool,
    ) -> Result<Self> {
        let resolution = resolution.unwrap_or_default().validate()?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_SOURCE_PIXELS {
            return Err(miette!("Source image exceeds the decoding pixel budget"));
        }
        let region = region.unwrap_or(ImageRegion {
            x: 0,
            y: 0,
            width,
            height,
        });
        if region.width == 0
            || region.height == 0
            || region.x.checked_add(region.width).is_none_or(|n| n > width)
            || region
                .y
                .checked_add(region.height)
                .is_none_or(|n| n > height)
        {
            return Err(miette!("Image region must fit the source image"));
        }
        let mut scale = 1.0_f64
            .min(f64::from(resolution.width) / f64::from(region.width))
            .min(f64::from(resolution.height) / f64::from(region.height))
            .min(
                (f64::from(resolution.max_pixels)
                    / (f64::from(region.width) * f64::from(region.height)))
                .sqrt(),
            );
        // Earlier OpenAI high/auto inputs scale the short edge to 768. Keep
        // their coordinate scale stable, including after a model switch.
        if gui {
            scale = scale.min(768.0 / f64::from(region.width.min(region.height)));
        }
        let mut output_width = (f64::from(region.width) * scale).floor().max(1.0) as u32;
        let mut output_height = (f64::from(region.height) * scale).floor().max(1.0) as u32;
        // A clamped one-pixel short edge must not make extreme aspect ratios
        // exceed a caller's smaller total-pixel budget.
        if output_width >= output_height {
            output_width = output_width.min(resolution.max_pixels / output_height).max(1);
        } else {
            output_height = output_height.min(resolution.max_pixels / output_width).max(1);
        }
        Ok(Self {
            source_width: width,
            source_height: height,
            region,
            width: output_width,
            height: output_height,
        })
    }
    pub fn source_point(self, x: f64, y: f64) -> Result<(f64, f64)> {
        self.validate()?;
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= f64::from(self.width)
            || y >= f64::from(self.height)
        {
            return Err(miette!("Point is outside the coordinate image"));
        }
        Ok((
            f64::from(self.region.x) + x * f64::from(self.region.width) / f64::from(self.width),
            f64::from(self.region.y) + y * f64::from(self.region.height) / f64::from(self.height),
        ))
    }
    pub fn validate(self) -> Result<()> {
        ImageResolution {
            width: self.width,
            height: self.height,
            max_pixels: MAX_PIXELS,
        }
        .validate()?;
        if u64::from(self.width) * u64::from(self.height) > u64::from(MAX_PIXELS)
            || self.width > self.region.width
            || self.height > self.region.height
        {
            return Err(miette!("Invalid image view dimensions"));
        }
        Self::plan(
            self.source_width,
            self.source_height,
            Some(self.region),
            None,
            false,
        )?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{0}")]
pub struct ImageRequestError(pub String);
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{0}")]
pub struct ImageRequestBudgetError(pub String);
pub const MAX_REQUEST_IMAGES: usize = 100;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

pub fn image_tokens(part: &crate::model_protocol::AgentContentPart, model: Option<&str>) -> usize {
    use crate::model_protocol::AgentContentPart;
    let dimensions = match part {
        AgentContentPart::Text { text } => return crate::context_budget::approx_token_count(text),
        AgentContentPart::Image { view, .. } | AgentContentPart::ResourceImage { view, .. } => {
            view.map(|v| (v.width, v.height))
        }
        AgentContentPart::ImageData { data_base64, .. } => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(data_base64)
                .ok()
                .and_then(|b| {
                    ImageReader::new(Cursor::new(b))
                        .with_guessed_format()
                        .ok()?
                        .into_dimensions()
                        .ok()
                })
        }
    }
    .unwrap_or((1024, 768));
    let (w, h) = dimensions;
    let model = model.unwrap_or("").to_ascii_lowercase();
    if model.contains("deepseek") {
        return 1024;
    }
    if model.contains("gpt-4o") || model == "gpt-4.1" || model.starts_with("gpt-4.1-2025") {
        let scale = (768.0 / f64::from(w.min(h))).min(1.0);
        let tiles = ((f64::from(w) * scale).floor() as usize).div_ceil(512)
            * ((f64::from(h) * scale).floor() as usize).div_ceil(512);
        return if model.contains("4o-mini") {
            2833 + 5667 * tiles
        } else {
            85 + 170 * tiles
        };
    }
    if model.contains("gpt") || model.starts_with("o3") || model.starts_with("o4") {
        return (w.div_ceil(32) as usize * h.div_ceil(32) as usize * 172).div_ceil(100);
    }
    w.div_ceil(28) as usize * h.div_ceil(28) as usize
}

pub fn fits_messages<'a>(
    messages: impl IntoIterator<Item = &'a crate::model_protocol::AgentMessage>,
) -> bool {
    use crate::model_protocol::{AgentContentPart, AgentMessage};
    let mut count = 0usize;
    let mut bytes = 0usize;
    for message in messages {
        if let AgentMessage::User { content } = message {
            for part in content.parts() {
                let size = match part {
                    AgentContentPart::Text { .. } => continue,
                    AgentContentPart::ImageData { data_base64, .. } => data_base64.len(),
                    AgentContentPart::ResourceImage { resource, view, .. } => {
                        let pixels =
                            view.map_or(786_432, |v| u64::from(v.width) * u64::from(v.height));
                        resource
                            .content
                            .size
                            .min(pixels * 4)
                            .min(MAX_ENCODED_BYTES as u64) as usize
                            * 4
                            / 3
                            + 1024
                    }
                    AgentContentPart::Image { .. } => MAX_ENCODED_BYTES * 4 / 3,
                };
                count += 1;
                bytes = bytes.saturating_add(size);
            }
        }
    }
    count <= MAX_REQUEST_IMAGES && bytes <= MAX_REQUEST_BYTES - 4 * 1024 * 1024
}

/// Inspect the final wire payload, including historical images and base64 overhead.
pub fn validate_payload(payload: &serde_json::Value) -> Result<()> {
    fn validate_image(encoded: &str) -> Result<()> {
        use base64::Engine;
        let encoded = if encoded.starts_with("data:") {
            encoded
                .split_once(";base64,")
                .ok_or_else(|| ImageRequestError("Invalid inline image encoding".into()))?
                .1
        } else {
            encoded
        };
        if encoded.len() > MAX_ENCODED_BYTES.div_ceil(3) * 4 {
            return Err(
                ImageRequestError("Inline image exceeds the encoded byte budget".into()).into(),
            );
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| ImageRequestError("Invalid inline image encoding".into()))?;
        let (width, height) = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| ImageRequestError("Invalid inline image".into()))?
            .into_dimensions()
            .map_err(|_| ImageRequestError("Invalid inline image".into()))?;
        if width == 0
            || height == 0
            || width > MAX_EDGE
            || height > MAX_EDGE
            || u64::from(width) * u64::from(height) > u64::from(MAX_PIXELS)
        {
            return Err(ImageRequestError(
                "Inline image exceeds the common resolution limits".into(),
            )
            .into());
        }
        Ok(())
    }
    fn validate_images(value: &serde_json::Value) -> Result<()> {
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    validate_images(value)?;
                }
            }
            serde_json::Value::Object(values) => {
                match values.get("type").and_then(|v| v.as_str()) {
                    Some("image") => {
                        if let Some(data) = value.pointer("/source/data").and_then(|v| v.as_str()) {
                            validate_image(data)?;
                        }
                    }
                    Some("image_url" | "input_image") => {
                        if let Some(url) = value
                            .get("image_url")
                            .and_then(|v| v.as_str().or_else(|| v.get("url")?.as_str()))
                            && url.starts_with("data:")
                        {
                            validate_image(url)?;
                        }
                    }
                    _ => {}
                }
                for (key, value) in values {
                    if key == "images" {
                        if let Some(images) = value.as_array() {
                            for image in images {
                                if let Some(encoded) = image.as_str() {
                                    validate_image(encoded)?;
                                }
                            }
                        }
                    } else {
                        validate_images(value)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn count(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Array(values) => values.iter().map(count).sum(),
            serde_json::Value::Object(values) => {
                let image = matches!(
                    values.get("type").and_then(|v| v.as_str()),
                    Some("image" | "image_url" | "input_image")
                );
                usize::from(image)
                    + values
                        .iter()
                        .map(|(key, value)| {
                            if key == "images" {
                                value.as_array().map_or(0, Vec::len)
                            } else {
                                count(value)
                            }
                        })
                        .sum::<usize>()
            }
            _ => 0,
        }
    }
    let count = count(payload);
    if count > MAX_REQUEST_IMAGES
        || (count > 0 && serde_json::to_vec(payload).into_diagnostic()?.len() > MAX_REQUEST_BYTES)
    {
        return Err(ImageRequestBudgetError("Image request exceeds the image-count or encoded request budget; reduce the active image context".into()).into());
    }
    validate_images(payload)
}

pub struct PreparedImage {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
    pub view: ImageView,
}
fn decode(bytes: &[u8]) -> Result<(DynamicImage, bool)> {
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(miette!("Source image exceeds the byte budget"));
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .into_diagnostic()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_SOURCE_PIXELS * 8);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().into_diagnostic()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
        return Err(miette!("Source image exceeds the decoding pixel budget"));
    }
    let orientation = decoder.orientation().into_diagnostic()?;
    let mut image = DynamicImage::from_decoder(decoder).into_diagnostic()?;
    image.apply_orientation(orientation);
    Ok((
        image,
        orientation != image::metadata::Orientation::NoTransforms,
    ))
}
pub fn prepare(
    bytes: &[u8],
    region: Option<ImageRegion>,
    resolution: Option<ImageResolution>,
    gui: bool,
) -> Result<PreparedImage> {
    let (image, oriented) = decode(bytes)?;
    let view = ImageView::plan(image.width(), image.height(), region, resolution, gui)?;
    render(image, view, (!oriented).then_some(bytes))
}
pub fn render_view(bytes: &[u8], view: ImageView) -> Result<PreparedImage> {
    view.validate()?;
    let (image, oriented) = decode(bytes)?;
    if (image.width(), image.height()) != (view.source_width, view.source_height) {
        return Err(miette!("Image source geometry changed"));
    }
    render(image, view, (!oriented).then_some(bytes))
}
fn render(image: DynamicImage, view: ImageView, original: Option<&[u8]>) -> Result<PreparedImage> {
    if let Some(bytes) = original.filter(|b| b.len() <= MAX_ENCODED_BYTES)
        && view.region.x == 0
        && view.region.y == 0
        && (view.region.width, view.region.height) == (view.source_width, view.source_height)
        && (view.width, view.height) == (view.source_width, view.source_height)
    {
        let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("image/png")
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some("image/jpeg")
        } else {
            None
        };
        if let Some(media_type) = media_type {
            return Ok(PreparedImage {
                bytes: bytes.to_vec(),
                media_type,
                view,
            });
        }
    }
    let r = view.region;
    let image = image.crop_imm(r.x, r.y, r.width, r.height).resize_exact(
        view.width,
        view.height,
        FilterType::Lanczos3,
    );
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .into_diagnostic()?;
    let mut bytes = bytes.into_inner();
    let mut media_type = "image/png";
    if bytes.len() > MAX_ENCODED_BYTES {
        bytes.clear();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
            .encode_image(&image.to_rgb8())
            .into_diagnostic()?;
        media_type = "image/jpeg";
    }
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(miette!(
            "Image encoding exceeds the byte budget; request a smaller resolution"
        ));
    }
    Ok(PreparedImage {
        bytes,
        media_type,
        view,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn four_k_overview_and_cropped_points_have_stable_geometry() {
        let overview = ImageView::plan(3840, 2160, None, None, true).unwrap();
        assert_eq!((overview.width, overview.height), (1024, 576));
        assert_eq!(
            overview.source_point(512.0, 288.0).unwrap(),
            (1920.0, 1080.0)
        );
        let crop = ImageView::plan(
            3840,
            2160,
            Some(ImageRegion {
                x: 2000,
                y: 800,
                width: 600,
                height: 400,
            }),
            None,
            true,
        )
        .unwrap();
        assert_eq!(crop.source_point(300.0, 200.0).unwrap(), (2300.0, 1000.0));
        assert_eq!(
            overview.source_point(512.0, 288.0).unwrap(),
            (1920.0, 1080.0)
        );
        assert!(crop.source_point(600.0, 0.0).is_err());
        assert!(crop.source_point(f64::NAN, 0.0).is_err());
    }
    #[test]
    fn invalid_regions_and_resolution_are_rejected() {
        for region in [
            ImageRegion {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 1,
            },
            ImageRegion {
                x: 9,
                y: 0,
                width: 2,
                height: 1,
            },
            ImageRegion {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            },
        ] {
            assert!(ImageView::plan(10, 10, Some(region), None, false).is_err());
        }
        assert!(
            ImageView::plan(
                10,
                10,
                None,
                Some(ImageResolution {
                    width: 3000,
                    height: 100,
                    max_pixels: 1000
                }),
                false
            )
            .is_err()
        );
    }
    #[test]
    fn every_requested_aspect_fits_common_limits_and_gui_avoids_hidden_resize() {
        let high = Some(ImageResolution {
            width: 1568,
            height: 1568,
            max_pixels: 1_150_000,
        });
        for (w, h) in [
            (1, 64000),
            (64000, 1),
            (3840, 2160),
            (2160, 3840),
            (8000, 8000),
            (2000, 2000),
        ] {
            for gui in [false, true] {
                let view = ImageView::plan(w, h, None, high, gui).unwrap();
                assert!(view.width <= 1568 && view.height <= 1568);
                assert!(u64::from(view.width) * u64::from(view.height) <= 1_150_000);
                assert!(view.width.div_ceil(28) * view.height.div_ceil(28) <= 1568);
                if gui {
                    assert!(view.width.min(view.height) <= 768);
                }
            }
        }
        let tiny = ImageView::plan(1,64000,None,Some(ImageResolution{width:1,height:1568,max_pixels:1}),false).unwrap();
        assert_eq!((tiny.width,tiny.height),(1,1));
    }
    #[test]
    fn wire_budget_counts_images_across_turns_and_never_retries_oversized_payloads() {
        use serde_json::json;
        for count in [14, 15, 20, 21, 100, 101] {
            let payload = json!({"messages":(0..count).map(|_|json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.com/image.png"}}]})).collect::<Vec<_>>()});
            assert_eq!(validate_payload(&payload).is_ok(), count <= 100);
        }
        let payload = json!({"input":[{"type":"input_image","image_url":"data:image/png;base64,AA=="}],"instructions":"x".repeat(MAX_REQUEST_BYTES)});
        let error = validate_payload(&payload).unwrap_err();
        assert!(!crate::core::should_retry_agent_turn_error(&error));
    }
    #[test]
    fn crop_reads_original_pixels_and_reencodes_real_image() {
        let image = image::RgbImage::from_fn(1200, 800, |x, y| {
            if x >= 600 && y >= 400 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            }
        });
        let mut original = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut original, image::ImageFormat::Png)
            .unwrap();
        let crop = prepare(
            original.get_ref(),
            Some(ImageRegion {
                x: 600,
                y: 400,
                width: 600,
                height: 400,
            }),
            None,
            false,
        )
        .unwrap();
        let decoded = image::load_from_memory(&crop.bytes).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (600, 400));
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0]);
        assert!(prepare(b"not an image", None, None, false).is_err());
    }
}
