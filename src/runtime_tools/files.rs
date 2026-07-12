use std::path::{Path, PathBuf};

use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{
    activity_event::{
        CodingEditActivityDescriptor, EXPLORED_STABLE_ID, ExploredActivityDescriptor,
        ExploredCallActivityAction, ExploredCallActivityDescriptor,
        PatchDiffLineActivityDescriptor, PatchDiffLineKind, PatchFileActivityDescriptor,
        PatchFileOperation, ToolCallActivityEvent,
    },
    context::Context,
    dashboard::SessionActivityEvent,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    runtime_tools::{
        RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    },
    schema_utils::{model_schema_for, structured_edit_args_schema},
};

const DEFAULT_READ_LINE_COUNT: usize = 80;

#[model_schema]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    line_count: Option<usize>,
    #[serde(default)]
    force_no_elide: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditFileArgs {
    edits: Vec<scope_engine::api::StructuredEdit>,
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![
        Box::new(StaticRuntimeTool::new_with_schema(
            "read_file",
            "Read a file path or path range and return line-hash anchored source lines. Lines already shown earlier in context are automatically elided with ~ to save space. If you need the exact original content of previously-seen lines (e.g. to count indent, verify characters, or perform other precise inspection), set force_no_elide=true to disable elision for this call.",
            model_schema_for::<ReadFileArgs>(),
            summarize_read_file_tool,
            render_read_file_call_ui,
            execute_read_file_runtime_tool,
        )),
        Box::new(StaticRuntimeTool::new_with_schema(
            "edit_file",
            "Apply structured line-hash anchored edits to ordinary files without SCOPE propagation review.",
            structured_edit_args_schema(),
            summarize_edit_file_tool,
            render_edit_file_call_ui,
            execute_edit_file_runtime_tool,
        )),
    ]
}

fn summarize_read_file_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: ReadFileArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "read_file".to_string(),
        summary: read_file_target_summary(&args),
    })
}

fn summarize_edit_file_tool(call: &AgentToolCall) -> Result<EpisodeActionRecord> {
    let args: EditFileArgs = parse_tool_args(call)?;
    Ok(EpisodeActionRecord {
        kind: "edit_file".to_string(),
        summary: format!("edits_count={}", args.edits.len()),
    })
}

fn render_read_file_call_ui(call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
    let args: ReadFileArgs = parse_tool_args(call)?;
    Ok(ToolCallActivityEvent::app(
        "Read File",
        vec![read_file_target_summary(&args)],
    ))
}

fn render_edit_file_call_ui(call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
    let args: EditFileArgs = parse_tool_args(call)?;
    let files = args
        .edits
        .iter()
        .map(|edit| PatchFileActivityDescriptor {
            path: edit.path.clone(),
            operation: PatchFileOperation::Update,
            added_lines: edit_content_line_count(edit.content.as_ref()),
            removed_lines: 0,
            diff_lines: structured_edit_preview_lines(edit.content.as_ref()),
        })
        .collect::<Vec<_>>();
    let added_lines = files.iter().map(|file| file.added_lines).sum::<usize>();
    Ok(ToolCallActivityEvent::coding_edit(
        CodingEditActivityDescriptor {
            stable_id: edit_file_stable_id(call),
            title: "File Edit".to_string(),
            tool_name: Some("edit_file".to_string()),
            tool_app: Some("Workspace".to_string()),
            selector: format!("{} structured edit(s)", args.edits.len()),
            file: args.edits.first().map(|edit| edit.path.clone()),
            added_lines,
            removed_lines: 0,
            propagation_count: 0,
            impact_lines: Vec::new(),
            diff_files: files,
        },
    ))
}

fn execute_read_file_runtime_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: ReadFileArgs = parse_tool_args(call)?;
        let resolved = resolve_runtime_file_path(context, &args.path);
        context
            .sandbox_policy
            .ensure_path_readable(&resolved, "read_file target")?;
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|err| miette!("failed to read {}: {err}", resolved.display()))?;
        let start_line = args.start_line.unwrap_or(1);
        if start_line == 0 {
            return Err(miette!("read_file `start_line` must be >= 1"));
        }
        let line_count = args.line_count.unwrap_or(DEFAULT_READ_LINE_COUNT).max(1);
        let total_lines = content.lines().count();
        if total_lines == 0 {
            if start_line != 1 {
                return Err(miette!(
                    "read_file line range starts after end of empty file: {start_line}"
                ));
            }
        } else if start_line > total_lines {
            return Err(miette!(
                "read_file line range starts after end of file: {start_line} > {total_lines}"
            ));
        }
        let end_line = if total_lines == 0 {
            0
        } else {
            start_line
                .saturating_add(line_count)
                .saturating_sub(1)
                .min(total_lines)
        };
        let model_content = prefix_file_lines_with_hash(&content, start_line, line_count);
        let display_path = display_tool_path(&args.path, &resolved);
        let actual_line_count = if total_lines == 0 {
            0
        } else {
            end_line - start_line + 1
        };
        let summary = if total_lines == 0 {
            format!("read {display_path} (empty file)")
        } else {
            format!("read {display_path}#L{start_line}-L{end_line}")
        };
        let ui_summary = if total_lines == 0 {
            format!("{display_path} (empty file)")
        } else {
            format!("{display_path}#L{start_line}-L{end_line}")
        };
        let skip_elision = args.force_no_elide.unwrap_or(false);
        Ok(ToolExecutionResult::from_activity_event(
            summary,
            json!({
                "path": args.path,
                "resolved_path": resolved.display().to_string(),
                "start_line": start_line,
                "end_line": end_line,
                "line_count": actual_line_count,
                "total_lines": total_lines,
                "content": model_content,
            }),
            Some(explored_tool_event(
                "Read",
                Some(ExploredCallActivityAction::Read),
                Some(args.path.clone()),
                None,
                ui_summary,
                vec![format!("{actual_line_count} lines")],
            )),
        )
        .with_model_content(model_content)
        .with_skip_source_elision(skip_elision))
    })
}

