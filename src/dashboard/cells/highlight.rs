use std::sync::OnceLock;

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Highlighter, Theme},
    parsing::{Scope, SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};
use two_face::theme::EmbeddedThemeName;

#[cfg(test)]
use std::str::FromStr;
#[cfg(test)]
use syntect::highlighting::{StyleModifier, ThemeItem, ThemeSettings};

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();
static COLOR_LEVEL: OnceLock<ColorLevel> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorLevel {
    TrueColor,
    Ansi256,
    Ansi16,
}

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| two_face::theme::extra().get(EmbeddedThemeName::TwoDark).clone())
}

fn color_level() -> ColorLevel {
    *COLOR_LEVEL.get_or_init(detect_color_level)
}

fn detect_color_level() -> ColorLevel {
    detect_color_level_from_env(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

fn detect_color_level_from_env(colorterm: Option<&str>, term: Option<&str>) -> ColorLevel {
    let colorterm = colorterm.unwrap_or_default().to_ascii_lowercase();
    let term = term.unwrap_or_default().to_ascii_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        ColorLevel::TrueColor
    } else if term.contains("256color") {
        ColorLevel::Ansi256
    } else {
        ColorLevel::Ansi16
    }
}

fn find_syntax_for_path(path: &str) -> Option<&'static SyntaxReference> {
    let ss = syntax_set();
    let extension = path.rsplit('.').next()?;
    let patched = match extension {
        "csharp" => "c#",
        "golang" => "go",
        "python3" => "python",
        "shell" => "bash",
        other => other,
    };
    ss.find_syntax_by_token(patched)
        .or_else(|| ss.find_syntax_by_extension(patched))
        .or_else(|| ss.find_syntax_by_name(patched))
}

fn convert_color(color: syntect::highlighting::Color) -> Option<Color> {
    match color.a {
        0 => Some(Color::Indexed(color.r)),
        1 => None,
        _ => Some(convert_rgb_for_level((color.r, color.g, color.b), color_level())),
    }
}

fn convert_rgb_for_level(rgb: (u8, u8, u8), level: ColorLevel) -> Color {
    match level {
        ColorLevel::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
        ColorLevel::Ansi256 => Color::Indexed(rgb_to_ansi256(rgb)),
        ColorLevel::Ansi16 => rgb_to_ansi16(rgb),
    }
}

fn rgb_to_ansi256((r, g, b): (u8, u8, u8)) -> u8 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + ((u16::from(r) - 8) / 10) as u8;
    }
    let r = ((u16::from(r) * 5) / 255) as u8;
    let g = ((u16::from(g) * 5) / 255) as u8;
    let b = ((u16::from(b) * 5) / 255) as u8;
    16 + 36 * r + 6 * g + b
}

fn rgb_to_ansi16(rgb: (u8, u8, u8)) -> Color {
    const ANSI16_PALETTE: &[(Color, (u8, u8, u8))] = &[
        (Color::Black, (0, 0, 0)),
        (Color::Red, (205, 49, 49)),
        (Color::Green, (13, 188, 121)),
        (Color::Yellow, (229, 229, 16)),
        (Color::Blue, (36, 114, 200)),
        (Color::Magenta, (188, 63, 188)),
        (Color::Cyan, (17, 168, 205)),
        (Color::Gray, (229, 229, 229)),
        (Color::DarkGray, (102, 102, 102)),
        (Color::LightRed, (241, 76, 76)),
        (Color::LightGreen, (35, 209, 139)),
        (Color::LightYellow, (245, 245, 67)),
        (Color::LightBlue, (59, 142, 234)),
        (Color::LightMagenta, (214, 112, 214)),
        (Color::LightCyan, (41, 184, 219)),
        (Color::White, (255, 255, 255)),
    ];
    ANSI16_PALETTE
        .iter()
        .min_by_key(|(_, candidate)| color_distance_squared(rgb, *candidate))
        .map(|(color, _)| *color)
        .unwrap_or(Color::White)
}

fn color_distance_squared((r1, g1, b1): (u8, u8, u8), (r2, g2, b2): (u8, u8, u8)) -> u32 {
    let dr = i32::from(r1) - i32::from(r2);
    let dg = i32::from(g1) - i32::from(g2);
    let db = i32::from(b1) - i32::from(b2);
    (dr * dr + dg * dg + db * db) as u32
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

fn highlight_block(code: &str, path: &str) -> Option<Vec<Vec<Span<'static>>>> {
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

pub(super) fn highlight_patch_lines(
    path: &str,
    lines: &[crate::tool_ui::PatchDiffLineUiData],
) -> Vec<Option<Vec<Span<'static>>>> {
    let mut highlighted = vec![None; lines.len()];
    let mut segment_start = 0usize;
    while segment_start < lines.len() {
        while segment_start < lines.len()
            && matches!(lines[segment_start].kind, crate::tool_ui::PatchDiffLineKind::HunkBreak)
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
    Some(convert_rgb_for_level(
        (background.r, background.g, background.b),
        color_level(),
    ))
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
        ColorLevel, detect_color_level_from_env, diff_scope_backgrounds_for_theme,
        highlight_patch_lines, rgb_to_ansi16, rgb_to_ansi256, theme_with_diff_backgrounds,
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
    fn detects_truecolor_and_256color_from_env() {
        assert_eq!(
            detect_color_level_from_env(Some("truecolor"), Some("xterm-256color")),
            ColorLevel::TrueColor
        );
        assert_eq!(
            detect_color_level_from_env(None, Some("screen-256color")),
            ColorLevel::Ansi256
        );
        assert_eq!(
            detect_color_level_from_env(None, Some("xterm")),
            ColorLevel::Ansi16
        );
    }

    #[test]
    fn rgb_quantizes_to_limited_palettes() {
        assert_eq!(rgb_to_ansi256((255, 0, 0)), 196);
        assert_eq!(rgb_to_ansi16((255, 0, 0)), Color::Red);
    }
}
