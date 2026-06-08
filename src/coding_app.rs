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
    apply_patch::{PatchOp, parse_apply_patch},
    context_budget::truncate_text_to_token_budget,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    runtime::scope_client::ScopeClient,
    tool_ui::{
        CodingEditUiData, CodingToolCallUiData, PatchDiffLineKind, PatchDiffLineUiData,
        PatchFileOperation, PatchFileUiData, ToolCallUiEvent, ToolUiEvent, compact_body_lines,
    },
};

const CODING_USAGE_PURPOSE: &str = "Coding is the app to use when a task requires researching, reading, modifying, developing, or otherwise operating on a project. It includes all Terminal capabilities plus additional project-aware tools.";
const CODING_WHEN_TO_FOCUS: &[&str] = &[
    "When a task requires researching, reading, modifying, developing, or otherwise operating on any project.",
    "When project work may need commands, tests, formatting, git, filesystem inspection, source-code navigation, semantic search, or edits; Coding includes all Terminal capabilities plus additional project-aware tools.",
    "Project operations must use Coding app rather than Terminal app. Focus Terminal directly only for non-project command execution or standalone process interaction.",
];
const CODING_HOW_TO_USE: &str = r#"Coding app is used to modify projects; think of it as a Coding Studio for the Agent.

First, if the project you need to edit is not open yet, use the currently exposed Coding open-project tool; app scope mangling exposes it as `coding__open_project`.

When editing source code, always prefer the currently exposed Coding app tools, such as `coding__edit_code`, `coding__read_code`, `coding__grep`, and `coding__glob`, instead of substituting terminal commands. Important: except for configuration, generated assets, or other non-source areas outside SCOPE engine responsibility, or cases where these tools genuinely cannot complete the task, do not use other tools or shell commands to edit source code. When Coding is focused, `apply_patch` is rejected for source files that SCOPE says it is responsible for; use `coding__edit_code` for those files when that mangled name is exposed.

After each edit, the tool automatically evaluates the impact of your changes and accumulates pending review events. You can also see the current number of pending review events in Coding app state. You do not need to handle them immediately. However, after you finish a series of edits (usually when a plan step is complete, or when you judge that too many review events have accumulated), call the currently exposed Coding review tool, such as `coding__next_review`, to acknowledge and claim review events; pass `limit` when many reviews are pending so several impact targets can be claimed in one response. Then follow their instructions to inspect the impact of your changes. This must always be done before reporting back to the user.

SCOPE engine configuration hints are returned by `coding__open_project` when that mangled name is exposed and retained in Coding app state, including available tree-sitter languages plus visible per-language `lsp_setup_hint` lines for LSP language/server setup guidance.

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
    pub edits: Vec<scope_engine::api::StructuredEdit>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingNextReviewArgs {
    /// Maximum number of review events to acknowledge and return.
    /// Omitted means one event to preserve existing behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

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
    coding_tool_group_calls: Vec<CodingToolCallUiData>,
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
            coding_tool_group_calls: Vec::new(),
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
        self.coding_tool_group_calls.clear();
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
            ui_event: ToolUiEvent::coding_open_project(
                project_root.display().to_string(),
                args.language,
                ui_lines,
            ),
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
            return Err(miette!("no coding project opened; call open_project first"));
        }
        Ok(())
    }

    fn coding_tool_group_event(
        &mut self,
        tool_name: impl Into<String>,
        summary: impl Into<String>,
        detail_lines: Vec<String>,
    ) -> ToolUiEvent {
        const MAX_GROUPED_CODING_CALLS: usize = 24;

        self.coding_tool_group_calls.push(CodingToolCallUiData {
            tool_name: tool_name.into(),
            summary: summary.into(),
            detail_lines,
        });
        if self.coding_tool_group_calls.len() > MAX_GROUPED_CODING_CALLS {
            let drop_count = self.coding_tool_group_calls.len() - MAX_GROUPED_CODING_CALLS;
            self.coding_tool_group_calls.drain(0..drop_count);
        }

        ToolUiEvent::coding_tool_group(
            self.coding_tool_group_stable_id(),
            "Explored",
            self.coding_tool_group_calls.clone(),
        )
    }

    fn coding_tool_group_stable_id(&self) -> String {
        let project_root = self.project_root_display();
        let mut hasher = Sha256::new();
        hasher.update(project_root.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        format!("coding-tools-{}", short_hash(&hash))
    }

    fn coding_edit_stable_id(&self, edits: &[scope_engine::api::StructuredEdit]) -> String {
        let project_root = self.project_root_display();
        let mut hasher = Sha256::new();
        hasher.update(project_root.as_bytes());
        if let Ok(json) = serde_json::to_string(edits) {
            hasher.update(json.as_bytes());
        }
        let hash = format!("{:x}", hasher.finalize());
        format!("coding-edit-{}", short_hash(&hash))
    }
}

