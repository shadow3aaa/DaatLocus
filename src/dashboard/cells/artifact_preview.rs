//! Inline image / SVG previews for agent-presented artifacts.
//!
//! A single [`Picker`] is created once at TUI startup (after the alternate
//! screen has been entered) and handed to the dashboard so the activity feed
//! can render real terminal graphics.  Decoded protocol state is cached keyed
//! by `(source path, render width)` so redraws, scrolling and viewport culling
//! never decode or encode an image twice.
//!
//! The activity feed renders cells as cached `Vec<Line>` with viewport culling.
//! A previewable cell appends `rows` blank placeholder lines to its card, which
//! reserves exactly that many rows in the scroll layout.  After the text layout
//! has been drawn, [`ArtifactPreviewState::render`] paints the graphics protocol
//! into the reserved [`Rect`].

use std::collections::HashMap;
use std::path::Path;

use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::StatefulWidget;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{FontSize, StatefulImage};

use super::common::{ArtifactActivityData, ArtifactKind};

/// Hard cap on the number of terminal rows a single inline preview may reserve.
pub const ARTIFACT_PREVIEW_MAX_ROWS: u16 = 14;
/// Pixel size an SVG source is rasterized to before it is handed to the picker.
const SVG_RASTER_MAX_DIMENSION: f32 = 1024.0;

/// Cache key for decoded preview state: one entry per `(source path, width)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArtifactPreviewKey {
    pub path: String,
    pub width: u16,
}

impl ArtifactPreviewKey {
    pub fn new(path: impl Into<String>, width: u16) -> Self {
        Self {
            path: path.into(),
            width,
        }
    }
}

/// A resolved preview reservation for a single artifact cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactPreviewRequest {
    pub key: ArtifactPreviewKey,
    pub kind: ArtifactKind,
    /// Number of terminal rows reserved for the preview.
    pub rows: u16,
}

struct PreviewEntry {
    rows: u16,
    protocol: Option<StatefulProtocol>,
    failed: bool,
}

/// Terminal image preview state shared across dashboard frames.
#[derive(Default)]
pub struct ArtifactPreviewState {
    picker: Option<Picker>,
    entries: HashMap<ArtifactPreviewKey, PreviewEntry>,
    resolves: usize,
}

impl ArtifactPreviewState {
    #[cfg(test)]
    pub fn new(picker: Option<Picker>) -> Self {
        Self {
            picker,
            entries: HashMap::new(),
            resolves: 0,
        }
    }

    /// Install the picker detected after the alternate screen was entered.
    pub fn set_picker(&mut self, picker: Picker) {
        self.picker = Some(picker);
        self.entries.clear();
    }

    /// Number of cached `(path, width)` entries; used by tests.
    #[cfg(test)]
    pub fn cached_entries(&self) -> usize {
        self.entries.len()
    }

    /// Number of times dimension resolution actually ran; used by tests.
    #[cfg(test)]
    pub fn resolve_count(&self) -> usize {
        self.resolves
    }

    /// Drop entries that no longer match the current render width.
    pub fn retain_width(&mut self, width: u16) {
        if self.entries.is_empty() {
            return;
        }
        self.entries.retain(|key, _| key.width == width);
    }

    /// Resolve (and cache) the preview reservation for `cell`.
    ///
    /// Returns `None` when previews are disabled, the artifact kind is not a
    /// raster preview, or the local file is missing / undecodable.
    pub fn plan(
        &mut self,
        cell: &ArtifactActivityData,
        width: u16,
    ) -> Option<ArtifactPreviewRequest> {
        if width == 0 {
            return None;
        }
        let picker = self.picker.as_ref()?;
        if !previewable_kind(cell.kind) {
            return None;
        }
        let path = cell.local_path.as_deref()?.trim();
        if path.is_empty() {
            return None;
        }
        let key = ArtifactPreviewKey::new(path, width);
        if let Some(entry) = self.entries.get(&key) {
            return preview_request(&key, cell.kind, entry);
        }

        self.resolves += 1;
        let font_size = picker.font_size();
        let entry = match source_pixel_dimensions(cell.kind, Path::new(path)) {
            Some((pixel_width, pixel_height)) => PreviewEntry {
                rows: preview_rows(pixel_width, pixel_height, font_size, width),
                protocol: None,
                failed: false,
            },
            None => PreviewEntry {
                rows: 0,
                protocol: None,
                failed: true,
            },
        };
        let request = preview_request(&key, cell.kind, &entry);
        self.entries.insert(key, entry);
        request
    }

    /// Draw the preview for `request` into `area`, decoding lazily on first use.
    pub fn render(&mut self, request: &ArtifactPreviewRequest, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(entry) = self.entries.get_mut(&request.key) else {
            return;
        };
        if entry.failed {
            return;
        }
        if entry.protocol.is_none() {
            let Some(picker) = self.picker.clone() else {
                entry.failed = true;
                return;
            };
            match load_source_image(request.kind, Path::new(&request.key.path)) {
                Some(image) => entry.protocol = Some(picker.new_resize_protocol(image)),
                None => {
                    entry.failed = true;
                    return;
                }
            }
        }
        if let Some(protocol) = entry.protocol.as_mut() {
            StatefulImage::<StatefulProtocol>::new().render(area, buf, protocol);
        }
    }
}

