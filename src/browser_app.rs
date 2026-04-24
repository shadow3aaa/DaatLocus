use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use async_trait::async_trait;
use miette::{Result, miette};
use schemars::schema_for;
use serde::{Deserialize, Serialize};
use serde_json::json;
use viewpoint_core::{AriaSnapshot, Browser, BrowserContext, DocumentLoadState, Page};

use crate::{
    app::{
        App, AppHowToUse, AppId, AppStateRender, AppToolExecutionContext, AppToolExecutionResult,
        AppToolScope, AppToolSpec, AppUsage,
    },
    context_budget::truncate_text_to_token_budget,
    core::{
        BrowserBackArgs, BrowserClickArgs, BrowserClosePageArgs, BrowserFillArgs,
        BrowserForwardArgs, BrowserOpenArgs, BrowserReloadArgs, BrowserSnapshotArgs,
        BrowserWaitArgs,
    },
    daat_locus_paths::daat_locus_paths_sync,
    reasoning::{episode::EpisodeActionRecord, runtime::AgentToolCall},
    tool_ui::{BrowserUiAction, BrowserUiData, ToolCallUiEvent, ToolUiEvent},
};

const BROWSER_USAGE_PURPOSE: &str = "Browser is the web viewing and interaction surface for tasks that require active browsing and exploration.";
const BROWSER_WHEN_TO_FOCUS: &[&str] = &[
    "When active web browsing is needed instead of judging only from injected event facts.",
    "When current visible page content must be read, candidate links must be opened, or investigation must continue across pages.",
    "When a web session needs continued interaction such as submitting search, filling controls, going back or forward, or reloading.",
];
const BROWSER_HOW_TO_USE_LINES: &[&str] = &[
    "Operate Browser only through browser tools; do not assume raw HTML access or human-style mechanical navigation.",
    "Use `browser_open_page` to open a new page, or continue on a known `page_id`.",
    "If the page may still be loading, call `browser_wait`; to understand current page content or obtain interactable element refs, call `browser_snapshot` for a compact semantic snapshot.",
    "Every page interaction must explicitly provide `page_id + element_ref`; do not guess page structure.",
    "Fillable controls such as inputs, search boxes, and filters are basic Browser operations. Read the page snapshot to obtain `element_ref`, then use `browser_fill`.",
    "Search result pages are usually leads, not final evidence; prefer opening candidate target pages to confirm.",
    "If an element ref becomes stale after page changes, Browser tools fail directly; read the page again instead of blindly retrying the old ref.",
    "Close pages that are no longer needed with `browser_close_page` to avoid tab buildup and memory waste.",
    "Do not declare failure just because the first page is mostly navigation or a header; wait and read the semantic snapshot before deciding it cannot be completed.",
    "If a title, summary, or body snippet has been confirmed, answer from the confirmed content and state the scope; fail only when key content is truly unavailable.",
    "Browser uses Daat Locus's own isolated browser runtime and never reuses the user's everyday browser profile. Browser tools fail directly if the runtime is not installed.",
];
const BROWSER_TOOL_SCOPES: &[AppToolScope] = &[AppToolScope::Browser];
const BROWSER_SNAPSHOT_MAX_DEPTH: usize = 6;
pub struct BrowserApp {
    browser: Option<Browser>,
    context: Option<BrowserContext>,
    pages: BTreeMap<String, BrowserPageState>,
    init_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserPageState {
    pub page_id: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserOpenResult {
    pub page: BrowserPageState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserSnapshotResult {
    pub page: BrowserPageState,
    pub snapshot: String,
    pub line_count: usize,
    pub ref_count: usize,
    pub interactive_ref_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserActionResult {
    pub page: BrowserPageState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserWaitResult {
    pub page: BrowserPageState,
    pub wait_state: String,
}

#[derive(Debug, Clone, Copy, Default)]
struct CompactSnapshotStats {
    line_count: usize,
    ref_count: usize,
    interactive_ref_count: usize,
}

#[derive(Debug, Clone)]
struct RenderedSnapshotLines {
    lines: Vec<String>,
    relevant: bool,
}

impl BrowserApp {
    pub fn new() -> Self {
        Self {
            browser: None,
            context: None,
            pages: BTreeMap::new(),
            init_error: None,
        }
    }

    async fn ensure_ready(&mut self) -> Result<()> {
        if self.context.is_some() {
            return Ok(());
        }
        let paths = daat_locus_paths_sync();
        let executable = paths.browser_executable_path();
        if !executable.exists() {
            let reason = format!(
                "browser runtime is not installed: expected Chromium binary at {}",
                executable.display()
            );
            self.init_error = Some(reason.clone());
            return Err(miette!(reason));
        }
        let browser = Browser::launch()
            .executable_path(executable)
            .headless(true)
            .launch()
            .await
            .map_err(|err| {
                let reason = format!("failed to launch browser backend: {err}");
                self.init_error = Some(reason.clone());
                miette!(reason)
            })?;
        let context = browser.new_context().await.map_err(|err| {
            let reason = format!("failed to create browser context: {err}");
            self.init_error = Some(reason.clone());
            miette!(reason)
        })?;
        self.browser = Some(browser);
        self.context = Some(context);
        self.init_error = None;
        self.pages.clear();
        Ok(())
    }

    fn context_ref(&self) -> Result<&BrowserContext> {
        self.context
            .as_ref()
            .ok_or_else(|| miette!("browser backend is not ready"))
    }

    async fn find_page(&mut self, page_id: &str) -> Result<Page> {
        self.ensure_ready().await?;
        let pages = self
            .context_ref()?
            .pages()
            .await
            .map_err(|err| miette!("failed to list browser pages: {err}"))?;
        pages
            .into_iter()
            .find(|page| page.target_id() == page_id)
            .ok_or_else(|| miette!("unknown browser page `{page_id}`"))
    }

    async fn capture_page_state(&self, page: &Page) -> BrowserPageState {
        let title = page
            .title()
            .await
            .unwrap_or_else(|_| "(untitled)".to_string());
        let url = page
            .url()
            .await
            .unwrap_or_else(|_| "about:blank".to_string());
        BrowserPageState {
            page_id: page.target_id().to_string(),
            title: summarize_state_text(&title),
            url: summarize_state_text(&url),
        }
    }

    async fn replace_page_state(&mut self, page: &Page) -> BrowserPageState {
        let state = self.capture_page_state(page).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        state
    }

    fn normalize_wait_state(state: Option<&str>) -> Result<(&'static str, &'static str)> {
        match state
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("load") | Some("complete") => {
                Ok(("load", "() => document.readyState === 'complete'"))
            }
            Some("dom") | Some("interactive") | Some("domcontentloaded") => {
                Ok(("dom", "() => document.readyState !== 'loading'"))
            }
            Some(other) => Err(miette!(
                "unsupported browser wait state `{other}`; use `dom` or `load`"
            )),
        }
    }

    async fn refresh_pages(&mut self) -> Result<()> {
        if self.context.is_none() {
            return Ok(());
        }
        let pages = self
            .context_ref()?
            .pages()
            .await
            .map_err(|err| miette!("failed to list browser pages: {err}"))?;
        self.pages.clear();
        let mut updated = BTreeMap::new();
        for page in pages {
            let state = self.capture_page_state(&page).await;
            updated.insert(state.page_id.clone(), state);
        }
        self.pages = updated;
        Ok(())
    }

    pub async fn open_page(&mut self, url: &str) -> Result<BrowserOpenResult> {
        self.ensure_ready().await?;
        let page = self
            .context_ref()?
            .new_page()
            .await
            .map_err(|err| miette!("failed to create browser page: {err}"))?;
        page.goto(url)
            .wait_until(DocumentLoadState::Load)
            .goto()
            .await
            .map_err(|err| miette!("failed to open `{url}`: {err}"))?;
        let state = self.capture_page_state(&page).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        self.refresh_pages().await?;
        let page = self
            .pages
            .get(&state.page_id)
            .cloned()
            .ok_or_else(|| miette!("opened browser page disappeared"))?;
        Ok(BrowserOpenResult { page })
    }

    pub async fn snapshot_page(&mut self, page_id: &str) -> Result<BrowserSnapshotResult> {
        let page = self.find_page(page_id).await?;
        let snapshot = page
            .aria_snapshot()
            .await
            .map_err(|err| miette!("failed to inspect page `{page_id}`: {err}"))?;
        let state = self.replace_page_state(&page).await;
        let (snapshot, stats) = compact_browser_snapshot(&snapshot);
        Ok(BrowserSnapshotResult {
            page: state,
            snapshot,
            line_count: stats.line_count,
            ref_count: stats.ref_count,
            interactive_ref_count: stats.interactive_ref_count,
        })
    }

    pub async fn wait_for_page(
        &mut self,
        page_id: &str,
        state: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> Result<BrowserWaitResult> {
        let page = self.find_page(page_id).await?;
        let (wait_state, expression) = Self::normalize_wait_state(state)?;
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(15_000).max(1));
        page.wait_for_function(expression)
            .timeout(timeout)
            .wait()
            .await
            .map_err(|err| {
                miette!("failed to wait for `{wait_state}` on page `{page_id}`: {err}")
            })?;
        let state = self.replace_page_state(&page).await;
        Ok(BrowserWaitResult {
            page: state,
            wait_state: wait_state.to_string(),
        })
    }

    pub async fn click(&mut self, page_id: &str, element_ref: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        page.locator_from_ref(element_ref)
            .click()
            .await
            .map_err(|err| miette!("failed to click `{element_ref}` on page `{page_id}`: {err}"))?;
        let state = self.capture_page_state(&page).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult { page: state })
    }

    pub async fn fill(
        &mut self,
        page_id: &str,
        element_ref: &str,
        value: &str,
    ) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        page.locator_from_ref(element_ref)
            .fill(value)
            .await
            .map_err(|err| miette!("failed to fill `{element_ref}` on page `{page_id}`: {err}"))?;
        let state = self.capture_page_state(&page).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult { page: state })
    }

    pub async fn go_back(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        page.go_back()
            .await
            .map_err(|err| miette!("failed to go back on page `{page_id}`: {err}"))?;
        let state = self.capture_page_state(&page).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult { page: state })
    }

