use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::analyzer::Analyzer;
use crate::api::{PropagationResult, PropagationSource};
use crate::treesitter::{SymbolMatch, TreeSitterAnalyzer};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeDiffActionKind {
    Add,
    Delete,
    Update,
}

#[derive(Debug, Clone)]
struct ScopeDiffAction {
    kind: ScopeDiffActionKind,
    selector: String,
    body: String,
}

#[derive(Debug, Clone)]
struct PlannedEdit {
    selector: String,
    full_path: std::path::PathBuf,
    start_line: usize,
    old_count: usize,
    replacement: Vec<String>,
    primary_symbol: Option<SymbolMatch>,
}

type HunkHeader = (Option<usize>, Option<usize>, Option<usize>, Option<usize>);

/// A single hunk inside a selector-relative hunk patch.
#[derive(Debug, Clone)]
struct Hunk {
    /// Old starting line (1-based). For selector-scoped edits this is
    /// selector-relative until resolved.
    old_start: Option<usize>,
    /// Number of lines in old hunk. Missing counts are inferred from hunk body.
    old_count: Option<usize>,
    /// New starting line (1-based). For selector-scoped edits this is
    /// selector-relative until resolved.
    _new_start: Option<usize>,
    /// Number of lines in new hunk. Missing counts are inferred from hunk body.
    _new_count: Option<usize>,
    /// Lines: ` ` for context, `+` for added, `-` for removed.
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone)]
struct ResolvedHunk {
    /// Old file starting line (1-based).
    old_start: usize,
    /// Number of lines in old file hunk.
    old_count: usize,
    /// New file starting line (1-based).
    _new_start: usize,
    /// Number of lines in new file hunk.
    _new_count: usize,
    /// Lines: ` ` for context, `+` for added, `-` for removed.
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HunkLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// Parse a selector-relative hunk body and apply it to existing content.
#[cfg(test)]
fn apply_selector_hunk_patch(original: &str, patch: &str) -> Result<String, String> {
    let hunks = parse_selector_hunks(patch)?;
    if hunks.is_empty() {
        return Err("no hunks found in patch".to_string());
    }
    let resolved = resolve_hunks(original, &hunks, 1, None)?;
    apply_hunks(original, &resolved)
}

/// Parse the selector-relative hunk format.
fn parse_selector_hunks(patch: &str) -> Result<Vec<Hunk>, String> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current_lines: Vec<HunkLine> = Vec::new();
    let mut current_header: Option<HunkHeader> = None;

    for line in patch.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("@@") {
            // Flush previous hunk
            if let Some((old_start, old_count, _new_start, _new_count)) = current_header.take() {
                if current_lines.is_empty() {
                    return Err("empty hunk body after @@ header".to_string());
                }
                hunks.push(Hunk {
                    old_start,
                    old_count,
                    _new_start,
                    _new_count,
                    lines: std::mem::take(&mut current_lines),
                });
            }
            // Parse header: @@, @@ -OldStart +NewStart @@, or
            // @@ -OldStart,OldCount +NewStart,NewCount @@.
            let header = parse_hunk_header(trimmed)?;
            current_header = Some(header);
        } else {
            if current_header.is_none() && is_hunk_body_line(line) {
                // Headerless selector hunks are resolved later from their body.
                current_header = Some((None, None, None, None));
            }

            if current_header.is_some() {
                // Inside a hunk body
                if line.is_empty() {
                    // Empty lines are context
                    current_lines.push(HunkLine::Context(String::new()));
                } else if let Some(stripped) = line.strip_prefix('+') {
                    current_lines.push(HunkLine::Added(stripped.to_string()));
                } else if let Some(stripped) = line.strip_prefix('-') {
                    current_lines.push(HunkLine::Removed(stripped.to_string()));
                } else if let Some(stripped) = line.strip_prefix(' ') {
                    current_lines.push(HunkLine::Context(stripped.to_string()));
                }
            }
            // Ignore lines that don't start with +, -, or space outside hunks
        }
    }

    // Flush last hunk
    if let Some((old_start, old_count, _new_start, _new_count)) = current_header.take() {
        if current_lines.is_empty() {
            return Err("empty hunk body after @@ header".to_string());
        }
        hunks.push(Hunk {
            old_start,
            old_count,
            _new_start,
            _new_count,
            lines: current_lines,
        });
    }

    if hunks.is_empty() {
        return Err("no valid hunks found".to_string());
    }

    Ok(hunks)
}

fn is_hunk_body_line(line: &str) -> bool {
    line.starts_with('+') || line.starts_with('-') || line.starts_with(' ')
}

