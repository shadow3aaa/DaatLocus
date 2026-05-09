//!
//! Based on the codex renderable.rs pattern but adapted for DaatLocus:
//! - Renderable: minimal trait (render + desired_height), no cursor support for now.
//! - ColumnRenderable: stacks children vertically.
//! - FlexRenderable: column with flex factors, allocates remaining space proportionally.
//! - ViewportCulledColumn: wraps a column, renders only children overlapping the viewport.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

// ---------------------------------------------------------------------------
// Renderable trait
// ---------------------------------------------------------------------------

#[allow(dead_code)]
/// A renderable item that can produce its own desired height and render into a buffer.
pub trait Renderable {
    /// Render self into `buf` within `area`.  The caller guarantees that `area` fits in `buf`.
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// Return the height (in rows) this item would like to occupy at the given width.
    fn desired_height(&self, width: u16) -> u16;
}

// ---------------------------------------------------------------------------
// ColumnRenderable
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[allow(dead_code)]
/// Stacks children vertically, one after the other.
pub struct ColumnRenderable {
    children: Vec<Box<dyn Renderable>>,
}

#[allow(dead_code)]
impl ColumnRenderable {
    pub fn new() -> Self {
        Self { children: vec![] }
    }

    pub fn push(&mut self, child: impl Renderable + 'static) {
        self.children.push(Box::new(child));
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }
}

impl Renderable for ColumnRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;
        for child in &self.children {
            let child_h = child.desired_height(area.width);
            let child_area = Rect::new(area.x, y, area.width, child_h);
            let clipped = child_area.intersection(area);
            if !clipped.is_empty() {
                child.render(clipped, buf);
            }
            y = y.saturating_add(child_h);
            if y >= area.bottom() {
                break;
            }
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.children
            .iter()
            .map(|c| c.desired_height(width))
            .sum()
    }
}

// ---------------------------------------------------------------------------
// FlexRenderable
// ---------------------------------------------------------------------------
#[allow(dead_code)]

/// Lays out children in a column, allocating remaining height to flex children
/// proportionally to their flex factor.  Loosely inspired by Flutter's Flex widget.
pub struct FlexRenderable {
#[allow(dead_code)]
    children: Vec<FlexChild>,
}

#[allow(dead_code)]
struct FlexChild {
    flex: i32,
    child: Box<dyn Renderable>,
}

#[allow(dead_code)]
impl FlexRenderable {
    pub fn new() -> Self {
        Self { children: vec![] }
    }

    /// Add a child.  `flex` > 0 means the child gets a share of remaining space;
    /// `flex == 0` means the child uses only its `desired_height`.
    pub fn push(&mut self, flex: i32, child: impl Renderable + 'static) {
        self.children.push(FlexChild {
            flex,
            child: Box::new(child),
        });
    }

    /// Allocate vertical space among children and return their Rects.
    fn allocate(&self, area: Rect) -> Vec<Rect> {
        let n = self.children.len();
        if n == 0 {
            return vec![];
        }

        let mut sizes = vec![0u16; n];
        let mut allocated = 0u16;
        let mut total_flex: i32 = 0;
        let mut last_flex_idx: usize = 0;

        let max_h = area.height;

        // Pass 1: non-flex children.
        for (i, fc) in self.children.iter().enumerate() {
            if fc.flex > 0 {
                total_flex += fc.flex;
                last_flex_idx = i;
            } else {
                let h = fc
                    .child
                    .desired_height(area.width)
                    .min(max_h.saturating_sub(allocated));
                sizes[i] = h;
                allocated = allocated.saturating_add(h);
            }
        }

        let free_space = max_h.saturating_sub(allocated);

        // Pass 2: flex children.
        if total_flex > 0 && free_space > 0 {
            let space_per_flex = free_space / total_flex as u16;
            let mut allocated_flex = 0u16;
            for (i, fc) in self.children.iter().enumerate() {
                if fc.flex > 0 {
                    let max_child = if i == last_flex_idx {
                        free_space.saturating_sub(allocated_flex)
                    } else {
                        space_per_flex * fc.flex as u16
                    };
                    let h = fc.child.desired_height(area.width).min(max_child);
                    sizes[i] = h;
                    allocated_flex = allocated_flex.saturating_add(h);
                }
            }
        }

        let mut rects = Vec::with_capacity(n);
        let mut y = area.y;
        for &h in &sizes {
            rects.push(Rect::new(area.x, y, area.width, h));
            y = y.saturating_add(h);
        }
        rects
    }
}

impl Renderable for FlexRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let rects = self.allocate(area);
        for (rect, fc) in rects.into_iter().zip(self.children.iter()) {
            if !rect.is_empty() {
                fc.child.render(rect, buf);
            }
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        // allocate with u16::MAX height so flex children aren't artificially capped
        let rects = self.allocate(Rect::new(0, 0, width, u16::MAX));
        rects.last().map(|r| r.bottom()).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// ViewportCulledColumn
// ---------------------------------------------------------------------------

/// A column of children that only renders those overlapping a scroll viewport.
///
/// Implements `Renderable` using stored scroll offset (set via `set_scroll`).
/// Also exposes `render_with_scroll` for direct scroll control (returns `max_scroll`).
#[allow(dead_code)]
pub struct ViewportCulledColumn {
    children: Vec<Box<dyn Renderable>>,
    scroll: u16,
}

#[allow(dead_code)]
impl ViewportCulledColumn {
    pub fn new() -> Self {
        Self {
            children: vec![],
            scroll: 0,
        }
    }

    pub fn push(&mut self, child: impl Renderable + 'static) {
        self.children.push(Box::new(child));
    }

    /// Set scroll offset for `Renderable::render`.
    pub fn set_scroll(&mut self, scroll: u16) {
        self.scroll = scroll;
    }
}

impl Renderable for ViewportCulledColumn {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let viewport_top = self.scroll;
        let viewport_bottom = self.scroll.saturating_add(area.height);

        let mut y: u16 = 0;
        for child in &self.children {
            let child_h = child.desired_height(area.width);
            let child_bottom = y.saturating_add(child_h);

            if child_bottom > viewport_top && y < viewport_bottom {
                // Child overlaps viewport — compute its screen-relative Rect using
                // signed offset so that cells starting above the viewport get a
                // negative screen_y (their top lines render off-buffer and are
                // silently dropped by ratatui's Paragraph::render).
                let offset: i32 = y as i32 - viewport_top as i32;
                let screen_y: i32 = area.y as i32 + offset;
                let child_area = Rect::new(
                    area.x,
                    screen_y.max(0) as u16,
                    area.width,
                    child_h,
                );
                // Render the full child. Ratatui ignores rows outside the buffer,
                // so partial overlap works correctly — top rows offset above the
                // viewport are dropped, visible rows render at the right offset.
                // Using `intersection` would shift line 0 (title) to the clipped
                // top, causing a sticky-header illusion.
                child.render(child_area, buf);
            }

            y = y.saturating_add(child_h);
            if y >= viewport_bottom {
                break;
            }
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.children
            .iter()
            .map(|c| c.desired_height(width))
            .sum()
    }
}