fn preview_request(
    key: &ArtifactPreviewKey,
    kind: ArtifactKind,
    entry: &PreviewEntry,
) -> Option<ArtifactPreviewRequest> {
    (!entry.failed && entry.rows > 0).then(|| ArtifactPreviewRequest {
        key: key.clone(),
        kind,
        rows: entry.rows,
    })
}

const fn previewable_kind(kind: ArtifactKind) -> bool {
    matches!(kind, ArtifactKind::Image | ArtifactKind::Svg)
}

/// Fit an image of `pixel_width` x `pixel_height` into `width` columns while
/// capping the reserved height to [`ARTIFACT_PREVIEW_MAX_ROWS`].
///
/// The result preserves the source aspect ratio and never reserves fewer than
/// one row.
fn preview_rows(pixel_width: u32, pixel_height: u32, font_size: FontSize, width: u16) -> u16 {
    let font_width = u32::from(font_size.width.max(1));
    let font_height = u32::from(font_size.height.max(1));
    let available_width = u32::from(width.max(1));
    let max_rows = u32::from(ARTIFACT_PREVIEW_MAX_ROWS);

    let natural_cols = pixel_width.div_ceil(font_width).max(1);
    let natural_rows = pixel_height.div_ceil(font_height).max(1);

    let mut cols = natural_cols.min(available_width);
    let mut rows = natural_rows
        .saturating_mul(cols)
        .div_ceil(natural_cols)
        .max(1);
    if rows > max_rows {
        rows = max_rows;
        cols = natural_cols
            .saturating_mul(rows)
            .div_ceil(natural_rows)
            .clamp(1, available_width);
        rows = natural_rows
            .saturating_mul(cols)
            .div_ceil(natural_cols)
            .max(1);
    }
    u16::try_from(rows.clamp(1, max_rows)).unwrap_or(ARTIFACT_PREVIEW_MAX_ROWS)
}

fn source_pixel_dimensions(kind: ArtifactKind, path: &Path) -> Option<(u32, u32)> {
    let (width, height) = match kind {
        ArtifactKind::Image => image::image_dimensions(path).ok()?,
        ArtifactKind::Svg => {
            let data = std::fs::read(path).ok()?;
            let tree =
                resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
            let size = tree.size();
            (size.width() as u32, size.height() as u32)
        }
        ArtifactKind::Html | ArtifactKind::Url => return None,
    };
    (width > 0 && height > 0).then_some((width, height))
}

fn load_source_image(kind: ArtifactKind, path: &Path) -> Option<DynamicImage> {
    match kind {
        ArtifactKind::Image => image::ImageReader::open(path).ok()?.decode().ok(),
        ArtifactKind::Svg => {
            let data = std::fs::read(path).ok()?;
            rasterize_svg(&data)
        }
        ArtifactKind::Html | ArtifactKind::Url => None,
    }
}

fn rasterize_svg(data: &[u8]) -> Option<DynamicImage> {
    let tree = resvg::usvg::Tree::from_data(data, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let (width, height) = (size.width(), size.height());
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    let scale = (SVG_RASTER_MAX_DIMENSION / width.max(height)).min(1.0);
    let pixel_width = ((width * scale).ceil() as u32).max(1);
    let pixel_height = ((height * scale).ceil() as u32).max(1);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixel_width, pixel_height)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let rgba = image::RgbaImage::from_raw(pixel_width, pixel_height, pixmap.data().to_vec())?;
    Some(DynamicImage::ImageRgba8(rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_rows_fill_width_and_cap_height() {
        let font = FontSize::new(10, 20);
        // 2000x400 pixels is 200x20 cells; at 40 columns it needs 4 rows.
        assert_eq!(preview_rows(2000, 400, font, 40), 4);
        // Tall source is capped and narrowed to keep the ratio.
        assert_eq!(preview_rows(200, 2000, font, 40), ARTIFACT_PREVIEW_MAX_ROWS);
        // A tiny source never reserves more rows than it needs.
        assert_eq!(preview_rows(100, 100, font, 40), 5);
        // Zero width is defensive; always at least one row.
        assert_eq!(preview_rows(10, 10, font, 0), 1);
    }

    #[test]
    fn preview_rows_never_exceed_cap() {
        let font = FontSize::new(8, 16);
        for height in [1_u32, 50, 500, 5_000, 50_000] {
            let rows = preview_rows(64, height, font, 120);
            assert!(
                (1..=ARTIFACT_PREVIEW_MAX_ROWS).contains(&rows),
                "rows={rows}"
            );
        }
    }
}