    pub async fn go_forward(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        page.go_forward()
            .await
            .map_err(|err| miette!("failed to go forward on page `{page_id}`: {err}"))?;
        let state = self.capture_page_state(&page).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult { page: state })
    }

    pub async fn reload(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        page.reload()
            .await
            .map_err(|err| miette!("failed to reload page `{page_id}`: {err}"))?;
        let state = self.capture_page_state(&page).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult { page: state })
    }

    pub async fn close_page(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let mut page = self.find_page(page_id).await?;
        let state = self
            .pages
            .get(page_id)
            .cloned()
            .ok_or_else(|| miette!("unknown browser page `{page_id}`"))?;
        page.close()
            .await
            .map_err(|err| miette!("failed to close page `{page_id}`: {err}"))?;
        self.pages.remove(page_id);
        self.refresh_pages().await?;
        Ok(BrowserActionResult { page: state })
    }
}

fn is_interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "checkbox"
            | "combobox"
            | "link"
            | "listbox"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "option"
            | "radio"
            | "searchbox"
            | "slider"
            | "spinbutton"
            | "switch"
            | "tab"
            | "textbox"
            | "treeitem"
    )
}

fn is_content_role(role: &str) -> bool {
    matches!(
        role,
        "article"
            | "banner"
            | "cell"
            | "columnheader"
            | "complementary"
            | "form"
            | "gridcell"
            | "heading"
            | "img"
            | "listitem"
            | "main"
            | "navigation"
            | "paragraph"
            | "region"
            | "rowheader"
            | "search"
            | "status"
            | "strong"
    )
}

