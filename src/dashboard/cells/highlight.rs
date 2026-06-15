use std::{path::Path, sync::OnceLock};

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Highlighter, Theme, ThemeSet},
    parsing::{Scope, SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

#[cfg(test)]
use std::str::FromStr;
#[cfg(test)]
use syntect::highlighting::{StyleModifier, ThemeItem, ThemeSettings};

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn theme() -> &'static Theme {
    THEME_SET
        .get_or_init(ThemeSet::load_defaults)
        .themes
        .get("base16-ocean.dark")
        .expect("syntect default themes include base16-ocean.dark")
}

fn find_syntax_for_path(path: &str) -> Option<&'static SyntaxReference> {
    let ss = syntax_set();
    let path = Path::new(path);
    let file_name = path.file_name().and_then(|name| name.to_str());
    if let Some(syntax) = file_name.and_then(|name| ss.find_syntax_by_extension(name)) {
        return Some(syntax);
    }
    let syntax_name = path
        .extension()
        .and_then(|extension| extension.to_str())
        .or(file_name)?;
    let lower = syntax_name.to_ascii_lowercase();
    let patched = match lower.as_str() {
        "csharp" => "c#",
        "golang" => "go",
        "jsx" => "JavaScript (Babel)",
        "objc" => "Objective-C",
        "objcpp" => "Objective-C++",
        "python3" => "python",
        "pwsh" => "PowerShell",
        "shell" => "bash",
        "tsx" => "TypescriptReact",
        "asm" => "Assembly (x86_64)",
        _ => syntax_name,
    };
    ss.find_syntax_by_token(patched)
        .or_else(|| ss.find_syntax_by_extension(patched))
        .or_else(|| ss.find_syntax_by_name(patched))
        .or_else(|| {
            if patched == syntax_name {
                None
            } else {
                ss.find_syntax_by_token(syntax_name)
                    .or_else(|| ss.find_syntax_by_extension(syntax_name))
                    .or_else(|| ss.find_syntax_by_name(syntax_name))
            }
        })
}

fn convert_color(color: syntect::highlighting::Color) -> Option<Color> {
    match color.a {
        0 => None,
        _ => Some(Color::Rgb(color.r, color.g, color.b)),
    }
}

fn convert_style(style: syntect::highlighting::Style) -> Style {
    let mut converted = Style::default();
    if let Some(fg) = convert_color(style.foreground) {
        converted = converted.fg(fg);
    }
    if style.font_style.contains(FontStyle::BOLD) {
        converted = converted.add_modifier(Modifier::BOLD);
    }
    converted
}

pub(super) fn highlight_block(code: &str, path: &str) -> Option<Vec<Vec<Span<'static>>>> {
    if code.is_empty() || code.len() > 512 * 1024 || code.lines().count() > 10_000 {
        return None;
    }
    let syntax = find_syntax_for_path(path)?;
    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(code) {
        let ranges = highlighter.highlight_line(line, syntax_set()).ok()?;
        let mut spans = Vec::new();
        for (style, text) in ranges {
            let text = text.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                continue;
            }
            spans.push(Span::styled(text.to_string(), convert_style(style)));
        }
        if spans.is_empty() {
            spans.push(Span::raw(String::new()));
        }
        lines.push(spans);
    }
    Some(lines)
}

pub(super) fn highlight_shell_command(command: &str) -> Option<Vec<Span<'static>>> {
    highlight_block(command, "shell").and_then(
        |mut lines| {
            if lines.len() == 1 { lines.pop() } else { None }
        },
    )
}

pub(super) fn highlight_patch_lines(
    path: &str,
    lines: &[crate::tool_ui::PatchDiffLineUiData],
) -> Vec<Option<Vec<Span<'static>>>> {
    let mut highlighted = vec![None; lines.len()];
    let mut segment_start = 0usize;
    while segment_start < lines.len() {
        while segment_start < lines.len()
            && matches!(
                lines[segment_start].kind,
                crate::tool_ui::PatchDiffLineKind::HunkBreak
            )
        {
            segment_start += 1;
        }
        if segment_start >= lines.len() {
            break;
        }
        let segment_end = lines[segment_start..]
            .iter()
            .position(|line| matches!(line.kind, crate::tool_ui::PatchDiffLineKind::HunkBreak))
            .map(|offset| segment_start + offset)
            .unwrap_or(lines.len());
        let code = lines[segment_start..segment_end]
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(spans) = highlight_block(&code, path) {
            for (index, line_spans) in spans.into_iter().enumerate() {
                if segment_start + index < highlighted.len() {
                    highlighted[segment_start + index] = Some(line_spans);
                }
            }
        }
        segment_start = segment_end + 1;
    }
    highlighted
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct DiffScopeBackgrounds {
    pub inserted: Option<Color>,
    pub deleted: Option<Color>,
}

