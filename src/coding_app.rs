use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    app::{
        App, AppHowToUse, AppId, AppStateRender, AppToolExecutionContext, AppToolExecutionResult,
        AppToolScope, AppToolSpec, AppUsage,
    },
    context_budget::truncate_text_to_token_budget,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    runtime::scope_client::ScopeClient,
    tool_ui::{ToolCallUiEvent, ToolUiEvent, compact_body_lines},
};

const CODING_USAGE_PURPOSE: &str = "Coding is the app to use for repository-level project editing, backed by scope-engine semantic code operations.";
const CODING_WHEN_TO_FOCUS: &[&str] = &[
    "When performing repository-level project edits rather than isolated file or shell operations.",
    "When source code should be read by selector rather than raw file slices.",
    "When code search results should include containing symbol selectors.",
    "When hunk-only semantic edits, deletions, or propagation review are useful.",
];
const CODING_HOW_TO_USE: &str = r#"Coding app is used to modify projects; think of it as a Coding Studio for the Agent.

First, if the project you need to edit is not open yet, use `coding_open_project` to open it.

When editing source code, always prefer Coding app tools such as `coding_edit_code`, `coding_read_code`, `grep`, and `glob` instead of substituting terminal commands. Important: except for configuration, generated assets, or other non-source areas outside SCOPE engine responsibility, or cases where these tools genuinely cannot complete the task, do not use other tools or shell commands to edit source code.

After each edit, the tool automatically evaluates the impact of your changes and accumulates pending review events. You can also see the current number of pending review events in Coding app state. You do not need to handle them immediately. However, after you finish a series of edits (usually when a plan step is complete, or when you judge that too many review events have accumulated), call `coding_next_review` to acknowledge and claim review events, then follow their instructions to inspect the impact of your changes. This must always be done before reporting back to the user.

SCOPE engine configuration hints are returned by `coding_open_project` and retained in Coding app state, including available tree-sitter languages plus visible per-language `lsp_setup_hint` lines for LSP language/server setup guidance.

