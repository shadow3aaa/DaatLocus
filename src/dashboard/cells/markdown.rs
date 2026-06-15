//! Markdown rendering for dashboard activity cells.
//!
//! This renderer is intentionally small and deterministic. It keeps markdown
//! source as the input of record, parses with `pulldown-cmark`, and can wrap
//! block text at a caller-provided width so resize behavior is controlled by
//! Daat rather than by a second render pass.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::highlight::highlight_block;

#[derive(Clone, Copy)]
struct MarkdownStyles {
    base: Style,
    emphasis: Style,
    strong: Style,
    strikethrough: Style,
    code: Style,
    link: Style,
    blockquote: Style,
}

impl MarkdownStyles {
    fn new(base_color: Color) -> Self {
        Self {
            base: Style::default().fg(base_color),
            emphasis: Style::default()
                .fg(base_color)
                .add_modifier(Modifier::ITALIC),
            strong: Style::default().fg(base_color).add_modifier(Modifier::BOLD),
            strikethrough: Style::default()
                .fg(base_color)
                .add_modifier(Modifier::CROSSED_OUT),
            code: Style::default().fg(Color::Yellow),
            link: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::UNDERLINED),
            blockquote: Style::default().fg(Color::Green),
        }
    }
}

struct RenderedMarkdownLine {
    line: Line<'static>,
    code_block: bool,
    initial_indent: String,
    subsequent_indent: String,
}

struct ListState {
    next: Option<u64>,
}

struct MarkdownWriter {
    lines: Vec<RenderedMarkdownLine>,
    styles: MarkdownStyles,
    style_stack: Vec<Style>,
    current: Vec<Span<'static>>,
    current_style: Style,
    current_code_block: bool,
    current_initial_indent: String,
    current_subsequent_indent: String,
    list_stack: Vec<ListState>,
    blockquote_depth: usize,
    in_code_block: bool,
    code_block_language: Option<String>,
    code_block_indent: String,
    code_block_buffer: String,
    link: Option<String>,
    wrap_width: Option<usize>,
}

impl MarkdownWriter {
    fn new(base_color: Color, wrap_width: Option<u16>) -> Self {
        let styles = MarkdownStyles::new(base_color);
        Self {
            lines: Vec::new(),
            styles,
            style_stack: vec![styles.base],
            current: Vec::new(),
            current_style: styles.base,
            current_code_block: false,
            current_initial_indent: String::new(),
            current_subsequent_indent: String::new(),
            list_stack: Vec::new(),
            blockquote_depth: 0,
            in_code_block: false,
            code_block_language: None,
            code_block_indent: String::new(),
            code_block_buffer: String::new(),
            link: None,
            wrap_width: wrap_width.map(usize::from).filter(|width| *width > 0),
        }
    }

    fn run(mut self, input: &str) -> Vec<Line<'static>> {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_TASKLISTS);