fn is_structural_role(role: &str) -> bool {
    matches!(
        role,
        "application"
            | "directory"
            | "document"
            | "generic"
            | "grid"
            | "group"
            | "ignored"
            | "list"
            | "menu"
            | "menubar"
            | "none"
            | "presentation"
            | "row"
            | "rowgroup"
            | "table"
            | "tablist"
            | "toolbar"
            | "tree"
            | "treegrid"
    )
}

fn normalize_snapshot_role(node: &AriaSnapshot) -> String {
    node.role
        .as_deref()
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(|role| role.to_ascii_lowercase())
        .unwrap_or_else(|| "generic".to_string())
}

fn normalized_snapshot_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.replace('\n', " "))
}

fn compact_snapshot_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn escape_snapshot_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn should_create_snapshot_ref(role: &str, name: Option<&str>) -> bool {
    is_interactive_role(role) || (is_content_role(role) && name.is_some())
}

fn should_include_snapshot_node(role: &str, name: Option<&str>) -> bool {
    !(is_structural_role(role) && name.is_none())
}

fn collect_snapshot_ref_duplicate_counts(node: &AriaSnapshot, counts: &mut HashMap<String, usize>) {
    let role = normalize_snapshot_role(node);
    let name = normalized_snapshot_text(node.name.as_deref());
    if node.node_ref.is_some() && should_create_snapshot_ref(&role, name.as_deref()) {
        let key = format!("{role}:{}", name.as_deref().unwrap_or(""));
        *counts.entry(key).or_insert(0) += 1;
    }
    for child in &node.children {
        collect_snapshot_ref_duplicate_counts(child, counts);
    }
}