fn execute_edit_file_runtime_tool<'a>(
    context: &'a mut Context,
    call: &'a AgentToolCall,
) -> ToolFuture<'a> {
    Box::pin(async move {
        let args: EditFileArgs = parse_tool_args(call)?;
        if args.edits.is_empty() {
            return Err(miette!("edit_file `edits` must not be empty"));
        }
        for edit in &args.edits {
            let resolved = resolve_runtime_file_path(context, &edit.path);
            if resolved.exists() {
                context
                    .sandbox_policy
                    .ensure_path_readable(&resolved, "edit_file target")?;
            }
            context
                .sandbox_policy
                .ensure_path_writable(&resolved, "edit_file target")?;
        }
        let result = scope_engine::patch::edit_file_apply(&args.edits, &context.execution_cwd)
            .map_err(|err| miette!("edit_file failed: {err}"))?;
        let added_lines = result
            .files
            .iter()
            .map(|file| file.added_lines)
            .sum::<usize>();
        let removed_lines = result
            .files
            .iter()
            .map(|file| file.removed_lines)
            .sum::<usize>();
        let diff_files = applied_edit_ui_files(&result);
        let title = if result.files.len() == 1 {
            "Edited File"
        } else {
            "Edited Files"
        };
        Ok(ToolExecutionResult::from_activity_event(
            format!("edited {} file(s)", result.files.len()),
            json!({
                "changed_files": result.files.len(),
                "added_lines": added_lines,
                "removed_lines": removed_lines,
                "files": result.files.iter().map(|file| {
                    json!({
                        "path": file.path,
                        "operation": match file.operation {
                            scope_engine::api::AppliedStructuredEditOperation::Add => "add",
                            scope_engine::api::AppliedStructuredEditOperation::Update => "update",
                        },
                        "added_lines": file.added_lines,
                        "removed_lines": file.removed_lines,
                    })
                }).collect::<Vec<_>>(),
            }),
            Some(SessionActivityEvent::CodingEdit(
                CodingEditActivityDescriptor {
                    stable_id: edit_file_stable_id(call),
                    title: title.to_string(),
                    tool_name: Some("edit_file".to_string()),
                    tool_app: Some("Workspace".to_string()),
                    selector: "hash-anchored file edit".to_string(),
                    file: result.files.first().map(|file| file.path.clone()),
                    added_lines,
                    removed_lines,
                    propagation_count: 0,
                    impact_lines: Vec::new(),
                    diff_files,
                }
                .into(),
            )),
        ))
    })
}

fn explored_tool_event(
    tool_name: impl Into<String>,
    action: Option<ExploredCallActivityAction>,
    target: Option<String>,
    secondary_target: Option<String>,
    summary: impl Into<String>,
    detail_lines: Vec<String>,
) -> SessionActivityEvent {
    SessionActivityEvent::Explored(
        ExploredActivityDescriptor {
            stable_id: EXPLORED_STABLE_ID.to_string(),
            title: "Explored".to_string(),
            calls: vec![ExploredCallActivityDescriptor {
                tool_name: tool_name.into(),
                action,
                target,
                secondary_target,
                summary: summary.into(),
                detail_lines,
            }],
        }
        .into(),
    )
}

fn resolve_runtime_file_path(context: &Context, path: &str) -> PathBuf {
    context
        .sandbox_policy
        .resolve_path(Path::new(path), Some(&context.execution_cwd))
}

fn read_file_target_summary(args: &ReadFileArgs) -> String {
    match (args.start_line, args.line_count) {
        (Some(start), Some(count)) => format!("{}#L{}+{}", args.path, start, count),
        (Some(start), None) => format!("{}#L{}+default", args.path, start),
        (None, Some(count)) => format!("{}#L1+{}", args.path, count),
        (None, None) => format!("{}#L1+default", args.path),
    }
}

