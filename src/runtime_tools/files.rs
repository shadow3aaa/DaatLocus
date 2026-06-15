use std::path::{Path, PathBuf};

use miette::{Result, miette};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{
    context::Context,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    runtime_tools::{
        RuntimeTool, StaticRuntimeTool, ToolExecutionResult, ToolFuture, parse_tool_args,
    },
    schema_utils::structured_edit_args_schema,
    tool_ui::{
        EXPLORED_STABLE_ID, ExploredCallUiAction, ExploredCallUiData, PatchDiffLineKind,
        PatchDiffLineUiData, PatchFileOperation, PatchFileUiData, ToolCallUiEvent, ToolUiEvent,
    },
};

const DEFAULT_READ_LINE_COUNT: usize = 80;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    line_count: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditFileArgs {
    edits: Vec<scope_engine::api::StructuredEdit>,
}

pub(super) fn register_tools() -> Vec<Box<dyn RuntimeTool>> {
    vec![
        Box::new(StaticRuntimeTool::new::<ReadFileArgs>(
            "read_file",
            "Read a file path or path range and return line-hash anchored source lines.",
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

fn render_read_file_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: ReadFileArgs = parse_tool_args(call)?;
    Ok(ToolCallUiEvent::app(
        "Read File",
        vec![read_file_target_summary(&args)],
    ))
}

fn render_edit_file_call_ui(call: &AgentToolCall) -> Result<ToolCallUiEvent> {
    let args: EditFileArgs = parse_tool_args(call)?;
    let files = args
        .edits
        .iter()
        .map(|edit| PatchFileUiData {
            path: edit.path.clone(),
            operation: PatchFileOperation::Update,
            added_lines: edit_content_line_count(edit.content.as_ref()),
            removed_lines: 0,
            diff_lines: structured_edit_preview_lines(edit.content.as_ref()),
        })
        .collect();
    Ok(ToolCallUiEvent::patch(
        format!("{} structured edit(s)", args.edits.len()),
        files,
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
        Ok(ToolExecutionResult::new(
            summary.clone(),
            json!({
                "path": args.path,
                "resolved_path": resolved.display().to_string(),
                "start_line": start_line,
                "end_line": end_line,
                "line_count": actual_line_count,
                "total_lines": total_lines,
                "content": model_content,
            }),
            explored_tool_event(
                "Read",
                Some(ExploredCallUiAction::Read),
                Some(args.path.clone()),
                None,
                ui_summary,
                vec![format!("{actual_line_count} lines")],
            ),
        )
        .with_model_content(model_content))
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
        let ui_summary = match result.files.as_slice() {
            [file] => file.path.clone(),
            files => format!("{} files", files.len()),
        };
        let mut detail_lines = vec![format!("+{} -{}", added_lines, removed_lines)];
        detail_lines.extend(result.files.iter().map(|file| {
            format!(
                "{} (+{} -{})",
                file.path, file.added_lines, file.removed_lines
            )
        }));
        Ok(ToolExecutionResult::new(
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
            explored_tool_event("Edit", None, None, None, ui_summary, detail_lines),
        ))
    })
}

fn explored_tool_event(
    tool_name: impl Into<String>,
    action: Option<ExploredCallUiAction>,
    target: Option<String>,
    secondary_target: Option<String>,
    summary: impl Into<String>,
    detail_lines: Vec<String>,
) -> ToolUiEvent {
    ToolUiEvent::explored(
        EXPLORED_STABLE_ID,
        "Explored",
        vec![ExploredCallUiData {
            tool_name: tool_name.into(),
            action,
            target,
            secondary_target,
            summary: summary.into(),
            detail_lines,
        }],
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

fn edit_content_line_count(content: Option<&scope_engine::api::EditContent>) -> usize {
    match content {
        Some(scope_engine::api::EditContent::Lines(lines)) => lines.len(),
        Some(scope_engine::api::EditContent::Text(text)) => text.lines().count(),
        None => 0,
    }
}

fn structured_edit_preview_lines(
    content: Option<&scope_engine::api::EditContent>,
) -> Vec<PatchDiffLineUiData> {
    match content {
        Some(scope_engine::api::EditContent::Lines(lines)) => lines
            .iter()
            .map(|line| patch_preview_add_line(line))
            .collect(),
        Some(scope_engine::api::EditContent::Text(text)) => {
            text.lines().map(patch_preview_add_line).collect()
        }
        None => Vec::new(),
    }
}

fn patch_preview_add_line(line: impl AsRef<str>) -> PatchDiffLineUiData {
    PatchDiffLineUiData {
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
