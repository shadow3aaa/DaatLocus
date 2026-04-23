use std::path::{Path, PathBuf};

use diffy::{Line as DiffyLine, Patch as DiffyPatch};
use miette::{Result, miette};

use crate::sandbox::RuntimeSandboxPolicy;

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
    pub lines: Vec<PatchHunkLine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PatchHunkLineKind {
    Context,
    Delete,
    Add,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PatchHunkLine {
    pub kind: PatchHunkLineKind,
    pub text: String,
}

impl PatchHunk {
    pub fn old_lines(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter(|line| !matches!(line.kind, PatchHunkLineKind::Add))
            .map(|line| line.text.clone())
            .collect()
    }

    pub fn new_lines(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter(|line| !matches!(line.kind, PatchHunkLineKind::Delete))
            .map(|line| line.text.clone())
            .collect()
    }

    pub fn added_lines(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| matches!(line.kind, PatchHunkLineKind::Add))
            .count()
    }

    pub fn removed_lines(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| matches!(line.kind, PatchHunkLineKind::Delete))
            .count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PatchPreviewLineKind {
    Context,
    Delete,
    Add,
    HunkBreak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PatchPreviewLine {
    pub kind: PatchPreviewLineKind,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PatchPreviewFile {
    pub path: String,
    pub operation: PatchOperationKind,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub diff_lines: Vec<PatchPreviewLine>,
}

pub(crate) fn parse_apply_patch(patch_text: &str) -> Result<Vec<PatchOp>> {
    let file_blocks = collect_unified_diff_file_blocks(patch_text)?;
    let mut ops = Vec::new();
    for file_block in file_blocks {
        let patch = DiffyPatch::from_str(&file_block)
            .map_err(|err| miette!("invalid unified diff file patch: {err}"))?;
        let old_path = patch
            .original()
            .ok_or_else(|| miette!("unified diff is missing `--- <path>` after metadata"))?;
        let new_path = patch
            .modified()
            .ok_or_else(|| miette!("unified diff is missing `+++ <path>` after `---`"))?;
        let file_kind = classify_file_patch(old_path, new_path)?;
        let hunks = patch
            .hunks()
            .iter()
            .map(diffy_hunk_to_patch_hunk)
            .collect::<Result<Vec<_>>>()?;
        match file_kind {
            UnifiedFilePatchKind::Add { path } => {
                let lines = collect_added_file_lines(&path, &hunks)?;
                ops.push(PatchOp::Add { path, lines });
            }
            UnifiedFilePatchKind::Delete { path } => {
                validate_deleted_file_hunks(&path, &hunks)?;
                ops.push(PatchOp::Delete { path });
            }
            UnifiedFilePatchKind::Update { path } => {
                if hunks.is_empty() {
                    return Err(miette!("update diff for `{path}` contains no hunks"));
                }
                ops.push(PatchOp::Update { path, hunks });
            }
        }
    }

    if ops.is_empty() {
        return Err(miette!(
            "apply_patch expects unified diff input with `---`, `+++`, and `@@` sections"
        ));
    }

    Ok(ops)
}

pub(crate) fn summarize_apply_patch_error(message: &str) -> String {
    if message.contains("expects unified diff input") {
        return "patch 必须使用 unified diff 格式，并包含 `---`、`+++`、`@@`".to_string();
    }
    if message.contains("must contain `--- <path>`") {
        return "每个文件 patch 都必须以 `--- <path>` 开始".to_string();
    }
    if message.contains("missing `+++ <path>`") {
        return "`---` 后必须紧跟 `+++ <path>`".to_string();
    }
    if message.contains("expected unified diff hunk header") {
        return "文件头之后必须提供 `@@ ... @@` hunk header".to_string();
    }
    if message.contains("hunk contains no lines") {
        return "unified diff hunk 不能为空".to_string();
    }
    if message.contains("hunk lines must start with space/+/-") {
        return "hunk 中每一行都必须以空格、`+` 或 `-` 开头".to_string();
    }
    if message.contains("rename patches are not supported") {
        return "暂不支持 rename patch；请改成单独的删除和新增".to_string();
    }
    if message.contains("new file diff for")
        || message.contains("deleted file diff for")
        || message.contains("contains no hunks")
    {
        return "文件 patch 内容和 `---` / `+++` 头不匹配，请检查 add/delete/update diff"
            .to_string();
    }
    if message.contains("patch hunk old text not found uniquely in target file") {
        return "patch 上下文不足，旧文本无法在目标文件中唯一定位；请提供更多上下文".to_string();
    }
    if message.contains("patch hunk old text matched") {
        return "patch 上下文过少，旧文本匹配了多个位置；请提供更多上下文".to_string();
    }
    message.to_string()
}

enum UnifiedFilePatchKind {
    Add { path: String },
    Delete { path: String },
    Update { path: String },
}

fn is_unified_diff_metadata_line(line: &str) -> bool {
    matches!(
        line,
        _
            if line.starts_with("diff --git ")
                || line.starts_with("index ")
                || line.starts_with("new file mode ")
                || line.starts_with("deleted file mode ")
                || line.starts_with("old mode ")
                || line.starts_with("new mode ")
                || line.starts_with("similarity index ")
                || line.starts_with("rename from ")
                || line.starts_with("rename to ")
    )
}

fn classify_file_patch(old_path: &str, new_path: &str) -> Result<UnifiedFilePatchKind> {
    let old_path = normalize_unified_diff_path(old_path)?;
    let new_path = normalize_unified_diff_path(new_path)?;
    match (old_path.as_str(), new_path.as_str()) {
        ("/dev/null", "/dev/null") => {
            Err(miette!("invalid unified diff: both paths are /dev/null"))
        }
        ("/dev/null", path) => Ok(UnifiedFilePatchKind::Add {
            path: path.to_string(),
        }),
        (path, "/dev/null") => Ok(UnifiedFilePatchKind::Delete {
            path: path.to_string(),
        }),
        (old, new) if old == new => Ok(UnifiedFilePatchKind::Update {
            path: old.to_string(),
        }),
        _ => Err(miette!("rename patches are not supported")),
    }
}

fn normalize_unified_diff_path(path: &str) -> Result<String> {
    let raw = path.split('\t').next().unwrap_or(path).trim();
    if raw.is_empty() {
        return Err(miette!("unified diff file header is missing a path"));
    }
    if raw == "/dev/null" {
        return Ok(raw.to_string());
    }
    let unquoted = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw);
    Ok(unquoted
        .strip_prefix("a/")
        .or_else(|| unquoted.strip_prefix("b/"))
        .unwrap_or(unquoted)
        .to_string())
}

fn diffy_hunk_to_patch_hunk(hunk: &diffy::Hunk<'_, str>) -> Result<PatchHunk> {
    let mut patch_hunk = PatchHunk::default();
    for line in hunk.lines() {
        let (kind, text) = match line {
            DiffyLine::Context(text) => (PatchHunkLineKind::Context, text),
            DiffyLine::Delete(text) => (PatchHunkLineKind::Delete, text),
            DiffyLine::Insert(text) => (PatchHunkLineKind::Add, text),
        };
        patch_hunk.lines.push(PatchHunkLine {
            kind,
            text: trim_diff_line_ending(text),
        });
    }
    if patch_hunk.lines.is_empty() {
        return Err(miette!("unified diff hunk contains no lines"));
    }
    Ok(patch_hunk)
}

fn trim_diff_line_ending(text: &str) -> String {
    text.trim_end_matches(['\n', '\r']).to_string()
}

fn collect_unified_diff_file_blocks(patch_text: &str) -> Result<Vec<String>> {
    let lines = patch_text.lines().collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].is_empty() || is_unified_diff_metadata_line(lines[i]) {
            i += 1;
            continue;
        }
        if !lines[i].starts_with("--- ") {
            return Err(miette!(
                "unified diff must contain `--- <path>` before each file patch"
            ));
        }
        let start = i;
        i += 1;
        while i < lines.len() {
            if lines[i].starts_with("--- ") {
                break;
            }
            i += 1;
        }
        blocks.push(lines[start..i].join("\n"));
    }
    if blocks.is_empty() {
        return Err(miette!(
            "apply_patch expects unified diff input with `---`, `+++`, and `@@` sections"
        ));
    }
    Ok(blocks)
}