fn prefix_file_lines_with_hash(content: &str, start_line: usize, line_count: usize) -> String {
    content
        .lines()
        .skip(start_line.saturating_sub(1))
        .take(line_count)
        .enumerate()
        .map(|(index, line)| {
            let line_num = start_line + index;
            let hash = scope_engine::patch::line_hash(line);
            format!("{line_num}#{hash}|{line}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn display_tool_path(requested: &str, resolved: &Path) -> String {
    if requested.trim().is_empty() {
        resolved.display().to_string()
    } else {
        requested.to_string()
    }
}

fn edit_file_stable_id(call: &AgentToolCall) -> String {
    let suffix = call
        .id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if suffix.is_empty() {
        "file-edit-unknown".to_string()
    } else {
        format!("file-edit-{suffix}")
    }
}

fn applied_edit_ui_files(
    summary: &scope_engine::api::AppliedStructuredEditSummary,
) -> Vec<PatchFileActivityDescriptor> {
    summary
        .files
        .iter()
        .map(|file| PatchFileActivityDescriptor {
            path: file.path.clone(),
            operation: match file.operation {
                scope_engine::api::AppliedStructuredEditOperation::Add => PatchFileOperation::Add,
                scope_engine::api::AppliedStructuredEditOperation::Update => {
                    PatchFileOperation::Update
                }
            },
            added_lines: file.added_lines,
            removed_lines: file.removed_lines,
            diff_lines: applied_edit_diff_lines(&file.original_content, &file.new_content),
        })
        .collect()
}

fn applied_edit_diff_lines(
    original: &str,
    new_content: &str,
) -> Vec<PatchDiffLineActivityDescriptor> {
    let patch = diffy::create_patch(original, new_content);
    let mut lines = Vec::new();

    for (hunk_index, hunk) in patch.hunks().iter().enumerate() {
        if hunk_index > 0 {
            lines.push(PatchDiffLineActivityDescriptor {
                kind: PatchDiffLineKind::HunkBreak,
                old_lineno: None,
                new_lineno: None,
                text: String::new(),
            });
        }

        let mut old_lineno = hunk.old_range().start();
        let mut new_lineno = hunk.new_range().start();
        for line in hunk.lines() {
            match line {
                diffy::Line::Context(text) => {
                    lines.push(PatchDiffLineActivityDescriptor {
                        kind: PatchDiffLineKind::Context,
                        old_lineno: Some(old_lineno),
                        new_lineno: Some(new_lineno),
                        text: diff_line_text(text),
                    });
                    old_lineno += 1;
                    new_lineno += 1;
                }
                diffy::Line::Delete(text) => {
                    lines.push(PatchDiffLineActivityDescriptor {
                        kind: PatchDiffLineKind::Delete,
                        old_lineno: Some(old_lineno),
                        new_lineno: None,
                        text: diff_line_text(text),
                    });
                    old_lineno += 1;
                }
                diffy::Line::Insert(text) => {
                    lines.push(PatchDiffLineActivityDescriptor {
                        kind: PatchDiffLineKind::Add,
                        old_lineno: None,
                        new_lineno: Some(new_lineno),
                        text: diff_line_text(text),
                    });
                    new_lineno += 1;
                }
            }
        }
    }

    lines
}

fn diff_line_text(text: &str) -> String {
    text.trim_end_matches(['\r', '\n']).to_string()
}
fn edit_content_line_count(content: Option<&scope_engine::api::EditContent>) -> usize {
    match content {
        Some(scope_engine::api::EditContent::Lines(lines)) => lines.len(),
        Some(scope_engine::api::EditContent::Text(text)) => text.lines().count(),
        None => 0,
    }
}

fn structured_edit_preview_lines(
    content: Option<&scope_engine::api::EditContent>,
) -> Vec<PatchDiffLineActivityDescriptor> {
    match content {
        Some(scope_engine::api::EditContent::Lines(lines)) => {
            lines.iter().map(patch_preview_add_line).collect()
        }
        Some(scope_engine::api::EditContent::Text(text)) => {
            text.lines().map(patch_preview_add_line).collect()
        }
        None => Vec::new(),
    }
}

fn patch_preview_add_line(line: impl AsRef<str>) -> PatchDiffLineActivityDescriptor {
    PatchDiffLineActivityDescriptor {
        kind: PatchDiffLineKind::Add,
        old_lineno: None,
        new_lineno: None,
        text: line.as_ref().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_edit_preview_lines_render_added_diff_rows() {
        let lines = structured_edit_preview_lines(Some(&scope_engine::api::EditContent::Text(
            "alpha\nbeta".to_string(),
        )));

        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.kind == PatchDiffLineKind::Add));
        assert_eq!(lines[0].text, "alpha");
        assert_eq!(lines[1].text, "beta");
    }
}
