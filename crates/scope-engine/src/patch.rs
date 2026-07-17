use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::analyzer::Analyzer;
pub use crate::api::{
    AppliedStructuredEditFile, AppliedStructuredEditOperation, AppliedStructuredEditSummary,
};
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

struct PreparedFileEdits {
    display_path: String,
    full_path: PathBuf,
    existed: bool,
    original: String,
    new_content: String,
    planned: Vec<PlannedEdit>,
}

struct PreparedStructuredEdits {
    files: Vec<PreparedFileEdits>,
}

#[must_use]
pub fn line_hash(line_content: &str) -> String {
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
    context: &PropagationCollectionContext<'_>,
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

    let lsp_references_for = |symbol_name: &str| {
        let Ok(lsp_guard) = lsp_analyzer.lock() else {
            return Vec::new();
        };
        let Some(lsp) = &*lsp_guard else {
            return Vec::new();
        };
        let (line, character) =
            find_symbol_position(new_content, symbol_name).unwrap_or_else(|| {
                let hint_line = edits.first().map_or(1, |edit| edit.start_line);
                (hint_line, 0)
            });
        lsp.find_references_for_symbol(full_path, line, character, project_root)
    };

    for sym_name in &modified_symbol_names {
        let lsp_refs = lsp_references_for(sym_name);
        if lsp_refs.is_empty() {
            let rel = normalize_for_comparison(full_path)
                .strip_prefix(normalize_for_comparison(project_root))
                .ok()
                .map_or_else(
                    || full_path.to_string_lossy().to_string(),
                    |p| p.to_string_lossy().to_string(),
                );
            let selector = format!("{rel}::{sym_name}");
            if seen.insert(selector.clone()) {
                let first_line = edits.first().map_or(1, |edit| edit.start_line);
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
                            .filter_map(std::result::Result::ok)
                            .filter(|e| {
                                e.path().is_dir()
                                    && e.path().file_name().is_some_and(|n| n == "src")
                            })
                            .filter_map(|e| std::fs::read_dir(e.path()).ok())
                            .flat_map(|entries| {
                                let normalized_root = normalize_for_comparison(project_root);
                                entries
                                    .filter_map(std::result::Result::ok)
                                    .filter_map(move |e| {
                                        normalize_for_comparison(&e.path())
                                            .strip_prefix(&normalized_root)
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
                        "symbol \"{sym_name}\" was modified; no LSP available to find references"
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

/// Applies structured edits and discovers references that need propagation review.
///
/// # Errors
///
/// Returns an error when an edit is invalid or cannot be written.
pub fn edit_code_apply(
    edits: &[StructuredEdit],
    project_root: &Path,
    lsp_analyzer: &Mutex<Option<Box<dyn Analyzer + Send>>>,
) -> Result<(Vec<PropagationResult>, AppliedStructuredEditSummary), String> {
    let analyzer = TreeSitterAnalyzer::new();
    let prepared = prepare_structured_edits(edits, project_root, &analyzer, true)?;
    let applied_summary = applied_summary_from_prepared(&prepared);
    write_prepared_structured_edits(&prepared, Some(lsp_analyzer))?;

    let mut results = Vec::new();
    for file in &prepared.files {
        results.extend(collect_propagation_results(
            &PropagationCollectionContext {
                full_path: &file.full_path,
                original: &file.original,
                new_content: &file.new_content,
                project_root,
                lsp_analyzer,
                analyzer: &analyzer,
            },
            &file.planned,
        ));
    }

    Ok((results, applied_summary))
}

/// Applies structured edits without collecting propagation information.
///
/// # Errors
///
/// Returns an error when an edit is invalid or cannot be written.
pub fn edit_file_apply(
    edits: &[StructuredEdit],
    project_root: &Path,
) -> Result<AppliedStructuredEditSummary, String> {
    let analyzer = TreeSitterAnalyzer::new();
    let prepared = prepare_structured_edits(edits, project_root, &analyzer, false)?;
    write_prepared_structured_edits(&prepared, None)?;
    Ok(applied_summary_from_prepared(&prepared))
}

struct PreparedEditContext<'a> {
    project_root: &'a Path,
    analyzer: &'a TreeSitterAnalyzer,
    validate_parse: bool,
}

struct EditGroup<'a> {
    display_path: String,
    edits: Vec<&'a StructuredEdit>,
}

fn group_edits_by_file<'a>(
    edits: &'a [StructuredEdit],
    project_root: &Path,
) -> HashMap<PathBuf, EditGroup<'a>> {
    let mut edits_by_file = HashMap::new();
    for edit in edits {
        let full_path = if Path::new(&edit.path).is_absolute() {
            PathBuf::from(&edit.path)
        } else {
            project_root.join(&edit.path)
        };
        let display_path = display_path_for_edit(project_root, &full_path);
        edits_by_file
            .entry(full_path)
            .and_modify(|group: &mut EditGroup<'a>| group.edits.push(edit))
            .or_insert_with(|| EditGroup {
                display_path,
                edits: vec![edit],
            });
    }
    edits_by_file
}

fn read_original_content(
    group: &EditGroup<'_>,
    full_path: &Path,
) -> Result<(bool, String), String> {
    if full_path.exists() {
        return std::fs::read_to_string(full_path)
            .map(|content| (true, content))
            .map_err(|error| format!("cannot read {}: {error}", full_path.display()));
    }

    for edit in &group.edits {
        let start_valid = edit
            .start
            .as_deref()
            .is_none_or(|start| start == "1#" || start.starts_with("1#"));
        if !start_valid {
            return Err(format!(
                "cannot create new file {}: start anchor must be `1#`",
                full_path.display()
            ));
        }
    }
    Ok((false, String::new()))
}

fn edit_operation(edit: &StructuredEdit, original_is_empty: bool) -> Result<EditOp, String> {
    match (edit.op.clone(), original_is_empty) {
        (Some(EditOp::Replace) | None, true) => Ok(EditOp::Append),
        (Some(operation), _) => Ok(operation),
        (None, false) => Err(format!(
            "op is required for editing existing file {}",
            edit.path
        )),
    }
}

fn edit_start_anchor(edit: &StructuredEdit, original_is_empty: bool) -> Result<String, String> {
    match &edit.start {
        Some(start) => Ok(start.clone()),
        None if original_is_empty => Ok("1#".to_string()),
        None => Err(format!(
            "start anchor is required for editing existing file {}",
            edit.path
        )),
    }
}

fn replacement_lines(edit: &StructuredEdit) -> Vec<String> {
    edit.content
        .as_ref()
        .map(|content| content.clone().into_lines())
        .unwrap_or_default()
}

fn planned_edit_for(
    edit: &StructuredEdit,
    full_path: &Path,
    original: &str,
    context: &PreparedEditContext<'_>,
) -> Result<PlannedEdit, String> {
    let operation = edit_operation(edit, original.is_empty())?;
    let start = edit_start_anchor(edit, original.is_empty())?;
    let (start_line, start_hash) = parse_start_anchor(&start)?;
    if !original.is_empty() {
        verify_line(original, start_line, &start_hash)?;
    }

    let primary_symbol_name = if context.validate_parse && !original.is_empty() {
        context
            .analyzer
            .find_containing_symbol(full_path, start_line, context.project_root)
            .and_then(|selector| crate::selector::parse_selector(&selector).ok())
            .and_then(|parsed| parsed.name().map(str::to_string))
    } else {
        None
    };

    let replacement = replacement_lines(edit);
    match operation {
        EditOp::Replace => {
            let end_anchor = edit
                .end
                .as_deref()
                .ok_or_else(|| format!("replace requires `end` anchor for {}", edit.path))?;
            let (end_line, end_hash) = parse_start_anchor(end_anchor)?;
            if end_line < start_line {
                return Err(format!(
                    "replace end line {} is before start line {} in {}",
                    end_line, start_line, edit.path
                ));
            }
            if !original.is_empty() {
                verify_line(original, end_line, &end_hash)?;
            }
            Ok(PlannedEdit {
                start_line,
                old_count: end_line - start_line + 1,
                replacement,
                primary_symbol_name,
            })
        }
        EditOp::Append => Ok(PlannedEdit {
            start_line: if original.is_empty() {
                1
            } else {
                start_line + 1
            },
            old_count: 0,
            replacement,
            primary_symbol_name,
        }),
        EditOp::Prepend => Ok(PlannedEdit {
            start_line: if original.is_empty() { 1 } else { start_line },
            old_count: 0,
            replacement,
            primary_symbol_name,
        }),
    }
}

fn prepare_file_edits(
    full_path: PathBuf,
    group: EditGroup<'_>,
    context: &PreparedEditContext<'_>,
) -> Result<PreparedFileEdits, String> {
    let (existed, original) = read_original_content(&group, &full_path)?;
    let planned = group
        .edits
        .into_iter()
        .map(|edit| planned_edit_for(edit, &full_path, &original, context))
        .collect::<Result<Vec<_>, _>>()?;
    let new_content =
        apply_planned_edits_to_content(&original, &planned, &full_path.to_string_lossy())?;
    let extension = full_path
        .extension()
        .and_then(|extension| extension.to_str());
    if context.validate_parse
        && extension.is_some_and(|extension| !extension.is_empty())
        && !new_content.is_empty()
        && let Some(diagnostic) = context
            .analyzer
            .parse_error_diagnostic(extension.expect("checked above"), &new_content)
    {
        return Err(format!(
            "edit rejected: tree-sitter cannot parse the result for {}\n{}",
            full_path.display(),
            diagnostic.message()
        ));
    }

    Ok(PreparedFileEdits {
        display_path: group.display_path,
        full_path,
        existed,
        original,
        new_content,
        planned,
    })
}

fn prepare_structured_edits(
    edits: &[StructuredEdit],
    project_root: &Path,
    analyzer: &TreeSitterAnalyzer,
    validate_parse: bool,
) -> Result<PreparedStructuredEdits, String> {
    if edits.is_empty() {
        return Err("edits array is empty".to_string());
    }

    let context = PreparedEditContext {
        project_root,
        analyzer,
        validate_parse,
    };
    let files = group_edits_by_file(edits, project_root)
        .into_iter()
        .map(|(full_path, group)| prepare_file_edits(full_path, group, &context))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PreparedStructuredEdits { files })
}

fn write_prepared_structured_edits(
    prepared: &PreparedStructuredEdits,
    lsp_analyzer: Option<&Mutex<Option<Box<dyn Analyzer + Send>>>>,
) -> Result<(), String> {
    for file in &prepared.files {
        if let Some(parent) = file.full_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "cannot create parent dirs for {}: {e}",
                    file.full_path.display()
                )
            })?;
        }
        std::fs::write(&file.full_path, &file.new_content)
            .map_err(|e| format!("cannot write {}: {e}", file.full_path.display()))?;
        if let Some(lsp_analyzer) = lsp_analyzer
            && let Ok(lsp_guard) = lsp_analyzer.lock()
            && let Some(ref lsp) = *lsp_guard
        {
            lsp.notify_did_change(&file.full_path, 1, &file.new_content);
        }
    }
    Ok(())
}