fn parse_scope_diff(diff: &str) -> Result<Vec<ScopeDiffAction>, String> {
    let mut lines = diff.lines();
    let first = lines
        .next()
        .ok_or_else(|| "SCOPE Diff is empty".to_string())?;
    if first.trim_end() != "*** Begin Patch" {
        return Err("SCOPE Diff must start with `*** Begin Patch`".to_string());
    }

    let mut actions = Vec::new();
    let mut current: Option<(ScopeDiffActionKind, String, Vec<String>)> = None;
    let mut saw_end = false;

    for line in lines {
        if line.trim_end() == "*** End Patch" {
            if let Some((kind, selector, body_lines)) = current.take() {
                actions.push(ScopeDiffAction {
                    kind,
                    selector,
                    body: body_lines.join("\n"),
                });
            }
            saw_end = true;
            break;
        }

        if let Some((kind, selector)) = parse_scope_diff_action_header(line)? {
            if let Some((prev_kind, prev_selector, body_lines)) = current.take() {
                actions.push(ScopeDiffAction {
                    kind: prev_kind,
                    selector: prev_selector,
                    body: body_lines.join("\n"),
                });
            }
            current = Some((kind, selector, Vec::new()));
        } else if let Some((_, _, body_lines)) = current.as_mut() {
            body_lines.push(line.to_string());
        } else if !line.trim().is_empty() {
            return Err(format!(
                "unexpected content before first SCOPE Diff action: {line}"
            ));
        }
    }

    if !saw_end {
        return Err("SCOPE Diff must end with `*** End Patch`".to_string());
    }
    if actions.is_empty() {
        return Err("SCOPE Diff contains no actions".to_string());
    }
    Ok(actions)
}

fn parse_scope_diff_action_header(
    line: &str,
) -> Result<Option<(ScopeDiffActionKind, String)>, String> {
    let Some(rest) = line.strip_prefix("*** ") else {
        return Ok(None);
    };
    let Some((name, selector)) = rest.split_once(": ") else {
        return Ok(None);
    };

    let kind = match name {
        "Add" => ScopeDiffActionKind::Add,
        "Delete" => ScopeDiffActionKind::Delete,
        "Update" => ScopeDiffActionKind::Update,
        _ => return Ok(None),
    };
    let selector = selector.trim().to_string();
    if selector.is_empty() {
        return Err(format!("SCOPE Diff {name} action has empty selector"));
    }
    Ok(Some((kind, selector)))
}

fn parse_add_body(body: &str) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for line in body.lines() {
        let Some(text) = line.strip_prefix('+') else {
            return Err("SCOPE Diff Add body lines must start with `+`".to_string());
        };
        lines.push(text.to_string());
    }
    if lines.is_empty() {
        return Err("SCOPE Diff Add action requires at least one body line".to_string());
    }
    Ok(lines)
}

fn parse_delete_guard(body: &str) -> Result<Option<Vec<String>>, String> {
    if body.trim().is_empty() {
        return Ok(None);
    }
    let mut lines = Vec::new();
    for line in body.lines() {
        let Some(text) = line.strip_prefix('-') else {
            return Err("SCOPE Diff Delete body lines must start with `-`".to_string());
        };
        lines.push(text.to_string());
    }
    Ok(Some(lines))
}

/// Parse `@@`, `@@ -OldStart +NewStart @@`, or
/// `@@ -OldStart,OldCount +NewStart,NewCount @@`.
fn parse_hunk_header(line: &str) -> Result<HunkHeader, String> {
    // Remove @@ markers and split
    let inner = line.trim_start_matches("@@").trim_end_matches("@@").trim();
    if inner.is_empty() {
        return Ok((None, None, None, None));
    }

    let parts: Vec<&str> = inner.split_whitespace().collect();
    if parts.is_empty() || parts.len() > 2 {
        return Err(format!("invalid hunk header: {line}"));
    }

    let old_part = parts[0]
        .strip_prefix('-')
        .ok_or_else(|| format!("invalid hunk header: {line}"))?;
    let (old_start, old_count) = parse_hunk_range(old_part)?;

    let (_new_start, _new_count) = if let Some(new_part) = parts.get(1) {
        let new_part = new_part
            .strip_prefix('+')
            .ok_or_else(|| format!("invalid hunk header: {line}"))?;
        parse_hunk_range(new_part)?
    } else {
        (None, None)
    };

    Ok((old_start, old_count, _new_start, _new_count))
}

fn parse_hunk_range(s: &str) -> Result<(Option<usize>, Option<usize>), String> {
    if let Some((start_str, count_str)) = s.split_once(',') {
        let start = start_str
            .parse::<usize>()
            .map_err(|_| format!("bad range: {s}"))?;
        let count = count_str
            .parse::<usize>()
            .map_err(|_| format!("bad count: {s}"))?;
        Ok((Some(start), Some(count)))
    } else {
        let start = s.parse::<usize>().map_err(|_| format!("bad range: {s}"))?;
        Ok((Some(start), None))
    }
}

fn infer_old_count(lines: &[HunkLine]) -> usize {
    lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Context(_) | HunkLine::Removed(_)))
        .count()
}

fn infer_new_count(lines: &[HunkLine]) -> usize {
    lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Context(_) | HunkLine::Added(_)))
        .count()
}

fn old_lines_for_hunk(lines: &[HunkLine]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(text) | HunkLine::Removed(text) => Some(text.clone()),
            HunkLine::Added(_) => None,
        })
        .collect()
}

fn has_old_side_anchor(lines: &[HunkLine]) -> bool {
    lines
        .iter()
        .any(|line| matches!(line, HunkLine::Context(_) | HunkLine::Removed(_)))
}

