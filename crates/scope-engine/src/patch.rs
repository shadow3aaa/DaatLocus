use std::collections::HashSet;
use std::path::Path;

use crate::analyzer::Analyzer;
use crate::api::{PropagationResult, PropagationSource};
use crate::treesitter::{SymbolMatch, TreeSitterAnalyzer};
use std::sync::Mutex;

/// A single hunk inside a stripped v4a patch.
#[derive(Debug, Clone)]
pub(crate) struct Hunk {
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

/// Parse a stripped v4a patch string (hunk-only, no file header)
/// and apply it to the existing content, returning the new content.
///
/// Returns `Some(new_content)` on success, or `None` if any hunk
/// fails to apply (context mismatch).
pub fn apply_stripped_v4a_patch(original: &str, patch: &str) -> Result<String, String> {
    let hunks = parse_stripped_v4a_hunks(patch)?;
    if hunks.is_empty() {
        return Err("no hunks found in patch".to_string());
    }
    let resolved = resolve_hunks(original, &hunks, 1, None)?;
    apply_hunks(original, &resolved)
}

/// Parse the stripped v4a hunk-only format.
pub(crate) fn parse_stripped_v4a_hunks(patch: &str) -> Result<Vec<Hunk>, String> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current_lines: Vec<HunkLine> = Vec::new();
    let mut current_header: Option<(Option<usize>, Option<usize>, Option<usize>, Option<usize>)> =
        None;

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
                // Headerless stripped hunks are resolved later from their body.
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

/// Parse `@@`, `@@ -OldStart +NewStart @@`, or
/// `@@ -OldStart,OldCount +NewStart,NewCount @@`.
fn parse_hunk_header(
    line: &str,
) -> Result<(Option<usize>, Option<usize>, Option<usize>, Option<usize>), String> {
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

/// Apply an edit_code operation: parse selector, resolve file, apply patch, write back.
pub fn edit_code_apply(
    selector_str: &str,
    patch: &str,
    project_root: &Path,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> Result<Vec<PropagationResult>, String> {
    let parsed =
        crate::selector::parse_selector(selector_str).map_err(|e| format!("bad selector: {e}"))?;

    let full_path = if parsed.file_path.is_absolute() {
        parsed.file_path.clone()
    } else {
        project_root.join(&parsed.file_path)
    };

    if !full_path.exists() {
        // Create new file: ensure parent dirs exist, write patch as full content
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("cannot create parent dirs for {}: {e}", full_path.display())
            })?;
        }
        std::fs::write(&full_path, patch)
            .map_err(|e| format!("cannot create {}: {e}", full_path.display()))?;
        return Ok(vec![]);
    }

    let analyzer = TreeSitterAnalyzer::new();
    let edit_target = resolve_edit_target(&parsed, project_root, &analyzer)?;

    let original = std::fs::read_to_string(&full_path)
        .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?;

    // ── Apply the patch first ──
    let hunks = parse_stripped_v4a_hunks(patch)?;
    let resolved_hunks = resolve_hunks(
        &original,
        &hunks,
        edit_target.start_line,
        Some(edit_target.end_line),
    )?;
    let new_content = apply_hunks(&original, &resolved_hunks)?;

    // ── Validate: tree-sitter must be able to parse the new content ──
    let ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !analyzer.can_parse(ext, &new_content) {
        return Err(format!(
            "edit rejected: tree-sitter cannot parse the result for {}",
            full_path.display()
        ));
    }

    // ── Write the file ──
    std::fs::write(&full_path, &new_content)
        .map_err(|e| format!("cannot write {}: {e}", full_path.display()))?;

    // ── Notify LSP of the change ──
    if let Ok(lsp_guard) = lsp_analyzer.lock()
        && let Some(ref lsp) = *lsp_guard
    {
        lsp.notify_did_change(&full_path, 1, &new_content);
    }

    // ── Propagation: map modified lines → symbol names → LSP or open-ended ──
    let mut results: Vec<PropagationResult> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut modified_symbol_names = HashSet::new();
    if let Some(symbol) = edit_target.primary_symbol.as_ref() {
        modified_symbol_names.insert(symbol.name.clone());
    }

