use std::collections::HashSet;
use std::path::Path;

use crate::api::AffectedSelector;
use crate::treesitter::TreeSitterAnalyzer;

/// A single hunk inside a stripped v4a patch.
#[derive(Debug, Clone)]
struct Hunk {
    /// Old file starting line (1-based).
    old_start: usize,
    /// Number of lines in old file hunk.
    old_count: usize,
    /// New file starting line (1-based).
    new_start: usize,
    /// Number of lines in new file hunk.
    new_count: usize,
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
pub fn apply_stripped_v4a_patch(
    original: &str,
    patch: &str,
) -> Result<String, String> {
    let hunks = parse_stripped_v4a_hunks(patch)?;
    if hunks.is_empty() {
        return Err("no hunks found in patch".to_string());
    }
    apply_hunks(original, &hunks)
}

/// Parse the stripped v4a hunk-only format.
pub fn parse_stripped_v4a_hunks(patch: &str) -> Result<Vec<Hunk>, String> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current_lines: Vec<HunkLine> = Vec::new();
    let mut current_header: Option<(usize, usize, usize, usize)> = None;

    for line in patch.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("@@") {
            // Flush previous hunk
            if let Some((old_start, old_count, new_start, new_count)) = current_header.take() {
                if current_lines.is_empty() {
                    return Err("empty hunk body after @@ header".to_string());
                }
                hunks.push(Hunk {
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    lines: std::mem::take(&mut current_lines),
                });
            }
            // Parse header: @@ -OldStart,OldCount +NewStart,NewCount @@
            let header = parse_hunk_header(trimmed)?;
            current_header = Some(header);
        } else if current_header.is_some() {
            // Inside a hunk body
            if line.is_empty() {
                // Empty lines are context
                current_lines.push(HunkLine::Context(String::new()));
            } else if line.starts_with('+') {
                current_lines.push(HunkLine::Added(line[1..].to_string()));
            } else if line.starts_with('-') {
                current_lines.push(HunkLine::Removed(line[1..].to_string()));
            } else if line.starts_with(' ') {
                current_lines.push(HunkLine::Context(line[1..].to_string()));
            }
            // Ignore lines that don't start with +, -, or space outside hunks
        }
    }

    // Flush last hunk
    if let Some((old_start, old_count, new_start, new_count)) = current_header.take() {
        if current_lines.is_empty() {
            return Err("empty hunk body after @@ header".to_string());
        }
        hunks.push(Hunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines: current_lines,
        });
    }

    if hunks.is_empty() {
        return Err("no valid hunks found".to_string());
    }

    Ok(hunks)
}

/// Parse `@@ -OldStart,OldCount +NewStart,NewCount @@` or `@@ -OldStart +NewStart @@`
fn parse_hunk_header(line: &str) -> Result<(usize, usize, usize, usize), String> {
    // Remove @@ markers and split
    let inner = line
        .trim_start_matches("@@")
        .trim_end_matches("@@")
        .trim();

    let parts: Vec<&str> = inner.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(format!("invalid hunk header: {line}"));
    }

    let old_part = parts[0].trim_start_matches('-');
    let new_part = parts[1].trim_start_matches('+');

    let (old_start, old_count) = parse_hunk_range(old_part)?;
    let (new_start, new_count) = parse_hunk_range(new_part)?;

    Ok((old_start, old_count, new_start, new_count))
}

fn parse_hunk_range(s: &str) -> Result<(usize, usize), String> {
    if let Some((start_str, count_str)) = s.split_once(',') {
        let start = start_str.parse::<usize>().map_err(|_| format!("bad range: {s}"))?;
        let count = count_str.parse::<usize>().map_err(|_| format!("bad count: {s}"))?;
        Ok((start, count))
    } else {
        let start = s.parse::<usize>().map_err(|_| format!("bad range: {s}"))?;
        Ok((start, 1))
    }
}