fn collect_added_file_lines(path: &str, hunks: &[PatchHunk]) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for hunk in hunks {
        for line in &hunk.lines {
            match line.kind {
                PatchHunkLineKind::Add => lines.push(line.text.clone()),
                PatchHunkLineKind::Context | PatchHunkLineKind::Delete => {
                    return Err(miette!("new file diff for `{path}` contains non-add lines"));
                }
            }
        }
    }
    Ok(lines)
}

fn validate_deleted_file_hunks(path: &str, hunks: &[PatchHunk]) -> Result<()> {
    for hunk in hunks {
        for line in &hunk.lines {
            match line.kind {
                PatchHunkLineKind::Delete => {}
                PatchHunkLineKind::Context | PatchHunkLineKind::Add => {
                    return Err(miette!(
                        "deleted file diff for `{path}` contains non-delete lines"
                    ));
                }
            }
        }
    }
    Ok(())
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
                    removed_lines += hunk.removed_lines();
                    added_lines += hunk.added_lines();
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

async fn read_lines_for_preview(root: &Path, path: &str, caller: &str) -> Result<Vec<String>> {
    let file_path = resolve_relative_path_within_root(root, path, caller)?;
    let content = tokio::fs::read_to_string(&file_path)
        .await
        .map_err(|err| miette!("failed to read {}: {err}", file_path.display()))?;
    Ok(content.lines().map(ToString::to_string).collect())
}

pub(crate) async fn build_patch_preview_in_root(
    root: &Path,
    patch_text: &str,
) -> Result<Vec<PatchPreviewFile>> {
    let ops = parse_apply_patch(patch_text)?;
    let mut files = Vec::new();

    for op in ops {
        match op {
            PatchOp::Add { path, lines } => {
                files.push(PatchPreviewFile {
                    path,
                    operation: PatchOperationKind::Add,
                    added_lines: lines.len(),
                    removed_lines: 0,
                    diff_lines: lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, text)| PatchPreviewLine {
                            kind: PatchPreviewLineKind::Add,
                            old_lineno: None,
                            new_lineno: Some(index + 1),
                            text,
                        })
                        .collect(),
                });
            }
            PatchOp::Delete { path } => {
                let existing_lines =
                    read_lines_for_preview(root, &path, "apply_patch delete preview").await?;
                files.push(PatchPreviewFile {
                    path,
                    operation: PatchOperationKind::Delete,
                    added_lines: 0,
                    removed_lines: existing_lines.len(),
                    diff_lines: existing_lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, text)| PatchPreviewLine {
                            kind: PatchPreviewLineKind::Delete,
                            old_lineno: Some(index + 1),
                            new_lineno: None,
                            text,
                        })
                        .collect(),
                });
            }
            PatchOp::Update { path, hunks } => {
                let mut file_lines =
                    read_lines_for_preview(root, &path, "apply_patch update preview").await?;
                let mut offset = 0usize;
                let mut diff_lines = Vec::new();
                let mut added_lines = 0usize;
                let mut removed_lines = 0usize;

                for (hunk_index, hunk) in hunks.into_iter().enumerate() {
                    let old_lines = hunk.old_lines();
                    let new_lines = hunk.new_lines();
                    let start = find_unique_hunk_start(&file_lines, &old_lines, offset)?;
                    let mut old_lineno = start + 1;
                    let mut new_lineno = start + 1;

                    if hunk_index > 0 {
                        diff_lines.push(PatchPreviewLine {
                            kind: PatchPreviewLineKind::HunkBreak,
                            old_lineno: None,
                            new_lineno: None,
                            text: String::new(),
                        });
                    }

                    for line in &hunk.lines {
                        match line.kind {
                            PatchHunkLineKind::Context => {
                                diff_lines.push(PatchPreviewLine {
                                    kind: PatchPreviewLineKind::Context,
                                    old_lineno: Some(old_lineno),
                                    new_lineno: Some(new_lineno),
                                    text: line.text.clone(),
                                });
                                old_lineno += 1;
                                new_lineno += 1;
                            }
                            PatchHunkLineKind::Delete => {
                                diff_lines.push(PatchPreviewLine {
                                    kind: PatchPreviewLineKind::Delete,
                                    old_lineno: Some(old_lineno),
                                    new_lineno: None,
                                    text: line.text.clone(),
                                });
                                old_lineno += 1;
                                removed_lines += 1;
                            }
                            PatchHunkLineKind::Add => {
                                diff_lines.push(PatchPreviewLine {
                                    kind: PatchPreviewLineKind::Add,
                                    old_lineno: None,
                                    new_lineno: Some(new_lineno),
                                    text: line.text.clone(),
                                });
                                new_lineno += 1;
                                added_lines += 1;
                            }
                        }
                    }

                    let end = start + old_lines.len();
                    file_lines.splice(start..end, new_lines.clone());
                    offset = start + new_lines.len();
                }

                files.push(PatchPreviewFile {
                    path,
                    operation: PatchOperationKind::Update,
                    added_lines,
                    removed_lines,
                    diff_lines,
                });
            }
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

