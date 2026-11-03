//! Core-only, device-routed discovery and incremental reading of instruction resources.
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{IntoDiagnostic, Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    actors::ActorSystem,
    app::{App, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    file_io::{self, ListDirectoryArgs, ReadFileArgs, SearchTextArgs},
    model_protocol::AgentToolCall,
    openskills::load_openskills_with_policy,
    schema_utils::model_schema_for,
};

pub struct SuperSkillsApp {
    system: ActorSystem,
}

#[model_schema]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSkillsArgs {
    /// Device from Actors devices; null selects the local device in a local account.
    stage_id: Option<String>,
    /// Absolute directory for project Skill discovery; null uses this device's default workspace.
    workspace: Option<String>,
    /// Literal name/description search; null lists all Skills, including explicit-only Skills.
    query: Option<String>,
    cursor: Option<usize>,
    limit: Option<usize>,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SkillLocation {
    /// Workspace returned by list_skills on the selected device.
    workspace: String,
    /// Exact SKILL.md source path returned by list_skills on that device.
    path: String,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    stage_id: Option<String>,
    skill: SkillLocation,
    /// Path relative to the Skill directory, such as SKILL.md or references/guide.md.
    file: ReadFileArgs,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DirectoryArgs {
    stage_id: Option<String>,
    skill: SkillLocation,
    /// Path relative to the Skill directory; use . for its root.
    directory: ListDirectoryArgs,
}

#[model_schema]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    stage_id: Option<String>,
    skill: SkillLocation,
    /// Search a relative file/directory; use . to search the Skill, with explicit depth and a bounded limit.
    search: SearchTextArgs,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SkillEntry {
    name: String,
    description: String,
    path: String,
    auto_use_enabled: bool,
    scope: crate::openskills::OpenSkillScope,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("x-dl-content" = "skills"))]
pub struct SkillsPage {
    workspace: String,
    skills: Vec<SkillEntry>,
    next_cursor: Option<usize>,
}

impl SuperSkillsApp {
    pub fn new(system: ActorSystem) -> Self {
        Self { system }
    }

    pub(crate) fn list(
        &self,
        args: &ListSkillsArgs,
        context: &AppToolExecutionContext,
    ) -> Result<SkillsPage> {
        self.system.require_local_stage(args.stage_id.as_deref())?;
        if args.query.as_ref().is_some_and(|query| query.len() > 512) {
            return Err(miette!("Skill query is too long"));
        }
        let workspace = self.workspace(args.workspace.as_deref())?;
        let catalog = load_openskills_with_policy(&workspace, &context.sandbox_policy);
        let query = args.query.as_deref().unwrap_or_default().to_lowercase();
        let cursor = args.cursor.unwrap_or(0);
        let limit = args.limit.unwrap_or(30).clamp(1, 100);
        let mut skills = catalog
            .skills()
            .iter()
            .filter(|skill| {
                skill.name.to_lowercase().contains(&query)
                    || skill.description.to_lowercase().contains(&query)
            })
            .skip(cursor)
            .take(limit + 1)
            .map(|skill| SkillEntry {
                name: skill.name.clone(),
                description: skill.description.clone(),
                path: skill.path.to_string_lossy().into_owned(),
                auto_use_enabled: skill.auto_use_enabled(),
                scope: skill.scope,
            })
            .collect::<Vec<_>>();
        let next_cursor = (skills.len() > limit).then(|| cursor.saturating_add(limit));
        skills.truncate(limit);
        Ok(SkillsPage {
            workspace: workspace.to_string_lossy().into_owned(),
            skills,
            next_cursor,
        })
    }

    fn workspace(&self, value: Option<&str>) -> Result<PathBuf> {
        match value {
            Some(value) if Path::new(value).is_absolute() => {
                file_io::resolve_file_path(self.system.device_workspace(), value)
            }
            Some(_) => Err(miette!("Skill workspace must be an absolute device path")),
            None => file_io::resolve_file_path(self.system.device_workspace(), "."),
        }
    }

    fn root(&self, location: &SkillLocation, context: &AppToolExecutionContext) -> Result<PathBuf> {
        let workspace = self.workspace(Some(&location.workspace))?;
        if !Path::new(&location.path).is_absolute() {
            return Err(miette!("Use the exact Skill source path from list_skills"));
        }
        let catalog = load_openskills_with_policy(&workspace, &context.sandbox_policy);
        let skill = catalog
            .skills()
            .iter()
            .find(|skill| skill.path == Path::new(&location.path))
            .ok_or_else(|| {
                miette!("Selected Skill is unavailable or unauthorized; list Skills again")
            })?;
        skill
            .path
            .parent()
            .map(Path::to_owned)
            .ok_or_else(|| miette!("Skill has no resource directory"))
    }

    fn relative_path(root: &Path, value: &str) -> Result<String> {
        let value = value.replace('\\', "/");
        // Reject traversal and platform-specific prefixes before canonicalization.
        // The file APIs also check the opened handle against this resource root.
        if value.is_empty()
            || value.contains(':')
            || Path::new(&value)
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(miette!(
                "Use a relative path within the selected Skill directory"
            ));
        }
        let path = file_io::resolve_file_path(root, &value)?;
        if !path.starts_with(root) {
            return Err(miette!("Skill resource path escapes its directory"));
        }
        Ok(path.to_string_lossy().into_owned())
    }

    fn relative_result(root: &Path, value: &str) -> Result<String> {
        Path::new(value)
            .strip_prefix(root)
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .map_err(|_| miette!("Skill resource result is outside its directory"))
    }
}

