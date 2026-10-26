//! Plain files, images, and discoverable instruction resources.
use std::path::PathBuf;

use async_trait::async_trait;
use miette::{Result, miette};
use serde_json::json;

use crate::{
    app::{App, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec, EmptyAppArgs},
    file_io::{
        CopyFileArgs, EditFileArgs, ListDirectoryArgs, ReadFileArgs, SearchTextArgs, ViewImageArgs,
        WriteFileArgs,
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

    fn tool_specs(&self) -> Vec<AppToolSpec> {
        let mut specs = vec![
            AppToolSpec { name: "copy_file".into(), description: "Copy authorized file bytes to a writable workspace. Use this to bring an Actor attachment into the workspace before native commands. Existing destinations require expected_sha256.".into(), input_schema: model_schema_for::<CopyFileArgs>() },
            AppToolSpec { name: "list_directory".into(), description: "List an authorized directory page, ordered by name. Continue using next_cursor.".into(), input_schema: model_schema_for::<ListDirectoryArgs>() },
            AppToolSpec { name: "search_text".into(), description: "Find literal text in authorized files beneath a path. Requires an explicit depth (1-32). Respects repository and global Git ignore rules and skips .git metadata unless include_ignored is true. Explicit search roots remain searchable. Returns original matching lines with file paths and line numbers.".into(), input_schema: model_schema_for::<SearchTextArgs>() },
            AppToolSpec { name: "read_file".into(), description: "Read plaintext lines and the whole-file sha256. Relative paths use the Actor workspace. Expand skill root aliases to their listed absolute paths before calling; start_line is 1-based or null, never 0.".into(), input_schema: model_schema_for::<ReadFileArgs>() },
            AppToolSpec { name: "write_file".into(), description: "Create a file or overwrite it using its current expected_sha256.".into(), input_schema: model_schema_for::<WriteFileArgs>() },
            AppToolSpec { name: "edit_file".into(), description: "Replace text using the current file expected_sha256: either one exact unique old_text/new_text pair or an ordered edits array applied sequentially on the updated text. Every edit must match uniquely (LF/CRLF equivalent) with new_text written in the matched region's line endings; any failure aborts the entire batch before a single write.".into(), input_schema: model_schema_for::<EditFileArgs>() },
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

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        match call.name.as_str() {
            "copy_file" => {
                let args: CopyFileArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|_| miette!("invalid copy_file arguments"))?;
                let result = context.files().copy_file(&args).await?;
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
                let result = context.files().list_directory(&args).await?;
                Ok(AppToolExecutionResult::new(
                    format!("listed {} entries", result.entries.len()),
                    serde_json::to_value(result)
                        .map_err(|_| miette!("directory result serialization failed"))?,
                    None,
                ))
            }
            "search_text" => {
                let args: SearchTextArgs = serde_json::from_value(call.arguments.clone())
                    .map_err(|error| miette!("invalid search_text arguments: {error}"))?;
                let result = context.files().search_text(&args).await?;
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
                let result = context.files().read_file(&args).await?;
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
                let result = context.files().write_file(&args).await?;
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
                let result = context.files().edit_file(&args).await?;
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
                let viewed = context
                    .files()
                    .view_image(&self.image_dir, self.supports_vision, &args)
                    .await?;
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

    const BUILTIN_SKILL: &[u8] = b"---\nname: builtin-fixture\ndescription: Trusted test skill.\n---\nUse this built-in instruction.\n";

    /// Fixtures that exercise missing-scope, built-in-exemption and
    /// search boundaries must stay outside the system temporary
    /// directories: the baseline temporary authorization would otherwise
    /// mask the boundaries these tests verify.
    fn non_temporary_scratch() -> tempfile::TempDir {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new()
            .prefix("notemp-files-app-")
            .tempdir_in(&base)
            .unwrap()
    }

    fn builtin_context() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        AppToolExecutionContext,
        PathBuf,
    ) {
        let workspace = non_temporary_scratch();
        let resources = non_temporary_scratch();
        let cwd = workspace.path().canonicalize().unwrap();
        let root = resources.path().canonicalize().unwrap();
        let skill = root.join("skills/builtin-fixture/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(&skill, BUILTIN_SKILL).unwrap();
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        policy.filesystem.readable_roots = vec![cwd.clone()];
        policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
            root: cwd.clone(),
            read_only_subpaths: vec![],
        }];
        policy.builtin_resources = vec![crate::builtin_resources::BuiltinResource {
            path: skill.clone(),
            content: BUILTIN_SKILL,
        }];
        let context = AppToolExecutionContext {
            restart_native: false,
            access_broker: None,
            execution_cwd: cwd,
            sandbox_policy: policy,
            tool_output_max_tokens: 1000,
            turn_epoch: 1,
            actor_call: None,
        };
        (workspace, resources, context, skill)
    }

    #[tokio::test]
    async fn builtin_read_preflight_needs_no_approval_and_execution_rechecks_bytes() {
        let (_workspace, _resources, context, skill) = builtin_context();
        let mut manager = crate::app::AppManager::new(vec![Box::new(FilesApp::new(
            context.execution_cwd.join("images"),
            false,
        ))])
        .unwrap();
        let call = AgentToolCall {
            id: "read-builtin".into(),
            name: "files__read_file".into(),
            arguments: json!({"path": skill, "start_line": null, "line_count": null}),
        };
        manager.validate_tool_call(&call, &context).unwrap();
        manager.validate_tool_call(&call, &context).unwrap();
        let output = manager.execute_tool(&call, &context).await.unwrap();
        assert_eq!(
            output.payload["content"].as_str().unwrap().as_bytes(),
            BUILTIN_SKILL
        );
        assert!(!context.sandbox_policy.is_path_readable(&skill));

        let list = AgentToolCall {
            id: "list-builtins".into(),
            name: "files__list_skills".into(),
            arguments: json!({}),
        };
        let catalog = manager.execute_tool(&list, &context).await.unwrap();
        assert!(
            catalog.payload["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| {
                    item["name"] == "builtin-fixture"
                        && item["path"].as_str() == Some(skill.to_string_lossy().as_ref())
                })
        );

        // Approval/preflight success must not authenticate later replacement bytes.
        let mut modified = BUILTIN_SKILL.to_vec();
        modified[0] = b'X';
        std::fs::write(&skill, modified).unwrap();
        assert!(manager.execute_tool(&call, &context).await.is_err());
        let catalog = manager.execute_tool(&list, &context).await.unwrap();
        assert!(
            !catalog.payload["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| { item["name"] == "builtin-fixture" })
        );
    }

    #[tokio::test]
    async fn builtin_copy_preflight_exempts_only_source_and_requests_mutation_scope() {
        let (_workspace, _resources, context, skill) = builtin_context();
        let mut app = FilesApp::new(context.execution_cwd.join("images"), false);
        let destination = context.execution_cwd.join("copied.md");
        let copy = AgentToolCall {
            id: "copy-builtin".into(),
            name: "copy_file".into(),
            arguments: json!({"source": skill, "destination": destination, "expected_sha256": null}),
        };
        app.execute_tool(&copy, &context).await.unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), BUILTIN_SKILL);
        assert!(!context.sandbox_policy.is_path_writable(&skill));

        for (name, arguments) in [
            (
                "write_file",
                json!({"path": skill, "content": "changed", "expected_sha256": "unused"}),
            ),
            (
                "edit_file",
                json!({"path": skill, "old_text": "built-in", "new_text": "changed", "expected_sha256": "unused"}),
            ),
            (
                "copy_file",
                json!({"source": destination, "destination": skill, "expected_sha256": "unused"}),
            ),
        ] {
            let call = AgentToolCall {
                id: "deny-builtin-mutation".into(),
                name: name.into(),
                arguments,
            };
            assert!(app.execute_tool(&call, &context).await.is_err());
        }
        assert_eq!(std::fs::read(&skill).unwrap(), BUILTIN_SKILL);
    }

    #[tokio::test]
    async fn builtin_traversal_and_extra_files_do_not_pass_files_boundary() {
        let (_workspace, _resources, context, skill) = builtin_context();
        let mut app = FilesApp::new(context.execution_cwd.join("images"), false);
        // Preserve the actual traversal instead of normalizing a Windows
        // verbatim path while constructing the fixture with Path::join.
        let traversal = format!(
            "{}{separator}child{separator}..{separator}SKILL.md",
            skill.parent().unwrap().display(),
            separator = std::path::MAIN_SEPARATOR,
        );
        std::fs::create_dir(skill.parent().unwrap().join("child")).unwrap();
        let call = AgentToolCall {
            id: "traversal".into(),
            name: "read_file".into(),
            arguments: json!({"path": traversal, "start_line": null, "line_count": null}),
        };

        assert!(app.execute_tool(&call, &context).await.is_err());

        let mut relative_context = context.clone();
        relative_context.execution_cwd = skill.parent().unwrap().to_path_buf();
        let relative_call = AgentToolCall {
            id: "relative-traversal".into(),
            name: "read_file".into(),
            arguments: json!({"path": "child/../SKILL.md", "start_line": null, "line_count": null}),
        };

        assert!(
            app.execute_tool(&relative_call, &relative_context)
                .await
                .is_err()
        );

        let extra = skill.with_file_name("extra.md");
        std::fs::write(&extra, BUILTIN_SKILL).unwrap();
        let call = AgentToolCall {
            id: "extra".into(),
            name: "read_file".into(),
            arguments: json!({"path": extra, "start_line": null, "line_count": null}),
        };
        assert!(app.execute_tool(&call, &context).await.is_err());
    }

    #[tokio::test]
    async fn execution_enforces_current_sandbox_after_schema_validation() {
        let dir = non_temporary_scratch();
        let outside = non_temporary_scratch();
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
            restart_native: false,
            access_broker: None,
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
        manager.validate_tool_call(&call, &base).unwrap();
        assert!(manager.execute_tool(&call, &base).await.is_err());
        assert!(!target.exists());
        let mut effective = base.clone();
        effective
            .sandbox_policy
            .filesystem
            .readable_roots
            .push(outside.path().to_owned());
        effective
            .sandbox_policy
            .filesystem
            .writable_roots
            .push(crate::sandbox::WritableRoot {
                root: outside.path().to_owned(),
                read_only_subpaths: vec![],
            });
        manager.validate_tool_call(&call, &effective).unwrap();
        manager.execute_tool(&call, &effective).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "approved");
        assert!(manager.execute_tool(&call, &base).await.is_err());
    }

    #[tokio::test]
    async fn denied_write_does_not_change_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.txt"), "alpha\n").unwrap();
        let mut policy = RuntimeSandboxPolicy::disabled();
        policy.filesystem.full_disk_write = false;
        policy.filesystem.writable_roots.clear();
        let context = AppToolExecutionContext {
            restart_native: false,
            access_broker: None,
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