pub(crate) async fn apply_patch_in_root(
    root: &Path,
    sandbox_policy: &RuntimeSandboxPolicy,
    patch_text: &str,
) -> Result<ApplyPatchSummary> {
    let normalized_root = crate::sandbox::normalize_path(root);
    sandbox_policy.ensure_path_writable(&normalized_root, "apply_patch workspace root")?;
    let ops = parse_apply_patch(patch_text)?;
    let mut summary = summarize_patch_ops(&ops);
    let mut applied_files = Vec::new();

    for op in ops {
        match op {
            PatchOp::Add { path, lines } => {
                let file_path = resolve_relative_path_within_root(root, &path, "apply_patch add")?;
                sandbox_policy.ensure_path_writable(&file_path, "apply_patch add target")?;
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
                sandbox_policy.ensure_path_readable(&file_path, "apply_patch delete target")?;
                sandbox_policy.ensure_path_writable(&file_path, "apply_patch delete target")?;
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
                sandbox_policy.ensure_path_readable(&file_path, "apply_patch update target")?;
                sandbox_policy.ensure_path_writable(&file_path, "apply_patch update target")?;
                let original = tokio::fs::read_to_string(&file_path)
                    .await
                    .map_err(|err| miette!("failed to read {}: {err}", file_path.display()))?;
                let mut lines = original
                    .lines()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                let mut offset = 0usize;
                for hunk in hunks {
                    let old_lines = hunk.old_lines();
                    let new_lines = hunk.new_lines();
                    let start = find_unique_hunk_start(&lines, &old_lines, offset)?;
                    let end = start + old_lines.len();
                    lines.splice(start..end, new_lines.clone());
                    offset = start + new_lines.len();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_apply_patch_accepts_unified_diff() {
        let patch = "\
--- a/src.txt
+++ b/src.txt
@@ -1,2 +1,2 @@
 alpha
-beta
+beta changed";
        let ops = parse_apply_patch(patch).expect("parse unified diff");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOp::Update { path, hunks } => {
                assert_eq!(path, "src.txt");
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].removed_lines(), 1);
                assert_eq!(hunks[0].added_lines(), 1);
            }
            _ => panic!("expected update op"),
        }
    }

    #[test]
    fn parse_apply_patch_rejects_legacy_patch_grammar() {
        let patch = "\
*** Begin Patch
*** Update File: src.txt
@@
-old
+new
*** End Patch";
        let error = match parse_apply_patch(patch) {
            Ok(_) => panic!("legacy grammar should fail"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("unified diff must contain `--- <path>`"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn build_patch_preview_in_root_tracks_update_line_numbers() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let file_path = tempdir.path().join("src.txt");
        tokio::fs::write(&file_path, "alpha\nbeta\ngamma\n")
            .await
            .expect("write fixture");

        let patch = "\
--- a/src.txt
+++ b/src.txt
@@ -1,3 +1,3 @@
 alpha
-beta
+beta changed
 gamma";

        let preview = build_patch_preview_in_root(tempdir.path(), patch)
            .await
            .expect("build preview");
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].path, "src.txt");
        assert_eq!(
            preview[0]
                .diff_lines
                .iter()
                .map(|line| (
                    line.kind,
                    line.old_lineno,
                    line.new_lineno,
                    line.text.clone()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    PatchPreviewLineKind::Context,
                    Some(1),
                    Some(1),
                    "alpha".to_string(),
                ),
                (
                    PatchPreviewLineKind::Delete,
                    Some(2),
                    None,
                    "beta".to_string(),
                ),
                (
                    PatchPreviewLineKind::Add,
                    None,
                    Some(2),
                    "beta changed".to_string(),
                ),
                (
                    PatchPreviewLineKind::Context,
                    Some(3),
                    Some(3),
                    "gamma".to_string(),
                ),
            ]
        );
    }
}