fn build_snapshot_line(
    node: &AriaSnapshot,
    role: &str,
    name: Option<&str>,
    depth: usize,
    duplicate_counts: &HashMap<String, usize>,
    duplicate_seen: &mut HashMap<String, usize>,
    stats: &mut CompactSnapshotStats,
) -> String {
    let indent = "  ".repeat(depth);
    let mut line = format!("{indent}- {role}");
    if let Some(name) = name {
        line.push_str(&format!(
            " \"{}\"",
            escape_snapshot_value(&compact_snapshot_text(name, 160))
        ));
    }

    if let Some(node_ref) = node.node_ref.as_deref() {
        if should_create_snapshot_ref(role, name) {
            let key = format!("{role}:{}", name.unwrap_or(""));
            let nth = duplicate_seen.entry(key.clone()).or_insert(0);
            let current_nth = *nth;
            *nth += 1;
            line.push_str(&format!(" [ref={node_ref}]"));
            if duplicate_counts.get(&key).copied().unwrap_or(0) > 1 && current_nth > 0 {
                line.push_str(&format!(" [nth={current_nth}]"));
            }
            stats.ref_count += 1;
            if is_interactive_role(role) {
                stats.interactive_ref_count += 1;
            }
        }
    }

    if let Some(value_text) = normalized_snapshot_text(node.value_text.as_deref()) {
        line.push_str(&format!(
            " value=\"{}\"",
            escape_snapshot_value(&compact_snapshot_text(&value_text, 120))
        ));
    } else if let Some(value_now) = node.value_now {
        line.push_str(&format!(" value={value_now}"));
    }

    if let Some(description) = normalized_snapshot_text(node.description.as_deref()) {
        line.push_str(&format!(
            " description=\"{}\"",
            escape_snapshot_value(&compact_snapshot_text(&description, 120))
        ));
    }

    if let Some(level) = node.level {
        line.push_str(&format!(" level={level}"));
    }
    if node.disabled == Some(true) {
        line.push_str(" disabled");
    }
    if node.expanded == Some(true) {
        line.push_str(" expanded");
    }
    if node.selected == Some(true) {
        line.push_str(" selected");
    }
    if let Some(checked) = node.checked {
        line.push_str(&format!(" checked={checked:?}"));
    }
    if node.pressed == Some(true) {
        line.push_str(" pressed");
    }
    if node.is_frame == Some(true) {
        line.push_str(" frame_boundary");
    }

    stats.line_count += 1;
    line
}

fn render_compact_snapshot_lines(
    node: &AriaSnapshot,
    depth: usize,
    duplicate_counts: &HashMap<String, usize>,
    duplicate_seen: &mut HashMap<String, usize>,
    stats: &mut CompactSnapshotStats,
) -> RenderedSnapshotLines {
    if depth > BROWSER_SNAPSHOT_MAX_DEPTH {
        return RenderedSnapshotLines {
            lines: Vec::new(),
            relevant: false,
        };
    }

    let role = normalize_snapshot_role(node);
    let name = normalized_snapshot_text(node.name.as_deref());

    let mut child_lines = Vec::new();
    let mut child_relevant = false;
    for child in &node.children {
        let rendered = render_compact_snapshot_lines(
            child,
            depth + 1,
            duplicate_counts,
            duplicate_seen,
            stats,
        );
        if rendered.relevant {
            child_relevant = true;
            child_lines.extend(rendered.lines);
        }
    }

    let has_ref = node.node_ref.is_some() && should_create_snapshot_ref(&role, name.as_deref());
    let has_meaningful_text = name.is_some()
        || node
            .value_text
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || node
            .description
            .as_deref()
            .is_some_and(|description| !description.trim().is_empty());
    let include_node = should_include_snapshot_node(&role, name.as_deref());
    let include_line = include_node
        && (!is_structural_role(&role) || has_ref || has_meaningful_text || child_relevant);

    let mut lines = Vec::new();
    if include_line {
        lines.push(build_snapshot_line(
            node,
            &role,
            name.as_deref(),
            depth,
            duplicate_counts,
            duplicate_seen,
            stats,
        ));
    }
    lines.extend(child_lines);

    RenderedSnapshotLines {
        relevant: include_line || child_relevant,
        lines,
    }
}

