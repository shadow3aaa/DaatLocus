use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tool_ui::{
    PatchDiffLineKind, PatchFileOperation, PatchFileUiData, ReplyDisposition, ReplySubject,
};

use super::{
    ActivityCell, LiveActivityCell,
    apps::{BrowserActivityCell, LiveBrowserActivityCell},
    common::{
        AssistantActivityCell, ErrorActivityCell, GenericAppActivityCell, TerminalWaitActivityCell,
        UserActivityCell,
    },
    exec::{ExecResultActivityCell, LiveExecActivityCell},
    messages::{PatchActivityCell, ReplyActivityCell, TelegramActivityCell},
    plan::{PlanActivityCell, PlanStepDisplayStatus},
    workflow::{ActivateWorkflowActivityCell, CreateWorkflowActivityCell, DeepRecallActivityCell},
};

pub const WEB_ACTIVITY_VERSION: u8 = 1;

pub fn default_web_activity_version() -> u8 {
    WEB_ACTIVITY_VERSION
}

pub fn sync_web_activity_state(state: &mut crate::dashboard::DashboardState) {
    let previous_items = state
        .web_activity_items
        .iter()
        .map(|item| (item.id.clone(), item.clone()))
        .collect::<HashMap<_, _>>();
    let previous_live_items = state
        .live_web_activity_items
        .iter()
        .map(|entry| (entry.item.id.clone(), entry.item.clone()))
        .collect::<HashMap<_, _>>();

    state.web_activity_version = WEB_ACTIVITY_VERSION;
    let mut items = render_web_activity_items(&state.activity_cells);
    for item in &mut items {
        preserve_item_timestamps(item, &previous_items);
    }
    state.web_activity_items = items;

    let mut live_items = render_live_web_activity_items(&state.live_activity_cells);
    for entry in &mut live_items {
        preserve_item_timestamps(&mut entry.item, &previous_live_items);
    }
    state.live_web_activity_items = live_items;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WebActivityItem {
    pub web_activity_version: u8,
    pub id: String,
    pub kind: WebActivityKind,
    pub status: WebActivityStatus,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<WebActivityActor>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<WebActivitySource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<WebActivityTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<WebActivityBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_blocks: Vec<WebActivityBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WebActivityError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebActivityKind {
    Message,
    Tool,
    App,
    Plan,
    Workflow,
    Memory,
    Patch,
    Error,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebActivityStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Dismissed,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebActivityActor {
    User,
    Assistant,
    Telegram,
    Tool,
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebActivitySource {
    pub source_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebActivityTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebActivityBlock {
    Text {
        text: String,
    },
    Code {
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    Kv {
        entries: Vec<WebActivityKvEntry>,
    },
    List {
        items: Vec<String>,
    },
    Diff {
        files: Vec<WebActivityDiffFile>,
    },
    Link {
        label: String,
        url: String,
    },
    Artifact {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebActivityKvEntry {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebActivityDiffFile {
    pub path: String,
    pub operation: String,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub lines: Vec<WebActivityDiffLine>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebActivityDiffLine {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_lineno: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_lineno: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebActivityError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LiveWebActivityItem {
    pub key: String,
    pub item: WebActivityItem,
}

pub fn render_web_activity_items(cells: &[ActivityCell]) -> Vec<WebActivityItem> {
    cells
        .iter()
        .enumerate()
        .map(|(index, cell)| web_activity_item_from_cell(cell, &format!("activity-{index}"), false))
        .collect()
}

pub fn render_live_web_activity_items(cells: &[LiveActivityCell]) -> Vec<LiveWebActivityItem> {
    cells
        .iter()
        .enumerate()
        .map(|(index, live_cell)| {
            let key = if live_cell.key.is_empty() {
                format!("live-{index}")
            } else {
                live_cell.key.clone()
            };
            let id = format!("live-{key}");
            LiveWebActivityItem {
                key,
                item: web_activity_item_from_cell(&live_cell.cell, &id, true),
            }
        })
        .collect()
}

pub fn web_activity_item_from_cell(cell: &ActivityCell, id: &str, live: bool) -> WebActivityItem {
    let now = chrono::Utc::now().timestamp_millis();
    let mut item = WebActivityItem {
        web_activity_version: WEB_ACTIVITY_VERSION,
        id: id.to_string(),
        kind: WebActivityKind::Unknown,
        status: if live {
            WebActivityStatus::Running
        } else {
            WebActivityStatus::Completed
        },
        title: "Activity".to_string(),
        actor: None,
        created_at: now,
        updated_at: now,
        source: None,
        tool: None,
        blocks: Vec::new(),
        detail_blocks: Vec::new(),
        error: None,
        metadata: None,
    };

    match cell {
        ActivityCell::Assistant(cell) => apply_assistant_cell(&mut item, cell),
        ActivityCell::User(cell) => apply_user_cell(&mut item, cell),
        ActivityCell::AppAttention(cell) => apply_simple_tool_item(
            &mut item,
            "app_attention",
            Some("App"),
            cell.title.clone(),
            cell.body_lines.clone(),
        ),
        ActivityCell::Browser(cell) => apply_browser_cell(&mut item, cell),
        ActivityCell::LiveBrowser(cell) => apply_live_browser_cell(&mut item, cell),
        ActivityCell::GenericApp(cell) => apply_generic_app_cell(&mut item, cell),
        ActivityCell::PlanResult(cell) => apply_plan_cell(&mut item, cell),
        ActivityCell::CreateWorkflowResult(cell) => apply_create_workflow_cell(&mut item, cell),
        ActivityCell::ActivateWorkflowResult(cell) => apply_activate_workflow_cell(&mut item, cell),
        ActivityCell::DeepRecallResult(cell) => apply_deep_recall_cell(&mut item, cell),
        ActivityCell::ExecResult(cell) => apply_exec_cell(&mut item, cell),
        ActivityCell::LiveExec(cell) => apply_live_exec_cell(&mut item, cell),
        ActivityCell::Patch(cell) => apply_patch_cell(&mut item, cell),
        ActivityCell::Telegram(cell) => apply_telegram_cell(&mut item, cell),
        ActivityCell::Reply(cell) => apply_reply_cell(&mut item, cell),
        ActivityCell::TerminalWait(cell) => apply_terminal_wait_cell(&mut item, cell),
        ActivityCell::Error(cell) => apply_error_cell(&mut item, cell),
    }

    if matches!(item.kind, WebActivityKind::Error) {
        item.status = WebActivityStatus::Failed;
    }
    item
}

fn apply_assistant_cell(item: &mut WebActivityItem, cell: &AssistantActivityCell) {
    item.kind = WebActivityKind::Message;
    item.actor = Some(WebActivityActor::Assistant);
    item.title = if cell.title.trim().is_empty() {
        "Agent".to_string()
    } else {
        cell.title.clone()
    };
    item.blocks = text_blocks(primary_lines(&cell.title, &cell.body_lines));
}

fn apply_user_cell(item: &mut WebActivityItem, cell: &UserActivityCell) {
    item.kind = WebActivityKind::Message;
    item.actor = Some(WebActivityActor::User);
    item.title = if cell.title.trim().is_empty() {
        "You".to_string()
    } else {
        cell.title.clone()
    };
    item.blocks = text_blocks(primary_lines(&cell.title, &cell.body_lines));
}

fn apply_telegram_cell(item: &mut WebActivityItem, cell: &TelegramActivityCell) {
    item.kind = WebActivityKind::Message;
    item.actor = Some(WebActivityActor::Telegram);
    item.title = if cell.title.trim().is_empty() {
        "Telegram".to_string()
    } else {
        cell.title.clone()
    };
    item.source = Some(WebActivitySource {
        source_type: "telegram".to_string(),
        label: Some(item.title.clone()),
    });
    item.blocks = text_blocks(cell.message_lines.clone());
    item.detail_blocks = text_blocks(cell.detail_lines.clone());
    if item.blocks.is_empty() {
        item.blocks = text_blocks(vec![item.title.clone()]);
    }
}

fn apply_reply_cell(item: &mut WebActivityItem, cell: &ReplyActivityCell) {
    item.kind = WebActivityKind::Message;
    item.actor = Some(WebActivityActor::Assistant);
    item.status = match cell.disposition {
        ReplyDisposition::Resolved => WebActivityStatus::Completed,
        ReplyDisposition::Dismissed => WebActivityStatus::Dismissed,
        ReplyDisposition::Failed => WebActivityStatus::Failed,
    };
    item.title = match (cell.subject.clone(), cell.disposition.clone()) {
        (ReplySubject::Notice, ReplyDisposition::Resolved) => "Resolved Notice".to_string(),
        (ReplySubject::Notice, ReplyDisposition::Dismissed) => "Dismissed Notice".to_string(),
        (ReplySubject::Notice, ReplyDisposition::Failed) => "Failed Notice".to_string(),
        (_, ReplyDisposition::Resolved) => "Agent reply".to_string(),
        (_, ReplyDisposition::Dismissed) => "Dismissed message".to_string(),
        (_, ReplyDisposition::Failed) => "Failed reply".to_string(),
    };
    item.blocks = text_blocks(cell.message_lines.clone());
}

fn apply_exec_cell(item: &mut WebActivityItem, cell: &ExecResultActivityCell) {
    item.kind = WebActivityKind::Tool;
    item.actor = Some(WebActivityActor::Tool);
    item.title = if cell.title.trim().is_empty() {
        "Tool result".to_string()
    } else {
        cell.title.clone()
    };
    let exit_code = cell.meta.as_deref().and_then(parse_exit_code);
    item.status = match exit_code {
        Some(0) | None => WebActivityStatus::Completed,
        Some(_) => WebActivityStatus::Failed,
    };
    item.tool = Some(WebActivityTool {
        name: "terminal".to_string(),
        app: Some("Terminal".to_string()),
        input_preview: Some(item.title.clone()),
        output_preview: compact_preview(&cell.output_lines),
        output_ref: None,
        duration_ms: None,
        exit_code,
        affected_files: Vec::new(),
    });
    item.blocks = output_blocks(&cell.output_lines);
    item.detail_blocks = kv_block(optional_kv_entries(vec![
        ("meta", cell.meta.clone()),
        ("command", Some(cell.title.clone())),
    ]));
}

fn apply_live_exec_cell(item: &mut WebActivityItem, cell: &LiveExecActivityCell) {
    item.kind = WebActivityKind::Tool;
    item.actor = Some(WebActivityActor::Tool);
    item.status = WebActivityStatus::Running;
    item.title = if cell.title.trim().is_empty() {
        "Tool running".to_string()
    } else {
        cell.title.clone()
    };
    let duration_ms = cell.started_at_ms.and_then(|started_at_ms| {
        let now = chrono::Utc::now().timestamp_millis();
        (now >= started_at_ms).then_some((now - started_at_ms) as u128)
    });
    item.created_at = cell.started_at_ms.unwrap_or(item.created_at);
    item.tool = Some(WebActivityTool {
        name: "terminal".to_string(),
        app: Some("Terminal".to_string()),
        input_preview: compact_preview(&cell.call_lines).or_else(|| Some(item.title.clone())),
        output_preview: compact_preview(&cell.output_lines),
        output_ref: None,
        duration_ms,
        exit_code: cell.meta.as_deref().and_then(parse_exit_code),
        affected_files: Vec::new(),
    });
    item.blocks = if cell.output_lines.is_empty() {
        text_blocks(vec!["running...".to_string()])
    } else {
        output_blocks(&cell.output_lines)
    };
    item.detail_blocks = kv_block(optional_kv_entries(vec![
        ("meta", cell.meta.clone()),
        ("input", compact_preview(&cell.call_lines)),
    ]));
}

fn apply_terminal_wait_cell(item: &mut WebActivityItem, cell: &TerminalWaitActivityCell) {
    item.kind = WebActivityKind::Tool;
    item.actor = Some(WebActivityActor::Tool);
    item.status = WebActivityStatus::Running;
    item.title = if cell.title.trim().is_empty() {
        "Terminal wait".to_string()
    } else {
        cell.title.clone()
    };
    item.tool = Some(WebActivityTool {
        name: "terminal".to_string(),
        app: Some("Terminal".to_string()),
        input_preview: Some(item.title.clone()),
        output_preview: compact_preview(&cell.body_lines),
        output_ref: None,
        duration_ms: None,
        exit_code: None,
        affected_files: Vec::new(),
    });
    item.blocks = text_blocks(cell.body_lines.clone());
}

fn apply_browser_cell(item: &mut WebActivityItem, cell: &BrowserActivityCell) {
    item.kind = WebActivityKind::Tool;
    item.actor = Some(WebActivityActor::Tool);
    item.title = if cell.title.trim().is_empty() {
        "Browser snapshot".to_string()
    } else {
        cell.title.clone()
    };
    item.tool = Some(WebActivityTool {
        name: "browser".to_string(),
        app: Some("Browser".to_string()),
        input_preview: cell.url.clone(),
        output_preview: Some(
            [
                cell.line_count.map(|value| format!("{value} lines")),
                cell.ref_count.map(|value| format!("{value} refs")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · "),
        )
        .filter(|value| !value.is_empty()),
        output_ref: cell.url.clone(),
        duration_ms: None,
        exit_code: None,
        affected_files: Vec::new(),
    });
    item.blocks = cell
        .url
        .as_ref()
        .map(|url| {
            vec![WebActivityBlock::Link {
                label: compact_url(url),
                url: url.clone(),
            }]
        })
        .unwrap_or_default();
    item.detail_blocks = kv_block(optional_kv_entries(vec![
        ("lines", cell.line_count.map(|value| value.to_string())),
        ("refs", cell.ref_count.map(|value| value.to_string())),
    ]));
}

fn apply_live_browser_cell(item: &mut WebActivityItem, cell: &LiveBrowserActivityCell) {
    item.kind = WebActivityKind::Tool;
    item.actor = Some(WebActivityActor::Tool);
    item.status = WebActivityStatus::Running;
    item.title = if cell.title.trim().is_empty() {
        "Browser action".to_string()
    } else {
        cell.title.clone()
    };
    item.tool = Some(WebActivityTool {
        name: "browser".to_string(),
        app: Some("Browser".to_string()),
        input_preview: cell.url.clone(),
        output_preview: compact_preview(&cell.body_lines),
        output_ref: cell.url.clone(),
        duration_ms: None,
        exit_code: None,
        affected_files: Vec::new(),
    });
    item.blocks = if let Some(url) = &cell.url {
        vec![WebActivityBlock::Link {
            label: compact_url(url),
            url: url.clone(),
        }]
    } else {
        text_blocks(cell.body_lines.clone())
    };
    item.detail_blocks = text_blocks(cell.body_lines.clone());
}

fn apply_generic_app_cell(item: &mut WebActivityItem, cell: &GenericAppActivityCell) {
    apply_simple_tool_item(
        item,
        "app",
        Some("App"),
        cell.title.clone(),
        cell.body_lines.clone(),
    );
}

fn apply_simple_tool_item(
    item: &mut WebActivityItem,
    tool_name: &str,
    app: Option<&str>,
    title: String,
    lines: Vec<String>,
) {
    item.kind = WebActivityKind::Tool;
    item.actor = Some(WebActivityActor::Tool);
    item.title = if title.trim().is_empty() {
        "Tool".to_string()
    } else {
        title
    };
    item.tool = Some(WebActivityTool {
        name: tool_name.to_string(),
        app: app.map(ToString::to_string),
        input_preview: Some(item.title.clone()),
        output_preview: compact_preview(&lines),
        output_ref: None,
        duration_ms: None,
        exit_code: None,
        affected_files: Vec::new(),
    });
    item.blocks = text_blocks(lines);
}

fn apply_plan_cell(item: &mut WebActivityItem, cell: &PlanActivityCell) {
    item.kind = WebActivityKind::Plan;
    item.actor = Some(WebActivityActor::System);
    item.title = "Plan".to_string();
    item.blocks = vec![WebActivityBlock::List {
        items: cell
            .steps
            .iter()
            .map(|step| format!("{} {}", plan_status_marker(step.status), step.text))
            .collect(),
    }];
    item.metadata = Some(serde_json::json!({
        "steps": cell.steps.iter().map(|step| serde_json::json!({
            "status": plan_status_name(step.status),
            "text": step.text.clone(),
        })).collect::<Vec<_>>()
    }));
}

fn apply_create_workflow_cell(item: &mut WebActivityItem, cell: &CreateWorkflowActivityCell) {
    item.kind = WebActivityKind::Workflow;
    item.actor = Some(WebActivityActor::System);
    item.title = format!("Created Workflow: {}", cell.workflow_id);
    item.blocks = kv_block(vec![WebActivityKvEntry {
        key: "workflow_id".to_string(),
        value: cell.workflow_id.clone(),
    }]);
}

fn apply_activate_workflow_cell(item: &mut WebActivityItem, cell: &ActivateWorkflowActivityCell) {
    item.kind = WebActivityKind::Workflow;
    item.actor = Some(WebActivityActor::System);
    item.title = format!("Activated Workflow: {}", cell.workflow_id);
    item.blocks = kv_block(vec![WebActivityKvEntry {
        key: "workflow_id".to_string(),
        value: cell.workflow_id.clone(),
    }]);
}

fn apply_deep_recall_cell(item: &mut WebActivityItem, cell: &DeepRecallActivityCell) {
    item.kind = WebActivityKind::Memory;
    item.actor = Some(WebActivityActor::System);
    item.title = format!("Recalled {} Memories", cell.memory_count);
    item.blocks = kv_block(vec![WebActivityKvEntry {
        key: "memory_count".to_string(),
        value: cell.memory_count.to_string(),
    }]);
}

fn apply_patch_cell(item: &mut WebActivityItem, cell: &PatchActivityCell) {
    item.kind = WebActivityKind::Patch;
    item.actor = Some(WebActivityActor::Tool);
    item.title = if cell.summary_line.trim().is_empty() {
        "Patch".to_string()
    } else {
        cell.summary_line.clone()
    };
    let affected_files = cell
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    item.tool = Some(WebActivityTool {
        name: "apply_patch".to_string(),
        app: Some("Workspace".to_string()),
        input_preview: Some(item.title.clone()),
        output_preview: Some(format_patch_summary(&cell.files)),
        output_ref: None,
        duration_ms: None,
        exit_code: None,
        affected_files,
    });
    item.blocks = vec![WebActivityBlock::Diff {
        files: cell
            .files
            .iter()
            .map(web_diff_file_from_patch_file)
            .collect(),
    }];
}

fn apply_error_cell(item: &mut WebActivityItem, cell: &ErrorActivityCell) {
    item.kind = WebActivityKind::Error;
    item.actor = Some(WebActivityActor::Tool);
    item.status = WebActivityStatus::Failed;
    item.title = if cell.title.trim().is_empty() {
        "Error".to_string()
    } else {
        cell.title.clone()
    };
    item.error = Some(WebActivityError {
        message: item.title.clone(),
        details: cell.body_lines.clone(),
    });
    item.blocks = text_blocks(primary_lines(&cell.title, &cell.body_lines));
}

fn web_diff_file_from_patch_file(file: &PatchFileUiData) -> WebActivityDiffFile {
    WebActivityDiffFile {
        path: file.path.clone(),
        operation: match &file.operation {
            PatchFileOperation::Add => "add",
            PatchFileOperation::Delete => "delete",
            PatchFileOperation::Update => "update",
        }
        .to_string(),
        added_lines: file.added_lines,
        removed_lines: file.removed_lines,
        lines: file
            .diff_lines
            .iter()
            .map(|line| WebActivityDiffLine {
                kind: match &line.kind {
                    PatchDiffLineKind::Context => "context",
                    PatchDiffLineKind::Delete => "delete",
                    PatchDiffLineKind::Add => "add",
                    PatchDiffLineKind::HunkBreak => "hunk_break",
                }
                .to_string(),
                old_lineno: line.old_lineno,
                new_lineno: line.new_lineno,
                text: line.text.clone(),
            })
            .collect(),
    }
}

fn preserve_item_timestamps(
    item: &mut WebActivityItem,
    previous_items: &HashMap<String, WebActivityItem>,
) {
    let Some(previous) = previous_items.get(&item.id) else {
        return;
    };

    item.created_at = previous.created_at;
    let mut comparable = item.clone();
    comparable.updated_at = previous.updated_at;
    if comparable == *previous {
        item.updated_at = previous.updated_at;
    }
}

fn text_blocks(lines: Vec<String>) -> Vec<WebActivityBlock> {
    let text = lines
        .into_iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        Vec::new()
    } else {
        vec![WebActivityBlock::Text { text }]
    }
}

fn output_blocks(lines: &[String]) -> Vec<WebActivityBlock> {
    let output = lines
        .iter()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if output.trim().is_empty() {
        text_blocks(vec!["(no output)".to_string()])
    } else {
        vec![WebActivityBlock::Code {
            code: output,
            language: None,
        }]
    }
}

fn kv_block(entries: Vec<WebActivityKvEntry>) -> Vec<WebActivityBlock> {
    if entries.is_empty() {
        Vec::new()
    } else {
        vec![WebActivityBlock::Kv { entries }]
    }
}

fn optional_kv_entries(entries: Vec<(&str, Option<String>)>) -> Vec<WebActivityKvEntry> {
    entries
        .into_iter()
        .filter_map(|(key, value)| {
            let value = value?;
            if value.trim().is_empty() {
                None
            } else {
                Some(WebActivityKvEntry {
                    key: key.to_string(),
                    value,
                })
            }
        })
        .collect()
}

fn primary_lines(title: &str, body_lines: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    if !title.trim().is_empty() {
        lines.push(title.to_string());
    }
    lines.extend(body_lines.iter().cloned());
    lines
}

fn compact_preview(lines: &[String]) -> Option<String> {
    let preview = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" · ");
    if preview.is_empty() {
        None
    } else if preview.chars().count() > 240 {
        Some(preview.chars().take(240).collect::<String>() + "...")
    } else {
        Some(preview)
    }
}

fn compact_url(url: &str) -> String {
    let compact = url.trim().replace('\n', "");
    if compact.chars().count() > 88 {
        compact.chars().take(88).collect::<String>() + "..."
    } else {
        compact
    }
}

fn parse_exit_code(meta: &str) -> Option<i32> {
    let normalized = meta.replace("exit=", " exit=");
    normalized
        .split_whitespace()
        .find_map(|part| part.strip_prefix("exit="))?
        .parse::<i32>()
        .ok()
}

fn plan_status_marker(status: PlanStepDisplayStatus) -> &'static str {
    match status {
        PlanStepDisplayStatus::Pending => "○",
        PlanStepDisplayStatus::InProgress => "●",
        PlanStepDisplayStatus::Completed => "✓",
    }
}

fn plan_status_name(status: PlanStepDisplayStatus) -> &'static str {
    match status {
        PlanStepDisplayStatus::Pending => "pending",
        PlanStepDisplayStatus::InProgress => "in_progress",
        PlanStepDisplayStatus::Completed => "completed",
    }
}

fn format_patch_summary(files: &[PatchFileUiData]) -> String {
    let added = files.iter().map(|file| file.added_lines).sum::<usize>();
    let removed = files.iter().map(|file| file.removed_lines).sum::<usize>();
    let file_count = files.len();
    format!("{file_count} file(s), +{added} -{removed}")
}
