//! Markdown rendering for the TUI dashboard.
//!
//! Converts markdown text into styled ratatui [`Line`]s using pulldown-cmark.
//! Supports block-level elements (headings, lists, code blocks, blockquotes,
//! paragraphs, horizontal rules) and inline formatting (bold, italic, code,
//! strikethrough, links).
//!
//! The design follows codex's Writer state-machine pattern: iterate
//! pulldown-cmark events, maintain indent and inline-style stacks, and emit
//! ratatui [`Line`]s with proper prefixes and styling.

use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

// ── Styles ────────────────────────────────────────────────────────────────

struct MdStyles {
    base: Style,
    h1: Style,
    h2: Style,
    h3: Style,
    h4: Style,
    h5: Style,
    h6: Style,
    code: Style,
    emphasis: Style,
    strong: Style,
    strikethrough: Style,
    list_marker: Style,
    blockquote: Style,
}

impl MdStyles {
    fn new(base_color: Color) -> Self {
        Self {
            base: Style::default().fg(base_color),
            h1: Style::default()
                .fg(base_color)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            h2: Style::default().fg(base_color).add_modifier(Modifier::BOLD),
            h3: Style::default().fg(base_color).add_modifier(Modifier::BOLD),
            h4: Style::default()
                .fg(base_color)
                .add_modifier(Modifier::ITALIC),
            h5: Style::default()
                .fg(base_color)
                .add_modifier(Modifier::ITALIC),
            h6: Style::default()
                .fg(base_color)
                .add_modifier(Modifier::ITALIC),
            code: Style::default().fg(Color::Yellow),
            emphasis: Style::default()
                .fg(base_color)
                .add_modifier(Modifier::ITALIC),
            strong: Style::default().fg(base_color).add_modifier(Modifier::BOLD),
            strikethrough: Style::default()
                .fg(base_color)
                .add_modifier(Modifier::CROSSED_OUT),
            list_marker: Style::default().fg(Color::DarkGray),
            blockquote: Style::default().fg(Color::Green),
        }
    }
}

// ── Indent context ────────────────────────────────────────────────────────

/// One level of structural indentation: blockquote `> ` or list prefix.
#[derive(Clone, Debug)]
struct IndentCtx {
    /// Prefix to apply to continuation lines inside this block.
    prefix: String,
    /// Whether this indent is a list (affects blank-line handling).
    is_list: bool,
}

// ── Public API ────────────────────────────────────────────────────────────

/// Render a full markdown text into styled ratatui [`Line`]s.
///
/// Supports:
/// - Paragraphs (with soft/hard breaks)
/// - Headings h1–h6
/// - Blockquotes (nested)
/// - Unordered (`-`, `*`) and ordered (`1.`) lists (nested)
/// - Fenced and indented code blocks
/// - Horizontal rules (`---`, `***`)
/// - Inline: **bold**, *italic*, `code`, ~~strikethrough~~, [links](url)
pub fn render_markdown(input: &str, base_color: Color) -> Vec<Line<'static>> {
    if input.is_empty() {
        return Vec::new();
    }
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(input, options);
    let mut w = Writer::new(parser, base_color);
    w.run();
    w.lines
}

// ── Writer ────────────────────────────────────────────────────────────────

struct Writer<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    iter: I,
    lines: Vec<Line<'static>>,
    styles: MdStyles,
    /// Stack of active inline styles (bold, italic, heading, etc.)
    inline_styles: Vec<Style>,
    /// Stack of structural indents (blockquotes, lists).
    indent_stack: Vec<IndentCtx>,
    /// Per-list-level next item index (None = unordered, Some(n) = ordered).
    list_counters: Vec<Option<u64>>,
    /// Whether a blank line is needed before the next list item at this depth.
    list_item_needs_blank: Vec<bool>,
    /// Current link URL being built (None when not inside a link).
    link_url: Option<String>,
    /// Need a newline before the next piece of text.
    needs_newline: bool,
    /// The line currently being built; spans are appended to it.
    current: Line<'static>,
    /// Whether to emit a marker (bullet / number) on the next text event.
    pending_marker: bool,
    /// Inside a code block.
    in_code_block: bool,
    /// Language tag of the current code block (for fenced blocks).
    code_block_lang: Option<String>,
}