impl Default for CodingApp {
    fn default() -> Self {
        Self::new()
    }
}

impl CodingApp {
    fn reject_scope_owned_apply_patch(&self, call: &AgentToolCall) -> Result<()> {
        self.require_project()?;
        let patch_text = extract_coding_apply_patch_text(call)?;
        let ops = parse_apply_patch(&patch_text)?;
        let mut blocked = Vec::new();
        for op in ops {
            let path = match op {
                PatchOp::Add { path, .. }
                | PatchOp::Delete { path }
                | PatchOp::Update { path, .. } => path,
            };
            let response = self.scope.is_responsible_source(Path::new(&path))?;
            if response.is_responsible {
                blocked.push(format!("{} ({})", response.path, response.reason));
            }
        }
        if blocked.is_empty() {
            return Ok(());
        }
        Err(miette!(
            "apply_patch is forbidden for SCOPE-owned source files while Coding is focused. Use edit_code instead. Blocked file(s): {}",
            blocked.join(", ")
        ))
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
                name: "open_project".to_string(),
                description: "Open a project for semantic code operations using scope-engine.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingOpenProjectArgs)).unwrap(),
            },
            AppToolSpec {
                name: "read_code".to_string(),
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
                name: "edit_code".to_string(),
                description: "Apply structured edits via selector+guard+content and return propagation results.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingEditCodeArgs)).unwrap(),
            },
            AppToolSpec {
                name: "next_review".to_string(),
                description: "Acknowledge and return accumulated scope-engine propagation review events, optionally batched with limit.".to_string(),
                scope: AppToolScope::Coding,
                input_schema: serde_json::to_value(schema_for!(CodingNextReviewArgs)).unwrap(),
            },
        ])
    }

    fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        let summary = match call.name.as_str() {
            "open_project" => {
                let args: CodingOpenProjectArgs = parse_coding_tool_args(call)?;
                format!(
                    "project_root={} language={}",
                    summarize_coding_inline_text(&args.project_root),
                    args.language.unwrap_or_else(|| "auto".to_string())
                )
            }
            "read_code" => {
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
            "edit_code" => {
                let args: CodingEditCodeArgs = parse_coding_tool_args(call)?;
                format!("edits_count={}", args.edits.len())
            }
            "next_review" => {
                let args: CodingNextReviewArgs = parse_coding_tool_args(call)?;
                match args.limit {
                    Some(limit) => format!("propagation review batch limit={limit}"),
                    None => "next propagation review".to_string(),
                }
            }
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

    fn before_runtime_tool_call(
        &self,
        call: &AgentToolCall,
        _context: &AppToolExecutionContext,
    ) -> Result<()> {
        if call.name == "apply_patch" {
            self.reject_scope_owned_apply_patch(call)?;
        }
        Ok(())
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        match call.name.as_str() {
            "open_project" => {
                let args: CodingOpenProjectArgs = parse_coding_tool_args(call)?;
                self.open_project(args, context)
            }
            "read_code" => {
                self.require_project()?;
                let args: CodingReadCodeArgs = parse_coding_tool_args(call)?;
                let result = self.scope.read_code(&args.selector)?;
                self.last_action = Some(format!("read {}", args.selector));
                let model_content = format!(
                    "path={}\ncontent=\n{}",
                    result.path,
                    truncate_text_to_token_budget(&result.content, context.tool_output_max_tokens)
                );
                let mut output = AppToolExecutionResult {
                    summary: format!("read code {}", args.selector),
                    payload: serde_json::to_value(&result).unwrap(),
                    model_content: Some(model_content),
                    ui_event: self.coding_tool_group_event(
                        "Read",
                        coding_target_summary(&args.selector),
                        vec![format!(
                            "{} lines",
                            coding_count_label(result.content.lines().count(), "line", "lines")
                        )],
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
                let mut detail_lines = Vec::new();
                if let Some(include) = args.include.as_deref() {
                    detail_lines.push(format!("include {}", summarize_coding_inline_text(include)));
                }
                let model_content = format_grep_matches_for_model(&result.matches);
                let mut output = AppToolExecutionResult {
                    summary: format!("found {} matches", result.matches.len()),
                    payload: serde_json::to_value(&result).unwrap(),
                    model_content: Some(truncate_text_to_token_budget(
                        &model_content,
                        context.tool_output_max_tokens,
                    )),
                    ui_event: self.coding_tool_group_event(
                        "Search",
                        coding_pattern_result_summary(
                            &args.pattern,
                            args.path.as_deref(),
                            result.matches.len(),
                            "match",
                            "matches",
                        ),
                        detail_lines,
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
                let mut detail_lines = Vec::new();
                if let Some(path) = args.path.as_deref() {
                    detail_lines.push(format!("under {}", summarize_coding_inline_text(path)));
                }
                let model_content = result.files.join("\n");
                let mut output = AppToolExecutionResult {
                    summary: format!("found {} files", result.files.len()),
                    payload: serde_json::to_value(&result).unwrap(),
                    model_content: Some(truncate_text_to_token_budget(
                        &model_content,
                        context.tool_output_max_tokens,
                    )),
                    ui_event: self.coding_tool_group_event(
                        "List",
                        coding_pattern_result_summary(
                            &args.pattern,
                            args.path.as_deref(),
                            result.files.len(),
                            "file",
                            "files",
                        ),
                        detail_lines,
                    ),
                    turn_boundary_reason: None,
                };
                if let Some(path) = args.path.as_deref() {
                    self.append_scoped_instructions_to_result(&mut output, path, context)?;
                }
                Ok(output)
            }
            "edit_code" => {
                self.require_project()?;
                let args: CodingEditCodeArgs = parse_coding_tool_args(call)?;
                let results = self.scope.edit_code(&args.edits)?;
                self.last_action = Some("edited code".to_string());
                let diff_files = structured_edit_ui_files(&args.edits, &results);
                let added_lines = diff_files
                    .iter()
                    .map(|file| file.added_lines)
                    .sum::<usize>();
                let removed_lines = diff_files
                    .iter()
                    .map(|file| file.removed_lines)
                    .sum::<usize>();
                let impact_lines = build_coding_edit_impact_lines(&results);
                let summary = format!("edited code; propagation_results={}", results.len());
                let mut output = AppToolExecutionResult {
                    summary: summary.clone(),
                    payload: json!({ "propagation_results": results }),
                    model_content: None,
                    ui_event: ToolUiEvent::coding_edit(CodingEditUiData {
                        stable_id: self.coding_edit_stable_id(&args.edits),
                        title: "Edited Code".to_string(),
                        selector: "hash-anchored edit".to_string(),
                        file: args.edits.first().map(|e| e.path.clone()),
                        added_lines,
                        removed_lines,
                        propagation_count: results.len(),
                        impact_lines,
                        diff_files,
                    }),
                    turn_boundary_reason: None,
                };
                self.append_scoped_instructions_to_result(
                    &mut output,
                    args.edits
                        .first()
                        .map(|e| e.path.as_str())
                        .unwrap_or_default(),
                    context,
                )?;
                Ok(output)
            }
            "next_review" => {
                self.require_project()?;
                let args: CodingNextReviewArgs = parse_coding_tool_args(call)?;
                let response = self.scope.ack_next_events(args.limit);
                self.last_action = Some(if response.returned == 1 {
                    "acknowledged next review".to_string()
                } else {
                    format!("acknowledged {} reviews", response.returned)
                });
                let review_present = response.returned > 0;
                let review_title = coding_review_title(&response);
                let review_summary = coding_review_summary(&response);
                let model_content = coding_review_model_content(&response).map(|content| {
                    truncate_text_to_token_budget(&content, context.tool_output_max_tokens)
                });
                Ok(AppToolExecutionResult {
                    summary: if review_present {
                        format!(
                            "acknowledged {} coding impact review target(s); remaining={}",
                            response.returned, response.remaining
                        )
                    } else {
                        "no coding review event pending".to_string()
                    },
                    payload: serde_json::to_value(&response).unwrap(),
                    model_content,
                    ui_event: ToolUiEvent::coding_review(
                        review_title,
                        review_summary,
                        review_present,
                    ),
                    turn_boundary_reason: None,
                })
            }
            _ => Err(miette!("unknown coding tool `{}`", call.name)),
        }
    }
}

fn extract_coding_apply_patch_text(call: &AgentToolCall) -> Result<String> {
    if let Some(input) = call
        .arguments
        .as_object()
        .and_then(|value| value.get("input"))
        && let Some(text) = input.as_str()
    {
        return Ok(text.to_string());
    }
    if let Some(patch) = call
        .arguments
        .as_object()
        .and_then(|value| value.get("patch"))
        && let Some(text) = patch.as_str()
    {
        return Ok(text.to_string());
    }
    if let Some(text) = call.arguments.as_str() {
        return Ok(text.to_string());
    }
    Err(miette!(
        "invalid arguments for tool `apply_patch`: expected a patch string in `input`"
    ))
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

fn coding_count_label(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

fn coding_target_summary(selector: &str) -> String {
    summarize_coding_inline_text(selector)
}

fn coding_review_title(response: &scope_engine::api::NextReviewResponse) -> String {
    match response.returned {
        0 => "No review pending".to_string(),
        1 => match response.review.as_ref() {
            Some(review) => format!(
                "Reviewing impact of {}",
                coding_target_summary(review_modified_symbol(review))
            ),
            None => "Reviewing impact target".to_string(),
        },
        returned => format!("Reviewing {returned} impact targets"),
    }
}

fn coding_review_summary(response: &scope_engine::api::NextReviewResponse) -> String {
    if response.returned == 0 {
        return "no pending propagation review".to_string();
    }

    format!(
        "{} acquired; {} remaining",
        coding_count_label(response.returned, "impact target", "impact targets"),
        response.remaining
    )
}

fn coding_review_model_content(response: &scope_engine::api::NextReviewResponse) -> Option<String> {
    if response.returned == 0 {
        return None;
    }

    let mut lines = vec![
        format!("returned={}", response.returned),
        format!("remaining={}", response.remaining),
    ];
    for (index, review) in response.reviews.iter().enumerate() {
        lines.push(format!(
            "{}. {}",
            index + 1,
            review_instruction_summary(review)
        ));
    }

    Some(lines.join("\n"))
}

fn review_modified_symbol(review: &scope_engine::api::ReviewEvent) -> &str {
    match review {
        scope_engine::api::ReviewEvent::KnownReferences {
            modified_symbol, ..
        }
        | scope_engine::api::ReviewEvent::InvestigateImpact {
            modified_symbol, ..
        } => modified_symbol,
    }
}

fn summarize_review_references(references: &[scope_engine::api::Reference]) -> String {
    if references.is_empty() {
        return "none".to_string();
    }

    let mut parts = references
        .iter()
        .take(3)
        .map(|reference| {
            summarize_coding_inline_text(&format!("{}:L{}", reference.selector, reference.line))
        })
        .collect::<Vec<_>>();
    if references.len() > 3 {
        parts.push(format!("+{} more", references.len() - 3));
    }
    parts.join(", ")
}

fn review_instruction_summary(review: &scope_engine::api::ReviewEvent) -> String {
    match review {
        scope_engine::api::ReviewEvent::KnownReferences {
            modified_symbol,
            change_summary,
            references,
            file_snippet,
        } => format!(
            "known references: target={} refs={} change={} snippet={}",
            summarize_coding_inline_text(modified_symbol),
            summarize_review_references(references),
            summarize_coding_inline_text(change_summary),
            summarize_coding_inline_text(file_snippet)
        ),
        scope_engine::api::ReviewEvent::InvestigateImpact {
            modified_symbol,
            change_summary,
            diff_summary,
            file_snippet,
            project_files,
        } => format!(
            "investigate impact: target={} project_files={} change={} diff={} snippet={}",
            summarize_coding_inline_text(modified_symbol),
            project_files.len(),
            summarize_coding_inline_text(change_summary),
            summarize_coding_inline_text(diff_summary),
            summarize_coding_inline_text(file_snippet)
        ),
    }
}

fn build_coding_edit_impact_lines(results: &[scope_engine::api::PropagationResult]) -> Vec<String> {
    results
        .iter()
        .take(8)
        .map(|result| {
            let mut line = format!(
                "{} — {}",
                summarize_coding_inline_text(&result.selector),
                summarize_coding_inline_text(&result.reason)
            );
            if let Some(snippet) = result.file_snippet.as_deref() {
                let compact = summarize_coding_inline_text(snippet);
                if !compact.is_empty() {
                    line.push_str(" · ");
                    line.push_str(&compact);
                }
            }
            line
        })
        .collect()
}

fn structured_edit_ui_files(
    edits: &[scope_engine::api::StructuredEdit],
    results: &[scope_engine::api::PropagationResult],
) -> Vec<PatchFileUiData> {
    let mut files = Vec::new();
    for edit in edits {
        let path = edit.path.clone();
        let (added, removed) = match edit.op {
            scope_engine::api::EditOp::Append | scope_engine::api::EditOp::Prepend => {
                let line_count = edit
                    .content
                    .as_ref()
                    .map(|c| match c {
                        scope_engine::api::EditContent::Lines(lines) => lines.len(),
                        scope_engine::api::EditContent::Text(text) => text.lines().count(),
                    })
                    .unwrap_or(0);
                (line_count, 0)
            }
            scope_engine::api::EditOp::Replace => {
                let line_count = edit
                    .content
                    .as_ref()
                    .map(|c| match c {
                        scope_engine::api::EditContent::Lines(lines) => lines.len(),
                        scope_engine::api::EditContent::Text(text) => text.lines().count(),
                    })
                    .unwrap_or(0);
                (line_count, 0)
            }
        };
        let diff_lines: Vec<PatchDiffLineUiData> = edit
            .content
            .as_ref()
            .map(|content| {
                let lines: Vec<String> = match content {
                    scope_engine::api::EditContent::Lines(lines) => lines.clone(),
                    scope_engine::api::EditContent::Text(text) => {
                        text.lines().map(str::to_string).collect()
                    }
                };
                lines
                    .iter()
                    .map(|line| PatchDiffLineUiData {
                        kind: PatchDiffLineKind::Context,
                        old_lineno: None,
                        new_lineno: None,
                        text: line.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.push(PatchFileUiData {
            path,
            operation: match edit.op {
                scope_engine::api::EditOp::Append | scope_engine::api::EditOp::Prepend => {
                    PatchFileOperation::Add
                }
                scope_engine::api::EditOp::Replace => PatchFileOperation::Update,
            },
            added_lines: added,
            removed_lines: removed,
            diff_lines,
        });
    }
    if files.is_empty() {
        // Fallback to results-based UI
        files.extend(results.iter().map(|result| {
            PatchFileUiData {
                path: selector_path(&result.selector).to_string(),
                operation: PatchFileOperation::Update,
                added_lines: result
                    .diff_summary
                    .as_deref()
                    .map(count_change_lines)
                    .unwrap_or(0),
                removed_lines: 0,
                diff_lines: result
                    .file_snippet
                    .as_deref()
                    .map(|snippet| {
                        snippet
                            .lines()
                            .map(|line| PatchDiffLineUiData {
                                kind: PatchDiffLineKind::Context,
                                old_lineno: None,
                                new_lineno: None,
                                text: line.to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        }));
    }
    files
}

fn count_change_lines(text: &str) -> usize {
    text.lines().filter(|line| !line.trim().is_empty()).count()
}

fn format_grep_matches_for_model(matches: &[scope_engine::api::SearchMatch]) -> String {
    if matches.is_empty() {
        return String::new();
    }
    let mut lines = vec![format!("Found {} matches", matches.len()), String::new()];
    let mut current_file: Option<&str> = None;
    let mut current_selector: Option<Option<&str>> = None;

    for item in matches {
        if current_file != Some(item.file.as_str()) {
            if current_file.is_some() {
                lines.push(String::new());
            }
            lines.push(format!("{}:", item.file));
            current_file = Some(item.file.as_str());
            current_selector = None;
        }

        let selector = item.selector.as_deref();
        if current_selector != Some(selector) {
            match selector {
                Some(sel) => lines.push(format!("  {sel}:")),
                None => lines.push("  Unclassified matches:".to_string()),
            }
            current_selector = Some(selector);
        }

        lines.push(format!("    Line {}: {}", item.line, item.text));
    }

    lines.join("\n")
}

fn coding_pattern_result_summary(
    pattern: &str,
    path: Option<&str>,
    count: usize,
    singular: &str,
    plural: &str,
) -> String {
    let mut summary = format!(
        "{} — {}",
        summarize_coding_inline_text(pattern),
        coding_count_label(count, singular, plural)
    );
    if let Some(path) = path {
        summary.push_str(" in ");
        summary.push_str(&summarize_coding_inline_text(path));
    }
    summary
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