fn compact_browser_snapshot(snapshot: &AriaSnapshot) -> (String, CompactSnapshotStats) {
    let snapshot_root = preferred_snapshot_root(snapshot).unwrap_or(snapshot);
    let mut duplicate_counts = HashMap::new();
    collect_snapshot_ref_duplicate_counts(snapshot_root, &mut duplicate_counts);

    let mut duplicate_seen = HashMap::new();
    let mut stats = CompactSnapshotStats::default();
    let rendered = render_compact_snapshot_lines(
        snapshot_root,
        0,
        &duplicate_counts,
        &mut duplicate_seen,
        &mut stats,
    );

    let snapshot = if rendered.lines.is_empty() {
        "- generic".to_string()
    } else {
        rendered.lines.join("\n")
    };
    (snapshot, stats)
}

fn preferred_snapshot_root(snapshot: &AriaSnapshot) -> Option<&AriaSnapshot> {
    let mut best_main = None;
    let mut best_article = None;
    collect_preferred_snapshot_root(snapshot, &mut best_main, &mut best_article);
    best_main.or(best_article)
}

fn collect_preferred_snapshot_root<'a>(
    node: &'a AriaSnapshot,
    best_main: &mut Option<&'a AriaSnapshot>,
    best_article: &mut Option<&'a AriaSnapshot>,
) {
    let role = normalize_snapshot_role(node);
    if role == "main" && best_main.is_none() {
        *best_main = Some(node);
    } else if role == "article" && best_article.is_none() {
        *best_article = Some(node);
    }

    if best_main.is_some() && best_article.is_some() {
        return;
    }

    for child in &node.children {
        collect_preferred_snapshot_root(child, best_main, best_article);
        if best_main.is_some() && best_article.is_some() {
            break;
        }
    }
}