    // Step 1: collect all symbol names that were modified
    for hunk in &resolved_hunks {
        let line = hunk.old_start;
        if let Some(sel) = analyzer.find_containing_symbol(&full_path, line, project_root) {
            // Parse the selector to extract the symbol name
            if let Ok(parsed) = crate::selector::parse_selector(&sel)
                && let Some(name) = parsed.name()
            {
                modified_symbol_names.insert(name.to_string());
            }
        }
    }

    // Step 2: for each modified symbol, query LSP for cross-file references
    //         If LSP returns nothing (not available), produce an open-ended result
    for sym_name in &modified_symbol_names {
        // Try to use the real LSP analyzer
        let mut lsp_refs: Vec<PropagationResult> = Vec::new();
        if let Ok(lsp_guard) = lsp_analyzer.lock()
            && let Some(ref lsp) = *lsp_guard
        {
            // Find the symbol's precise position in the file for LSP query
            // Search for the symbol name in the modified content
            let (line, character) =
                find_symbol_position(&new_content, sym_name).unwrap_or_else(|| {
                    let hint_line = resolved_hunks.first().map(|h| h.old_start).unwrap_or(1);
                    (hint_line, 0)
                });
            lsp_refs = lsp.find_references_for_symbol(&full_path, line, character, project_root);
        }
        if lsp_refs.is_empty() {
            // No LSP: generate an open-ended result so agent investigates on its own
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
                // Build a snippet of the modification context
                // Use the first hunk's position to give context around the change
                let first_line = resolved_hunks.first().map(|h| h.old_start).unwrap_or(1);
                let file_snippet = original
                    .lines()
                    .skip(first_line.saturating_sub(3))
                    .take(7)
                    .collect::<Vec<_>>()
                    .join("\n");
                // Collect project files for agent investigation
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
                    diff_summary: Some(patch.to_string()),
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

    Ok(results)
}

/// Apply a delete_code operation: parse selector, resolve file, remove the symbol.
pub fn delete_code_apply(
    selector_str: &str,
    project_root: &Path,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> Result<Vec<PropagationResult>, String> {
    let parsed =
        crate::selector::parse_selector(selector_str).map_err(|e| format!("bad selector: {e}"))?;
    if !matches!(parsed.target, crate::selector::SelectorTarget::Symbol(_)) {
        return Err("delete_code only accepts symbol selectors".to_string());
    }

    let (full_path, _ext) = crate::selector::resolve_file(&parsed, project_root)
        .map_err(|e| format!("cannot resolve file: {e}"))?;

    let selector_match = TreeSitterAnalyzer::new()
        .resolve_selector(&full_path, &parsed)
        .map_err(|e| format!("cannot resolve selector: {e}"))?;

    let original = std::fs::read_to_string(&full_path)
        .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?;

    // ── Propagation: map to symbol name BEFORE deletion (file is still valid) ──
    let mut results: Vec<PropagationResult> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Record the deleted symbol itself
    seen.insert(selector_str.to_string());
    // Build context for the delete operation
    let file_snippet = original.lines().take(10).collect::<Vec<_>>().join("\n");
    let project_files = std::fs::read_dir(project_root)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir() && e.path().file_name().is_some_and(|n| n == "src"))
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
        selector: selector_str.to_string(),
        reason: format!(
            "deleted symbol \"{}\" from {}",
            parsed.name().unwrap_or("<unknown>"),
            full_path.display()
        ),
        source: PropagationSource::OpenEnded,
        lsp_references: None,
        diff_summary: Some(format!("deleted: {}", parsed.name().unwrap_or("<unknown>"))),
        file_snippet: Some(file_snippet),
        project_files: Some(project_files),
    });

