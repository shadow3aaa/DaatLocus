use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use daat_locus_macros::model_schema;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    activity_event::{
        CodingEditActivityDescriptor, CodingOpenProjectActivityDescriptor,
        CodingReviewActivityDescriptor, EXPLORED_STABLE_ID, ExploredActivityDescriptor,
        ExploredCallActivityAction, ExploredCallActivityDescriptor,
        PatchDiffLineActivityDescriptor, PatchDiffLineKind, PatchFileActivityDescriptor,
        PatchFileOperation, ToolCallActivityEvent, compact_body_lines,
    },
    app::{
        App, AppDocs, AppId, AppStateRender, AppToolExecutionContext, AppToolExecutionResult,
        AppToolSpec,
    },
    context_budget::truncate_text_to_token_budget,
    dashboard::SessionActivityEvent,
    reasoning::{episode::EpisodeActionRecord, prompts::APP_CODING, runtime::AgentToolCall},
    runtime::scope_engine::ScopeEngineHandle,
    schema_utils::{model_schema_for, structured_edit_args_schema},
};

const MAX_RENDERED_LSP_SETUP_HINTS: usize = 5;
const PROJECT_INSTRUCTION_FILENAMES: &[&str] =
    &["AGENTS.override.md", "AGENTS.md", "CLAUDE.md", "GEMINI.md"];

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodingOpenProjectArgs {
    pub project_root: String,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodingReadCodeArgs {
    pub path: String,
    pub anchor: String,
    pub mode: CodingReadCodeMode,
}

type CodingSearchCodeArgs = scope_engine::api::SearchCodeInput;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingEditCodeArgs {
    pub edits: Vec<scope_engine::api::StructuredEdit>,
}

#[model_schema]
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CodingNextReviewArgs {
    /// Maximum number of review events to acknowledge and return.
    /// Omitted means one event to preserve existing behavior.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[model_schema]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CodingReadCodeMode {
    Around,
    Full,
}

#[model_schema]
#[derive(Serialize, Deserialize)]
pub struct CodingSearchCodeArgsSchema {
    pub query: String,
    pub mode: CodingSearchModeSchema,
    pub path: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub types: Vec<String>,
    pub type_not: Vec<String>,
    #[serde(rename = "case")]
    pub case_mode: CodingSearchCaseSchema,
    pub word: bool,
    pub whole_line: bool,
    pub hidden: bool,
    pub respect_ignore: bool,
    pub follow: bool,
    pub limit: Option<usize>,
}

#[model_schema]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodingSearchModeSchema {
    Literal,
    Regex,
}

