//! Plain files, images, and discoverable instruction resources.
use std::path::PathBuf;

use async_trait::async_trait;
use miette::{Result, miette};
use serde_json::json;

use crate::{
    actors::ToolPermissionRequest,
    app::{
        App, AppDocs, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec,
        EmptyAppArgs,
    },
    file_io::{
        self, CopyFileArgs, EditFileArgs, ListDirectoryArgs, ReadFileArgs, SearchTextArgs,
        ViewImageArgs, WriteFileArgs,
    },
    model_protocol::AgentToolCall,
    openskills::load_openskills_with_policy,
    schema_utils::model_schema_for,
};

pub struct FilesApp {
    image_dir: PathBuf,
    supports_vision: bool,
}

impl FilesApp {
    pub fn new(image_dir: PathBuf, supports_vision: bool) -> Self {
        Self {
            image_dir,
            supports_vision,
        }
    }
}

#[async_trait]
impl App for FilesApp {
    fn can_passivate(&self) -> bool {
        true
    }
    fn id(&self) -> AppId {
        AppId::new("files").expect("built-in App id")
    }

    fn docs(&self) -> AppDocs {
        AppDocs::from_markdown(include_str!("app_instructions/files.md"))
    }

    fn tool_specs(&self) -> Vec<AppToolSpec> {
        let mut specs = vec![
            AppToolSpec { name: "copy_file".into(), description: "Copy authorized file bytes to a writable workspace. Use this to bring an Actor attachment into the workspace before native commands. Existing destinations require expected_sha256.".into(), input_schema: model_schema_for::<CopyFileArgs>() },
            AppToolSpec { name: "list_directory".into(), description: "List an authorized directory page, ordered by name. Continue using next_cursor.".into(), input_schema: model_schema_for::<ListDirectoryArgs>() },
            AppToolSpec { name: "search_text".into(), description: "Find literal text in authorized files beneath a path. Returns original matching lines with file paths and line numbers.".into(), input_schema: model_schema_for::<SearchTextArgs>() },
            AppToolSpec { name: "read_file".into(), description: "Read plaintext lines and the whole-file sha256. Relative paths use the Actor workspace.".into(), input_schema: model_schema_for::<ReadFileArgs>() },
            AppToolSpec { name: "write_file".into(), description: "Create a file or overwrite it using its current expected_sha256.".into(), input_schema: model_schema_for::<WriteFileArgs>() },
            AppToolSpec { name: "edit_file".into(), description: "Replace exact unique old_text using the current file expected_sha256.".into(), input_schema: model_schema_for::<EditFileArgs>() },
            AppToolSpec { name: "list_skills".into(), description: "List authorized skill metadata and source paths. Read selected SKILL.md files with read_file.".into(), input_schema: model_schema_for::<EmptyAppArgs>() },
        ];
        if self.supports_vision {
            specs.push(AppToolSpec {
                name: "view_image".into(),
                description: "Read a local image into the model context.".into(),
                input_schema: model_schema_for::<ViewImageArgs>(),
            });
        }
        specs
    }

    fn permission_request(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<ToolPermissionRequest> {
        if call.name == "copy_file" {
            let args: CopyFileArgs = serde_json::from_value(call.arguments.clone())
                .map_err(|_| miette!("invalid copy_file arguments"))?;
            let source = file_io::resolve_file_path(&context.execution_cwd, &args.source)?;
            let destination =
                file_io::resolve_file_path(&context.execution_cwd, &args.destination)?;
            let parent = destination
                .parent()
                .ok_or_else(|| miette!("copy destination has no parent"))?
                .to_path_buf();
            return Ok(ToolPermissionRequest {
                operation_details: serde_json::Value::Null,
                readable_roots: vec![source, destination]
                    .into_iter()
                    .filter(|path| !context.sandbox_policy.is_path_readable(path))
                    .collect(),
                writable_roots: if context.sandbox_policy.is_path_writable(&parent) {
                    Vec::new()
                } else {
                    vec![parent]
                },
            });
        }
        let path = call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str);
        let Some(path) = path else {
            return Ok(ToolPermissionRequest::default());
        };
        let actual = file_io::resolve_file_path(&context.execution_cwd, path)?;
        let write = matches!(call.name.as_str(), "write_file" | "edit_file");
        let writable_roots = if write {
            let parent = actual
                .parent()
                .ok_or_else(|| miette!("file path has no parent"))?;
            if context.sandbox_policy.is_path_writable(parent) {
                Vec::new()
            } else {
                vec![parent.to_path_buf()]
            }
        } else {
            Vec::new()
        };
        Ok(ToolPermissionRequest {
            operation_details: serde_json::Value::Null,
            readable_roots: if context.sandbox_policy.is_path_readable(&actual) {
                Vec::new()
            } else {
                vec![actual]
            },
            writable_roots,
        })
    }

