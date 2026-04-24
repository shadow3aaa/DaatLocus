use crate::{
    apply_patch::{
        PatchOperationKind, PatchPreviewLineKind, apply_patch_in_root, build_patch_preview_in_root,
    },
    context::Context,
    runtime_tools::ToolExecutionResult,
    tool_ui::ToolUiEvent,
};
use serde_json::json;

pub(crate) async fn execute_apply_patch_tool(
    context: &Context,
    patch_text: &str,
) -> miette::Result<ToolExecutionResult> {
    let preview_files = build_patch_preview_in_root(&context.execution_cwd, patch_text).await?;
    let summary =
        apply_patch_in_root(&context.execution_cwd, &context.sandbox_policy, patch_text).await?;
    Ok(ToolExecutionResult::new(
        format!("patched {} file(s)", summary.changed_files),
        json!({
            "changed_files": summary.changed_files,
            "added_files": summary.added_files,
            "deleted_files": summary.deleted_files,
            "updated_files": summary.updated_files,
            "added_lines": summary.added_lines,
            "removed_lines": summary.removed_lines,
            "files": summary.files.iter().map(|file| {
                json!({
                    "path": file.path,
                    "operation": match file.operation {
                        PatchOperationKind::Add => "add",
                        PatchOperationKind::Delete => "delete",
                        PatchOperationKind::Update => "update",
                    },
                    "added_lines": file.added_lines,
                    "removed_lines": file.removed_lines,
                })
            }).collect::<Vec<_>>(),
        }),
        ToolUiEvent::patch(
            format!(
                "{} file(s) changed (+{} -{})",
                summary.changed_files, summary.added_lines, summary.removed_lines
            ),
            preview_files
                .into_iter()
                .map(|file| crate::tool_ui::PatchFileUiData {
                    path: file.path,
                    operation: match file.operation {
                        PatchOperationKind::Add => crate::tool_ui::PatchFileOperation::Add,
                        PatchOperationKind::Delete => crate::tool_ui::PatchFileOperation::Delete,
                        PatchOperationKind::Update => crate::tool_ui::PatchFileOperation::Update,
                    },
                    added_lines: file.added_lines,
                    removed_lines: file.removed_lines,
                    diff_lines: file
                        .diff_lines
                        .into_iter()
                        .map(|line| crate::tool_ui::PatchDiffLineUiData {
                            kind: match line.kind {
                                PatchPreviewLineKind::Context => {
                                    crate::tool_ui::PatchDiffLineKind::Context
                                }
                                PatchPreviewLineKind::Delete => {
                                    crate::tool_ui::PatchDiffLineKind::Delete
                                }
                                PatchPreviewLineKind::Add => crate::tool_ui::PatchDiffLineKind::Add,
                                PatchPreviewLineKind::HunkBreak => {
                                    crate::tool_ui::PatchDiffLineKind::HunkBreak
                                }
                            },
                            old_lineno: line.old_lineno,
                            new_lineno: line.new_lineno,
                            text: line.text,
                        })
                        .collect(),
                })
                .collect(),
        ),
    ))
}