Coding app keeps app-level usage rules here. Selector grammar, selector operation support, grep bridge expectations, and structured selector result fields are owned by SCOPE and appended below from SCOPE's compiled usage interface."#;
const CODING_TOOL_SCOPES: &[AppToolScope] = &[AppToolScope::Coding, AppToolScope::Terminal];
const MAX_RENDERED_LSP_SETUP_HINTS: usize = 5;
const PROJECT_INSTRUCTION_FILENAMES: &[&str] =
    &["AGENTS.override.md", "AGENTS.md", "CLAUDE.md", "GEMINI.md"];

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingOpenProjectArgs {
    pub project_root: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingReadCodeArgs {
    pub selector: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingGrepArgs {
    pub pattern: String,
    pub path: Option<String>,
    pub include: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingGlobArgs {
    pub pattern: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingEditCodeArgs {
    pub selector: String,
    pub patch: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingDeleteCodeArgs {
    pub selector: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingNextReviewArgs {}

#[derive(Debug, Clone)]
struct ProjectInstructionDocument {
    path: PathBuf,
    scope_dir: PathBuf,
    name: String,
    content: String,
    sha256: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct DeliveredProjectInstructionKey {
    turn_epoch: u64,
    scope_dir: PathBuf,
    sha256: String,
}

#[derive(Debug, Clone)]
struct CodingConfigHintSummary {
    tree_sitter_languages: usize,
    lsp_languages: usize,
    lsp_setup_hints: Vec<CodingLspSetupHint>,
}

#[derive(Debug, Clone)]
struct CodingLspSetupHint {
    language: String,
    lsp_server: String,
    lsp_binary: String,
    lsp_available: bool,
    setup_hints: String,
    install_command: Option<String>,
    download_url: Option<String>,
}

impl CodingConfigHintSummary {
    fn from_hints(hints: &Value) -> Self {
        let lsp_entries = hints
            .get("lsp_languages")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();

        Self {
            tree_sitter_languages: hints
                .get("tree_sitter_languages")
                .and_then(|value| value.as_array())
                .map(|items| items.len())
                .unwrap_or(0),
            lsp_languages: lsp_entries.len(),
            lsp_setup_hints: lsp_entries
                .iter()
                .map(CodingLspSetupHint::from_config_hint)
                .collect(),
        }
    }

    fn count_state_line(&self) -> String {
        format!(
            "scope_config_hints=tree_sitter_languages:{} lsp_languages:{}",
            self.tree_sitter_languages, self.lsp_languages
        )
    }

    fn state_lines(&self) -> Vec<String> {
        let mut lines = vec![self.count_state_line()];
        lines.extend(
            self.lsp_setup_hints
                .iter()
                .take(MAX_RENDERED_LSP_SETUP_HINTS)
                .map(CodingLspSetupHint::state_line),
        );
        if self.lsp_setup_hints.len() > MAX_RENDERED_LSP_SETUP_HINTS {
            lines.push(format!(
                "lsp_setup_hint_more={}",
                self.lsp_setup_hints.len() - MAX_RENDERED_LSP_SETUP_HINTS
            ));
        }
        lines
    }

    fn model_content(&self) -> String {
        let mut lines = vec![self.count_state_line()];
        lines.extend(
            self.lsp_setup_hints
                .iter()
                .map(CodingLspSetupHint::model_content),
        );
        lines.join("\n")
    }
}

impl CodingLspSetupHint {
    fn from_config_hint(entry: &Value) -> Self {
        Self {
            language: json_string(entry, "language"),
            lsp_server: json_string(entry, "lsp_server"),
            lsp_binary: json_string(entry, "lsp_binary"),
            lsp_available: entry
                .get("lsp_available")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            setup_hints: json_string(entry, "setup_hints"),
            install_command: format_install_command(entry.get("install_command")),
            download_url: entry
                .get("download_url")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
        }
    }

    fn state_line(&self) -> String {
        format!(
            "lsp_setup_hint language={} server={} binary={} available={} setup_hints={}",
            self.language,
            self.lsp_server,
            self.lsp_binary,
            self.lsp_available,
            summarize_coding_inline_text(&self.setup_hints)
        )
    }

    fn model_content(&self) -> String {
        let mut line = format!(
            "lsp_setup_hint language={} server={} binary={} available={}\n  setup_hints: {}",
            self.language, self.lsp_server, self.lsp_binary, self.lsp_available, self.setup_hints
        );
        if let Some(install_command) = self.install_command.as_ref() {
            line.push_str(&format!("\n  install_command: {install_command}"));
        }
        if let Some(download_url) = self.download_url.as_ref() {
            line.push_str(&format!("\n  download_url: {download_url}"));
        }
        line
    }
}

fn json_string(entry: &Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string()
}

fn load_instruction_documents_in_dir(dir: &Path) -> Result<Vec<ProjectInstructionDocument>> {
    let mut documents = Vec::new();
    for name in PROJECT_INSTRUCTION_FILENAMES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let content = fs::read_to_string(&path).map_err(|err| {
            miette!(
                "failed to read project instruction file {}: {err}",
                path.display()
            )
        })?;
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let sha256 = format!("{:x}", hasher.finalize());
        documents.push(ProjectInstructionDocument {
            path,
            scope_dir: dir.to_path_buf(),
            name: (*name).to_string(),
            content,
            sha256,
        });
    }
    Ok(documents)
}

fn instruction_payload(instruction: &ProjectInstructionDocument) -> Value {
    json!({
        "path": instruction.path,
        "scope_dir": instruction.scope_dir,
        "name": instruction.name,
        "sha256": instruction.sha256,
        "content": instruction.content,
    })
}

fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn render_project_instructions(label: &str, instructions: &[ProjectInstructionDocument]) -> String {
    let mut rendered = Vec::new();
    rendered.push(format!("<{label}>"));
    for instruction in instructions {
        rendered.push(format!(
            "<instruction file=\"{}\" scope_dir=\"{}\" sha256=\"{}\">",
            instruction.path.display(),
            instruction.scope_dir.display(),
            instruction.sha256
        ));
        rendered.push(instruction.content.clone());
        rendered.push("</instruction>".to_string());
    }
    rendered.push(format!("</{label}>"));
    rendered.join("\n")
}

fn selector_path(selector: &str) -> &str {
    let symbol_path = selector.split_once("::").map(|(path, _)| path);
    let hash_path = selector.split_once('#').map(|(path, _)| path);
    symbol_path.or(hash_path).unwrap_or(selector)
}

fn format_install_command(value: Option<&Value>) -> Option<String> {
    let command = value?.get("command")?.as_str()?;
    let args = value
        .and_then(|entry| entry.get("args"))
        .and_then(|args| args.as_array())
        .map(|args| {
            args.iter()
                .filter_map(|arg| arg.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    Some(if args.is_empty() {
        command.to_string()
    } else {
        format!("{command} {args}")
    })
}

pub struct CodingApp {
    scope: ScopeClient,
    project_root: Option<PathBuf>,
    language: Option<String>,
    config_hint_summary: Option<CodingConfigHintSummary>,
    root_instructions: Vec<ProjectInstructionDocument>,
    delivered_scoped_instructions: HashSet<DeliveredProjectInstructionKey>,
    last_action: Option<String>,
}

impl CodingApp {
    pub fn new() -> Self {
        Self {
            scope: ScopeClient::new(),
            project_root: None,
            language: None,
            config_hint_summary: None,
            root_instructions: Vec::new(),
            delivered_scoped_instructions: HashSet::new(),
            last_action: None,
        }
    }

    fn project_root_display(&self) -> String {
        self.project_root
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "none".to_string())
    }

    fn open_project(
        &mut self,
        args: CodingOpenProjectArgs,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        let requested = Path::new(&args.project_root);
        let project_root = context.resolve_tool_path(requested, None);
        context
            .sandbox_policy
            .ensure_path_readable(&project_root, "coding project root")?;
        if !project_root.is_dir() {
            return Err(miette!(
                "coding project root is not a directory: {}",
                project_root.display()
            ));
        }

        let response = self
            .scope
            .open_project(project_root.clone(), args.language.as_deref());
        if let Some(error) = response.error {
            return Err(miette!(
                "scope-engine open_project failed: {}",
                error.message
            ));
        }

        let config_hints_response = ScopeClient::get_config_hints();
        if let Some(error) = config_hints_response.error {
            return Err(miette!(
                "scope-engine get_config_hints failed: {}",
                error.message
            ));
        }
        let config_hints = config_hints_response
            .result
            .unwrap_or(serde_json::Value::Null);
        let config_hint_summary = CodingConfigHintSummary::from_hints(&config_hints);

        let root_instructions = load_instruction_documents_in_dir(&project_root)?;

        self.project_root = Some(project_root.clone());
        self.language = args.language.clone();
        self.config_hint_summary = Some(config_hint_summary.clone());
        self.root_instructions = root_instructions.clone();
        self.delivered_scoped_instructions.clear();
        self.last_action = Some("opened project".to_string());

        let mut model_parts = vec![config_hint_summary.model_content()];
        if !root_instructions.is_empty() {
            model_parts.push(render_project_instructions(
                "root_project_instructions",
                &root_instructions,
            ));
        }
        let model_content = truncate_text_to_token_budget(
            &model_parts.join("\n\n"),
            context.tool_output_max_tokens,
        );
        let mut ui_lines = vec![format!("project_root={}", project_root.display())];
        ui_lines.extend(config_hint_summary.state_lines());
        ui_lines.extend(root_instructions.iter().map(|instruction| {
            format!(
                "root_instruction file={} sha256={}",
                instruction.path.display(),
                short_hash(&instruction.sha256)
            )
        }));

        Ok(AppToolExecutionResult {
            summary: format!("opened coding project {}", project_root.display()),
            payload: json!({
                "project_root": project_root,
                "language": args.language,
                "scope_response": response.result,
                "config_hints": config_hints,
                "root_project_instructions": root_instructions.iter().map(instruction_payload).collect::<Vec<_>>(),
            }),
            model_content: Some(model_content),
            ui_event: ToolUiEvent::app("coding_open_project", ui_lines),
            turn_boundary_reason: None,
        })
    }

    fn scoped_instructions_for_path_once_per_turn(
        &mut self,
        relative_or_absolute_path: &str,
        turn_epoch: u64,
    ) -> Result<Vec<ProjectInstructionDocument>> {
        let Some(project_root) = self.project_root.clone() else {
            return Ok(Vec::new());
        };
        let candidate = Path::new(relative_or_absolute_path);
        if candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Ok(Vec::new());
        }
        let full_path = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            project_root.join(candidate)
        };
        let mut current_dir = if full_path.is_dir() {
            full_path.as_path()
        } else {
            full_path.parent().unwrap_or(project_root.as_path())
        };
        let mut scope_dirs = Vec::new();
        loop {
            if current_dir == project_root {
                break;
            }
            if !current_dir.starts_with(&project_root) {
                break;
            }
            scope_dirs.push(current_dir.to_path_buf());
            let Some(parent) = current_dir.parent() else {
                break;
            };
            current_dir = parent;
        }
        scope_dirs.reverse();
        let mut scoped = Vec::new();
        for scope_dir in scope_dirs {
            scoped.extend(load_instruction_documents_in_dir(&scope_dir)?);
        }
        let mut newly_delivered = Vec::new();
        for instruction in scoped {
            let key = DeliveredProjectInstructionKey {
                turn_epoch,
                scope_dir: instruction.scope_dir.clone(),
                sha256: instruction.sha256.clone(),
            };
            if self.delivered_scoped_instructions.insert(key) {
                newly_delivered.push(instruction);
            }
        }
        Ok(newly_delivered)
    }

    fn append_scoped_instructions_to_result(
        &mut self,
        result: &mut AppToolExecutionResult,
        path: &str,
        context: &AppToolExecutionContext,
    ) -> Result<()> {
        let scoped_instructions =
            self.scoped_instructions_for_path_once_per_turn(path, context.turn_epoch)?;
        if scoped_instructions.is_empty() {
            return Ok(());
        }
        let rendered =
            render_project_instructions("scoped_project_instructions", &scoped_instructions);
        let content = match result.model_content.take() {
            Some(existing) if !existing.trim().is_empty() => format!("{rendered}\n\n{existing}"),
            _ => rendered,
        };
        result.model_content = Some(truncate_text_to_token_budget(
            &content,
            context.tool_output_max_tokens,
        ));
        if let Some(payload_object) = result.payload.as_object_mut() {
            payload_object.insert(
                "scoped_project_instructions".to_string(),
                Value::Array(
                    scoped_instructions
                        .iter()
                        .map(instruction_payload)
                        .collect(),
                ),
            );
        }
        if let ToolUiEvent::App(ui_data) = &mut result.ui_event {
            ui_data
                .body_lines
                .extend(scoped_instructions.iter().map(|instruction| {
                    format!(
                        "scoped_instruction file={} sha256={}",
                        instruction.path.display(),
                        short_hash(&instruction.sha256)
                    )
                }));
        }
        Ok(())
    }

    fn require_project(&self) -> Result<()> {
        if self.project_root.is_none() {
            return Err(miette!(
                "no coding project opened; call coding_open_project first"
            ));
        }
        Ok(())
    }
}

impl Default for CodingApp {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl App for CodingApp {
    fn id(&self) -> AppId {
        AppId::coding()
    }

    fn render_state(&self) -> AppStateRender {
        let mut lines = vec![
            "kind=coding".to_string(),
            format!("project_root={}", self.project_root_display()),
            format!(
                "pending_review_events={}",
                self.scope.pending_review_count()
            ),
        ];
        if let Some(language) = self.language.as_ref() {
            lines.push(format!("language={language}"));
        }
        if let Some(summary) = self.config_hint_summary.as_ref() {
            lines.extend(summary.state_lines());
        }
        if !self.root_instructions.is_empty() {
            lines.extend(
                render_project_instructions("root_project_instructions", &self.root_instructions)
                    .lines()
                    .map(ToString::to_string),
            );
        }
        if let Some(last_action) = self.last_action.as_ref() {
            lines.push(format!("last_action={last_action}"));
        }
        AppStateRender {
            title: "Coding".to_string(),
            lines,
        }
    }

    fn usage(&self) -> AppUsage {
        AppUsage {
            description: CODING_USAGE_PURPOSE.to_string(),
            when_to_focus: CODING_WHEN_TO_FOCUS
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            body_markdown: None,
        }
    }

    fn how_to_use(&self) -> AppHowToUse {
        let scope_usage = ScopeClient::usage();
        AppHowToUse {
            lines: Vec::new(),
            body_markdown: Some(format!(
                "{}\n\n---\n\n{}",
                CODING_HOW_TO_USE, scope_usage.usage_markdown
            )),
        }
    }

    fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        CODING_TOOL_SCOPES
    }

    fn tool_specs(&self) -> Result<Vec<AppToolSpec>> {
        Ok(vec![
            AppToolSpec {
                name: "coding_open_project".to_string(),
                description: "Open a project for semantic code operations using scope-engine.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingOpenProjectArgs)).unwrap(),
            },
            AppToolSpec {
                name: "coding_read_code".to_string(),
                description: "Read selector-resolved code content and language metadata.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingReadCodeArgs)).unwrap(),
            },
            AppToolSpec {
                name: "grep".to_string(),
                description: "Search file contents using a regex pattern.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingGrepArgs)).unwrap(),
            },
            AppToolSpec {
                name: "glob".to_string(),
                description: "Find files by glob pattern.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingGlobArgs)).unwrap(),
            },
            AppToolSpec {
                name: "coding_edit_code".to_string(),
                description: "Apply a stripped v4a hunk-only patch to selector-resolved code and return propagation results.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingEditCodeArgs)).unwrap(),
            },
            AppToolSpec {
                name: "coding_delete_code".to_string(),
                description: "Delete selector-resolved code and return propagation results.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingDeleteCodeArgs)).unwrap(),
            },
            AppToolSpec {
                name: "coding_next_review".to_string(),
                description: "Acknowledge and return the next accumulated scope-engine propagation review event, if any.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingNextReviewArgs)).unwrap(),
            },
        ])
    }

    fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        let summary = match call.name.as_str() {
            "coding_open_project" => {
                let args: CodingOpenProjectArgs = parse_coding_tool_args(call)?;
                format!(
                    "project_root={} language={}",
                    summarize_coding_inline_text(&args.project_root),
                    args.language.unwrap_or_else(|| "auto".to_string())
                )
            }
            "coding_read_code" => {
                let args: CodingReadCodeArgs = parse_coding_tool_args(call)?;
                format!("selector={}", summarize_coding_inline_text(&args.selector))
            }
            "grep" => {
                let args: CodingGrepArgs = parse_coding_tool_args(call)?;
                format!("pattern={}", summarize_coding_inline_text(&args.pattern))
            }
            "glob" => {
                let args: CodingGlobArgs = parse_coding_tool_args(call)?;
                format!("pattern={}", summarize_coding_inline_text(&args.pattern))
            }
            "coding_edit_code" => {
                let args: CodingEditCodeArgs = parse_coding_tool_args(call)?;
                format!(
                    "selector={} patch_chars={}",
                    summarize_coding_inline_text(&args.selector),
                    args.patch.len()
                )
            }
            "coding_delete_code" => {
                let args: CodingDeleteCodeArgs = parse_coding_tool_args(call)?;
                format!("selector={}", summarize_coding_inline_text(&args.selector))
            }
            "coding_next_review" => "next propagation review".to_string(),
            _ => return Err(miette!("unknown coding tool `{}`", call.name)),
        };
        Ok(EpisodeActionRecord {
            kind: call.name.clone(),
            summary,
        })
    }

    fn render_tool_call_ui(&self, call: &AgentToolCall) -> Result<ToolCallUiEvent> {
        Ok(ToolCallUiEvent::app(
            call.name.clone(),
            compact_coding_argument_lines(&call.arguments),
        ))
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        match call.name.as_str() {
            "coding_open_project" => {
                let args: CodingOpenProjectArgs = parse_coding_tool_args(call)?;
                self.open_project(args, context)
            }
            "coding_read_code" => {
                self.require_project()?;
                let args: CodingReadCodeArgs = parse_coding_tool_args(call)?;
                let result = self.scope.read_code(&args.selector)?;
                self.last_action = Some(format!("read {}", args.selector));
                let selector_info = serde_json::to_string_pretty(&result.selector_info)
                    .unwrap_or_else(|_| "{}".to_string());
                let model_content = format!(
                    "selector={}\nselector_info={}\nlanguage={}\ncontent=\n{}",
                    result.selector,
                    selector_info,
                    result.language,
                    truncate_text_to_token_budget(&result.content, context.tool_output_max_tokens)
                );
                let mut output = AppToolExecutionResult {
                    summary: format!("read code {}", result.selector),
                    payload: serde_json::to_value(&result).unwrap(),
                    model_content: Some(model_content),
                    ui_event: ToolUiEvent::app(
                        "coding_read_code",
                        vec![
                            format!("selector={}", result.selector),
                            format!("language={}", result.language),
                        ],
                    ),
                    turn_boundary_reason: None,
                };
                self.append_scoped_instructions_to_result(
                    &mut output,
                    selector_path(&args.selector),
                    context,
                )?;
                Ok(output)
            }
            "grep" => {
                self.require_project()?;
                let args: CodingGrepArgs = parse_coding_tool_args(call)?;
                let result = self.scope.grep_code(
                    &args.pattern,
                    args.path.as_deref(),
                    args.include.as_deref(),
                )?;
                self.last_action = Some(format!("searched {}", args.pattern));
                let mut output = AppToolExecutionResult {
                    summary: format!("found {} matches", result.matches.len()),
                    payload: serde_json::to_value(&result).unwrap(),
                    model_content: Some(truncate_text_to_token_budget(
                        &result.output,
                        context.tool_output_max_tokens,
                    )),
                    ui_event: ToolUiEvent::app(
                        "grep",
                        vec![format!("matches={}", result.matches.len())],
                    ),
                    turn_boundary_reason: None,
                };
                if let Some(path) = args.path.as_deref() {
                    self.append_scoped_instructions_to_result(&mut output, path, context)?;
                }
                Ok(output)
            }
            "glob" => {
                self.require_project()?;
                let args: CodingGlobArgs = parse_coding_tool_args(call)?;
                let result = self.scope.glob_files(&args.pattern, args.path.as_deref())?;
                self.last_action = Some(format!("globbed {}", args.pattern));
                let mut output = AppToolExecutionResult {
                    summary: format!("found {} files", result.files.len()),
                    payload: serde_json::to_value(&result).unwrap(),
                    model_content: Some(truncate_text_to_token_budget(
                        &result.output,
                        context.tool_output_max_tokens,
                    )),
                    ui_event: ToolUiEvent::app(
                        "glob",
                        vec![format!("files={}", result.files.len())],
                    ),
                    turn_boundary_reason: None,
                };
                if let Some(path) = args.path.as_deref() {
                    self.append_scoped_instructions_to_result(&mut output, path, context)?;
                }
                Ok(output)
            }
            "coding_edit_code" => {
                self.require_project()?;
                let args: CodingEditCodeArgs = parse_coding_tool_args(call)?;
                let results = self.scope.edit_code(&args.selector, &args.patch)?;
                self.last_action = Some(format!("edited {}", args.selector));
                let summary = format!(
                    "edited code {}; propagation_results={}",
                    args.selector,
                    results.len()
                );
                let mut output = AppToolExecutionResult {
                    summary: summary.clone(),
                    payload: json!({ "propagation_results": results }),
                    model_content: None,
                    ui_event: ToolUiEvent::app(
                        "coding_edit_code",
                        vec![
                            format!("selector={}", args.selector),
                            format!("propagation_results={}", results.len()),
                        ],
                    ),
                    turn_boundary_reason: None,
                };
                self.append_scoped_instructions_to_result(
                    &mut output,
                    selector_path(&args.selector),
                    context,
                )?;
                Ok(output)
            }
            "coding_delete_code" => {
                self.require_project()?;
                let args: CodingDeleteCodeArgs = parse_coding_tool_args(call)?;
                let results = self.scope.delete_code(&args.selector)?;
                self.last_action = Some(format!("deleted {}", args.selector));
                let mut output = AppToolExecutionResult {
                    summary: format!(
                        "deleted code {}; propagation_results={}",
                        args.selector,
                        results.len()
                    ),
                    payload: json!({ "propagation_results": results }),
                    model_content: None,
                    ui_event: ToolUiEvent::app(
                        "coding_delete_code",
                        vec![
                            format!("selector={}", args.selector),
                            format!("propagation_results={}", results.len()),
                        ],
                    ),
                    turn_boundary_reason: None,
                };
                self.append_scoped_instructions_to_result(
                    &mut output,
                    selector_path(&args.selector),
                    context,
                )?;
                Ok(output)
            }
            "coding_next_review" => {
                self.require_project()?;
                let _args: CodingNextReviewArgs = parse_coding_tool_args(call)?;
                let review = self.scope.ack_next_event();
                self.last_action = Some("acknowledged next review".to_string());
                Ok(AppToolExecutionResult {
                    summary: if review.is_some() {
                        "acknowledged coding review event".to_string()
                    } else {
                        "no coding review event pending".to_string()
                    },
                    payload: json!({ "review": review }),
                    model_content: None,
                    ui_event: ToolUiEvent::app(
                        "coding_next_review",
                        vec![format!(
                            "review={}",
                            if review.is_some() { "present" } else { "none" }
                        )],
                    ),
                    turn_boundary_reason: None,
                })
            }
            _ => Err(miette!("unknown coding tool `{}`", call.name)),
        }
    }
}

fn parse_coding_tool_args<T: for<'de> Deserialize<'de>>(call: &AgentToolCall) -> Result<T> {
    serde_json::from_value(call.arguments.clone()).map_err(|err| {
        miette!(
            "invalid arguments for coding tool `{}`: {}; arguments={}",
            call.name,
            err,
            call.arguments
        )
    })
}

fn summarize_coding_inline_text(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let compact = text.replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

fn compact_coding_argument_lines(arguments: &Value) -> Vec<String> {
    let text = match arguments {
        Value::Object(map) if map.is_empty() => return Vec::new(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                format!("{key}={}", summarize_coding_inline_text(&value.to_string()))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => summarize_coding_inline_text(&other.to_string()),
    };
    compact_body_lines(&text, 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_hint_summary_keeps_lsp_setup_hints_visible() {
        let hints = json!({
            "tree_sitter_languages": [
                {"name": "rust", "extensions": ["rs"]}
            ],
            "lsp_languages": [
                {
                    "language": "rust",
                    "lsp_server": "rust-analyzer",
                    "lsp_binary": "rust-analyzer",
                    "lsp_available": false,
                    "setup_hints": "Install rust-analyzer and ensure it is on PATH.",
                    "install_command": {
                        "command": "rustup",
                        "args": ["component", "add", "rust-analyzer"]
                    },
                    "download_url": "https://rust-analyzer.github.io/"
                }
            ]
        });

        let summary = CodingConfigHintSummary::from_hints(&hints);

        assert_eq!(summary.tree_sitter_languages, 1);
        assert_eq!(summary.lsp_languages, 1);
        assert_eq!(summary.lsp_setup_hints.len(), 1);

        let state = summary.state_lines().join("\n");
        assert!(state.contains("scope_config_hints=tree_sitter_languages:1 lsp_languages:1"));
        assert!(state.contains("lsp_setup_hint language=rust"));
        assert!(state.contains("server=rust-analyzer"));
        assert!(state.contains("setup_hints=Install rust-analyzer and ensure it is on PATH."));

        let model_content = summary.model_content();
        assert!(
            model_content.contains("setup_hints: Install rust-analyzer and ensure it is on PATH.")
        );
        assert!(model_content.contains("install_command: rustup component add rust-analyzer"));
        assert!(model_content.contains("download_url: https://rust-analyzer.github.io/"));
    }

    #[test]
    fn config_hint_summary_caps_rendered_state_hints() {
        let lsp_languages = (0..=MAX_RENDERED_LSP_SETUP_HINTS)
            .map(|idx| {
                json!({
                    "language": format!("lang{idx}"),
                    "lsp_server": format!("server{idx}"),
                    "lsp_binary": format!("binary{idx}"),
                    "lsp_available": true,
                    "setup_hints": format!("hint {idx}"),
                })
            })
            .collect::<Vec<_>>();
        let hints = json!({
            "tree_sitter_languages": [],
            "lsp_languages": lsp_languages,
        });

        let summary = CodingConfigHintSummary::from_hints(&hints);
        let state_lines = summary.state_lines();

        assert_eq!(summary.lsp_languages, MAX_RENDERED_LSP_SETUP_HINTS + 1);
        assert_eq!(
            state_lines
                .iter()
                .filter(|line| line.starts_with("lsp_setup_hint language="))
                .count(),
            MAX_RENDERED_LSP_SETUP_HINTS
        );
        assert_eq!(
            state_lines.last().map(String::as_str),
            Some("lsp_setup_hint_more=1")
        );
        assert!(
            summary
                .model_content()
                .contains("lsp_setup_hint language=lang5")
        );
    }

    #[test]
    fn loads_project_instruction_documents_with_hash() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("AGENTS.md"), "Root rule\n").expect("write agents");

        let instructions =
            load_instruction_documents_in_dir(temp.path()).expect("load instructions");

        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].name, "AGENTS.md");
        assert_eq!(instructions[0].content, "Root rule\n");
        assert_eq!(instructions[0].scope_dir, temp.path());
        assert_eq!(instructions[0].sha256.len(), 64);
        assert!(
            render_project_instructions("root_project_instructions", &instructions)
                .contains("Root rule")
        );
    }

    #[test]
    fn loads_large_project_instruction_documents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let content = "A".repeat(40 * 1024);
        std::fs::write(temp.path().join("AGENTS.md"), &content).expect("write agents");

        let instructions =
            load_instruction_documents_in_dir(temp.path()).expect("load large instructions");

        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].name, "AGENTS.md");
        assert_eq!(instructions[0].content, content);
    }

    #[test]
    fn scoped_instructions_are_returned_once_per_turn_and_again_when_changed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("src/nested");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(nested.join("AGENTS.md"), "Nested rule v1\n").expect("write agents");
        let mut app = CodingApp::new();
        app.project_root = Some(temp.path().to_path_buf());

        let first = app
            .scoped_instructions_for_path_once_per_turn("src/nested/file.rs", 7)
            .expect("first load");
        let second = app
            .scoped_instructions_for_path_once_per_turn("src/nested/file.rs", 7)
            .expect("second load");

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].content, "Nested rule v1\n");
        assert!(second.is_empty());

        std::fs::write(nested.join("AGENTS.md"), "Nested rule v2\n").expect("update agents");
        let changed = app
            .scoped_instructions_for_path_once_per_turn("src/nested/file.rs", 7)
            .expect("changed load");
        let next_turn = app
            .scoped_instructions_for_path_once_per_turn("src/nested/file.rs", 8)
            .expect("next turn load");

        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].content, "Nested rule v2\n");
        assert_eq!(next_turn.len(), 1);
        assert_eq!(next_turn[0].content, "Nested rule v2\n");
    }
}
