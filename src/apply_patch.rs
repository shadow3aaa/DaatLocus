use std::path::{Path, PathBuf};

use miette::{Result, miette};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PatchOperationKind {
    Add,
    Delete,
    Update,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PatchFileSummary {
    pub path: String,
    pub operation: PatchOperationKind,
    pub added_lines: usize,
    pub removed_lines: usize,
}

#[derive(Default)]
pub(crate) struct ApplyPatchSummary {
    pub changed_files: usize,
    pub added_files: usize,
    pub deleted_files: usize,
    pub updated_files: usize,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub files: Vec<PatchFileSummary>,
}

pub(crate) enum PatchOp {
    Add { path: String, lines: Vec<String> },
    Delete { path: String },
    Update { path: String, hunks: Vec<PatchHunk> },
}

#[derive(Default)]
pub(crate) struct PatchHunk {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
}

pub(crate) fn parse_apply_patch(patch_text: &str) -> Result<Vec<PatchOp>> {
    let lines = patch_text.lines().collect::<Vec<_>>();
    if lines.first().copied() != Some("*** Begin Patch") {
        return Err(miette!(
            "apply_patch patch must start with `*** Begin Patch`"
        ));
    }
    if lines.last().copied() != Some("*** End Patch") {
        return Err(miette!("apply_patch patch must end with `*** End Patch`"));
    }

    let mut ops = Vec::new();
    let mut i = 1;
    while i + 1 < lines.len() {
        let line = lines[i];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            i += 1;
            let mut added = Vec::new();
            while i < lines.len() && !lines[i].starts_with("*** ") {
                let raw = lines[i];
                let Some(content) = raw.strip_prefix('+') else {
                    return Err(miette!("add file lines must start with `+`, got `{raw}`"));
                };
                added.push(content.to_string());
                i += 1;
            }
            ops.push(PatchOp::Add {
                path: path.to_string(),
                lines: added,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            ops.push(PatchOp::Delete {
                path: path.to_string(),
            });
            i += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            i += 1;
            let mut hunks = Vec::new();
            let mut current = PatchHunk::default();
            let mut saw_change_line = false;
            while i < lines.len() && !lines[i].starts_with("*** ") {
                let raw = lines[i];
                if raw == "@@" || raw.starts_with("@@ ") {
                    if saw_change_line {
                        hunks.push(current);
                        current = PatchHunk::default();
                        saw_change_line = false;
                    }
                    i += 1;
                    continue;
                }
                let (prefix, body) = raw.split_at(1);
                match prefix {
                    " " => {
                        current.old_lines.push(body.to_string());
                        current.new_lines.push(body.to_string());
                        saw_change_line = true;
                    }
                    "-" => {
                        current.old_lines.push(body.to_string());
                        saw_change_line = true;
                    }
                    "+" => {
                        current.new_lines.push(body.to_string());
                        saw_change_line = true;
                    }
                    _ => {
                        return Err(miette!(
                            "update file hunk lines must start with space/+/- or @@, got `{raw}`"
                        ));
                    }
                }
                i += 1;
            }
            if saw_change_line {
                hunks.push(current);
            }
            if hunks.is_empty() {
                return Err(miette!("update file `{path}` contains no hunks"));
            }
            ops.push(PatchOp::Update {
                path: path.to_string(),
                hunks,
            });
            continue;
        }

        return Err(miette!("unknown patch directive: {line}"));
    }

    Ok(ops)
}

pub(crate) fn summarize_patch_ops(ops: &[PatchOp]) -> ApplyPatchSummary {
    let mut summary = ApplyPatchSummary::default();
    for op in ops {
        match op {
            PatchOp::Add { path, lines } => {
                summary.changed_files += 1;
                summary.added_files += 1;
                summary.added_lines += lines.len();
                summary.files.push(PatchFileSummary {
                    path: path.clone(),
                    operation: PatchOperationKind::Add,
                    added_lines: lines.len(),
                    removed_lines: 0,
                });
            }
            PatchOp::Delete { path } => {
                summary.changed_files += 1;
                summary.deleted_files += 1;
                summary.files.push(PatchFileSummary {
                    path: path.clone(),
                    operation: PatchOperationKind::Delete,
                    added_lines: 0,
                    removed_lines: 0,
                });
            }
            PatchOp::Update { path, hunks } => {
                let mut added_lines = 0usize;
                let mut removed_lines = 0usize;
                for hunk in hunks {
                    let shared = hunk.old_lines.len().min(hunk.new_lines.len());
                    removed_lines += hunk.old_lines.len().saturating_sub(shared);
                    added_lines += hunk.new_lines.len().saturating_sub(shared);
                }
                summary.changed_files += 1;
                summary.updated_files += 1;
                summary.added_lines += added_lines;
                summary.removed_lines += removed_lines;
                summary.files.push(PatchFileSummary {
                    path: path.clone(),
                    operation: PatchOperationKind::Update,
                    added_lines,
                    removed_lines,
                });
            }
        }
    }
    summary
}

fn resolve_relative_path_within_root(
    root: &Path,
    relative_path: &str,
    caller: &str,
) -> Result<PathBuf> {
    let candidate = Path::new(relative_path);
    if candidate.is_absolute() {
        return Err(miette!(
            "{caller} requires a workspace-relative path, got absolute path: {}",
            candidate.display(),
        ));
    }
    if candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(miette!(
            "{caller} path must not escape the workspace: {}",
            candidate.display(),
        ));
    }
    Ok(root.join(candidate))
}

fn find_unique_hunk_start(haystack: &[String], needle: &[String], offset: usize) -> Result<usize> {
    if needle.is_empty() {
        return Ok(offset.min(haystack.len()));
    }
    let mut matches = Vec::new();
    for start in offset..=haystack.len().saturating_sub(needle.len()) {
        if haystack[start..start + needle.len()] == *needle {
            matches.push(start);
        }
    }
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    if matches.is_empty() {
        for start in 0..=haystack.len().saturating_sub(needle.len()) {
            if haystack[start..start + needle.len()] == *needle {
                matches.push(start);
            }
        }
        if matches.len() == 1 {
            return Ok(matches[0]);
        }
    }
    match matches.len() {
        0 => Err(miette!(
            "patch hunk old text not found uniquely in target file"
        )),
        n => Err(miette!(
            "patch hunk old text matched {n} locations in target file; provide more context"
        )),
    }
}

pub(crate) async fn apply_patch_in_root(
    root: &Path,
    patch_text: &str,
) -> Result<ApplyPatchSummary> {
    let ops = parse_apply_patch(patch_text)?;
    let mut summary = summarize_patch_ops(&ops);
    let mut applied_files = Vec::new();

    for op in ops {
        match op {
            PatchOp::Add { path, lines } => {
                let file_path = resolve_relative_path_within_root(root, &path, "apply_patch add")?;
                if tokio::fs::try_exists(&file_path)
                    .await
                    .map_err(|err| miette!("failed to stat {}: {err}", file_path.display()))?
                {
                    return Err(miette!("apply_patch cannot add existing file {path}"));
                }
                if let Some(parent) = file_path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|err| miette!("failed to create {}: {err}", parent.display()))?;
                }
                let mut content = lines.join("\n");
                if !content.is_empty() {
                    content.push('\n');
                }
                tokio::fs::write(&file_path, content)
                    .await
                    .map_err(|err| miette!("failed to write {}: {err}", file_path.display()))?;
                applied_files.push(path);
            }
            PatchOp::Delete { path } => {
                let file_path =
                    resolve_relative_path_within_root(root, &path, "apply_patch delete")?;
                let removed_lines = tokio::fs::read_to_string(&file_path)
                    .await
                    .map(|text| text.lines().count())
                    .map_err(|err| miette!("failed to read {}: {err}", file_path.display()))?;
                tokio::fs::remove_file(&file_path)
                    .await
                    .map_err(|err| miette!("failed to delete {}: {err}", file_path.display()))?;
                if let Some(file) = summary
                    .files
                    .iter_mut()
                    .find(|file| file.path == path && file.operation == PatchOperationKind::Delete)
                {
                    file.removed_lines = removed_lines;
                }
                summary.removed_lines += removed_lines;
                applied_files.push(path);
            }
            PatchOp::Update { path, hunks } => {
                let file_path =
                    resolve_relative_path_within_root(root, &path, "apply_patch update")?;
                let original = tokio::fs::read_to_string(&file_path)
                    .await
                    .map_err(|err| miette!("failed to read {}: {err}", file_path.display()))?;
                let mut lines = original
                    .lines()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                let mut offset = 0usize;
                for hunk in hunks {
                    let start = find_unique_hunk_start(&lines, &hunk.old_lines, offset)?;
                    let end = start + hunk.old_lines.len();
                    lines.splice(start..end, hunk.new_lines.clone());
                    offset = start + hunk.new_lines.len();
                }
                let mut updated = lines.join("\n");
                if original.ends_with('\n') || !updated.is_empty() {
                    updated.push('\n');
                }
                tokio::fs::write(&file_path, updated)
                    .await
                    .map_err(|err| miette!("failed to write {}: {err}", file_path.display()))?;
                applied_files.push(path);
            }
        }
    }

    summary.files.sort_by(|a, b| a.path.cmp(&b.path));
    debug_assert_eq!(summary.files.len(), applied_files.len());
    Ok(summary)
}