#[async_trait]
impl App for SuperSkillsApp {
    fn id(&self) -> AppId {
        AppId::new("super_skills").expect("static App ID")
    }

    fn can_passivate(&self) -> bool {
        true
    }

    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![
            AppToolSpec { name: "list_skills".into(), description: "Search authorized Skill metadata on a device selected with Actors devices. Includes coordination and explicit-only Skills; returns source paths and next_cursor, without bodies.".into(), input_schema: model_schema_for::<ListSkillsArgs>() },
            AppToolSpec { name: "read_file".into(), description: "Read a selected Skill document by relative path, starting with SKILL.md. Follow references as needed. start_line is 1-based; null uses line 1 and an 80-line page (maximum 2000).".into(), input_schema: model_schema_for::<ReadArgs>() },
            AppToolSpec { name: "list_directory".into(), description: "List files by Skill-relative directory path. Use . for the root; continue with next_cursor. Returned paths are Skill-relative.".into(), input_schema: model_schema_for::<DirectoryArgs>() },
            AppToolSpec { name: "search_text".into(), description: "Find literal text in Skill documents. Use Skill-relative paths (. for the root), depth 1-32 and a bounded limit. Returns relative paths and line numbers.".into(), input_schema: model_schema_for::<SearchArgs>() },
        ]
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        let caller = context
            .actor_call
            .as_ref()
            .ok_or_else(|| miette!("Core input is required"))?;
        self.system
            .record_for_tool(crate::stage::shared::ACTOR_ID, caller)?;
        let mut output = match call.name.as_str() {
            "list_skills" => {
                let args: ListSkillsArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                let page = self.list(&args, context)?;
                AppToolExecutionResult::structured("Skills", &page, None)
            }
            "read_file" => {
                let mut args: ReadArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                self.system.require_local_stage(args.stage_id.as_deref())?;
                let root = self.root(&args.skill, context)?;
                let relative = args.file.path.clone();
                args.file.path = Self::relative_path(&root, &relative)?;
                let mut result = context.files().read_file_within(&root, &args.file).await?;
                result.path = relative;
                AppToolExecutionResult::structured(
                    format!("read {} Skill lines", result.line_count),
                    &result,
                    None,
                )
            }
            "list_directory" => {
                let mut args: DirectoryArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                self.system.require_local_stage(args.stage_id.as_deref())?;
                let root = self.root(&args.skill, context)?;
                args.directory.path = Self::relative_path(&root, &args.directory.path)?;
                let mut result = context
                    .files()
                    .list_directory_within(&root, &args.directory)
                    .await?;
                for entry in &mut result.entries {
                    entry.path = Self::relative_result(&root, &entry.path)?;
                }
                AppToolExecutionResult::structured(
                    format!("listed {} Skill entries", result.entries.len()),
                    &result,
                    None,
                )
            }
            "search_text" => {
                let mut args: SearchArgs =
                    serde_json::from_value(call.arguments.clone()).into_diagnostic()?;
                self.system.require_local_stage(args.stage_id.as_deref())?;
                let root = self.root(&args.skill, context)?;
                args.search.path = Self::relative_path(&root, &args.search.path)?;
                let mut result = context
                    .files()
                    .search_text_within(&root, &args.search)
                    .await?;
                for entry in &mut result.matches {
                    entry.path = Self::relative_result(&root, &entry.path)?;
                }
                AppToolExecutionResult::structured(
                    format!("found {} Skill matches", result.matches.len()),
                    &result,
                    None,
                )
            }
            _ => Err(miette!("Unknown SuperSkills tool")),
        }?;
        output.payload["stage_id"] =
            serde_json::to_value(self.system.store().public_stage_id()?).into_diagnostic()?;
        if let Some(skill) = call.arguments.get("skill") {
            output.payload["skill"] = skill.clone();
        }
        Ok(output)
    }
}