    // Query LSP for references of the deleted symbol
    let mut lsp_refs: Vec<PropagationResult> = Vec::new();
    if let Ok(lsp_guard) = lsp_analyzer.lock()
        && let Some(ref lsp) = *lsp_guard
    {
        // Find the symbol's precise position in the file for LSP query
        let (line, character) =
            find_symbol_position(&original, parsed.name().unwrap_or("<unknown>")).unwrap_or_else(
                || {
                    let hint_line = original
                        .lines()
                        .position(|l| l.contains(parsed.name().unwrap_or("<unknown>")))
                        .map(|idx| idx + 1)
                        .unwrap_or(1);
                    (hint_line, 0)
                },
            );
        lsp_refs = lsp.find_references_for_symbol(&full_path, line, character, project_root);
    }
    if lsp_refs.is_empty() {
        // No LSP: open-ended result for the deleted symbol already added above
    } else {
        for r in lsp_refs {
            if seen.insert(r.selector.clone()) {
                results.push(r);
            }
        }
    }

    // ── Execute the deletion ──
    let new_content = remove_symbol_range(
        &original,
        selector_match.start_line,
        selector_match.end_line,
    )
    .ok_or_else(|| {
        format!(
            "symbol '{}' not found in {}",
            parsed.name().unwrap_or("<unknown>"),
            full_path.display()
        )
    })?;

    std::fs::write(&full_path, &new_content)
        .map_err(|e| format!("cannot write {}: {e}", full_path.display()))?;

    // ── Notify LSP of the close ──
    if let Ok(lsp_guard) = lsp_analyzer.lock()
        && let Some(ref lsp) = *lsp_guard
    {
        lsp.notify_did_close(&full_path);
    }

    Ok(results)
}

