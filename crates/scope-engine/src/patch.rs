use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::analyzer::Analyzer;
use crate::api::{EditOp, PropagationResult, PropagationSource, StructuredEdit};
use crate::treesitter::TreeSitterAnalyzer;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

#[derive(Debug, Clone)]
struct PlannedEdit {
    start_line: usize,
    old_count: usize,
    replacement: Vec<String>,
    primary_symbol_name: Option<String>,
}

fn line_hash(line_content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(line_content.as_bytes());
    let result = hasher.finalize();
    format!("{:02x}", result[0])
}

fn parse_start_anchor(anchor: &str) -> Result<(usize, String), String> {
    let (line_str, hash_str) = anchor
        .split_once('#')
        .ok_or_else(|| format!("invalid start anchor (expected line#hash): {anchor}"))?;
    let line = line_str
        .parse::<usize>()
        .map_err(|_| format!("invalid line number in anchor: {anchor}"))?;
    if line == 0 {
        return Err(format!("line number must be >= 1 in anchor: {anchor}"));
    }
    Ok((line, hash_str.to_string()))
}

fn verify_line(content: &str, line_num: usize, expected_hash: &str) -> Result<(), String> {
    let lines: Vec<&str> = content.lines().collect();
    if line_num > lines.len() {
        return Err(format!(
            "line {line_num} out of bounds (file has {} lines)",
            lines.len()
        ));
    }
    let actual = lines[line_num - 1];
    let actual_hash = line_hash(actual);
    if actual_hash != expected_hash {
        return Err(format!(
            "line {line_num} hash mismatch: expected {expected_hash}, got {actual_hash} — file may have changed; re-read before editing"
        ));
    }
    Ok(())
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
                "overlapping edits in {}: edit at line {} overlaps previous edit ending at line {}",
                file_display, pair[1].start_line, prev_end
            ));
        }
    }

    let mut lines = original.lines().map(str::to_string).collect::<Vec<_>>();
    for edit in sorted.iter().rev() {
        let start_idx = edit.start_line.saturating_sub(1);
        let end_idx = start_idx + edit.old_count;
        if start_idx > lines.len() || end_idx > lines.len() {
            return Err(format!(
                "edit exceeds file bounds in {}: lines {}-{} but file has {} lines",
                file_display,
                edit.start_line,
                edit.start_line + edit.old_count,
                lines.len()
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
    } = context;

    let mut results: Vec<PropagationResult> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut modified_symbol_names = HashSet::new();

    for edit in edits {
        if let Some(ref name) = edit.primary_symbol_name {
            modified_symbol_names.insert(name.clone());
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
                    diff_summary: Some("hash-based edit".to_string()),
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

fn normalized_edit_content(content: Option<&crate::api::EditContent>) -> Option<Vec<String>> {
    content.map(|c| c.clone().into_lines())
}

pub fn edit_code_apply(
    edits: &[StructuredEdit],
    project_root: &Path,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> Result<Vec<PropagationResult>, String> {
    if edits.is_empty() {
        return Err("edits array is empty".to_string());
    }

    // Group edits by file path
    let mut edits_by_file: HashMap<std::path::PathBuf, Vec<&StructuredEdit>> = HashMap::new();
    for edit in edits {
        let full_path = if std::path::Path::new(&edit.path).is_absolute() {
            std::path::PathBuf::from(&edit.path)
        } else {
            project_root.join(&edit.path)
        };
        edits_by_file.entry(full_path).or_default().push(edit);
    }

    let analyzer = TreeSitterAnalyzer::new();
    let mut writes = Vec::new();
    let mut all_planned: HashMap<std::path::PathBuf, Vec<PlannedEdit>> = HashMap::new();

    for (full_path, file_edits) in &edits_by_file {
        let original = if full_path.exists() {
            std::fs::read_to_string(full_path)
                .map_err(|e| format!("cannot read {}: {e}", full_path.display()))?
        } else {
            // New file creation: only allow if all edits are Append to line 1
            let can_create = file_edits.iter().all(|e| {
                matches!(e.op, EditOp::Append | EditOp::Prepend) && e.start == "1#"
                    || (e.start.starts_with("1#") && e.start.len() > 2)
            });
            if !can_create {
                return Err(format!(
                    "cannot create new file {}: only append/prepend at line 1 is allowed",
                    full_path.display()
                ));
            }
            String::new()
        };

        let mut planned: Vec<PlannedEdit> = Vec::new();

        for edit in file_edits {
            let (start_line, start_hash) = parse_start_anchor(&edit.start)?;

            if !original.is_empty() {
                verify_line(&original, start_line, &start_hash)?;
            }

            let mut primary_symbol_name = None;
            if !original.is_empty()
                && let Some(sel) =
                    analyzer.find_containing_symbol(full_path, start_line, project_root)
                && let Ok(parsed) = crate::selector::parse_selector(&sel)
            {
                primary_symbol_name = parsed.name().map(str::to_string);
            }

            match edit.op {
                EditOp::Replace => {
                    let (end_line, end_hash) = match &edit.end {
                        Some(end_anchor) => parse_start_anchor(end_anchor)?,
                        None => {
                            return Err(format!("replace requires `end` anchor for {}", edit.path));
                        }
                    };
                    if end_line < start_line {
                        return Err(format!(
                            "replace end line {} is before start line {} in {}",
                            end_line, start_line, edit.path
                        ));
                    }
                    if !original.is_empty() {
                        verify_line(&original, end_line, &end_hash)?;
                    }
                    let old_count = end_line - start_line + 1;
                    let replacement =
                        normalized_edit_content(edit.content.as_ref()).unwrap_or_default();
                    planned.push(PlannedEdit {
                        start_line,
                        old_count,
                        replacement,
                        primary_symbol_name,
                    });
                }
                EditOp::Append => {
                    let replacement =
                        normalized_edit_content(edit.content.as_ref()).unwrap_or_default();
                    let insert_line = if original.is_empty() {
                        1
                    } else {
                        start_line + 1
                    };
                    planned.push(PlannedEdit {
                        start_line: insert_line,
                        old_count: 0,
                        replacement,
                        primary_symbol_name,
                    });
                }
                EditOp::Prepend => {
                    let replacement =
                        normalized_edit_content(edit.content.as_ref()).unwrap_or_default();
                    let insert_line = if original.is_empty() { 1 } else { start_line };
                    planned.push(PlannedEdit {
                        start_line: insert_line,
                        old_count: 0,
                        replacement,
                        primary_symbol_name,
                    });
                }
            }
        }

        let new_content =
            apply_planned_edits_to_content(&original, &planned, &full_path.to_string_lossy())?;

        let ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !ext.is_empty() && !new_content.is_empty() && !analyzer.can_parse(ext, &new_content) {
            return Err(format!(
                "edit rejected: tree-sitter cannot parse the result for {}",
                full_path.display()
            ));
        }

        all_planned.insert(full_path.clone(), planned);
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
        if let Some(planned) = all_planned.get(full_path) {
            results.extend(collect_propagation_results(
                PropagationCollectionContext {
                    full_path,
                    original,
                    new_content,
                    project_root,
                    lsp_analyzer,
                    analyzer: &analyzer,
                },
                planned,
            ));
        }
    }

    Ok(results)
}

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use crate::api;
    use std::io::Write;
    use std::path::PathBuf;

    fn setup_temp_rust_project() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_rust_file(dir: &Path, filename: &str, content: &str) -> (PathBuf, Vec<String>) {
        let src_dir = dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let path = src_dir.join(filename);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();

        let hashes: Vec<String> = content
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{}#{}", i + 1, line_hash(line)))
            .collect();

        (path, hashes)
    }

    #[test]
    fn replace_with_hashes() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"hello\");\n}\n\npub fn world() {\n    println!(\"world\");\n}\n";
        let (_, hashes) = write_rust_file(dir.path(), "lib.rs", rust_code);

        let edits = vec![api::StructuredEdit {
            path: "src/lib.rs".to_string(),
            op: api::EditOp::Replace,
            start: hashes[0].clone(),
            end: Some(hashes[2].clone()),
            content: Some(api::EditContent::Lines(vec![
                "pub fn hello() {".to_string(),
                "    println!(\"hello world\");".to_string(),
                "}".to_string(),
            ])),
        }];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let propagation = edit_code_apply(&edits, dir.path(), &lsp).unwrap();
        assert!(!propagation.is_empty(), "Should have propagation results");

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("hello world"));
        assert!(!modified.contains("\"hello\""));
    }

    #[test]
    fn append_and_replace_delete() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn keep() {\n    println!(\"keep\");\n}\n\npub fn remove_me() {\n    println!(\"remove\");\n}\n";
        let (_, hashes) = write_rust_file(dir.path(), "lib.rs", rust_code);
        // Line 1: "pub fn keep() {" -> hashes[0]
        // Line 5: "pub fn remove_me() {" -> hashes[4]
        // Line 7: "}" -> hashes[6]

        let edits = vec![
            api::StructuredEdit {
                path: "src/lib.rs".to_string(),
                op: api::EditOp::Append,
                start: hashes[0].clone(),
                end: None,
                content: Some(api::EditContent::Lines(vec![
                    "pub fn added() {".to_string(),
                    "    println!(\"added\");".to_string(),
                    "}".to_string(),
                    "".to_string(),
                ])),
            },
            api::StructuredEdit {
                path: "src/lib.rs".to_string(),
                op: api::EditOp::Replace,
                start: hashes[4].clone(),
                end: Some(hashes[6].clone()),
                content: None, // delete
            },
        ];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        edit_code_apply(&edits, dir.path(), &lsp).unwrap();

        let modified = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(modified.contains("pub fn added()"));
        assert!(modified.contains("pub fn keep()"));
        assert!(!modified.contains("remove_me"));
    }

    #[test]
    fn creates_new_file() {
        let dir = setup_temp_rust_project();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();

        let edits = vec![api::StructuredEdit {
            path: "src/new_file.rs".to_string(),
            op: api::EditOp::Append,
            start: "1#00".to_string(),
            end: None,
            content: Some(api::EditContent::Lines(vec![
                "pub fn created() {".to_string(),
                "    println!(\"created\");".to_string(),
                "}".to_string(),
            ])),
        }];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let result = edit_code_apply(&edits, dir.path(), &lsp);
        assert!(
            result.is_ok(),
            "new file creation should succeed: {result:?}"
        );

        let created = std::fs::read_to_string(dir.path().join("src/new_file.rs")).unwrap();
        assert!(created.contains("pub fn created()"));
    }

    #[test]
    fn hash_mismatch_rejects_edit() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"hello\");\n}\n";
        write_rust_file(dir.path(), "lib.rs", rust_code);

        let edits = vec![api::StructuredEdit {
            path: "src/lib.rs".to_string(),
            op: api::EditOp::Replace,
            start: "1#ff".to_string(),     // wrong hash
            end: Some("3#ff".to_string()), // wrong hash
            content: Some(api::EditContent::Text(
                "pub fn hello() {\n    println!(\"goodbye\");\n}\n".to_string(),
            )),
        }];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(&edits, dir.path(), &lsp).unwrap_err();
        assert!(
            err.contains("hash mismatch"),
            "expected hash mismatch, got: {err}"
        );

        let unchanged = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(unchanged, rust_code);
    }

    #[test]
    fn rejects_empty_edits() {
        let dir = setup_temp_rust_project();
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(&[], dir.path(), &lsp).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn replace_requires_end() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"hello\");\n}\n";
        let (_, hashes) = write_rust_file(dir.path(), "lib.rs", rust_code);

        let edits = vec![api::StructuredEdit {
            path: "src/lib.rs".to_string(),
            op: api::EditOp::Replace,
            start: hashes[0].clone(),
            end: None, // missing end
            content: Some(api::EditContent::Text("replaced".to_string())),
        }];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(&edits, dir.path(), &lsp).unwrap_err();
        assert!(err.contains("requires `end`"));
    }
}