    fn before_tool_call(
        &self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<()> {
        if call.name == "copy_file" {
            let args: CopyFileArgs = serde_json::from_value(call.arguments.clone())
                .map_err(|_| miette!("invalid copy_file arguments"))?;
            let source = file_io::resolve_file_path(&context.execution_cwd, &args.source)?;
            let destination =
                file_io::resolve_file_path(&context.execution_cwd, &args.destination)?;
            context
                .sandbox_policy
                .ensure_path_readable(&source, "copy source")?;
            context
                .sandbox_policy
                .ensure_path_writable(&destination, "copy destination")?;
            context.sandbox_policy.ensure_path_writable(
                destination
                    .parent()
                    .ok_or_else(|| miette!("copy destination has no parent"))?,
                "atomic copy directory",
            )?;
        }
        if let Some(path) = call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
        {
            let actual = file_io::resolve_file_path(&context.execution_cwd, path)?;
            if matches!(call.name.as_str(), "write_file" | "edit_file") {
                context
                    .sandbox_policy
                    .ensure_path_writable(&actual, "file target")?;
                let parent = actual
                    .parent()
                    .ok_or_else(|| miette!("file path has no parent"))?;
                context
                    .sandbox_policy
                    .ensure_path_writable(parent, "atomic write directory")?;
            } else {
                context
                    .sandbox_policy
                    .ensure_path_readable(&actual, "file target")?;
            }
        }
        Ok(())
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        self.before_tool_call(call, context)?;
        match call.name.as_str() {
            "copy_file" => {
                let args: CopyFileArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|_| miette!("invalid copy_file arguments"))?;
                let result =
                    file_io::copy_file(&context.execution_cwd, &context.sandbox_policy, &args)?;
                Ok(AppToolExecutionResult::new(
                    format!("copied {}", args.destination),
                    serde_json::to_value(result)
                        .map_err(|_| miette!("copy result serialization failed"))?,
                    None,
                ))
            }
            "list_directory" => {
                let args: ListDirectoryArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|_| miette!("invalid list_directory arguments"))?;
                let result = file_io::list_directory(
                    &context.execution_cwd,
                    &context.sandbox_policy,
                    &args,
                )?;
                Ok(AppToolExecutionResult::new(
                    format!("listed {} entries", result.entries.len()),
                    serde_json::to_value(result)
                        .map_err(|_| miette!("directory result serialization failed"))?,
                    None,
                ))
            }
            "search_text" => {
                let args: SearchTextArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|_| miette!("invalid search_text arguments"))?;
                let result =
                    file_io::search_text(&context.execution_cwd, &context.sandbox_policy, &args)?;
                Ok(AppToolExecutionResult::new(
                    format!("found {} matching lines", result.matches.len()),
                    serde_json::to_value(result)
                        .map_err(|_| miette!("search result serialization failed"))?,
                    None,
                ))
            }
            "read_file" => {
                let mut args: ReadFileArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|_| miette!("invalid read_file arguments"))?;
                args.line_count = Some(args.line_count.unwrap_or(80).clamp(1, 2000));
                let result =
                    file_io::read_file(&context.execution_cwd, &context.sandbox_policy, &args)?;
                // Struct serialization keeps the freshness and pagination metadata
                // before the plaintext when the execution loop bounds model output.
                let content = serde_json::to_string(&result)
                    .map_err(|err| miette!("file result serialization failed: {err}"))?;
                let mut output = AppToolExecutionResult::new(
                    format!("read {} lines from {}", result.line_count, args.path),
                    serde_json::to_value(&result)
                        .map_err(|err| miette!("file result serialization failed: {err}"))?,
                    Some(content),
                );
                if result.content.lines().all(|line| {
                    line.len().saturating_mul(6)
                        < context
                            .tool_output_max_tokens
                            .saturating_mul(4)
                            .saturating_sub(1024)
                }) {
                    output.overflow_continuation = Some(format!(
                        "Re-read this file range using files__read_file: path={}, start_line={}, line_count=1. Advance one line at a time to retain every source line.",
                        serde_json::to_string(&args.path)
                            .map_err(|_| miette!("invalid file path"))?,
                        result.start_line
                    ));
                }
                Ok(output)
            }
            "write_file" => {
                let args: WriteFileArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|_| miette!("invalid write_file arguments"))?;
                let result =
                    file_io::write_file(&context.execution_cwd, &context.sandbox_policy, &args)?;
                Ok(AppToolExecutionResult::new(
                    format!("wrote {}", args.path),
                    serde_json::to_value(result)
                        .map_err(|err| miette!("file result serialization failed: {err}"))?,
                    None,
                ))
            }
            "edit_file" => {
                let args: EditFileArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|_| miette!("invalid edit_file arguments"))?;
                let result =
                    file_io::edit_file(&context.execution_cwd, &context.sandbox_policy, &args)?;
                Ok(AppToolExecutionResult::new(
                    format!("edited {}", args.path),
                    serde_json::to_value(result)
                        .map_err(|err| miette!("edit result serialization failed: {err}"))?,
                    None,
                ))
            }
            "view_image" if self.supports_vision => {
                let args: ViewImageArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|_| miette!("invalid view_image arguments"))?;
                let viewed = file_io::view_image(
                    &context.execution_cwd,
                    &context.sandbox_policy,
                    &self.image_dir,
                    self.supports_vision,
                    &args,
                )?;
                let mut result = AppToolExecutionResult::new(
                    format!("viewed image {}", args.path),
                    json!({ "path": args.path, "media_type": viewed.media_type, "byte_count": viewed.byte_count }),
                    None,
                );
                result.model_images.push(viewed.model_image);
                Ok(result)
            }
            "list_skills" => {
                let catalog =
                    load_openskills_with_policy(&context.execution_cwd, &context.sandbox_policy);
                let skills = catalog.skills().iter().map(|skill| json!({ "name": skill.name, "description": skill.description, "path": skill.path, "auto_use_enabled": skill.auto_use_enabled() })).collect::<Vec<_>>();
                Ok(AppToolExecutionResult::new(
                    format!("listed {} skills", skills.len()),
                    json!({ "skills": skills }),
                    catalog.render_prompt_block(),
                ))
            }
            _ => Err(miette!("unknown Files tool")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::RuntimeSandboxPolicy;

    #[tokio::test]
    async fn permission_request_is_effect_free_and_final_context_controls_access() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("new.txt");
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        policy.filesystem.readable_roots = vec![dir.path().into()];
        policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
            root: dir.path().into(),
            read_only_subpaths: vec![],
        }];
        let base = AppToolExecutionContext {
            execution_cwd: dir.path().into(),
            sandbox_policy: policy,
            tool_output_max_tokens: 1000,
            turn_epoch: 1,
            actor_call: None,
        };
        let mut manager = crate::app::AppManager::new(vec![Box::new(FilesApp::new(
            dir.path().join("images"),
            false,
        ))])
        .unwrap();
        let call = AgentToolCall {
            id: "call".into(),
            name: "files__write_file".into(),
            arguments: json!({"path": target, "content":"approved", "expected_sha256":null}),
        };
        manager
            .validate_tool_call_for_approval(&call, &base)
            .unwrap();
        let request = manager.permission_request(&call, &base).unwrap();
        assert_eq!(
            request.writable_roots,
            [outside.path().canonicalize().unwrap()]
        );
        assert_eq!(
            request.readable_roots,
            [outside.path().canonicalize().unwrap().join("new.txt")]
        );
        assert!(!target.exists());
        assert!(manager.validate_tool_call(&call, &base).is_err());
        let mut effective = base.clone();
        effective
            .sandbox_policy
            .filesystem
            .readable_roots
            .extend(request.readable_roots);
        effective.sandbox_policy.filesystem.writable_roots.extend(
            request
                .writable_roots
                .into_iter()
                .map(|root| crate::sandbox::WritableRoot {
                    root,
                    read_only_subpaths: vec![],
                }),
        );
        manager.validate_tool_call(&call, &effective).unwrap();
        manager.execute_tool(&call, &effective).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "approved");
        assert!(manager.validate_tool_call(&call, &base).is_err());
    }

    #[tokio::test]
    async fn denied_write_does_not_change_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.txt"), "alpha\n").unwrap();
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_write = false;
        policy.filesystem.writable_roots.clear();
        let context = AppToolExecutionContext {
            execution_cwd: dir.path().into(),
            sandbox_policy: policy,
            tool_output_max_tokens: 1000,
            turn_epoch: 1,
            actor_call: None,
        };
        let mut app = FilesApp::new(dir.path().join("images"), false);
        let call = AgentToolCall {
            id: "call".into(),
            name: "edit_file".into(),
            arguments: json!({"path":"source.txt", "old_text":"alpha", "new_text":"changed", "expected_sha256": "unused"}),
        };
        assert!(app.execute_tool(&call, &context).await.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("source.txt")).unwrap(),
            "alpha\n"
        );
    }
}