pub(super) fn diff_scope_backgrounds() -> DiffScopeBackgrounds {
    diff_scope_backgrounds_for_theme(theme())
}

fn diff_scope_backgrounds_for_theme(theme: &Theme) -> DiffScopeBackgrounds {
    let highlighter = Highlighter::new(theme);
    DiffScopeBackgrounds {
        inserted: scope_background_color(&highlighter, "markup.inserted")
            .or_else(|| scope_background_color(&highlighter, "diff.inserted")),
        deleted: scope_background_color(&highlighter, "markup.deleted")
            .or_else(|| scope_background_color(&highlighter, "diff.deleted")),
    }
}

fn scope_background_color(highlighter: &Highlighter<'_>, scope_name: &str) -> Option<Color> {
    let scope = Scope::new(scope_name).ok()?;
    let background = highlighter.style_mod_for_stack(&[scope]).background?;
    convert_color(background)
}

#[cfg(test)]
fn theme_item(scope: &str, background: (u8, u8, u8)) -> ThemeItem {
    ThemeItem {
        scope: syntect::highlighting::ScopeSelectors::from_str(scope)
            .expect("scope selector should parse"),
        style: StyleModifier {
            background: Some(syntect::highlighting::Color {
                r: background.0,
                g: background.1,
                b: background.2,
                a: 255,
            }),
            ..StyleModifier::default()
        },
    }
}

#[cfg(test)]
fn theme_with_diff_backgrounds() -> Theme {
    Theme {
        name: Some("test-diff-theme".to_string()),
        author: None,
        settings: ThemeSettings::default(),
        scopes: vec![
            theme_item("markup.inserted", (10, 20, 30)),
            theme_item("markup.deleted", (40, 50, 60)),
        ],
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::{
        diff_scope_backgrounds_for_theme, find_syntax_for_path, highlight_patch_lines,
        theme_with_diff_backgrounds,
    };
    use crate::tool_ui::{PatchDiffLineKind, PatchDiffLineUiData};

    #[test]
    fn rust_patch_lines_receive_highlighted_spans() {
        let highlighted = highlight_patch_lines(
            "src/main.rs",
            &[PatchDiffLineUiData {
                kind: PatchDiffLineKind::Context,
                old_lineno: Some(1),
                new_lineno: Some(1),
                text: "fn main() {}".to_string(),
            }],
        );
        let spans = highlighted[0].as_ref().expect("expected highlight spans");
        assert!(spans.iter().any(|span| span.style.fg.is_some()));
    }

    #[test]
    fn diff_scope_backgrounds_expose_insert_or_delete_theme_color() {
        let backgrounds = diff_scope_backgrounds_for_theme(&theme_with_diff_backgrounds());
        assert!(backgrounds.inserted.is_some());
        assert!(backgrounds.deleted.is_some());
    }

    #[test]
    fn rust_patch_lines_use_truecolor_spans() {
        let highlighted = highlight_patch_lines(
            "src/main.rs",
            &[PatchDiffLineUiData {
                kind: PatchDiffLineKind::Context,
                old_lineno: Some(1),
                new_lineno: Some(1),
                text: "let message = \"hello\";".to_string(),
            }],
        );
        let spans = highlighted[0].as_ref().expect("expected highlight spans");
        assert!(
            spans
                .iter()
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(_, _, _))))
        );
    }

    #[test]
    fn extended_syntax_set_covers_common_frontend_and_named_files() {
        for path in [
            "webui/src/components/status-page.tsx",
            "webui/src/lib/daemon-api.ts",
            "webui/package.json",
            "Dockerfile",
            ".bashrc",
        ] {
            assert!(
                find_syntax_for_path(path).is_some(),
                "expected syntax for {path}"
            );
        }
    }

    #[test]
    fn code_fence_aliases_cover_common_short_tokens() {
        for syntax_name in ["pwsh", "jsx", "tsx", "objc", "objcpp", "asm"] {
            assert!(
                find_syntax_for_path(syntax_name).is_some(),
                "expected syntax alias for {syntax_name}"
            );
        }
    }

    #[test]
    fn typescript_react_patch_lines_receive_highlighted_spans() {
        let highlighted = highlight_patch_lines(
            "webui/src/components/status-page.tsx",
            &[PatchDiffLineUiData {
                kind: PatchDiffLineKind::Context,
                old_lineno: Some(1),
                new_lineno: Some(1),
                text: "const element = <Button className=\"primary\">Save</Button>;".to_string(),
            }],
        );
        let spans = highlighted[0]
            .as_ref()
            .expect("expected TSX highlight spans");
        assert!(
            spans.iter().any(|span| span.style.fg.is_some()),
            "expected TSX spans to carry syntax colours: {spans:?}"
        );
    }
}