impl<'a, I> Writer<'a, I>
where
    I: Iterator<Item = Event<'a>>,
{
    fn new(iter: I, base_color: Color) -> Self {
        Self {
            iter,
            lines: Vec::new(),
            styles: MdStyles::new(base_color),
            inline_styles: Vec::new(),
            indent_stack: Vec::new(),
            list_counters: Vec::new(),
            list_item_needs_blank: Vec::new(),
            link_url: None,
            needs_newline: false,
            current: Line::default(),
            pending_marker: false,
            in_code_block: false,
            code_block_lang: None,
        }
    }

    fn run(&mut self) {
        while let Some(ev) = self.iter.next() {
            self.handle_event(ev);
        }
        self.flush_current();
    }

    // ── event dispatch ────────────────────────────────────────────────

    fn handle_event(&mut self, event: Event<'a>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(text),
            Event::Code(code) => self.inline_code(code),
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => {
                self.flush_current();
                self.push_line(Line::from("───"));
                self.needs_newline = true;
            }
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
        }
    }

    // ── tags ──────────────────────────────────────────────────────────

    fn start_tag(&mut self, tag: Tag<'a>) {
        match tag {
            Tag::Paragraph => {
                self.needs_newline = true;
            }
            Tag::Heading { level, .. } => self.start_heading(level),
            Tag::BlockQuote(_) => self.start_blockquote(),
            Tag::CodeBlock(kind) => self.start_codeblock(kind),
            Tag::List(start) => self.start_list(start),
            Tag::Item => self.start_item(),
            Tag::Emphasis => self.push_inline(self.styles.emphasis),
            Tag::Strong => self.push_inline(self.styles.strong),
            Tag::Strikethrough => self.push_inline(self.styles.strikethrough),
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.to_string());
            }
            Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::Image { .. }
            | Tag::MetadataBlock(_) => {}
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.needs_newline = true;
            }
            TagEnd::Heading(_) => {
                self.pop_inline();
                self.needs_newline = true;
            }
            TagEnd::BlockQuote(_) => {
                self.indent_stack.pop();
                self.needs_newline = true;
            }
            TagEnd::CodeBlock => {
                self.indent_stack.pop();
                self.in_code_block = false;
                self.code_block_lang = None;
                self.needs_newline = true;
            }
            TagEnd::List(_) => {
                self.list_counters.pop();
                self.list_item_needs_blank.pop();
                self.needs_newline = true;
            }
            TagEnd::Item => {
                if self.list_item_needs_blank.last_mut().is_some_and(|n| *n)
                    && let Some(last) = self.list_item_needs_blank.last_mut()
                {
                    *last = false;
                }
                self.indent_stack.pop();
                self.pending_marker = false;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_inline(),
            TagEnd::Link => {
                self.flush_link_url();
            }
            TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::Image
            | TagEnd::MetadataBlock(_) => {}
            _ => {}
        }
    }

    // ── block elements ────────────────────────────────────────────────

    fn start_heading(&mut self, level: HeadingLevel) {
        self.flush_current();
        self.blank_line_if_needed();
        let marker = "#".repeat(level as usize);
        let style = match level {
            HeadingLevel::H1 => self.styles.h1,
            HeadingLevel::H2 => self.styles.h2,
            HeadingLevel::H3 => self.styles.h3,
            HeadingLevel::H4 => self.styles.h4,
            HeadingLevel::H5 => self.styles.h5,
            HeadingLevel::H6 => self.styles.h6,
        };
        self.push_line(Line::from(vec![Span::styled(format!("{marker} "), style)]));
        self.push_inline(style);
    }

    fn start_blockquote(&mut self) {
        self.flush_current();
        self.blank_line_if_needed();
        let prefix = "│ ".to_string();
        self.indent_stack.push(IndentCtx {
            prefix,
            is_list: false,
        });
    }

    fn start_list(&mut self, start: Option<u64>) {
        self.flush_current();
        self.list_counters.push(start);
        self.list_item_needs_blank.push(false);
    }

    fn start_item(&mut self) {
        if self.list_item_needs_blank.last().copied().unwrap_or(false) {
            self.push_blank_line();
        }
        if let Some(last) = self.list_item_needs_blank.last_mut() {
            *last = false;
        }
        self.pending_marker = true;

        // Build marker text and indent
        let marker = if let Some(counter) = self.list_counters.last_mut() {
            match counter {
                None => "• ".to_string(),
                Some(n) => {
                    *n += 1;
                    format!("{}. ", *n - 1)
                }
            }
        } else {
            "• ".to_string()
        };

        // Continuation indent: align with text after marker
        let prefix = " ".repeat(marker.len());

        self.indent_stack.push(IndentCtx {
            prefix,
            is_list: true,
        });

        // Emit the marker as the first line of this list item
        self.push_line(Line::from(vec![Span::styled(
            marker,
            self.styles.list_marker,
        )]));
        self.needs_newline = false;
    }

    fn start_codeblock(&mut self, kind: pulldown_cmark::CodeBlockKind) {
        use pulldown_cmark::CodeBlockKind;
        self.flush_current();
        self.push_blank_line();
        self.in_code_block = true;
        let (lang, prefix) = match kind {
            CodeBlockKind::Fenced(info) => {
                let lang = info
                    .split([',', ' ', '\t'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                (lang, String::new())
            }
            CodeBlockKind::Indented => (None, "    ".to_string()),
        };
        self.code_block_lang = lang;
        self.indent_stack.push(IndentCtx {
            prefix,
            is_list: false,
        });
        self.needs_newline = true;
    }

    // ── inline / text ─────────────────────────────────────────────────

    fn text(&mut self, text: CowStr<'a>) {
        if self.pending_marker {
            self.push_line(Line::default());
            self.pending_marker = false;
        }
        if self.in_code_block {
            for (i, line) in text.lines().enumerate() {
                if i > 0 || self.needs_newline {
                    self.flush_current();
                    self.needs_newline = false;
                }
                self.push_code_line(line);
            }
            return;
        }
        for (i, line) in text.lines().enumerate() {
            if i > 0 || self.needs_newline {
                self.flush_current();
                self.needs_newline = false;
            }
            let style = self
                .inline_styles
                .last()
                .copied()
                .unwrap_or(self.styles.base);
            self.current
                .push_span(Span::styled(line.to_string(), style));
        }
    }

    fn inline_code(&mut self, code: CowStr<'a>) {
        if self.pending_marker {
            self.push_line(Line::default());
            self.pending_marker = false;
        }
        self.current
            .push_span(Span::styled(code.to_string(), self.styles.code));
    }

    fn soft_break(&mut self) {
        if self.pending_marker {
            return;
        }
        self.flush_current();
        self.needs_newline = false;
    }

    fn hard_break(&mut self) {
        self.flush_current();
        self.needs_newline = true;
    }

    // ── inline style stack ────────────────────────────────────────────

    fn push_inline(&mut self, style: Style) {
        let current = self
            .inline_styles
            .last()
            .copied()
            .unwrap_or(self.styles.base);
        self.inline_styles.push(current.patch(style));
    }

    fn pop_inline(&mut self) {
        self.inline_styles.pop();
    }

    fn flush_link_url(&mut self) {
        if let Some(url) = self.link_url.take() {
            self.current.push_span(Span::styled(
                format!(" ({url})"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    // ── line management ───────────────────────────────────────────────

    /// Push a code line (already styled) with prefix.
    fn push_code_line(&mut self, text: &str) {
        let prefix = self.prefix_spans(false);
        let mut spans: Vec<Span<'static>> = prefix
            .into_iter()
            .map(|s| Span::styled(s.content.to_string(), s.style))
            .collect();
        spans.push(Span::styled(text.to_string(), self.styles.code));
        self.lines.push(Line::from(spans));
    }

    /// Flush the current line (with prefix) into self.lines.
    fn flush_current(&mut self) {
        if self.current.spans.is_empty() {
            return;
        }
        let prefix = self.prefix_spans(false);
        let content = std::mem::take(&mut self.current);
        if prefix.is_empty() {
            self.lines.push(content);
        } else {
            let mut spans: Vec<Span<'static>> = prefix
                .into_iter()
                .map(|s| Span::styled(s.content.to_string(), s.style))
                .collect();
            spans.extend(content.spans);
            self.lines.push(Line::from(spans));
        }
    }

    /// Push a line as-is (no prefix), flushing current first.
    fn push_line(&mut self, line: Line<'static>) {
        self.flush_current();
        self.lines.push(line);
    }

    fn push_blank_line(&mut self) {
        self.flush_current();
        // In pure-list contexts, emit a plain blank line without blockquote prefix.
        if self.indent_stack.iter().all(|ctx| ctx.is_list) {
            self.lines.push(Line::default());
        } else {
            self.push_line(Line::default());
            self.flush_current();
        }
    }

    fn blank_line_if_needed(&mut self) {
        if self.needs_newline && !self.lines.is_empty() {
            // Only add blank if the last line is not already blank
            let last_blank = self
                .lines
                .last()
                .map(|l| l.spans.is_empty())
                .unwrap_or(true);
            if !last_blank {
                self.lines.push(Line::default());
            }
        }
        self.needs_newline = false;
    }

    // ── prefix assembly ───────────────────────────────────────────────

    /// Build the prefix spans for the current line based on the indent stack.
    ///
    /// When `include_marker` is true, the deepest list entry contributes its
    /// marker text (replacing the normal indent).
    fn prefix_spans(&self, include_marker: bool) -> Vec<Span<'static>> {
        if self.indent_stack.is_empty() {
            return Vec::new();
        }
        let mut parts: Vec<Span<'static>> = Vec::new();
        for (i, ctx) in self.indent_stack.iter().enumerate() {
            let is_last = i == self.indent_stack.len() - 1;
            if is_last && include_marker && ctx.is_list {
                // Marker line: skip the innermost prefix; marker text is added separately
                continue;
            }
            let style = if ctx.is_list {
                Style::default()
            } else {
                // blockquote style
                self.styles.blockquote
            };
            parts.push(Span::styled(ctx.prefix.clone(), style));
        }
        parts
    }
}
