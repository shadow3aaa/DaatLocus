//! Layout-tree Renderable trait and combinators for viewport-culled TUI rendering.
//!
//! Adapted from codex-rs tui/src/render/renderable.rs.
//! The trait and combinators are infrastructure for the layout-tree rendering
//! architecture; they are used by ActivityCellRenderable now and by future
//! components (command bar, popups, etc.).
#![allow(dead_code)]

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// A widget that can compute its desired height and render itself into a buffer.
pub trait Renderable {
    /// Render into the given area of the buffer.
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// Desired height in rows for the given available width.
    /// Used for viewport-culling and layout.
    fn desired_height(&self, width: u16) -> u16;
}

// ---------------------------------------------------------------------------
// Blanket impls
// ---------------------------------------------------------------------------

impl<R: Renderable> Renderable for &R {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        (*self).render(area, buf);
    }
    fn desired_height(&self, width: u16) -> u16 {
        (*self).desired_height(width)
    }
}

impl<R: Renderable> Renderable for Box<R> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.as_ref().render(area, buf);
    }
    fn desired_height(&self, width: u16) -> u16 {
        self.as_ref().desired_height(width)
    }
}

impl<R: Renderable> Renderable for Option<R> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if let Some(r) = self {
            r.render(area, buf);
        }
    }
    fn desired_height(&self, width: u16) -> u16 {
        self.as_ref().map_or(0, |r| r.desired_height(width))
    }
}

// ---------------------------------------------------------------------------
// ColumnRenderable – stacks children vertically
// ---------------------------------------------------------------------------

pub struct ColumnRenderable<'a> {
    children: Vec<Box<dyn Renderable + 'a>>,
}

impl<'a> ColumnRenderable<'a> {
    pub fn new() -> Self {
        Self { children: vec![] }
    }

    pub fn push(&mut self, child: impl Renderable + 'a) {
        self.children.push(Box::new(child));
    }
}

impl Renderable for ColumnRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;
        for child in &self.children {
            let h = child.desired_height(area.width);
            let child_area = Rect::new(area.x, y, area.width, h).intersection(area);
            if !child_area.is_empty() {
                child.render(child_area, buf);
            }
            y += h;
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.children.iter().map(|c| c.desired_height(width)).sum()
    }
}

// ---------------------------------------------------------------------------
// FlexRenderable – column with flex-grow children
// ---------------------------------------------------------------------------

struct FlexChild<'a> {
    flex: i32,
    child: Box<dyn Renderable + 'a>,
}

pub struct FlexRenderable<'a> {
    children: Vec<FlexChild<'a>>,
}

impl<'a> FlexRenderable<'a> {
    pub fn new() -> Self {
        Self { children: vec![] }
    }

    /// Push a child. `flex` > 0 means the child will share remaining space
    /// proportional to its flex value.
    pub fn push(&mut self, flex: i32, child: impl Renderable + 'a) {
        self.children.push(FlexChild {
            flex,
            child: Box::new(child),
        });
    }

    fn allocate(&self, area: Rect) -> Vec<Rect> {
        let max_height = area.height;
        let n = self.children.len();
        let mut sizes = vec![0u16; n];
        let mut allocated = 0u16;
        let mut total_flex: i32 = 0;
        let mut last_flex_idx: Option<usize> = None;

        // Pass 1: non-flex children get their desired height
        for (i, fc) in self.children.iter().enumerate() {
            if fc.flex > 0 {
                total_flex += fc.flex;
                last_flex_idx = Some(i);
            } else {
                let h = fc
                    .child
                    .desired_height(area.width)
                    .min(max_height.saturating_sub(allocated));
                sizes[i] = h;
                allocated += h;
            }
        }

        let free = max_height.saturating_sub(allocated);

        // Pass 2: flex children share remaining space
        if total_flex > 0 && free > 0 {
            let per_flex = free / total_flex as u16;
            let mut flex_allocated = 0u16;
            for (i, fc) in self.children.iter().enumerate() {
                if fc.flex > 0 {
                    let max_child = if Some(i) == last_flex_idx {
                        free.saturating_sub(flex_allocated)
                    } else {
                        per_flex * fc.flex as u16
                    };
                    let h = fc.child.desired_height(area.width).min(max_child);
                    sizes[i] = h;
                    flex_allocated += h;
                }
            }
        }

        let mut y = area.y;
        sizes
            .into_iter()
            .map(|h| {
                let r = Rect::new(area.x, y, area.width, h);
                y += h;
                r
            })
            .collect()
    }
}

impl Renderable for FlexRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let rects = self.allocate(area);
        for (rect, fc) in rects.into_iter().zip(self.children.iter()) {
            fc.child.render(rect, buf);
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.allocate(Rect::new(0, 0, width, u16::MAX))
            .last()
            .map(|r| r.bottom())
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// ViewportCulledColumn – stacks children with scroll-offset culling
// ---------------------------------------------------------------------------

/// Like `ColumnRenderable` but skips rendering children that fall entirely
/// outside a scroll viewport. `scroll_offset` is the number of rows scrolled
/// past the top of the content.
pub struct ViewportCulledColumn<'a> {
    children: Vec<Box<dyn Renderable + 'a>>,
}

impl<'a> ViewportCulledColumn<'a> {
    pub fn new() -> Self {
        Self { children: vec![] }
    }

    pub fn push(&mut self, child: impl Renderable + 'a) {
        self.children.push(Box::new(child));
    }

    /// Render children that overlap with the viewport defined by
    /// `scroll_offset`..(scroll_offset + area.height).
    pub fn render_with_scroll(&self, area: Rect, buf: &mut Buffer, scroll_offset: u16) -> u16 {
        let viewport_top = scroll_offset;
        let viewport_bottom = scroll_offset + area.height;
        let mut content_y: u16 = 0;
        let total_height = self.desired_height(area.width);

        for child in &self.children {
            let child_h = child.desired_height(area.width);
            let child_bottom = content_y + child_h;

            // Check if child overlaps with viewport
            if child_bottom > viewport_top && content_y < viewport_bottom {
                // Child is (at least partially) visible
                let rel_y = viewport_top.saturating_sub(content_y);

                let visible_h = (child_h - rel_y)
                    .min(area.height.saturating_sub(area.y.saturating_sub(area.y)));

                let child_area = Rect::new(
                    area.x,
                    area.y + content_y.saturating_sub(viewport_top),
                    area.width,
                    visible_h,
                );

                if !child_area.is_empty() && child_area.intersects(area) {
                    child.render(
                        Rect::new(
                            area.x,
                            area.y + content_y.saturating_sub(viewport_top) - rel_y,
                            area.width,
                            child_h,
                        ),
                        buf,
                    );
                }
            }

            content_y += child_h;
        }

        total_height.saturating_sub(area.height)
    }
}

impl Renderable for ViewportCulledColumn<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        // Without scroll, behave like ColumnRenderable
        let mut y = area.y;
        for child in &self.children {
            let h = child.desired_height(area.width);
            let child_area = Rect::new(area.x, y, area.width, h).intersection(area);
            if !child_area.is_empty() {
                child.render(child_area, buf);
            }
            y += h;
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.children.iter().map(|c| c.desired_height(width)).sum()
    }
}