        for event in Parser::new_ext(input, options) {
            self.handle_event(event);
        }
        self.flush_current();
        self.lines
            .into_iter()
            .flat_map(|line| wrap_rendered_line(line, self.wrap_width))
            .collect()
    }

    fn handle_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.push_text(text.as_ref()),
            Event::Code(code) => self.push_span(Span::styled(code.to_string(), self.styles.code)),
            Event::SoftBreak if self.in_code_block => self.push_code_text("\n"),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak if self.in_code_block => self.push_code_text("\n"),
            Event::HardBreak => self.flush_current(),
            Event::Rule => {
                self.flush_current();
                self.push_line(Line::from(Span::styled("———", self.styles.base)), false);
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(html.as_ref()),
            Event::FootnoteReference(_) => {}
            Event::TaskListMarker(checked) => {
                self.push_text(if checked { "[x] " } else { "[ ] " });
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => self.push_text(math.as_ref()),
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.start_block_line(),
            Tag::Heading { level, .. } => self.start_heading(level),
            Tag::BlockQuote(_) => {
                self.flush_current();
                self.blockquote_depth = self.blockquote_depth.saturating_add(1);
            }
            Tag::CodeBlock(kind) => self.start_code_block(kind),
            Tag::List(start) => self.list_stack.push(ListState { next: start }),
            Tag::Item => self.start_list_item(),
            Tag::Emphasis => self.push_style(self.styles.emphasis),
            Tag::Strong => self.push_style(self.styles.strong),
            Tag::Strikethrough => self.push_style(self.styles.strikethrough),
            Tag::Link { dest_url, .. } => {
                self.link = Some(dest_url.to_string());
                self.push_style(self.styles.link);
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                self.push_text("[image");
                if !title.is_empty() {
                    self.push_text(": ");
                    self.push_text(title.as_ref());
                }
                if !dest_url.is_empty() {
                    self.push_text(" ");
                    self.push_text(dest_url.as_ref());
                }
                self.push_text("]");
            }
            Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::FootnoteDefinition(_)
            | Tag::MetadataBlock(_)
            | Tag::HtmlBlock
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Subscript
            | Tag::Superscript => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush_current(),
            TagEnd::BlockQuote(_) => {
                self.flush_current();
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.flush_current();
                self.flush_code_block();
                self.in_code_block = false;
                self.current_code_block = false;
                self.code_block_language = None;
                self.code_block_indent.clear();
                self.code_block_buffer.clear();
                self.current_style = self.styles.base;
            }
            TagEnd::List(_) => {
                self.flush_current();
                self.list_stack.pop();
            }
            TagEnd::Item => self.flush_current(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if let Some(link) = self.link.take() {
                    self.push_text(" (");
                    self.push_span(Span::styled(link, self.styles.link));
                    self.push_text(")");
                }
            }
            TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::FootnoteDefinition
            | TagEnd::MetadataBlock(_)
            | TagEnd::HtmlBlock
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Subscript
            | TagEnd::Superscript
            | TagEnd::Image => {}
        }
    }

    fn start_block_line(&mut self) {
        self.flush_current();
        self.current_initial_indent = blockquote_prefix(self.blockquote_depth);
        self.current_subsequent_indent = self.current_initial_indent.clone();
        if self.blockquote_depth > 0 {
            self.current_style = self.styles.blockquote;
        }
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        self.flush_current();
        let modifier = match level {
            HeadingLevel::H1 => Modifier::BOLD | Modifier::UNDERLINED,
            HeadingLevel::H2 | HeadingLevel::H3 => Modifier::BOLD,
            _ => Modifier::ITALIC,
        };
        self.current_style = self.styles.base.add_modifier(modifier);
        self.push_style(self.current_style);
    }

    fn start_code_block(&mut self, kind: CodeBlockKind<'_>) {
        self.flush_current();
        self.in_code_block = true;
        self.current_code_block = true;
        self.current_style = self.styles.code;
        self.code_block_buffer.clear();
        self.code_block_language = match kind {
            CodeBlockKind::Fenced(info) => {
                let language = info.split_whitespace().next().unwrap_or_default().trim();
                (!language.is_empty()).then(|| language.to_string())
            }
            CodeBlockKind::Indented => {
                self.code_block_indent = "    ".to_string();
                None
            }
        };
        if self.code_block_indent.is_empty() {
            self.current_initial_indent.clear();
            self.current_subsequent_indent.clear();
        } else {
            self.current_initial_indent = self.code_block_indent.clone();
            self.current_subsequent_indent = self.code_block_indent.clone();
        }
    }

    fn start_list_item(&mut self) {
        self.flush_current();
        let depth = self.list_stack.len().saturating_sub(1);
        let outer_indent = "  ".repeat(depth);
        let marker = self
            .list_stack
            .last_mut()
            .and_then(|state| {
                state.next.map(|value| {
                    state.next = Some(value.saturating_add(1));
                    format!("{value}. ")
                })
            })
            .unwrap_or_else(|| "- ".to_string());
        let prefix = format!(
            "{}{}{}",
            blockquote_prefix(self.blockquote_depth),
            outer_indent,
            marker
        );
        let subsequent = " ".repeat(prefix.width());
        self.current_initial_indent = prefix;
        self.current_subsequent_indent = subsequent;
    }

    fn push_text(&mut self, text: &str) {
        if self.in_code_block {
            self.push_code_text(text);
            return;
        }
        self.push_span(Span::styled(text.to_string(), self.current_inline_style()));
    }

    fn push_code_text(&mut self, text: &str) {
        self.code_block_buffer.push_str(text);
    }

    fn flush_code_block(&mut self) {
        let text = std::mem::take(&mut self.code_block_buffer);
        let highlighted = self
            .code_block_language
            .as_deref()
            .and_then(|language| highlight_block(&text, language));
        if let Some(highlighted_lines) = highlighted {
            for line_spans in highlighted_lines {
                self.push_code_spans(line_spans, true);
            }
            return;
        }

        let mut parts = text.split('\n').peekable();
        while let Some(part) = parts.next() {
            let part = part.trim_end_matches('\r');
            if part.is_empty() && parts.peek().is_none() {
                continue;
            }
            self.push_code_spans(
                vec![Span::styled(part.to_string(), self.styles.code)],
                false,
            );
        }
    }

    fn push_code_spans(&mut self, line_spans: Vec<Span<'static>>, highlighted: bool) {
        let mut spans = Vec::new();
        if !self.code_block_indent.is_empty() {
            spans.push(Span::styled(
                self.code_block_indent.clone(),
                self.styles.code,
            ));
        }
        if line_spans.is_empty() {
            spans.push(Span::raw(String::new()));
        } else {
            spans.extend(line_spans);
        }
        let mut line = Line::from(spans);
        if !highlighted {
            line.style = self.styles.code;
        }
        self.push_line(line, true);
    }

    fn push_span(&mut self, span: Span<'static>) {
        self.current.push(span);
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(style);
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    fn current_inline_style(&self) -> Style {
        *self.style_stack.last().unwrap_or(&self.styles.base)
    }

    fn flush_current(&mut self) {
        if self.current.is_empty() {
            self.current_initial_indent.clear();
            self.current_subsequent_indent.clear();
            self.current_style = self.styles.base;
            return;
        }
        let mut spans = Vec::new();
        if !self.current_initial_indent.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut self.current_initial_indent),
                self.current_style,
            ));
        }
        spans.append(&mut self.current);
        let mut line = Line::from(spans);
        line.style = self.current_style;
        self.lines.push(RenderedMarkdownLine {
            line,
            code_block: self.current_code_block,
            initial_indent: String::new(),
            subsequent_indent: std::mem::take(&mut self.current_subsequent_indent),
        });
        self.current_code_block = self.in_code_block;
        self.current_style = if self.blockquote_depth > 0 {
            self.styles.blockquote
        } else {
            self.styles.base
        };
    }

    fn push_line(&mut self, line: Line<'static>, code_block: bool) {
        self.lines.push(RenderedMarkdownLine {
            line,
            code_block,
            initial_indent: String::new(),
            subsequent_indent: String::new(),
        });
    }
}