fn applied_summary_from_prepared(
    prepared: &PreparedStructuredEdits,
) -> AppliedStructuredEditSummary {
    let files = prepared
        .files
        .iter()
        .map(|file| {
            let added_lines = file.planned.iter().map(|edit| edit.replacement.len()).sum();
            let removed_lines = file.planned.iter().map(|edit| edit.old_count).sum();
            AppliedStructuredEditFile {
                path: file.display_path.clone(),
                operation: if file.existed {
                    AppliedStructuredEditOperation::Update
                } else {
                    AppliedStructuredEditOperation::Add
                },
                added_lines,
                removed_lines,
                original_content: file.original.clone(),
                new_content: file.new_content.clone(),
            }
        })
        .collect();
    AppliedStructuredEditSummary { files }
}

fn display_path_for_edit(project_root: &Path, full_path: &Path) -> String {
    let normalized_root = normalize_for_comparison(project_root);
    let normalized_path = normalize_for_comparison(full_path);
    if let Ok(relative) = normalized_path.strip_prefix(&normalized_root) {
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !relative.is_empty() {
            return relative;
        }
    }
    full_path.to_string_lossy().replace('\\', "/")
}

fn normalize_for_comparison(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\")
        .map_or_else(|| p.to_path_buf(), PathBuf::from)
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
            op: Some(api::EditOp::Replace),
            start: Some(hashes[0].clone()),
            end: Some(hashes[2].clone()),
            content: Some(api::EditContent::Lines(vec![
                "pub fn hello() {".to_string(),
                "    println!(\"hello world\");".to_string(),
                "}".to_string(),
            ])),
        }];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let (propagation, applied_summary) = edit_code_apply(&edits, dir.path(), &lsp).unwrap();
        assert!(!propagation.is_empty(), "Should have propagation results");
        assert_eq!(applied_summary.files.len(), 1);
        assert_eq!(applied_summary.files[0].path, "src/lib.rs");
        assert_eq!(applied_summary.files[0].added_lines, 3);
        assert_eq!(applied_summary.files[0].removed_lines, 3);
        assert!(
            applied_summary.files[0]
                .original_content
                .contains("\"hello\"")
        );
        assert!(applied_summary.files[0].new_content.contains("hello world"));

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
                op: Some(api::EditOp::Append),
                start: Some(hashes[0].clone()),
                end: None,
                content: Some(api::EditContent::Lines(vec![
                    "pub fn added() {".to_string(),
                    "    println!(\"added\");".to_string(),
                    "}".to_string(),
                    String::new(),
                ])),
            },
            api::StructuredEdit {
                path: "src/lib.rs".to_string(),
                op: Some(api::EditOp::Replace),
                start: Some(hashes[4].clone()),
                end: Some(hashes[6].clone()),
                content: None, // delete
            },
        ];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let (_propagation, applied_summary) = edit_code_apply(&edits, dir.path(), &lsp).unwrap();
        assert_eq!(applied_summary.files.len(), 1);
        assert_eq!(applied_summary.files[0].added_lines, 4);
        assert_eq!(applied_summary.files[0].removed_lines, 3);

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
            op: Some(api::EditOp::Append),
            start: Some("1#00".to_string()),
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
        let (_propagation, applied_summary) = result.unwrap();
        assert_eq!(applied_summary.files.len(), 1);
        assert_eq!(
            applied_summary.files[0].operation,
            AppliedStructuredEditOperation::Add
        );
        assert_eq!(applied_summary.files[0].added_lines, 3);
        assert_eq!(applied_summary.files[0].removed_lines, 0);

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
            op: Some(api::EditOp::Replace),
            start: Some("1#ff".to_string()), // wrong hash
            end: Some("3#ff".to_string()),   // wrong hash
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
    fn parse_rejection_reports_tree_sitter_error_location() {
        let dir = setup_temp_rust_project();
        let rust_code = "pub fn hello() {\n    println!(\"hello\");\n}\n";
        let (_, hashes) = write_rust_file(dir.path(), "lib.rs", rust_code);

        let edits = vec![api::StructuredEdit {
            path: "src/lib.rs".to_string(),
            op: Some(api::EditOp::Replace),
            start: Some(hashes[0].clone()),
            end: Some(hashes[2].clone()),
            content: Some(api::EditContent::Lines(vec![
                "pub fn hello() {".to_string(),
                "    println!(\"hello\");".to_string(),
            ])),
        }];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(&edits, dir.path(), &lsp).unwrap_err();

        assert!(err.contains("tree-sitter cannot parse the result"));
        assert!(err.contains("src\\lib.rs") || err.contains("src/lib.rs"));
        assert!(err.contains("first parse error:"));
        assert!(err.contains('L'));
        assert!(err.contains('C'));
        assert!(err.contains("println!"));
        assert!(err.contains('^'));

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
            op: Some(api::EditOp::Replace),
            start: Some(hashes[0].clone()),
            end: None, // missing end
            content: Some(api::EditContent::Text("replaced".to_string())),
        }];
        let lsp: Mutex<Option<Box<dyn Analyzer + Send>>> = Mutex::new(None);
        let err = edit_code_apply(&edits, dir.path(), &lsp).unwrap_err();
        assert!(err.contains("requires `end`"));
    }
}