/// Apply parsed hunks to original content. Hunks must be sorted by old_start
/// (ascending) and non-overlapping, which is the standard unified diff format.
fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, String> {
    let original_lines: Vec<&str> = original.lines().collect();

    // Sort hunks by old_start; reverse apply so indices stay stable
    let mut sorted: Vec<&Hunk> = hunks.iter().collect();
    sorted.sort_by_key(|h| h.old_start);

    // Validate hunks don't overlap
    for w in sorted.windows(2) {
        let prev_end = w[0].old_start + w[0].old_count;
        if w[1].old_start < prev_end {
            return Err(format!(
                "overlapping hunks: first ends at line {}, second starts at line {}",
                prev_end,
                w[1].old_start
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
        for (i, (expected, actual)) in expected_context_lines.iter().zip(actual_lines.iter()).enumerate() {
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

/// Apply an edit_code operation: parse selector, resolve file, apply patch, write back.
pub fn edit_code_apply(
    selector_str: &str,
    patch: &str,
    project_root: &Path,
) -> Result<Vec<AffectedSelector>, String> {
    let parsed = crate::selector::parse_selector(selector_str)
        .map_err(|e| format!("bad selector: {e}"))?;

    let full_path = if parsed.file_path.is_absolute() {
        parsed.file_path.clone()
    } else {
        project_root.join(&parsed.file_path)
    };

    if !full_path.exists() {
        // Create new file: ensure parent dirs exist, write patch as full content
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create parent dirs for {}: {e}", full_path.display()))?;
        }
        std::fs::write(&full_path, patch)
            .map_err(|e| format!("cannot create {}: {e}", full_path.display()))?;
        return Ok(vec![]);
    }

    let original = std::fs::read_to_string(&full_path)
        .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?;

    // ── Propagation: find modified symbol → find its references ──
    let hunks = parse_stripped_v4a_hunks(patch)?;
    let analyzer = TreeSitterAnalyzer::new();
    let mut affected = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut modified_symbol_names = HashSet::new();

    // Step 1: collect all symbol names that were modified
    for hunk in &hunks {
        let line = if hunk.old_count > 0 {
            hunk.old_start
        } else {
            hunk.old_start
        };
        if let Some(sel) = analyzer.find_containing_symbol(&full_path, line, project_root) {
            // Parse the selector to extract the symbol name
            if let Ok(parsed) = crate::selector::parse_selector(&sel) {
                modified_symbol_names.insert(parsed.name);
            }
        }
    }

    // Step 2: for each modified symbol, find referencing symbols in the same file
    for sym_name in &modified_symbol_names {
        let refs = analyzer.find_referencing_symbols(&full_path, sym_name, project_root);
        for r in refs {
            if seen.insert(r.selector.clone()) {
                affected.push(r);
            }
        }
    }

    // ── Apply the patch ──
    let new_content = apply_stripped_v4a_patch(&original, patch)?;
    std::fs::write(&full_path, &new_content)
        .map_err(|e| format!("cannot write {}: {e}", full_path.display()))?;

    Ok(affected)
}

/// Apply a delete_code operation: parse selector, resolve file, remove the symbol.
pub fn delete_code_apply(
    selector_str: &str,
    project_root: &Path,
) -> Result<Vec<AffectedSelector>, String> {
    let parsed = crate::selector::parse_selector(selector_str)
        .map_err(|e| format!("bad selector: {e}"))?;

    let (full_path, _ext) = crate::selector::resolve_file(&parsed, project_root)
        .map_err(|e| format!("cannot resolve file: {e}"))?;

    let original = std::fs::read_to_string(&full_path)
        .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?;

    // Find the affected line before deletion for propagation
    let delete_line = original
        .lines()
        .position(|l| l.contains(&parsed.name))
        .map(|idx| idx + 1); // 1-based

    let new_content = remove_hunk_lines(&original, &parsed.name, 3)
        .ok_or_else(|| format!("symbol '{}' not found in {}", parsed.name, full_path.display()))?;

    std::fs::write(&full_path, &new_content)
        .map_err(|e| format!("cannot write {}: {e}", full_path.display()))?;

    // Propagate: find referencing symbols of the deleted symbol
    let analyzer = TreeSitterAnalyzer::new();
    let mut affected = analyzer.find_referencing_symbols(&full_path, &parsed.name, project_root);
    // Also include the deleted symbol itself so agent knows what was deleted
    affected.push(AffectedSelector {
        selector: selector_str.to_string(),
        reason: format!("deleted symbol {} from {}", parsed.name, full_path.display()),
    });

    // Also find containing symbol for the deleted line
    if let Some(line) = delete_line {
        let analyzer = TreeSitterAnalyzer::new();
        if let Some(sel) = analyzer.find_containing_symbol(&full_path, line, project_root) {
            if sel != selector_str {
                let mut seen: HashSet<String> = HashSet::from([selector_str.to_string()]);
                if seen.insert(sel.clone()) {
                    affected.push(AffectedSelector {
                        selector: sel,
                        reason: format!("contained deleted content at line {}", line),
                    });
                }
            }
        }
    }

    Ok(affected)
}

/// Remove lines matching a pattern with surrounding context (simple delete tool).
fn remove_hunk_lines(source: &str, pattern: &str, context_lines: usize) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let target_idx = lines.iter().position(|l| l.contains(pattern))?;

    let start = target_idx.saturating_sub(context_lines);
    let end = (target_idx + context_lines + 1).min(lines.len());

    let result: Vec<&str> = lines[..start]
        .iter()
        .chain(lines[end..].iter())
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
    fn remove_hunk_lines_works() {
        let content = "line 1\nline 2\ntarget line here\nline 4\nline 5\n";
        let result = remove_hunk_lines(content, "target", 1);
        assert_eq!(result, Some("line 1\nline 5\n".to_string()));
    }
}