fn summarize_state_text(text: &str) -> String {
    const MAX_CHARS: usize = 160;
    let compact = text.trim().replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

fn parse_browser_tool_args<T: for<'de> Deserialize<'de>>(call: &AgentToolCall) -> Result<T> {
    serde_json::from_value(call.arguments.clone()).map_err(|err| {
        miette!(
            "invalid arguments for browser tool `{}`: {}; arguments={}",
            call.name,
            err,
            call.arguments
        )
    })
}

fn browser_page_meta(page: &BrowserPageState) -> String {
    format!(
        "page={} title={} url={}",
        page.page_id, page.title, page.url
    )
}

fn browser_action_model_content(
    summary: &str,
    page: &BrowserPageState,
    extra_lines: &[String],
    max_tokens: usize,
) -> String {
    let mut lines = vec![format!("summary={summary}"), browser_page_meta(page)];
    lines.extend(extra_lines.iter().cloned());
    truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
}

fn browser_snapshot_model_content(
    summary: &str,
    result: &BrowserSnapshotResult,
    max_tokens: usize,
) -> String {
    let mut lines = vec![
        format!("summary={summary}"),
        browser_page_meta(&result.page),
        format!(
            "snapshot_stats=lines={} refs={} interactive_refs={}",
            result.line_count, result.ref_count, result.interactive_ref_count
        ),
        "snapshot=".to_string(),
    ];
    lines.push(result.snapshot.clone());
    truncate_text_to_token_budget(&lines.join("\n"), max_tokens)
}

fn browser_action_result(
    action: BrowserUiAction,
    title: &str,
    result: &BrowserActionResult,
    extra_lines: Vec<String>,
    max_tokens: usize,
) -> AppToolExecutionResult {
    let model_content = browser_action_model_content(title, &result.page, &extra_lines, max_tokens);
    AppToolExecutionResult {
        summary: title.to_string(),
        payload: json!({ "page": result.page }),
        model_content: Some(model_content),
        ui_event: ToolUiEvent::Browser(BrowserUiData {
            action,
            title: title.to_string(),
            body_lines: {
                let mut lines = vec![format!("page={}", result.page.page_id)];
                lines.extend(extra_lines);
                lines
            },
            url: Some(result.page.url.clone()),
            line_count: None,
            ref_count: None,
        }),
        turn_boundary_reason: None,
    }
}

fn browser_wait_result(result: &BrowserWaitResult, max_tokens: usize) -> AppToolExecutionResult {
    let extra_lines = vec![format!("wait_state={}", result.wait_state)];
    let model_content = browser_action_model_content(
        "waited for browser page",
        &result.page,
        &extra_lines,
        max_tokens,
    );
    AppToolExecutionResult {
        summary: "waited for browser page".to_string(),
        payload: json!({
            "page": result.page,
            "wait_state": result.wait_state,
        }),
        model_content: Some(model_content),
        ui_event: ToolUiEvent::Browser(BrowserUiData {
            action: BrowserUiAction::Wait,
            title: "waited for browser page".to_string(),
            body_lines: {
                let mut lines = vec![format!("page={}", result.page.page_id)];
                lines.extend(extra_lines);
                lines
            },
            url: Some(result.page.url.clone()),
            line_count: None,
            ref_count: None,
        }),
        turn_boundary_reason: None,
    }
}

#[async_trait]
impl App for BrowserApp {
    fn id(&self) -> AppId {
        AppId::browser()
    }

    fn render_state(&self) -> AppStateRender {
        let mut lines = vec!["kind=browser".to_string()];
        if self.pages.is_empty() {
            lines.push("pages=none".to_string());
        } else {
            for page in self.pages.values() {
                lines.push(format!("title={} url={}", page.title, page.url));
            }
        }
        if let Some(err) = self.init_error.as_deref() {
            lines.push(format!("last_error={}", summarize_state_text(err)));
        }
        AppStateRender {
            title: "Browser".to_string(),
            lines,
        }
    }

    fn usage(&self) -> AppUsage {
        AppUsage {
            description: BROWSER_USAGE_PURPOSE.to_string(),
            when_to_focus: BROWSER_WHEN_TO_FOCUS
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            body_markdown: None,
        }
    }

    fn how_to_use(&self) -> AppHowToUse {
        AppHowToUse {
            lines: BROWSER_HOW_TO_USE_LINES
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
            body_markdown: None,
        }
    }

    fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        BROWSER_TOOL_SCOPES
    }

    fn tool_specs(&self) -> Result<Vec<AppToolSpec>> {
        Ok(vec![
            AppToolSpec {
                name: "browser_open_page".to_string(),
                description: "Create a browser page, open the specified URL, and return the new `page_id`.".to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserOpenArgs)).unwrap(),
            },
            AppToolSpec {
                name: "browser_snapshot".to_string(),
                description: "Read a compact semantic snapshot of the specified page, preserving high-value nodes and interactable element refs first."
                    .to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserSnapshotArgs)).unwrap(),
            },
            AppToolSpec {
                name: "browser_wait".to_string(),
                description: "Wait until the specified page reaches a stable state. `state` may be `dom` or `load`.".to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserWaitArgs)).unwrap(),
            },
            AppToolSpec {
                name: "browser_click".to_string(),
                description: "Click a page element by `element_ref`; if page changes made the ref stale, the tool fails directly.".to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserClickArgs)).unwrap(),
            },
            AppToolSpec {
                name: "browser_fill".to_string(),
                description: "Fill an input by `element_ref`; if page changes made the ref stale, the tool fails directly.".to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserFillArgs)).unwrap(),
            },
            AppToolSpec {
                name: "browser_back".to_string(),
                description: "Navigate the specified page backward.".to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserBackArgs)).unwrap(),
            },
            AppToolSpec {
                name: "browser_forward".to_string(),
                description: "Navigate the specified page forward.".to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserForwardArgs)).unwrap(),
            },
            AppToolSpec {
                name: "browser_reload".to_string(),
                description: "Reload the specified page.".to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserReloadArgs)).unwrap(),
            },
            AppToolSpec {
                name: "browser_close_page".to_string(),
                description: "Close the specified browser page. Close pages that are no longer needed to save memory.".to_string(),
                input_schema: serde_json::to_value(schema_for!(BrowserClosePageArgs)).unwrap(),
            },
        ])
    }

    fn summarize_tool_call(&self, call: &AgentToolCall) -> Result<EpisodeActionRecord> {
        match call.name.as_str() {
            "browser_open_page" => {
                let args: BrowserOpenArgs = parse_browser_tool_args(call)?;
                Ok(EpisodeActionRecord {
                    kind: call.name.clone(),
                    summary: format!("url={}", summarize_state_text(&args.url)),
                })
            }
            "browser_snapshot" => {
                let args: BrowserSnapshotArgs = parse_browser_tool_args(call)?;
                Ok(EpisodeActionRecord {
                    kind: call.name.clone(),
                    summary: format!("page={}", args.page_id),
                })
            }
            "browser_wait" => {
                let args: BrowserWaitArgs = parse_browser_tool_args(call)?;
                Ok(EpisodeActionRecord {
                    kind: call.name.clone(),
                    summary: format!(
                        "page={} state={}",
                        args.page_id,
                        args.state.as_deref().unwrap_or("load")
                    ),
                })
            }
            "browser_click" => {
                let args: BrowserClickArgs = parse_browser_tool_args(call)?;
                Ok(EpisodeActionRecord {
                    kind: call.name.clone(),
                    summary: format!("page={} ref={}", args.page_id, args.element_ref),
                })
            }
            "browser_fill" => {
                let args: BrowserFillArgs = parse_browser_tool_args(call)?;
                Ok(EpisodeActionRecord {
                    kind: call.name.clone(),
                    summary: format!(
                        "page={} ref={} value={}",
                        args.page_id,
                        args.element_ref,
                        summarize_state_text(&args.value)
                    ),
                })
            }
            "browser_back" | "browser_forward" | "browser_reload" | "browser_close_page" => {
                let args: BrowserBackArgs = parse_browser_tool_args(call)?;
                Ok(EpisodeActionRecord {
                    kind: call.name.clone(),
                    summary: format!("page={}", args.page_id),
                })
            }
            _ => Err(miette!("unknown browser tool `{}`", call.name)),
        }
    }

    fn render_tool_call_ui(&self, call: &AgentToolCall) -> Result<ToolCallUiEvent> {
        match call.name.as_str() {
            "browser_open_page" => {
                let args: BrowserOpenArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::OpenPage,
                    title: "browser_open_page".to_string(),
                    body_lines: vec![format!("url={}", summarize_state_text(&args.url))],
                    url: Some(args.url),
                    line_count: None,
                    ref_count: None,
                }))
            }
            "browser_snapshot" => {
                let args: BrowserSnapshotArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::Snapshot,
                    title: "browser_snapshot".to_string(),
                    body_lines: vec![format!("page={}", args.page_id)],
                    url: None,
                    line_count: None,
                    ref_count: None,
                }))
            }
            "browser_wait" => {
                let args: BrowserWaitArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::Wait,
                    title: "browser_wait".to_string(),
                    body_lines: vec![
                        format!("page={}", args.page_id),
                        format!("state={}", args.state.as_deref().unwrap_or("load")),
                    ],
                    url: None,
                    line_count: None,
                    ref_count: None,
                }))
            }
            "browser_click" => {
                let args: BrowserClickArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::Click,
                    title: "browser_click".to_string(),
                    body_lines: vec![
                        format!("page={}", args.page_id),
                        format!("ref={}", args.element_ref),
                    ],
                    url: None,
                    line_count: None,
                    ref_count: None,
                }))
            }
            "browser_fill" => {
                let args: BrowserFillArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::Fill,
                    title: "browser_fill".to_string(),
                    body_lines: vec![
                        format!("page={}", args.page_id),
                        format!("ref={}", args.element_ref),
                        format!("value={}", summarize_state_text(&args.value)),
                    ],
                    url: None,
                    line_count: None,
                    ref_count: None,
                }))
            }
            "browser_back" => {
                let args: BrowserBackArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::Back,
                    title: "browser_back".to_string(),
                    body_lines: vec![format!("page={}", args.page_id)],
                    url: None,
                    line_count: None,
                    ref_count: None,
                }))
            }
            "browser_forward" => {
                let args: BrowserForwardArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::Forward,
                    title: "browser_forward".to_string(),
                    body_lines: vec![format!("page={}", args.page_id)],
                    url: None,
                    line_count: None,
                    ref_count: None,
                }))
            }
            "browser_reload" => {
                let args: BrowserReloadArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::Reload,
                    title: "browser_reload".to_string(),
                    body_lines: vec![format!("page={}", args.page_id)],
                    url: None,
                    line_count: None,
                    ref_count: None,
                }))
            }
            "browser_close_page" => {
                let args: BrowserClosePageArgs = parse_browser_tool_args(call)?;
                Ok(ToolCallUiEvent::Browser(BrowserUiData {
                    action: BrowserUiAction::ClosePage,
                    title: "browser_close_page".to_string(),
                    body_lines: vec![format!("page={}", args.page_id)],
                    url: None,
                    line_count: None,
                    ref_count: None,
                }))
            }
            _ => Err(miette!("unknown browser tool `{}`", call.name)),
        }
    }

    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        match call.name.as_str() {
            "browser_open_page" => {
                let args: BrowserOpenArgs = parse_browser_tool_args(call)?;
                let result = self.open_page(&args.url).await?;
                let summary = format!("opened page {}", result.page.page_id);
                let model_content = browser_action_model_content(
                    &summary,
                    &result.page,
                    &Vec::new(),
                    context.tool_output_max_tokens,
                );
                Ok(AppToolExecutionResult {
                    summary,
                    payload: json!({ "page": result.page }),
                    model_content: Some(model_content),
                    ui_event: ToolUiEvent::Browser(BrowserUiData {
                        action: BrowserUiAction::OpenPage,
                        title: "opened browser page".to_string(),
                        body_lines: vec![
                            format!("page={}", result.page.page_id),
                            format!("url={}", result.page.url),
                        ],
                        url: Some(result.page.url.clone()),
                        line_count: None,
                        ref_count: None,
                    }),
                    turn_boundary_reason: None,
                })
            }
            "browser_snapshot" => {
                let args: BrowserSnapshotArgs = parse_browser_tool_args(call)?;
                let result = self.snapshot_page(&args.page_id).await?;
                let summary = format!("captured browser snapshot for page {}", result.page.page_id);
                let model_content = browser_snapshot_model_content(
                    &summary,
                    &result,
                    context.tool_output_max_tokens,
                );
                Ok(AppToolExecutionResult {
                    summary,
                    payload: json!({
                        "page": result.page,
                        "snapshot": result.snapshot,
                        "line_count": result.line_count,
                        "ref_count": result.ref_count,
                        "interactive_ref_count": result.interactive_ref_count,
                    }),
                    model_content: Some(model_content),
                    ui_event: ToolUiEvent::Browser(BrowserUiData {
                        action: BrowserUiAction::Snapshot,
                        title: "captured browser snapshot".to_string(),
                        body_lines: vec![
                            format!("page={}", result.page.page_id),
                            format!("lines={}", result.line_count),
                            format!("refs={}", result.ref_count),
                        ],
                        url: Some(result.page.url.clone()),
                        line_count: Some(result.line_count),
                        ref_count: Some(result.ref_count),
                    }),
                    turn_boundary_reason: None,
                })
            }
            "browser_wait" => {
                let args: BrowserWaitArgs = parse_browser_tool_args(call)?;
                let result = self
                    .wait_for_page(&args.page_id, args.state.as_deref(), args.timeout_ms)
                    .await?;
                Ok(browser_wait_result(&result, context.tool_output_max_tokens))
            }
            "browser_click" => {
                let args: BrowserClickArgs = parse_browser_tool_args(call)?;
                let result = self.click(&args.page_id, &args.element_ref).await?;
                Ok(browser_action_result(
                    BrowserUiAction::Click,
                    "clicked browser element",
                    &result,
                    vec![format!("ref={}", args.element_ref)],
                    context.tool_output_max_tokens,
                ))
            }
            "browser_fill" => {
                let args: BrowserFillArgs = parse_browser_tool_args(call)?;
                let result = self
                    .fill(&args.page_id, &args.element_ref, &args.value)
                    .await?;
                Ok(browser_action_result(
                    BrowserUiAction::Fill,
                    "filled browser element",
                    &result,
                    vec![
                        format!("ref={}", args.element_ref),
                        format!("value={}", summarize_state_text(&args.value)),
                    ],
                    context.tool_output_max_tokens,
                ))
            }
            "browser_back" => {
                let args: BrowserBackArgs = parse_browser_tool_args(call)?;
                let result = self.go_back(&args.page_id).await?;
                Ok(browser_action_result(
                    BrowserUiAction::Back,
                    "went back in browser",
                    &result,
                    Vec::new(),
                    context.tool_output_max_tokens,
                ))
            }
            "browser_forward" => {
                let args: BrowserForwardArgs = parse_browser_tool_args(call)?;
                let result = self.go_forward(&args.page_id).await?;
                Ok(browser_action_result(
                    BrowserUiAction::Forward,
                    "went forward in browser",
                    &result,
                    Vec::new(),
                    context.tool_output_max_tokens,
                ))
            }
            "browser_reload" => {
                let args: BrowserReloadArgs = parse_browser_tool_args(call)?;
                let result = self.reload(&args.page_id).await?;
                Ok(browser_action_result(
                    BrowserUiAction::Reload,
                    "reloaded browser page",
                    &result,
                    Vec::new(),
                    context.tool_output_max_tokens,
                ))
            }
            "browser_close_page" => {
                let args: BrowserClosePageArgs = parse_browser_tool_args(call)?;
                let result = self.close_page(&args.page_id).await?;
                Ok(browser_action_result(
                    BrowserUiAction::ClosePage,
                    "closed browser page",
                    &result,
                    Vec::new(),
                    context.tool_output_max_tokens,
                ))
            }
            _ => Err(miette!("unknown browser tool `{}`", call.name)),
        }
    }

    async fn on_focus(&mut self) -> Result<()> {
        self.ensure_ready().await
    }

    async fn shutdown(&mut self) -> Result<()> {
        if let Some(context) = self.context.as_mut() {
            let _ = context.close().await;
        }
        if let Some(browser) = self.browser.as_ref() {
            let _ = browser.close().await;
        }
        self.context = None;
        self.browser = None;
        Ok(())
    }
}