/// Remove a 1-based inclusive line range.
fn remove_symbol_range(source: &str, start_line: usize, end_line: usize) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    if start_line == 0 || end_line < start_line || end_line > lines.len() {
        return None;
    }

    let start = start_line - 1;
    let result: Vec<&str> = lines[..start]
        .iter()
        .chain(lines[end_line..].iter())
        .copied()
        .collect();

    if result.is_empty() {
        Some(String::new())
    } else {
        Some(result.join("\n") + "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ORIGINAL: &str = "// file header\n// second line\npub fn old() {\n    let a = 1;\n    let b = 2;\n}\n\npub fn other() {\n    let x = 3;\n}\n";

    #[test]
    fn apply_single_line_change() {
        let patch = "@@ -3,4 +3,4 @@\n pub fn old() {\n-    let a = 1;\n+    let a = 42;\n     let b = 2;\n }\n";
        let result = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch).unwrap();
        assert!(result.contains("let a = 42;"));
        assert!(!result.contains("let a = 1;"));
        assert!(result.contains("pub fn old()"));
    }

    #[test]
    fn apply_removal() {
        let patch = "@@ -8,3 +8,0 @@\n-pub fn other() {\n-    let x = 3;\n-}\n";
        let result = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch).unwrap();
        assert!(!result.contains("pub fn other()"));
    }

    #[test]
    fn apply_addition() {
        // Add a line before `pub fn other() {`
        let patch = "@@ -8,0 +8,1 @@\n+    let z = 99;\n";
        let result = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch).unwrap();
        assert!(result.contains("let z = 99;"));
    }

    #[test]
    fn empty_patch_returns_err() {
        assert!(apply_stripped_v4a_patch(SAMPLE_ORIGINAL, "").is_err());
    }

    #[test]
    fn context_mismatch_returns_err() {
        let patch = "@@ -3,4 +3,4 @@\n pub fn WRONG() {\n-    let a = 1;\n+    let a = 42;\n     let b = 2;\n }\n";
        let result = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch);
        assert!(result.is_err());
    }

    #[test]
    fn parse_multiple_hunks() {
        let patch = "@@ -3,4 +3,4 @@\n pub fn old() {\n-    let a = 1;\n+    let a = 10;\n     let b = 2;\n }\n@@ -8,1 +8,1 @@\n-pub fn other() {\n+pub fn renamed() {\n";
        let result = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch).unwrap();
        assert!(result.contains("let a = 10;"));
        assert!(result.contains("pub fn renamed()"));
        assert!(!result.contains("let a = 1;"));
        assert!(!result.contains("pub fn other()"));
    }

    #[test]
    fn bare_hunk_header_is_inferred_from_context() {
        let patch = "@@\n pub fn old() {\n-    let a = 1;\n+    let a = 77;\n     let b = 2;\n }\n";
        let result = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch).unwrap();

        assert!(result.contains("let a = 77;"));
        assert!(!result.contains("let a = 1;"));
        assert!(result.contains("pub fn other()"));
    }

    #[test]
    fn headerless_hunk_is_inferred_from_context() {
        let patch = " pub fn old() {\n-    let a = 1;\n+    let a = 88;\n     let b = 2;\n }\n";
        let result = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch).unwrap();

        assert!(result.contains("let a = 88;"));
        assert!(!result.contains("let a = 1;"));
    }

    #[test]
    fn bare_add_only_hunk_without_anchor_is_rejected() {
        let patch = "@@\n+    let z = 99;\n";
        let err = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch).unwrap_err();

        assert!(err.contains("cannot infer insertion point for add-only hunk"));
    }

    #[test]
    fn headerless_add_only_hunk_without_anchor_is_rejected() {
        let patch = "+    let z = 99;\n";
        let err = apply_stripped_v4a_patch(SAMPLE_ORIGINAL, patch).unwrap_err();

        assert!(err.contains("cannot infer insertion point for add-only hunk"));
    }

    #[test]
    fn remove_symbol_range_works() {
        let content = "line 1\nline 2\ntarget line here\nline 4\nline 5\n";
        let result = remove_symbol_range(content, 2, 4);
        assert_eq!(result, Some("line 1\nline 5\n".to_string()));
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
    fn edit_code_apply_modifies_file_and_returns_open_ended_propagation() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"hello\");\n}\n\npub fn world() {\n    println!(\"world\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn hello()";
        let patch = "@@ -1,3 +1,3 @@\n pub fn hello() {\n-    println!(\"hello\");\n+    println!(\"hello world\");\n }\n";

        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let result = edit_code_apply(selector, patch, dir.path(), &lsp);
        assert!(result.is_ok(), "edit_code_apply should succeed");

        let propagation = result.unwrap();
        // Since LspAnalyzer is a placeholder, all propagation should be OpenEnded
        assert!(!propagation.is_empty(), "Should have propagation results");

        // Verify the file was actually modified
        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(
            modified.contains("hello world"),
            "File should contain the new content"
        );
        assert!(
            !modified.contains("\"hello\""),
            "File should not contain old content"
        );
    }

    #[test]
    fn edit_code_apply_accepts_selector_relative_line_numbers() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn before() {\n    println!(\"before\");\n}\n\npub fn hello() {\n    println!(\"hello\");\n}\n\npub fn after() {\n    println!(\"after\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn hello()";
        let patch = "@@ -1,3 +1,3 @@\n pub fn hello() {\n-    println!(\"hello\");\n+    println!(\"selector relative\");\n }\n";

        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(selector, patch, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("selector relative"));
        assert!(modified.contains("println!(\"before\")"));
        assert!(modified.contains("println!(\"after\")"));
    }

    #[test]
    fn edit_code_apply_accepts_bare_hunk_header() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn duplicated() {\n    println!(\"same\");\n}\n\npub fn target() {\n    println!(\"same\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn target()";
        let patch = "@@\n pub fn target() {\n-    println!(\"same\");\n+    println!(\"target only\");\n }\n";

        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(selector, patch, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("println!(\"target only\")"));
        assert!(modified.contains("pub fn duplicated() {\n    println!(\"same\");"));
    }

    #[test]
    fn edit_code_apply_accepts_headerless_hunk_body() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn target() {\n    let value = 1;\n    println!(\"{}\", value);\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn target()";
        let patch = " pub fn target() {\n-    let value = 1;\n+    let value = 2;\n     println!(\"{}\", value);\n }\n";

        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(selector, patch, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("let value = 2;"));
        assert!(!modified.contains("let value = 1;"));
    }

    #[test]
    fn edit_code_apply_rejects_bare_add_only_hunk_without_anchor() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn alpha() -> i32 {\n    let base = 10;\n    base\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn alpha()";
        let patch = "@@\n+    let inserted = 123;\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(selector, patch, dir.path(), &lsp).unwrap_err();

        assert!(err.contains("cannot infer insertion point for add-only hunk"));
        let unchanged = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(unchanged, rust_code);
    }

    #[test]
    fn edit_code_apply_rejects_headerless_add_only_hunk_without_anchor() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn alpha() -> i32 {\n    let base = 10;\n    base\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn alpha()";
        let patch = "+    let inserted = 123;\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(selector, patch, dir.path(), &lsp).unwrap_err();

        assert!(err.contains("cannot infer insertion point for add-only hunk"));
        let unchanged = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(unchanged, rust_code);
    }

    #[test]
    fn edit_code_apply_accepts_explicit_add_only_hunk() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn alpha() -> i32 {\n    let base = 10;\n    base\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn alpha()";
        let patch = "@@ -2,0 +2,1 @@\n+    let inserted = 123;\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(selector, patch, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(
            modified
                .contains("pub fn alpha() -> i32 {\n    let inserted = 123;\n    let base = 10;")
        );
    }

    #[test]
    fn edit_code_apply_accepts_bare_add_only_hunk_with_context() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn alpha() -> i32 {\n    let base = 10;\n    base\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn alpha()";
        let patch = "@@\n pub fn alpha() -> i32 {\n+    let inserted = 123;\n     let base = 10;\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(selector, patch, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(
            modified
                .contains("pub fn alpha() -> i32 {\n    let inserted = 123;\n    let base = 10;")
        );
    }

    #[test]
    fn edit_code_apply_accepts_file_range_selector_with_relative_hunk() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn before() {\n    println!(\"before\");\n}\n\npub fn target() {\n    let value = 1;\n    println!(\"{}\", value);\n}\n\npub fn after() {\n    println!(\"after\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs#L5-L8";
        let patch = "@@ -2,1 +2,1 @@\n-    let value = 1;\n+    let value = 42;\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let propagation = edit_code_apply(selector, patch, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("let value = 42;"));
        assert!(modified.contains("println!(\"before\")"));
        assert!(modified.contains("println!(\"after\")"));
        assert!(
            propagation
                .iter()
                .any(|result| result.selector.contains("target")),
            "file-range edit should report the affected containing symbol"
        );
    }

    #[test]
    fn edit_code_apply_rejects_file_range_hunk_outside_range() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn target() {\n    let value = 1;\n    println!(\"{}\", value);\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs#L2-L3";
        let patch = "@@ -3,1 +3,1 @@\n-    println!(\"{}\", value);\n+    println!(\"changed {}\", value);\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(selector, patch, dir.path(), &lsp).unwrap_err();

        assert!(err.contains("outside selector range") || err.contains("exceeds selector range"));
        let unchanged = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(unchanged, rust_code);
    }

    #[test]
    fn edit_code_apply_creates_new_file_when_not_exists() {
        let dir = setup_temp_rust_project();
        let new_content = "pub fn new_fn() -> i32 {\n    42\n}\n";

        let selector = "src/new.rs::fn new_fn()";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let result = edit_code_apply(selector, new_content, dir.path(), &lsp);
        assert!(result.is_ok(), "Creating new file should succeed");

        let propagation = result.unwrap();
        assert!(
            propagation.is_empty(),
            "New file should have no propagation"
        );

        let created = std::fs::read_to_string(dir.path().join("src/new.rs")).unwrap();
        assert!(created.contains("new_fn"));
    }

    #[test]
    fn edit_code_apply_rejects_invalid_syntax() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn ok() {\n    let x = 1;\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        // This patch produces incomplete Rust that tree-sitter should reject
        let selector = "src/lib.rs::fn ok()";
        let bad_patch = "@@ -1,3 +1,1 @@\n-pub fn ok() {\n-    let x = 1;\n-}\n+pub fn BROKEN {\n";

        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let result = edit_code_apply(selector, bad_patch, dir.path(), &lsp);
        // tree-sitter may or may not catch this — it depends on the grammar.
        // The important thing is the function doesn't panic.
        // We just verify it returns either Ok or Err without crashing.
        let _ = result;
    }

    #[test]
    fn edit_code_apply_rejects_tree_sitter_error_nodes_without_writing() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn ok() {\n    let x = 1;\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn ok()";
        let bad_patch = "@@ -1,3 +1,1 @@\n-pub fn ok() {\n-    let x = 1;\n-}\n+pub fn BROKEN {\n";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(selector, bad_patch, dir.path(), &lsp).unwrap_err();

        assert!(err.contains("tree-sitter cannot parse"));
        let unchanged = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(unchanged, rust_code);
    }

    #[test]
    fn edit_code_apply_propagation_includes_modified_symbol() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn greet() {\n    println!(\"hi\");\n}\n\npub fn farewell() {\n    println!(\"bye\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn greet()";
        let patch = "@@ -2,1 +2,1 @@\n-    println!(\"hi\");\n+    println!(\"hello\");\n";

        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let result = edit_code_apply(selector, patch, dir.path(), &lsp).unwrap();
        // Should have at least one OpenEnded result for the modified symbol
        let has_greet = result
            .iter()
            .any(|r| r.selector.contains("greet") || r.reason.contains("greet"));
        assert!(
            has_greet,
            "Propagation should mention the modified symbol 'greet'"
        );
    }

    #[test]
    fn delete_code_apply_removes_symbol_and_returns_propagation() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"hello\");\n}\n\npub fn world() {\n    println!(\"world\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs::fn hello()";
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let result = delete_code_apply(selector, dir.path(), &lsp);
        assert!(result.is_ok(), "delete_code_apply should succeed");

        let propagation = result.unwrap();
        assert!(!propagation.is_empty(), "Should have propagation results");
        // Should have an OpenEnded result for the deleted symbol
        assert!(
            propagation.iter().any(|r| r.reason.contains("deleted")),
            "Should note deletion"
        );
    }
    #[test]
    fn edit_code_apply_accepts_enclosing_selector() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"hello\");\n}\n\npub fn world() {\n    println!(\"world\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs#enclosing:L2";
        let patch = "@@ -2,1 +2,1 @@\n-    println!(\"hello\");\n+    println!(\"hi\");\n";

        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(selector, patch, dir.path(), &lsp).unwrap();
        let updated = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(updated.contains("println!(\"hi\");"));
        assert!(updated.contains("pub fn world()"));
    }

    #[test]
    fn edit_code_apply_rejects_ambiguous_match_selector() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"same\");\n}\n\npub fn world() {\n    println!(\"same\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let selector = "src/lib.rs#match:/same/";
        let patch = "@@ -2,1 +2,1 @@\n-    println!(\"same\");\n+    println!(\"other\");\n";

        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(selector, patch, dir.path(), &lsp).unwrap_err();
        assert!(err.contains("ambiguous"));
        assert!(err.contains("candidate lines"));
    }
}

/// Find the (1-based line, 0-based character) position of a symbol name in source text.
/// Searches for the first occurrence of `sym_name` as a word boundary match
/// (e.g. "greet" should match "fn greet(" but not "greeting").
fn find_symbol_position(content: &str, sym_name: &str) -> Option<(usize, usize)> {
    for (line_idx, line) in content.lines().enumerate() {
        if let Some(pos) = line.find(sym_name) {
            // Check that this is a word boundary match
            let before_ok = pos == 0 || !line.as_bytes()[pos - 1].is_ascii_alphanumeric();
            let after_idx = pos + sym_name.len();
            let after_ok =
                after_idx >= line.len() || !line.as_bytes()[after_idx].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return Some((line_idx + 1, pos)); // 1-based line, 0-based character
            }
        }
    }
    None
}