#[model_schema]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodingSearchCaseSchema {
    Sensitive,
    Insensitive,
    Smart,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectInstructionDocument {
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

pub(crate) fn load_instruction_documents_in_dir(
    dir: &Path,
) -> Result<Vec<ProjectInstructionDocument>> {
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
        let sha256 = hex::encode(hasher.finalize());
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

fn hash_instruction_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|err| {
        miette!(
            "failed to open project instruction file {}: {err}",
            path.display()
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).map_err(|err| {
            miette!(
                "failed to read project instruction file {}: {err}",
                path.display()
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn load_project_instruction_fingerprint_in_dir(dir: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for name in PROJECT_INSTRUCTION_FILENAMES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let sha256 = hash_instruction_file(&path)?;
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(sha256.as_bytes());
        hasher.update(b"\0");
    }
    Ok(hex::encode(hasher.finalize()))
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

pub(crate) fn project_instruction_fingerprint(
    instructions: &[ProjectInstructionDocument],
) -> String {
    let mut hasher = Sha256::new();
    for instruction in instructions {
        hasher.update(instruction.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(instruction.sha256.as_bytes());
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}

fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

pub(crate) fn render_project_instructions(
    label: &str,
    instructions: &[ProjectInstructionDocument],
) -> String {
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
    scope: ScopeEngineHandle,
    project_root: Option<PathBuf>,
    config_hint_summary: Option<CodingConfigHintSummary>,
    root_instructions: Vec<ProjectInstructionDocument>,
    root_instruction_fingerprint: Option<String>,
    delivered_scoped_instructions: HashSet<DeliveredProjectInstructionKey>,
    last_action: Option<String>,
}

impl CodingApp {
    pub fn new() -> Self {
        Self {
            scope: ScopeEngineHandle::new(),
            project_root: None,
            config_hint_summary: None,
            root_instructions: Vec::new(),
            root_instruction_fingerprint: None,
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

        if self.project_root.as_deref() == Some(project_root.as_path()) {
            let root_instruction_fingerprint =
                load_project_instruction_fingerprint_in_dir(&project_root)?;
            if self.root_instruction_fingerprint.as_deref()
                == Some(root_instruction_fingerprint.as_str())
            {
                self.last_action = Some("project already open".to_string());
                return Ok(AppToolExecutionResult::from_activity_event(
                    format!("coding project already open {}", project_root.display()),
                    json!({
                        "status": "already_open",
                        "project_root": project_root,
                        "root_project_instruction_fingerprint": root_instruction_fingerprint,
                    }),
                    Some(format!(
                        "status=already_open\nproject_root={}\nroot_project_instruction_fingerprint={}",
                        project_root.display(),
                        root_instruction_fingerprint,
                    )),
                    Some(SessionActivityEvent::CodingOpenProject(
                        CodingOpenProjectActivityDescriptor {
                            project_root: project_root.display().to_string(),
                            detail_lines: vec![
                                "status=already_open".to_string(),
                                format!("project_root={}", project_root.display()),
                                format!(
                                    "root_project_instruction_fingerprint={}",
                                    short_hash(&root_instruction_fingerprint)
                                ),
                            ],
                        }
                        .into(),
                    )),
                ));
            }

            let root_instructions = load_instruction_documents_in_dir(&project_root)?;
            debug_assert_eq!(
                project_instruction_fingerprint(&root_instructions),
                root_instruction_fingerprint
            );
            self.root_instructions = root_instructions.clone();
            self.root_instruction_fingerprint = Some(root_instruction_fingerprint.clone());
            self.last_action = Some("reloaded project instructions".to_string());
            let mut ui_lines = vec![
                "status=project_instructions_reloaded".to_string(),
                format!("project_root={}", project_root.display()),
                format!(
                    "root_project_instruction_fingerprint={}",
                    short_hash(&root_instruction_fingerprint)
                ),
            ];
            ui_lines.extend(root_instructions.iter().map(|instruction| {
                format!(
                    "root_instruction file={} sha256={}",
                    instruction.path.display(),
                    short_hash(&instruction.sha256)
                )
            }));
            return Ok(AppToolExecutionResult::from_activity_event(
                format!(
                    "reloaded coding project instructions {}",
                    project_root.display()
                ),
                json!({
                    "status": "project_instructions_reloaded",
                    "project_root": project_root,
                    "root_project_instruction_fingerprint": root_instruction_fingerprint,
                }),
                Some(format!(
                    "status=project_instructions_reloaded\nproject_root={}\nroot_project_instruction_fingerprint={}\nproject_instruction_context=available_in_next_preturn_context",
                    project_root.display(),
                    root_instruction_fingerprint,
                )),
                Some(SessionActivityEvent::CodingOpenProject(
                    CodingOpenProjectActivityDescriptor {
                        project_root: project_root.display().to_string(),
                        detail_lines: ui_lines,
                    }
                    .into(),
                )),
            ));
        }

        let root_instructions = load_instruction_documents_in_dir(&project_root)?;
        let root_instruction_fingerprint = project_instruction_fingerprint(&root_instructions);
        let output = self.scope.open_project(project_root.clone())?;
        let config_hints = ScopeEngineHandle::get_config_hints();
        let config_hint_summary = CodingConfigHintSummary::from_hints(&config_hints);

        self.project_root = Some(project_root.clone());
        self.config_hint_summary = Some(config_hint_summary.clone());
        self.root_instructions = root_instructions.clone();
        self.root_instruction_fingerprint = Some(root_instruction_fingerprint.clone());
        self.delivered_scoped_instructions.clear();
        self.last_action = Some("opened project".to_string());

        let model_parts = [
            config_hint_summary.model_content(),
            format!(
                "root_project_instruction_fingerprint={}\nproject_instruction_context=available_in_next_preturn_context",
                root_instruction_fingerprint
            ),
        ];
        let model_content = truncate_text_to_token_budget(
            &model_parts.join("\n\n"),
            context.tool_output_max_tokens,
        );
        let mut ui_lines = vec![format!("project_root={}", project_root.display())];
        ui_lines.extend(config_hint_summary.state_lines());
        ui_lines.push(format!(
            "root_project_instruction_fingerprint={}",
            short_hash(&root_instruction_fingerprint)
        ));
        ui_lines.extend(root_instructions.iter().map(|instruction| {
            format!(
                "root_instruction file={} sha256={}",
                instruction.path.display(),
                short_hash(&instruction.sha256)
            )
        }));

        Ok(AppToolExecutionResult::from_activity_event(
            format!("opened coding project {}", project_root.display()),
            json!({
                "status": "opened",
                "project_root": project_root,
                "scope_output": output,
                "config_hints": config_hints,
                "root_project_instruction_fingerprint": root_instruction_fingerprint,
            }),
            Some(model_content),
            Some(SessionActivityEvent::CodingOpenProject(
                CodingOpenProjectActivityDescriptor {
                    project_root: project_root.display().to_string(),
                    detail_lines: ui_lines,
                }
                .into(),
            )),
        ))
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
        if let Some(SessionActivityEvent::GenericApp(ui_data)) = &mut result.activity_event {
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

    fn explored_event(
        &self,
        tool_name: impl Into<String>,
        action: ExploredCallActivityAction,
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
                    action: Some(action),
                    target,
                    secondary_target,
                    summary: summary.into(),
                    detail_lines,
                }],
            }
            .into(),
        )
    }

    fn coding_edit_stable_id(&self, edits: &[scope_engine::api::StructuredEdit]) -> String {
        let project_root = self.project_root_display();
        let mut hasher = Sha256::new();
        hasher.update(project_root.as_bytes());
        if let Ok(json) = serde_json::to_string(edits) {
            hasher.update(json.as_bytes());
        }
        let hash = hex::encode(hasher.finalize());
        format!("coding-edit-{}", short_hash(&hash))
    }
}

impl Default for CodingApp {
    fn default() -> Self {
        Self::new()
    }
}

impl CodingApp {
    fn reject_scope_owned_edit_file(&self, call: &AgentToolCall) -> Result<()> {
        #[derive(Deserialize)]
        struct EditFilePathArgs {
            edits: Vec<EditFilePath>,
        }

        #[derive(Deserialize)]
        struct EditFilePath {
            path: String,
        }

        self.require_project()?;
        let args: EditFilePathArgs =
            serde_json::from_value(call.arguments.clone()).map_err(|err| {
                miette!(
                    "invalid arguments for tool `edit_file`: {}; arguments={}",
                    err,
                    call.arguments
                )
            })?;
        let mut blocked = Vec::new();
        for edit in args.edits {
            let output = self.scope.is_responsible_source(Path::new(&edit.path))?;
            if output.is_responsible {
                blocked.push(format!("{} ({})", output.path, output.reason));
            }
        }
        if blocked.is_empty() {
            return Ok(());
        }
        Err(miette!(
            "edit_file is forbidden for SCOPE-owned source files while a Coding project scope is open. Use edit_code instead. Blocked file(s): {}",
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
        if let Some(summary) = self.config_hint_summary.as_ref() {
            lines.extend(summary.state_lines());
        }
        if let Some(last_action) = self.last_action.as_ref() {
            lines.push(format!("last_action={last_action}"));
        }
        AppStateRender {
            title: "Coding".to_string(),
            lines,
        }
    }

    fn docs(&self) -> AppDocs {
        APP_CODING.app_docs()
    }

    fn cached_root_project_instructions(&self) -> Option<&[ProjectInstructionDocument]> {
        if self.root_instructions.is_empty() {
            None
        } else {
            Some(&self.root_instructions)
        }
    }

    fn tool_specs(&self) -> Result<Vec<AppToolSpec>> {
        Ok(vec![
            AppToolSpec {
                name: "open_project".to_string(),
                description: "Open a project for semantic code operations using scope-engine.".to_string(),
                input_schema: model_schema_for::<CodingOpenProjectArgs>(),
            },
            AppToolSpec {
                name: "search_code".to_string(),
                description:
                    "Search source content and return path-scoped line-hash hits; repeated visible source lines may be elided with ~."
                        .to_string(),
                input_schema: model_schema_for::<CodingSearchCodeArgsSchema>(),
            },
            AppToolSpec {
                name: "read_code".to_string(),
                description:
                    "Read code by path plus line-hash anchor in around or full mode; repeated visible source lines may be elided with ~."
                        .to_string(),
                input_schema: model_schema_for::<CodingReadCodeArgs>(),
            },
            AppToolSpec {
                name: "edit_code".to_string(),
                description: "Apply structured path + line-hash anchored edits and return propagation results.".to_string(),
                input_schema: structured_edit_args_schema(),
            },
            AppToolSpec {
                name: "next_review".to_string(),
                description: "Acknowledge and return accumulated scope-engine propagation review events, optionally batched with limit.".to_string(),
                input_schema: model_schema_for::<CodingNextReviewArgs>(),
            },
        ])
    }

    fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        let summary = match call.name.as_str() {
            "open_project" => {
                let args: CodingOpenProjectArgs = parse_coding_tool_args(call)?;
                format!(
                    "project_root={}",
                    summarize_coding_inline_text(&args.project_root)
                )
            }
            "search_code" => {
                let args: CodingSearchCodeArgs = parse_coding_tool_args(call)?;
                format!("query={}", summarize_coding_inline_text(&args.query))
            }
            "read_code" => {
                let args: CodingReadCodeArgs = parse_coding_tool_args(call)?;
                format!(
                    "target={}",
                    summarize_coding_inline_text(&read_args_summary(&args))
                )
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

    fn tool_call_activity_event(&self, call: &AgentToolCall) -> Result<ToolCallActivityEvent> {
        Ok(ToolCallActivityEvent::app(
            call.name.clone(),
            compact_coding_argument_lines(&call.arguments),
        ))
    }

    fn before_runtime_tool_call(
        &self,
        call: &AgentToolCall,
        _context: &AppToolExecutionContext,
    ) -> Result<()> {
        if self.project_root.is_some() && call.name == "edit_file" {
            self.reject_scope_owned_edit_file(call)?;
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
            "search_code" => {
                self.require_project()?;
                let args: CodingSearchCodeArgs = parse_coding_tool_args(call)?;
                let result = self.scope.search_code(args.clone())?;
                self.last_action = Some(format!("searched {}", args.query));
                let mut detail_lines = Vec::new();
                if !args.include.is_empty() {
                    detail_lines.push(format!(
                        "include {}",
                        summarize_coding_inline_text(&args.include.join(", "))
                    ));
                }
                if !args.exclude.is_empty() {
                    detail_lines.push(format!(
                        "exclude {}",
                        summarize_coding_inline_text(&args.exclude.join(", "))
                    ));
                }
                if !args.types.is_empty() {
                    detail_lines.push(format!(
                        "types {}",
                        summarize_coding_inline_text(&args.types.join(", "))
                    ));
                }
                let model_content = format_search_hits_for_model(&result.matches);
                let mut output = AppToolExecutionResult::from_activity_event(
                    format!("found {} matches", result.matches.len()),
                    serde_json::to_value(&result).unwrap(),
                    Some(truncate_text_to_token_budget(
                        &model_content,
                        context.tool_output_max_tokens,
                    )),
                    Some(self.explored_event(
                        "Search",
                        ExploredCallActivityAction::Search,
                        Some(args.query.clone()),
                        args.path.clone(),
                        coding_pattern_result_summary(
                            &args.query,
                            args.path.as_deref(),
                            result.matches.len(),
                            "match",
                            "matches",
                        ),
                        detail_lines,
                    )),
                );
                if let Some(path) = args.path.as_deref() {
                    self.append_scoped_instructions_to_result(&mut output, path, context)?;
                }
                Ok(output)
            }
            "read_code" => {
                self.require_project()?;
                let args: CodingReadCodeArgs = parse_coding_tool_args(call)?;
                let summary_target = read_args_summary(&args);
                let input = scope_engine::api::ReadCodeInput {
                    path: args.path.clone(),
                    anchor: args.anchor.clone(),
                    mode: match args.mode {
                        CodingReadCodeMode::Around => scope_engine::api::ReadCodeMode::Around,
                        CodingReadCodeMode::Full => scope_engine::api::ReadCodeMode::Full,
                    },
                };
                let result = self.scope.read_code(input)?;
                self.last_action = Some(format!("read {summary_target}"));
                let model_content =
                    truncate_text_to_token_budget(&result.content, context.tool_output_max_tokens);
                let mut output = AppToolExecutionResult::from_activity_event(
                    format!("read code {summary_target}"),
                    serde_json::to_value(&result).unwrap(),
                    Some(model_content),
                    Some(self.explored_event(
                        "Read",
                        ExploredCallActivityAction::Read,
                        Some(args.path.clone()),
                        None,
                        coding_target_summary(&args.path),
                        vec![format!(
                            "{} lines",
                            coding_count_label(result.content.lines().count(), "line", "lines")
                        )],
                    )),
                );
                self.append_scoped_instructions_to_result(&mut output, &args.path, context)?;
                Ok(output)
            }
            "edit_code" => {
                self.require_project()?;
                let args: CodingEditCodeArgs = parse_coding_tool_args(call)?;
                let edit_result = self.scope.edit_code(&args.edits)?;
                let results = edit_result.propagation_results;
                let applied_summary = edit_result.applied_summary;
                self.last_action = Some("edited code".to_string());
                let diff_files = applied_edit_ui_files(&applied_summary);
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
                let mut output = AppToolExecutionResult::from_activity_event(
                    summary.clone(),
                    json!({ "propagation_results": &results }),
                    None,
                    Some(SessionActivityEvent::CodingEdit(
                        CodingEditActivityDescriptor {
                            stable_id: self.coding_edit_stable_id(&args.edits),
                            title: "Edited Code".to_string(),
                            tool_name: None,
                            tool_app: None,
                            selector: "hash-anchored edit".to_string(),
                            file: args.edits.first().map(|e| e.path.clone()),
                            added_lines,
                            removed_lines,
                            propagation_count: results.len(),
                            impact_lines,
                            diff_files,
                        }
                        .into(),
                    )),
                );
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
                let output = self.scope.ack_next_events(args.limit);
                self.last_action = Some(if output.returned == 1 {
                    "acknowledged next review".to_string()
                } else {
                    format!("acknowledged {} reviews", output.returned)
                });
                let review_present = output.returned > 0;
                let review_title = coding_review_title(&output);
                let review_summary = coding_review_summary(&output);
                let model_content = coding_review_model_content(&output).map(|content| {
                    truncate_text_to_token_budget(&content, context.tool_output_max_tokens)
                });
                Ok(AppToolExecutionResult::from_activity_event(
                    if review_present {
                        format!(
                            "acknowledged {} coding impact review target(s); remaining={}",
                            output.returned, output.remaining
                        )
                    } else {
                        "no coding review event pending".to_string()
                    },
                    serde_json::to_value(&output).unwrap(),
                    model_content,
                    Some(SessionActivityEvent::CodingReview(
                        CodingReviewActivityDescriptor {
                            title: review_title,
                            summary: review_summary,
                            review_pending: review_present,
                        }
                        .into(),
                    )),
                ))
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

fn coding_review_title(output: &scope_engine::api::ReviewBatch) -> String {
    match output.returned {
        0 => "No review pending".to_string(),
        1 => match output.review.as_ref() {
            Some(review) => format!(
                "Reviewing impact of {}",
                coding_target_summary(review_modified_symbol(review))
            ),
            None => "Reviewing impact target".to_string(),
        },
        returned => format!("Reviewing {returned} impact targets"),
    }
}

fn coding_review_summary(output: &scope_engine::api::ReviewBatch) -> String {
    if output.returned == 0 {
        return "no pending propagation review".to_string();
    }

    format!(
        "{} acquired; {} remaining",
        coding_count_label(output.returned, "impact target", "impact targets"),
        output.remaining
    )
}

fn coding_review_model_content(output: &scope_engine::api::ReviewBatch) -> Option<String> {
    if output.returned == 0 {
        return None;
    }

    let mut lines = vec![
        format!("returned={}", output.returned),
        format!("remaining={}", output.remaining),
    ];
    for (index, review) in output.reviews.iter().enumerate() {
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

fn format_search_hits_for_model(matches: &[scope_engine::api::SearchHit]) -> String {
    if matches.is_empty() {
        return "found 0 matches".to_string();
    }

    matches
        .iter()
        .map(|hit| format!("{}|{}", hit.path, hit.hit))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_args_summary(args: &CodingReadCodeArgs) -> String {
    format!("{} {}", args.path, args.anchor)
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

    fn test_app_context(root: &Path) -> AppToolExecutionContext {
        AppToolExecutionContext {
            execution_cwd: root.to_path_buf(),
            sandbox_policy: crate::sandbox::RuntimeSandboxPolicy::disabled(),
            dashboard_tx: None,
            tool_output_max_tokens: 4096,
            turn_epoch: 1,
        }
    }

    fn pending_review_result(selector: &str) -> scope_engine::api::PropagationResult {
        scope_engine::api::PropagationResult {
            selector: selector.to_string(),
            reason: "changed".to_string(),
            source: scope_engine::api::PropagationSource::OpenEnded,
            lsp_references: None,
            diff_summary: Some("diff".to_string()),
            file_snippet: Some("fn main() {}".to_string()),
            project_files: Some(vec!["src/main.rs".to_string()]),
        }
    }

    #[test]
    fn applied_edit_ui_files_render_real_added_and_deleted_rows() {
        let summary = scope_engine::api::AppliedStructuredEditSummary {
            files: vec![scope_engine::api::AppliedStructuredEditFile {
                path: "src/app.rs".to_string(),
                operation: scope_engine::api::AppliedStructuredEditOperation::Update,
                added_lines: 1,
                removed_lines: 2,
                original_content: "fn main() {\n    old();\n    stale();\n}\n".to_string(),
                new_content: "fn main() {\n    new();\n}\n".to_string(),
            }],
        };

        let files = applied_edit_ui_files(&summary);

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].added_lines, 1);
        assert_eq!(files[0].removed_lines, 2);
        assert!(files[0].diff_lines.iter().any(|line| {
            line.kind == PatchDiffLineKind::Context
                && line.old_lineno == Some(1)
                && line.new_lineno == Some(1)
                && line.text == "fn main() {"
        }));
        assert!(files[0].diff_lines.iter().any(|line| {
            line.kind == PatchDiffLineKind::Delete
                && line.old_lineno == Some(2)
                && line.new_lineno.is_none()
                && line.text == "    old();"
        }));
        assert!(files[0].diff_lines.iter().any(|line| {
            line.kind == PatchDiffLineKind::Delete
                && line.old_lineno == Some(3)
                && line.new_lineno.is_none()
                && line.text == "    stale();"
        }));
        assert!(files[0].diff_lines.iter().any(|line| {
            line.kind == PatchDiffLineKind::Add
                && line.old_lineno.is_none()
                && line.new_lineno == Some(2)
                && line.text == "    new();"
        }));
    }

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
    fn search_hits_model_output_names_empty_results() {
        assert_eq!(format_search_hits_for_model(&[]), "found 0 matches");
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
        assert_eq!(
            load_project_instruction_fingerprint_in_dir(temp.path())
                .expect("load instruction fingerprint"),
            project_instruction_fingerprint(&instructions)
        );
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
    fn open_project_is_idempotent_when_root_instructions_are_unchanged() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("src/nested");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(temp.path().join("AGENTS.md"), "Root rule v1\n").expect("write root");
        std::fs::write(nested.join("AGENTS.md"), "Nested rule\n").expect("write nested");
        let context = test_app_context(temp.path());
        let mut app = CodingApp::new();

        let first = app
            .open_project(
                CodingOpenProjectArgs {
                    project_root: temp.path().display().to_string(),
                },
                &context,
            )
            .expect("first open");
        let first_model_content = first.model_content.as_deref().unwrap_or_default();
        assert!(first_model_content.contains("root_project_instruction_fingerprint="));
        assert!(first_model_content.contains("project_instruction_context="));
        assert!(!first_model_content.contains("Root rule v1"));
        assert_eq!(
            app.scoped_instructions_for_path_once_per_turn("src/nested/file.rs", 7)
                .expect("first scoped")
                .len(),
            1
        );
        let _ = app.scope.next_review_event(vec![
            pending_review_result("src/main.rs::fn main"),
            pending_review_result("src/lib.rs::fn lib"),
        ]);
        assert_eq!(app.scope.pending_review_count(), 1);

        let second = app
            .open_project(
                CodingOpenProjectArgs {
                    project_root: temp.path().display().to_string(),
                },
                &context,
            )
            .expect("second open");

        assert_eq!(
            second.payload.get("status").and_then(Value::as_str),
            Some("already_open")
        );
        assert!(second.payload.get("root_project_instructions").is_none());
        assert!(
            !second
                .model_content
                .as_deref()
                .unwrap_or_default()
                .contains("Root rule v1")
        );
        assert_eq!(app.scope.pending_review_count(), 1);
        assert!(
            app.scoped_instructions_for_path_once_per_turn("src/nested/file.rs", 7)
                .expect("second scoped")
                .is_empty()
        );
    }

    #[test]
    fn open_project_reloads_root_instructions_when_hash_changes_without_clearing_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("src/nested");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(temp.path().join("AGENTS.md"), "Root rule v1\n").expect("write root");
        std::fs::write(nested.join("AGENTS.md"), "Nested rule\n").expect("write nested");
        let context = test_app_context(temp.path());
        let mut app = CodingApp::new();

        app.open_project(
            CodingOpenProjectArgs {
                project_root: temp.path().display().to_string(),
            },
            &context,
        )
        .expect("first open");
        assert_eq!(
            app.scoped_instructions_for_path_once_per_turn("src/nested/file.rs", 7)
                .expect("first scoped")
                .len(),
            1
        );
        let _ = app.scope.next_review_event(vec![
            pending_review_result("src/main.rs::fn main"),
            pending_review_result("src/lib.rs::fn lib"),
        ]);
        assert_eq!(app.scope.pending_review_count(), 1);

        std::fs::write(temp.path().join("AGENTS.md"), "Root rule v2\n").expect("update root");
        let second = app
            .open_project(
                CodingOpenProjectArgs {
                    project_root: temp.path().display().to_string(),
                },
                &context,
            )
            .expect("second open");

        assert_eq!(
            second.payload.get("status").and_then(Value::as_str),
            Some("project_instructions_reloaded")
        );
        let second_model_content = second.model_content.as_deref().unwrap_or_default();
        assert!(second_model_content.contains("root_project_instruction_fingerprint="));
        assert!(second_model_content.contains("project_instruction_context="));
        assert!(!second_model_content.contains("Root rule v2"));
        assert_eq!(app.scope.pending_review_count(), 1);
        assert!(
            app.scoped_instructions_for_path_once_per_turn("src/nested/file.rs", 7)
                .expect("second scoped")
                .is_empty()
        );
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