fn resolve_hunks(
    original: &str,
    hunks: &[Hunk],
    selector_start_line: usize,
    selector_end_line: Option<usize>,
) -> Result<Vec<ResolvedHunk>, String> {
    let original_lines = original.lines().map(str::to_string).collect::<Vec<_>>();
    let mut search_offset = selector_start_line.saturating_sub(1);
    let selector_end_idx = selector_end_line
        .unwrap_or(original_lines.len())
        .min(original_lines.len());
    let mut resolved = Vec::with_capacity(hunks.len());

    for hunk in hunks {
        let old_count = hunk
            .old_count
            .unwrap_or_else(|| infer_old_count(&hunk.lines));
        let _new_count = hunk
            ._new_count
            .unwrap_or_else(|| infer_new_count(&hunk.lines));
        let relative_new_start = hunk._new_start.or(hunk.old_start).unwrap_or(1);
        let _new_start = selector_start_line + relative_new_start.saturating_sub(1);
        let old_start = if let Some(relative_old_start) = hunk.old_start {
            selector_start_line + relative_old_start.saturating_sub(1)
        } else {
            if !has_old_side_anchor(&hunk.lines) {
                return Err(
                    "cannot infer insertion point for add-only hunk without context or explicit selector-relative line"
                        .to_string(),
                );
            }
            let old_lines = old_lines_for_hunk(&hunk.lines);
            let found = find_hunk_start_in_range(
                &original_lines,
                &old_lines,
                search_offset,
                selector_end_idx,
                selector_start_line == 1
                    && selector_end_line.unwrap_or(original_lines.len()) >= original_lines.len(),
            )?;
            search_offset = found + old_count.max(1);
            found + 1
        };

        if old_start < selector_start_line || old_start.saturating_sub(1) > selector_end_idx {
            return Err(format!(
                "hunk start line {} is outside selector range {}-{}",
                old_start, selector_start_line, selector_end_idx
            ));
        }
        if old_count > 0 && old_start + old_count - 1 > selector_end_idx {
            return Err(format!(
                "hunk old range {}-{} exceeds selector range {}-{}",
                old_start,
                old_start + old_count - 1,
                selector_start_line,
                selector_end_idx
            ));
        }

        resolved.push(ResolvedHunk {
            old_start,
            old_count,
            _new_start,
            _new_count,
            lines: hunk.lines.clone(),
        });
    }

    Ok(resolved)
}

fn find_hunk_start_in_range(
    haystack: &[String],
    needle: &[String],
    start_idx: usize,
    end_idx: usize,
    require_unique: bool,
) -> Result<usize, String> {
    if needle.is_empty() {
        return Ok(start_idx.min(end_idx));
    }
    if needle.len() > haystack.len() || start_idx >= end_idx || needle.len() > end_idx - start_idx {
        return Err("hunk old text not found in selector range".to_string());
    }

    let mut matches = Vec::new();
    let last_start = end_idx - needle.len();
    for index in start_idx..=last_start {
        if haystack[index..index + needle.len()] == needle[..] {
            matches.push(index);
        }
    }

    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err("hunk old text not found in selector range".to_string()),
        [index, ..] if !require_unique => Ok(*index),
        _ => Err("hunk old text matched multiple locations in selector range; provide a hunk header or more context".to_string()),
    }
}

