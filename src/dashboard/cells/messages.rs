use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde::{Deserialize, Serialize};

use crate::tool_ui::{
    PatchDiffLineKind, PatchDiffLineUiData, PatchFileUiData, PatchUiData, ReplyDisposition,
    ReplySubject, ReplyUiData, TelegramUiAction, TelegramUiData,
};

use super::highlight::{diff_scope_backgrounds, highlight_patch_lines};
use super::primitives::{Cell, render_message_activity_lines};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchActivityCell {
    pub summary_line: String,
    pub files: Vec<PatchFileUiData>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramActivityCell {
    pub title: String,
    pub detail_lines: Vec<String>,
    pub message_lines: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyActivityCell {
    pub disposition: ReplyDisposition,
    pub subject: ReplySubject,
    pub message_lines: Vec<String>,
}

impl Cell for PatchActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        let visible_files = limit_patch_files(&self.files, 4);
        let diff_backgrounds = diff_scope_backgrounds();
        let old_lineno_width = visible_files
            .iter()
            .flat_map(|file| file.diff_lines.iter().filter_map(|line| line.old_lineno))
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(1);
        let new_lineno_width = visible_files
            .iter()
            .flat_map(|file| file.diff_lines.iter().filter_map(|line| line.new_lineno))
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(1);
        let file_noun = if self.files.len() == 1 {
            "File"
        } else {
            "Files"
        };

        let mut lines = vec![Line::from(vec![
            Span::styled(
                "∂",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("Edited {} {}", self.files.len(), file_noun),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
        ])];
        if !visible_files.is_empty() {
            lines.push(Line::from(""));
        }
        for (index, file) in visible_files.iter().enumerate() {
            if index > 0 {
                lines.push(Line::from(""));
            }
            lines.push(render_patch_file_header(file));
            lines.extend(render_patch_file_diff_lines(
                file,
                diff_backgrounds,
                old_lineno_width,
                new_lineno_width,
                18,
            ));
        }
        if self.files.len() > visible_files.len() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                format!("… {} more files", self.files.len() - visible_files.len()),
                Style::default().fg(Color::DarkGray),
            )]));
        }
        lines
    }
}

impl Cell for TelegramActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        render_message_activity_lines(
            "◦",
            Color::Cyan,
            &self.title,
            &self.detail_lines,
            &self.message_lines,
            6,
            6,
        )
    }
}

impl Cell for ReplyActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        let (title, color) = match self.disposition {
            ReplyDisposition::Resolved => (self.resolved_title(), Color::LightGreen),
            ReplyDisposition::Dismissed => ("Dismissed", Color::DarkGray),
            ReplyDisposition::Failed => ("Failed", Color::Red),
        };
        let mut lines = vec![Line::from(vec![
            Span::styled("✣", Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(
                title,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ])];
        for line in self.message_lines.iter().take(8) {
            lines.push(Line::from(vec![Span::styled(
                line.to_string(),
                Style::default().fg(Color::White),
            )]));
        }
        lines
    }
}

impl ReplyActivityCell {
    fn resolved_title(&self) -> &'static str {
        match self.subject {
            ReplySubject::Message => "Resolved Message",
            ReplySubject::Notice => "Resolved Notice",
        }
    }
}

impl From<PatchUiData> for PatchActivityCell {
    fn from(data: PatchUiData) -> Self {
        PatchActivityCell {
            summary_line: data.summary_line,
            files: data.files,
        }
    }
}

impl From<TelegramUiData> for TelegramActivityCell {
    fn from(data: TelegramUiData) -> Self {
        let mut detail_lines = data.detail_lines;
        if detail_lines.is_empty() {
            detail_lines.push(match data.action {
                TelegramUiAction::ListChats => "list chats".to_string(),
                TelegramUiAction::ReadHistory => "read history".to_string(),
                TelegramUiAction::SelectChat => "select chat".to_string(),
                TelegramUiAction::SendMessage => "send message".to_string(),
                TelegramUiAction::ResolveChat => "resolve chat".to_string(),
            });
        }
        TelegramActivityCell {
            title: data.title,
            detail_lines,
            message_lines: data.message_lines,
        }
    }
}

impl From<ReplyUiData> for ReplyActivityCell {
    fn from(data: ReplyUiData) -> Self {
        ReplyActivityCell {
            disposition: data.disposition,
            subject: data.subject,
            message_lines: data.message_lines,
        }
    }
}

fn limit_patch_files(files: &[PatchFileUiData], limit: usize) -> Vec<PatchFileUiData> {
    if files.len() <= limit {
        return files.to_vec();
    }
    files.iter().take(limit).cloned().collect()
}

fn render_patch_file_header(file: &PatchFileUiData) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(Span::styled(
        file.path.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled("(", Style::default().fg(Color::DarkGray)));
    spans.push(Span::styled(
        format!("+{}", file.added_lines),
        Style::default().fg(Color::LightGreen),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("-{}", file.removed_lines),
        Style::default().fg(Color::LightRed),
    ));
    spans.push(Span::styled(")", Style::default().fg(Color::DarkGray)));
    Line::from(spans)
}