fn blockquote_prefix(depth: usize) -> String {
    "> ".repeat(depth)
}

fn wrap_rendered_line(line: RenderedMarkdownLine, wrap_width: Option<usize>) -> Vec<Line<'static>> {
    let Some(width) = wrap_width else {
        return vec![line.line];
    };
    if line.code_block || line.line.width() <= width {
        return vec![line.line];
    }
    let text = rendered_line_text(&line.line);
    let initial_indent = line.initial_indent;
    let subsequent_indent = line.subsequent_indent;
    let options = textwrap::Options::new(width)
        .initial_indent(&initial_indent)
        .subsequent_indent(&subsequent_indent)
        .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit);
    textwrap::wrap(&text, options)
        .into_iter()
        .map(|wrapped| {
            let mut rendered = Line::from(Span::styled(wrapped.into_owned(), line.line.style));
            rendered.style = line.line.style;
            rendered
        })
        .collect()
}

fn rendered_line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

pub fn render_markdown(input: &str, base_color: Color) -> Vec<Line<'static>> {
    render_markdown_with_width(input, base_color, None)
}

pub fn render_markdown_with_width(
    input: &str,
    base_color: Color,
    width: Option<u16>,
) -> Vec<Line<'static>> {
    if input.is_empty() {
        return Vec::new();
    }
    MarkdownWriter::new(base_color, width).run(input)
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::{render_markdown, render_markdown_with_width, rendered_line_text};

    fn rendered_text(input: &str) -> Vec<String> {
        render_markdown(input, Color::White)
            .into_iter()
            .map(|line| rendered_line_text(&line))
            .collect()
    }

    #[test]
    fn headings_drop_atx_markers() {
        let lines = rendered_text("# Markdown 渲染测试\n\n## 基础样式");

        assert_eq!(lines.first().map(String::as_str), Some("Markdown 渲染测试"));
        assert!(lines.iter().any(|line| line == "基础样式"));
        assert!(!lines.iter().any(|line| line.starts_with('#')));
    }

    #[test]
    fn headings_keep_heading_style_after_marker_removal() {
        let line = render_markdown("# Markdown 渲染测试", Color::White)
            .into_iter()
            .next()
            .expect("expected heading line");

        assert_eq!(rendered_line_text(&line), "Markdown 渲染测试");
        assert!(line.style.add_modifier.contains(Modifier::BOLD));
        assert!(line.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn fenced_code_blocks_drop_delimiter_lines() {
        let lines = rendered_text("```rust\nfn main() {}\n```");
        let joined = lines.join("\n");

        assert!(joined.contains("fn main() {}"));
        assert!(!joined.contains("```"));
    }

    #[test]
    fn fenced_code_blocks_use_syntax_highlight_spans() {
        let lines = render_markdown("```rust\nlet message = \"hello\";\n```", Color::White);

        assert!(
            lines.iter().flat_map(|line| &line.spans).any(|span| {
                matches!(span.style.fg, Some(Color::Rgb(_, _, _))) && span.content.contains("hello")
            }),
            "fenced code blocks should keep truecolor syntax spans: {lines:?}"
        );
    }

    #[test]
    fn wraps_list_items_preserving_indent() {
        let lines =
            render_markdown_with_width("- first second third fourth", Color::White, Some(14))
                .into_iter()
                .map(|line| rendered_line_text(&line))
                .collect::<Vec<_>>();

        assert_eq!(lines, vec!["- first second", "  third fourth"]);
    }

    #[test]
    fn wraps_nested_lists_and_blockquotes() {
        let markdown = "- outer item with several words to wrap\n  - inner item that also needs wrapping\n> quoted text that should wrap";
        let lines = render_markdown_with_width(markdown, Color::White, Some(22))
            .into_iter()
            .map(|line| rendered_line_text(&line))
            .collect::<Vec<_>>();

        assert!(lines.contains(&"- outer item with".to_string()));
        assert!(lines.contains(&"  several words to".to_string()));
        assert!(lines.iter().any(|line| line.starts_with("  - inner")));
        assert!(lines.iter().any(|line| line.starts_with("> quoted text")));
    }

    #[test]
    fn appends_link_destination() {
        let lines = rendered_text("[Daat](https://example.com)");
        assert_eq!(lines, vec!["Daat (https://example.com)"]);
    }

    #[test]
    fn does_not_wrap_code_blocks() {
        let markdown = "````\nfn main() { println!(\"hi from a long line\"); }\n````";
        let lines = render_markdown_with_width(markdown, Color::White, Some(10))
            .into_iter()
            .map(|line| rendered_line_text(&line))
            .collect::<Vec<_>>();

        assert_eq!(
            lines,
            vec!["fn main() { println!(\"hi from a long line\"); }"]
        );
    }
}