/// Apply parsed hunks to original content. Hunks must be sorted by old_start
/// (ascending) and non-overlapping, which is the standard unified diff format.
#[cfg(test)]
fn apply_hunks(original: &str, hunks: &[ResolvedHunk]) -> Result<String, String> {
    let original_lines: Vec<&str> = original.lines().collect();

    // Sort hunks by old_start; reverse apply so indices stay stable
    let mut sorted: Vec<&ResolvedHunk> = hunks.iter().collect();
    sorted.sort_by_key(|h| h.old_start);

    // Validate hunks don't overlap
    for w in sorted.windows(2) {
        let prev_end = w[0].old_start + w[0].old_count;
        if w[1].old_start < prev_end {
            return Err(format!(
                "overlapping hunks: first ends at line {}, second starts at line {}",
                prev_end, w[1].old_start
            ));
        }
    }

    let mut result_lines: Vec<String> = original_lines.iter().map(|s| s.to_string()).collect();

    // Apply hunks in reverse order to keep line indices stable
    for hunk in sorted.iter().rev() {
        let old_start_idx = hunk.old_start.saturating_sub(1); // 0-based
        let old_end_idx = old_start_idx + hunk.old_count;

        if old_end_idx > result_lines.len() {
            return Err(format!(
                "hunk exceeds file bounds: old range {}-{} but file has {} lines",
                hunk.old_start,
                old_end_idx,
                result_lines.len()
            ));
        }

        // Verify context lines match
        let expected_context_lines: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(|hl| match hl {
                HunkLine::Context(s) => Some(s.as_str()),
                HunkLine::Removed(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();

        let actual_lines = if old_end_idx == old_start_idx {
            &[]
        } else {
            &result_lines[old_start_idx..old_end_idx]
        };

        if expected_context_lines.len() != actual_lines.len() {
            return Err(format!(
                "context length mismatch: expected {} lines, found {}",
                expected_context_lines.len(),
                actual_lines.len()
            ));
        }
        for (i, (expected, actual)) in expected_context_lines
            .iter()
            .zip(actual_lines.iter())
            .enumerate()
        {
            if expected != actual {
                return Err(format!(
                    "context mismatch at line {}: expected '{}', got '{}'",
                    old_start_idx + i + 1,
                    expected,
                    actual
                ));
            }
        }

        // Build replacement lines
        let replacement: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|hl| match hl {
                HunkLine::Context(s) | HunkLine::Added(s) => Some(s.clone()),
                HunkLine::Removed(_) => None,
            })
            .collect();

        // Replace
        result_lines.splice(old_start_idx..old_end_idx, replacement);
    }

    if result_lines.is_empty() {
        return Ok(String::new());
    }
    Ok(result_lines.join("\n") + "\n")
}

#[derive(Debug, Clone)]
struct EditTarget {
    start_line: usize,
    end_line: usize,
    primary_symbol: Option<SymbolMatch>,
}

fn resolve_edit_target(
    parsed: &crate::selector::ParsedSelector,
    project_root: &Path,
    analyzer: &TreeSitterAnalyzer,
) -> Result<EditTarget, String> {
    let (full_path, _) = crate::selector::resolve_file(parsed, project_root)
        .map_err(|e| format!("cannot resolve file: {e}"))?;
    match &parsed.target {
        crate::selector::SelectorTarget::Symbol(_) => {
            let symbol = analyzer
                .resolve_selector(&full_path, parsed)
                .map_err(|e| format!("cannot resolve selector: {e}"))?;
            Ok(EditTarget {
                start_line: symbol.start_line,
                end_line: symbol.end_line,
                primary_symbol: Some(symbol),
            })
        }
        crate::selector::SelectorTarget::Enclosing { line } => {
            let symbol = analyzer
                .find_containing_symbol_match(&full_path, *line)
                .ok_or_else(|| {
                    format!(
                        "no enclosing symbol found at {} line {}",
                        full_path.display(),
                        line
                    )
                })?;
            Ok(EditTarget {
                start_line: symbol.start_line,
                end_line: symbol.end_line,
                primary_symbol: Some(symbol),
            })
        }
        crate::selector::SelectorTarget::Match { pattern, around: None } => {
            let content = std::fs::read_to_string(&full_path)
                .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?;
            let regex = regex::Regex::new(pattern).map_err(|e| format!("regex error: {e}"))?;
            let hits = content
                .lines()
                .enumerate()
                .filter_map(|(idx, line)| regex.is_match(line).then_some(idx + 1))
                .collect::<Vec<_>>();
            match hits.as_slice() {
                [line] => {
                    let symbol = analyzer
                        .find_containing_symbol_match(&full_path, *line)
                        .ok_or_else(|| {
                            format!(
                                "unique match at {} line {} has no enclosing symbol",
                                full_path.display(),
                                line
                            )
                        })?;
                    Ok(EditTarget {
                        start_line: symbol.start_line,
                        end_line: symbol.end_line,
                        primary_symbol: Some(symbol),
                    })
                }
                [] => Err(format!("match selector found no matches for /{pattern}/")),
                _ => Err(format!(
                    "match selector is ambiguous for /{pattern}/; candidate lines: {}",
                    hits.iter()
                        .map(|line| line.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        crate::selector::SelectorTarget::LineRange {
            start_line,
            end_line,
        } => {
            let line_count = std::fs::read_to_string(&full_path)
                .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?
                .lines()
                .count()
                .max(1);
            let (start_line, end_line) = clamp_edit_range(*start_line, *end_line, line_count)?;
            Ok(EditTarget {
                start_line,
                end_line,
                primary_symbol: analyzer.find_containing_symbol_match(&full_path, start_line),
            })
        }
        crate::selector::SelectorTarget::BeforeLine { .. }
        | crate::selector::SelectorTarget::AfterLine { .. } => Err(
            "edit_code Update/Delete actions do not accept insertion selectors; use Add with #before:L or #after:L"
                .to_string(),
        ),
        crate::selector::SelectorTarget::AroundLine { .. }
        | crate::selector::SelectorTarget::Match { around: Some(_), .. }
        | crate::selector::SelectorTarget::Outline => Err(
            "edit_code only accepts symbol, file-range, enclosing, or unique match selectors; outline/context selectors are read-only"
                .to_string(),
        ),
    }
}

fn clamp_edit_range(
    start_line: usize,
    end_line: usize,
    line_count: usize,
) -> Result<(usize, usize), String> {
    if start_line == 0 || end_line == 0 || start_line > end_line {
        return Err(format!("invalid line range {start_line}-{end_line}"));
    }
    if start_line > line_count {
        return Err(format!(
            "line range starts after end of file: {start_line} > {line_count}"
        ));
    }
    Ok((start_line, end_line.min(line_count)))
}

fn plan_scope_diff_action(
    action: &ScopeDiffAction,
    project_root: &Path,
    analyzer: &TreeSitterAnalyzer,
) -> Result<Vec<PlannedEdit>, String> {
    let parsed = crate::selector::parse_selector(&action.selector)
        .map_err(|e| format!("bad selector `{}`: {e}", action.selector))?;
    let full_path = if parsed.file_path.is_absolute() {
        parsed.file_path.clone()
    } else {
        project_root.join(&parsed.file_path)
    };

    match action.kind {
        ScopeDiffActionKind::Add => {
            let replacement = parse_add_body(&action.body)?;
            let (start_line, primary_symbol) = resolve_add_insertion(
                &parsed,
                &full_path,
                project_root,
                analyzer,
                replacement.len(),
            )?;
            Ok(vec![PlannedEdit {
                selector: action.selector.clone(),
                full_path,
                start_line,
                old_count: 0,
                replacement,
                primary_symbol,
            }])
        }
        ScopeDiffActionKind::Delete => {
            let edit_target = resolve_edit_target(&parsed, project_root, analyzer)?;
            let original = std::fs::read_to_string(&full_path)
                .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?;
            if let Some(guard) = parse_delete_guard(&action.body)? {
                let selected = original
                    .lines()
                    .skip(edit_target.start_line.saturating_sub(1))
                    .take(edit_target.end_line - edit_target.start_line + 1)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if selected != guard {
                    return Err(format!(
                        "delete guard does not match selected range for `{}`",
                        action.selector
                    ));
                }
            }
            Ok(vec![PlannedEdit {
                selector: action.selector.clone(),
                full_path,
                start_line: edit_target.start_line,
                old_count: edit_target.end_line - edit_target.start_line + 1,
                replacement: Vec::new(),
                primary_symbol: edit_target.primary_symbol,
            }])
        }
        ScopeDiffActionKind::Update => {
            let edit_target = resolve_edit_target(&parsed, project_root, analyzer)?;
            let original = std::fs::read_to_string(&full_path)
                .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?;
            let hunks = parse_selector_hunks(&action.body)?;
            let resolved_hunks = resolve_hunks(
                &original,
                &hunks,
                edit_target.start_line,
                Some(edit_target.end_line),
            )?;
            let edits = resolved_hunks
                .into_iter()
                .map(|hunk| {
                    let replacement = hunk
                        .lines
                        .iter()
                        .filter_map(|hl| match hl {
                            HunkLine::Context(s) | HunkLine::Added(s) => Some(s.clone()),
                            HunkLine::Removed(_) => None,
                        })
                        .collect::<Vec<_>>();
                    PlannedEdit {
                        selector: action.selector.clone(),
                        full_path: full_path.clone(),
                        start_line: hunk.old_start,
                        old_count: hunk.old_count,
                        replacement,
                        primary_symbol: edit_target.primary_symbol.clone(),
                    }
                })
                .collect::<Vec<_>>();
            if edits.is_empty() {
                return Err(format!(
                    "update action for `{}` has no hunks",
                    action.selector
                ));
            }
            Ok(edits)
        }
    }
}

fn resolve_add_insertion(
    parsed: &crate::selector::ParsedSelector,
    full_path: &Path,
    project_root: &Path,
    analyzer: &TreeSitterAnalyzer,
    added_line_count: usize,
) -> Result<(usize, Option<SymbolMatch>), String> {
    match &parsed.target {
        crate::selector::SelectorTarget::LineRange { start_line, .. } => {
            let line_count = if full_path.exists() {
                std::fs::read_to_string(full_path)
                    .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?
                    .lines()
                    .count()
                    .max(1)
            } else {
                1
            };
            if *start_line == 0 || *start_line > line_count + 1 {
                return Err(format!("add insertion line {} is outside file bounds", start_line));
            }
            let primary_symbol = if full_path.exists() {
                analyzer.find_containing_symbol_match(full_path, *start_line)
            } else {
                None
            };
            Ok((*start_line, primary_symbol))
        }
        crate::selector::SelectorTarget::BeforeLine { line } => {
            let line_count = std::fs::read_to_string(full_path)
                .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?
                .lines()
                .count()
                .max(1);
            if *line == 0 || *line > line_count + 1 {
                return Err(format!("add insertion line {} is outside file bounds", line));
            }
            let primary_symbol = analyzer.find_containing_symbol_match(full_path, *line);
            Ok((*line, primary_symbol))
        }
        crate::selector::SelectorTarget::AfterLine { line } => {
            let line_count = std::fs::read_to_string(full_path)
                .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?
                .lines()
                .count()
                .max(1);
            if *line == 0 || *line > line_count {
                return Err(format!("add insertion line {} is outside file bounds", line));
            }
            let primary_symbol = analyzer.find_containing_symbol_match(full_path, *line);
            Ok((line + 1, primary_symbol))
        }
        crate::selector::SelectorTarget::Symbol(_)
        | crate::selector::SelectorTarget::Enclosing { .. }
        | crate::selector::SelectorTarget::Match { around: None, .. } => {
            let target = resolve_edit_target(parsed, project_root, analyzer)?;
            Ok((
                target.end_line + 1,
                target.primary_symbol.or_else(|| {
                    (added_line_count > 0)
                        .then(|| analyzer.find_containing_symbol_match(full_path, target.start_line))
                        .flatten()
                }),
            ))
        }
        crate::selector::SelectorTarget::AroundLine { .. }
        | crate::selector::SelectorTarget::Match { around: Some(_), .. }
        | crate::selector::SelectorTarget::Outline => Err(
            "SCOPE Diff Add accepts symbol, file-range, enclosing, or unique match selectors; outline/context selectors are read-only"
                .to_string(),
        ),
    }
}

fn apply_planned_edits_to_content(
    original: &str,
    edits: &[PlannedEdit],
    file_display: &str,
) -> Result<String, String> {
    let mut sorted: Vec<&PlannedEdit> = edits.iter().collect();
    sorted.sort_by_key(|edit| edit.start_line);
    for pair in sorted.windows(2) {
        let prev_end = pair[0].start_line + pair[0].old_count;
        if pair[1].start_line < prev_end {
            return Err(format!(
                "overlapping SCOPE Diff edits in {}: `{}` overlaps `{}`",
                file_display, pair[0].selector, pair[1].selector
            ));
        }
    }

    let mut lines = original.lines().map(str::to_string).collect::<Vec<_>>();
    for edit in sorted.iter().rev() {
        let start_idx = edit.start_line.saturating_sub(1);
        let end_idx = start_idx + edit.old_count;
        if start_idx > lines.len() || end_idx > lines.len() {
            return Err(format!(
                "SCOPE Diff edit for `{}` exceeds file bounds in {}",
                edit.selector, file_display
            ));
        }
        lines.splice(start_idx..end_idx, edit.replacement.clone());
    }

    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(lines.join("\n") + "\n")
    }
}

struct PropagationCollectionContext<'a> {
    full_path: &'a Path,
    original: &'a str,
    new_content: &'a str,
    project_root: &'a Path,
    lsp_analyzer: &'a Mutex<Option<Box<dyn Analyzer + Send>>>,
    analyzer: &'a TreeSitterAnalyzer,
    diff_summary: &'a str,
}

fn collect_propagation_results(
    context: PropagationCollectionContext<'_>,
    edits: &[PlannedEdit],
) -> Vec<PropagationResult> {
    let PropagationCollectionContext {
        full_path,
        original,
        new_content,
        project_root,
        lsp_analyzer,
        analyzer,
        diff_summary,
    } = context;

    let mut results: Vec<PropagationResult> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut modified_symbol_names = HashSet::new();

    for edit in edits {
        if let Some(symbol) = edit.primary_symbol.as_ref() {
            modified_symbol_names.insert(symbol.name.clone());
        }
        if let Some(sel) = analyzer.find_containing_symbol(full_path, edit.start_line, project_root)
            && let Ok(parsed) = crate::selector::parse_selector(&sel)
            && let Some(name) = parsed.name()
        {
            modified_symbol_names.insert(name.to_string());
        }
    }

    for sym_name in &modified_symbol_names {
        let mut lsp_refs: Vec<PropagationResult> = Vec::new();
        if let Ok(lsp_guard) = lsp_analyzer.lock()
            && let Some(ref lsp) = *lsp_guard
        {
            let (line, character) =
                find_symbol_position(new_content, sym_name).unwrap_or_else(|| {
                    let hint_line = edits.first().map(|edit| edit.start_line).unwrap_or(1);
                    (hint_line, 0)
                });
            lsp_refs = lsp.find_references_for_symbol(full_path, line, character, project_root);
        }
        if lsp_refs.is_empty() {
            let selector = format!(
                "{}::{}",
                full_path
                    .strip_prefix(project_root)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| full_path.to_string_lossy().to_string()),
                sym_name
            );
            if seen.insert(selector.clone()) {
                let first_line = edits.first().map(|edit| edit.start_line).unwrap_or(1);
                let file_snippet = original
                    .lines()
                    .skip(first_line.saturating_sub(3))
                    .take(7)
                    .collect::<Vec<_>>()
                    .join("\n");
                let project_files = std::fs::read_dir(project_root)
                    .ok()
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .filter(|e| {
                                e.path().is_dir()
                                    && e.path().file_name().is_some_and(|n| n == "src")
                            })
                            .filter_map(|e| std::fs::read_dir(e.path()).ok())
                            .flat_map(|entries| {
                                entries.filter_map(|e| e.ok()).filter_map(|e| {
                                    e.path()
                                        .strip_prefix(project_root)
                                        .ok()
                                        .map(|p| p.to_string_lossy().to_string())
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                results.push(PropagationResult {
                    selector,
                    reason: format!(
                        "symbol \"{}\" was modified; no LSP available to find references",
                        sym_name
                    ),
                    source: PropagationSource::OpenEnded,
                    lsp_references: None,
                    diff_summary: Some(diff_summary.to_string()),
                    file_snippet: Some(file_snippet),
                    project_files: Some(project_files),
                });
            }
        } else {
            for r in lsp_refs {
                if seen.insert(r.selector.clone()) {
                    results.push(r);
                }
            }
        }
    }

    results
}

fn find_symbol_position(content: &str, symbol_name: &str) -> Option<(usize, usize)> {
    content.lines().enumerate().find_map(|(line_idx, line)| {
        line.find(symbol_name)
            .map(|character| (line_idx + 1, character))
    })
}

/// Apply an edit_code operation: parse a SCOPE Diff, resolve selectors, apply edits, write back.
pub fn edit_code_apply(
    diff: &str,
    project_root: &Path,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> Result<Vec<PropagationResult>, String> {
    let actions = parse_scope_diff(diff)?;
    let analyzer = TreeSitterAnalyzer::new();
    let mut edits_by_file: HashMap<std::path::PathBuf, Vec<PlannedEdit>> = HashMap::new();

    for action in &actions {
        for planned in plan_scope_diff_action(action, project_root, &analyzer)? {
            edits_by_file
                .entry(planned.full_path.clone())
                .or_default()
                .push(planned);
        }
    }

    let mut writes = Vec::new();
    for (full_path, edits) in &edits_by_file {
        let original = if full_path.exists() {
            std::fs::read_to_string(full_path)
                .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?
        } else {
            if edits
                .iter()
                .any(|edit| edit.old_count != 0 || edit.start_line != 1)
            {
                return Err(format!(
                    "only Add at line 1 can create a new file: {}",
                    full_path.display()
                ));
            }
            String::new()
        };
        let new_content =
            apply_planned_edits_to_content(&original, edits, &full_path.to_string_lossy())?;
        let ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !ext.is_empty() && !new_content.is_empty() && !analyzer.can_parse(ext, &new_content) {
            return Err(format!(
                "edit rejected: tree-sitter cannot parse the result for {}",
                full_path.display()
            ));
        }
        writes.push((full_path.clone(), original, new_content));
    }

    for (full_path, _, new_content) in &writes {
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("cannot create parent dirs for {}: {e}", full_path.display())
            })?;
        }
        std::fs::write(full_path, new_content)
            .map_err(|e| format!("cannot write {}: {e}", full_path.display()))?;
        if let Ok(lsp_guard) = lsp_analyzer.lock()
            && let Some(ref lsp) = *lsp_guard
        {
            lsp.notify_did_change(full_path, 1, new_content);
        }
    }

    let mut results = Vec::new();
    for (full_path, original, new_content) in &writes {
        if let Some(edits) = edits_by_file.get(full_path) {
            results.extend(collect_propagation_results(
                PropagationCollectionContext {
                    full_path,
                    original,
                    new_content,
                    project_root,
                    lsp_analyzer,
                    analyzer: &analyzer,
                    diff_summary: diff,
                },
                edits,
            ));
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ORIGINAL: &str = "// file header\n// second line\npub fn old() {\n    let a = 1;\n    let b = 2;\n}\n\npub fn other() {\n    let x = 3;\n}\n";

    #[test]
    fn apply_single_line_change() {
        let patch = "@@ -3,4 +3,4 @@\n pub fn old() {\n-    let a = 1;\n+    let a = 42;\n     let b = 2;\n }\n";
        let result = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch).unwrap();
        assert!(result.contains("let a = 42;"));
        assert!(!result.contains("let a = 1;"));
        assert!(result.contains("pub fn old()"));
    }

    #[test]
    fn apply_removal() {
        let patch = "@@ -8,3 +8,0 @@\n-pub fn other() {\n-    let x = 3;\n-}\n";
        let result = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch).unwrap();
        assert!(!result.contains("pub fn other()"));
    }

    #[test]
    fn apply_addition() {
        // Add a line before `pub fn other() {`
        let patch = "@@ -8,0 +8,1 @@\n+    let z = 99;\n";
        let result = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch).unwrap();
        assert!(result.contains("let z = 99;"));
    }

    #[test]
    fn empty_patch_returns_err() {
        assert!(apply_selector_hunk_patch(SAMPLE_ORIGINAL, "").is_err());
    }

    #[test]
    fn context_mismatch_returns_err() {
        let patch = "@@ -3,4 +3,4 @@\n pub fn WRONG() {\n-    let a = 1;\n+    let a = 42;\n     let b = 2;\n }\n";
        let result = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch);
        assert!(result.is_err());
    }

    #[test]
    fn parse_multiple_hunks() {
        let patch = "@@ -3,4 +3,4 @@\n pub fn old() {\n-    let a = 1;\n+    let a = 10;\n     let b = 2;\n }\n@@ -8,1 +8,1 @@\n-pub fn other() {\n+pub fn renamed() {\n";
        let result = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch).unwrap();
        assert!(result.contains("let a = 10;"));
        assert!(result.contains("pub fn renamed()"));
        assert!(!result.contains("let a = 1;"));
        assert!(!result.contains("pub fn other()"));
    }

    #[test]
    fn bare_hunk_header_is_inferred_from_context() {
        let patch = "@@\n pub fn old() {\n-    let a = 1;\n+    let a = 77;\n     let b = 2;\n }\n";
        let result = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch).unwrap();

        assert!(result.contains("let a = 77;"));
        assert!(!result.contains("let a = 1;"));
        assert!(result.contains("pub fn other()"));
    }

    #[test]
    fn headerless_hunk_is_inferred_from_context() {
        let patch = " pub fn old() {\n-    let a = 1;\n+    let a = 88;\n     let b = 2;\n }\n";
        let result = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch).unwrap();

        assert!(result.contains("let a = 88;"));
        assert!(!result.contains("let a = 1;"));
    }

    #[test]
    fn bare_add_only_hunk_without_anchor_is_rejected() {
        let patch = "@@\n+    let z = 99;\n";
        let err = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch).unwrap_err();

        assert!(err.contains("cannot infer insertion point for add-only hunk"));
    }

    #[test]
    fn headerless_add_only_hunk_without_anchor_is_rejected() {
        let patch = "+    let z = 99;\n";
        let err = apply_selector_hunk_patch(SAMPLE_ORIGINAL, patch).unwrap_err();

        assert!(err.contains("cannot infer insertion point for add-only hunk"));
    }
}

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn setup_temp_rust_project() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_rust_file(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let src_dir = dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let path = src_dir.join(filename);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn edit_code_apply_updates_via_scope_diff() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"hello\");\n}\n\npub fn world() {\n    println!(\"world\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let diff = "*** Begin Patch\n*** Update: src/lib.rs::fn hello()\n@@ -1,3 +1,3 @@\n pub fn hello() {\n-    println!(\"hello\");\n+    println!(\"hello world\");\n }\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let propagation = edit_code_apply(diff, dir.path(), &lsp).unwrap();

        assert!(!propagation.is_empty(), "Should have propagation results");
        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("hello world"));
        assert!(!modified.contains("\"hello\""));
    }

    #[test]
    fn edit_code_apply_accepts_multiple_hunks_in_scope_diff_action() {
        let dir = setup_temp_rust_project();
        let rust_code =
            "pub fn target() {\n    let a = 1;\n    let b = 2;\n    println!(\"{}{}\", a, b);\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let diff = "*** Begin Patch\n*** Update: src/lib.rs::fn target()\n@@ -2,1 +2,1 @@\n-    let a = 1;\n+    let a = 10;\n@@ -3,1 +3,1 @@\n-    let b = 2;\n+    let b = 20;\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(diff, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("let a = 10;"));
        assert!(modified.contains("let b = 20;"));
    }

    #[test]
    fn edit_code_apply_adds_and_deletes_via_scope_diff() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn keep() {\n    println!(\"keep\");\n}\n\npub fn remove_me() {\n    println!(\"remove\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let diff = "*** Begin Patch\n*** Add: src/lib.rs#L1-L1\n+pub fn added() {\n+    println!(\"added\");\n+}\n+\n*** Delete: src/lib.rs::fn remove_me()\n-pub fn remove_me() {\n-    println!(\"remove\");\n-}\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(diff, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("pub fn added()"));
        assert!(modified.contains("pub fn keep()"));
        assert!(!modified.contains("remove_me"));
    }

    #[test]
    fn edit_code_apply_adds_after_line_in_tsx() {
        let dir = setup_temp_rust_project();
        let tsx_code = "function AgentChatActivityHeader() {\n  return <div />;\n}\n\nfunction agentChatActivityGlyph(bubble: { kind: string; toolName?: string; appName?: string }) {\n  if (bubble.kind === \"tool\") {\n    if (bubble.toolName === \"terminal\") {\n      return \"$\";\n    }\n    if (bubble.toolName === \"browser\") {\n      return \"↗\";\n    }\n    return \"⌁\";\n  }\n\n  return \"·\";\n}\n";
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/status-page.tsx"), tsx_code).unwrap();

        let diff = "*** Begin Patch\n*** Add: src/status-page.tsx#after:L12\n+    if (bubble.appName === \"Coding\" || bubble.toolName === \"coding_tool_group\") {\n+      return \"◎\";\n+    }\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(diff, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/status-page.tsx")).unwrap();
        assert!(modified.contains("bubble.toolName === \"browser\")"));
        assert!(modified.contains("bubble.toolName === \"coding_tool_group\""));
        assert!(modified.contains("return \"◎\";"));
    }

    #[test]
    fn edit_code_apply_creates_new_file_from_scope_diff_add() {
        let dir = setup_temp_rust_project();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();

        let diff = "*** Begin Patch\n*** Add: src/new_file.rs#L1-L1\n+pub fn created() {\n+    println!(\"created\");\n+}\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let result = edit_code_apply(diff, dir.path(), &lsp);
        assert!(
            result.is_ok(),
            "new file creation should succeed: {result:?}"
        );

        let created = std::fs::read_to_string(dir.path().join("src/new_file.rs")).unwrap();
        assert!(created.contains("pub fn created()"));
    }

    #[test]
    fn edit_code_apply_rejects_delete_guard_mismatch() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn target() {\n    println!(\"current\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let diff = "*** Begin Patch\n*** Delete: src/lib.rs::fn target()\n-pub fn target() {\n-    println!(\"stale\");\n-}\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(diff, dir.path(), &lsp).unwrap_err();

        assert!(err.contains("delete guard does not match"));
        let unchanged = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(unchanged, rust_code);
    }

    #[test]
    fn edit_code_apply_rejects_invalid_scope_diff_envelope() {
        let dir = setup_temp_rust_project();
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply("@@\n+not a scope diff\n", dir.path(), &lsp).unwrap_err();

        assert!(err.contains("SCOPE Diff must start"));
    }

    #[test]
    fn edit_code_apply_rejects_tree_sitter_error_nodes_without_writing() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn target() {\n    println!(\"ok\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let diff = "*** Begin Patch\n*** Update: src/lib.rs::fn target()\n@@ -1,3 +1,2 @@\n-pub fn target() {\n-    println!(\"ok\");\n-}\n+pub fn target( {\n+    println!(\"broken\");\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(diff, dir.path(), &lsp).unwrap_err();
        assert!(err.contains("tree-sitter cannot parse"));

        let unchanged = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(unchanged, rust_code);
    }

    #[test]
    fn edit_code_apply_accepts_enclosing_selector() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn target() {\n    let value = 1;\n    println!(\"{}\", value);\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let diff = "*** Begin Patch\n*** Update: src/lib.rs#enclosing:L2\n@@ -2,1 +2,1 @@\n-    let value = 1;\n+    let value = 9;\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(diff, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("let value = 9;"));
    }

    #[test]
    fn edit_code_apply_rejects_ambiguous_match_selector() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn first() {\n    println!(\"dup\");\n}\n\npub fn second() {\n    println!(\"dup\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let diff = "*** Begin Patch\n*** Update: src/lib.rs#match:/dup/\n@@\n-    println!(\"dup\");\n+    println!(\"changed\");\n*** End Patch\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(diff, dir.path(), &lsp).unwrap_err();

        assert!(err.contains("ambiguous"));
    }
}