fn render_patch_file_diff_lines(
    file: &PatchFileUiData,
    diff_backgrounds: super::highlight::DiffScopeBackgrounds,
    old_lineno_width: usize,
    new_lineno_width: usize,
    line_limit: usize,
) -> Vec<Line<'static>> {
    let highlighted = highlight_patch_lines(&file.path, &file.diff_lines);
    let visible_lines = file.diff_lines.iter().take(line_limit).collect::<Vec<_>>();
    let mut lines = visible_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            render_patch_diff_line(
                line,
                highlighted
                    .get(index)
                    .and_then(|spans| spans.as_ref())
                    .cloned(),
                diff_backgrounds,
                old_lineno_width,
                new_lineno_width,
            )
        })
        .collect::<Vec<_>>();
    if file.diff_lines.len() > visible_lines.len() {
        lines.push(Line::from(vec![
            Span::styled("…", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(
                format!(
                    "{} more line(s)",
                    file.diff_lines.len() - visible_lines.len()
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    lines
}

fn render_patch_diff_line(
    line: &PatchDiffLineUiData,
    highlighted_spans: Option<Vec<Span<'static>>>,
    diff_backgrounds: super::highlight::DiffScopeBackgrounds,
    old_lineno_width: usize,
    new_lineno_width: usize,
) -> Line<'static> {
    if matches!(line.kind, PatchDiffLineKind::HunkBreak) {
        return Line::from(vec![Span::styled(
            format!(
                "{:>old_width$} {:>new_width$} ⋮",
                "",
                "",
                old_width = old_lineno_width,
                new_width = new_lineno_width
            ),
            Style::default().fg(Color::DarkGray),
        )]);
    }

    let (gutter, text_style, background) = match line.kind {
        PatchDiffLineKind::Context => (" ", Style::default().fg(Color::Gray), None),
        PatchDiffLineKind::Delete => (
            "-",
            Style::default().fg(Color::LightRed),
            diff_backgrounds.deleted.or(Some(Color::Rgb(58, 24, 24))),
        ),
        PatchDiffLineKind::Add => (
            "+",
            Style::default().fg(Color::LightGreen),
            diff_backgrounds.inserted.or(Some(Color::Rgb(22, 44, 30))),
        ),
        PatchDiffLineKind::HunkBreak => unreachable!(),
    };
    let old_lineno = line
        .old_lineno
        .map(|lineno| lineno.to_string())
        .unwrap_or_default();
    let new_lineno = line
        .new_lineno
        .map(|lineno| lineno.to_string())
        .unwrap_or_default();

    let mut spans = vec![
        Span::styled(
            format!("{old_lineno:>old_width$}", old_width = old_lineno_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{new_lineno:>new_width$}", new_width = new_lineno_width),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(gutter, text_style.add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ];
    if let Some(highlighted_spans) = highlighted_spans {
        for span in highlighted_spans {
            let style = match background {
                Some(color) => span.style.bg(color),
                None => span.style,
            };
            spans.push(Span::styled(span.content.to_string(), style));
        }
    } else {
        let style = match background {
            Some(color) => text_style.bg(color),
            None => text_style,
        };
        spans.push(Span::styled(line.text.clone(), style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_ui::PatchFileOperation;

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn patch_activity_cell_renders_diff_lines() {
        let cell = PatchActivityCell {
            summary_line: "1 file(s) changed (+1 -1)".to_string(),
            files: vec![PatchFileUiData {
                path: "src/app.rs".to_string(),
                operation: PatchFileOperation::Update,
                added_lines: 1,
                removed_lines: 1,
                diff_lines: vec![
                    PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Context,
                        old_lineno: Some(1),
                        new_lineno: Some(1),
                        text: "fn main() {".to_string(),
                    },
                    PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Delete,
                        old_lineno: Some(2),
                        new_lineno: None,
                        text: "    println!(\"old\");".to_string(),
                    },
                    PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Add,
                        old_lineno: None,
                        new_lineno: Some(2),
                        text: "    println!(\"new\");".to_string(),
                    },
                ],
            }],
        };

        let lines = cell.render_lines();
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("∂  Edited 1 File"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("src/app.rs (+1 -1)"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains('-') && line.contains("println!(\"old\");"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains('+') && line.contains("println!(\"new\");"))
        );
    }

    #[test]
    fn reply_activity_cell_distinguishes_message_and_notice_resolution() {
        let message = ReplyActivityCell {
            disposition: ReplyDisposition::Resolved,
            subject: ReplySubject::Message,
            message_lines: Vec::new(),
        }
        .render_lines()
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert!(message.iter().any(|line| line.contains("Resolved Message")));

        let notice = ReplyActivityCell {
            disposition: ReplyDisposition::Resolved,
            subject: ReplySubject::Notice,
            message_lines: Vec::new(),
        }
        .render_lines()
        .into_iter()
        .map(|line| line_text(&line))
        .collect::<Vec<_>>();
        assert!(notice.iter().any(|line| line.contains("Resolved Notice")));
    }
}
