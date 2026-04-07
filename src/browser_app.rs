use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use async_trait::async_trait;
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use viewpoint_core::{AriaSnapshot, Browser, BrowserContext, DocumentLoadState, Page};

use crate::{
    app::{
        App, AppHowToUse, AppId, AppSkillContent, AppSkillSummary, AppStateRender, AppToolScope,
        AppUsage,
    },
    spinova_paths::spinova_paths_sync,
};

const BROWSER_USAGE_PURPOSE: &str =
    "Browser 是网页查看与交互界面，用于在需要主动浏览和探索网页时承载注意力。";
const BROWSER_WHEN_TO_FOCUS: &[&str] = &[
    "需要主动浏览网页，而不是仅根据已注入的事件事实做判断时。",
    "需要阅读页面当前可见内容、进入候选链接或跨多个页面继续调查时。",
    "需要在网页会话中继续操作，例如提交搜索、填写控件、前进后退或刷新时。",
];
const BROWSER_HOW_TO_USE_LINES: &[&str] = &[
    "Browser 只通过 browser tools 操作；不要假设自己可以直接读取原始 HTML 或像人类一样机械导航。",
    "先用 `browser_open_page` 打开新页面，或在已知 `page_id` 上继续工作。",
    "如果页面可能仍在加载，先调用 `browser_wait`；如果要理解当前页面内容或拿到可交互元素引用，使用 `browser_snapshot` 读取紧凑语义快照。",
    "一切页面交互都必须显式提供 `page_id + element_ref`；不要猜测页面结构。",
    "输入框、搜索框、筛选器等可填写控件都属于基础 Browser 操作；先阅读页面快照拿到 `element_ref`，再用 `browser_fill`。",
    "搜索结果页通常只是定位线索，不应默认把摘要当作最终事实；应优先进入候选目标页继续确认。",
    "如果元素引用因页面变化而失效，Browser tool 会直接报错；此时应重新读取页面，而不是盲目重试旧引用。",
    "不再需要的页面应调用 `browser_close_page` 关闭，避免标签页长期堆积并浪费内存。",
    "不要因为第一页主要是导航或页头就立刻宣告失败；应先等待并完整阅读当前页面的语义快照，再决定是否无法完成。",
    "如果已经查到标题、摘要或正文片段，应基于已确认内容回答并明确范围；只有在关键内容确实不可得时才收尾为失败。",
    "Browser 只使用 Spinova 自带的独立浏览器 runtime，不会复用用户日常浏览器 profile；如果 runtime 未安装，浏览器工具会直接报错。",
];
const BROWSER_SKILL_DEEP_RESEARCH_ID: &str = "browser.deep_research";
const BROWSER_SKILL_SOURCE_VERIFICATION_ID: &str = "browser.source_verification";
const BROWSER_SKILL_ARTICLE_READING_ID: &str = "browser.article_reading";
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
        let paths = spinova_paths_sync();
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

    fn render_backend_status(&self) -> &'static str {
        if self.context.is_some() {
            "ready"
        } else if self.init_error.is_some() {
            "error"
        } else {
            "not_initialized"
        }
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

#[async_trait]
impl App for BrowserApp {
    fn id(&self) -> AppId {
        AppId::Browser
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn render_state(&self) -> AppStateRender {
        let mut lines = vec![
            "kind=browser".to_string(),
            format!("backend_status={}", self.render_backend_status()),
            format!("page_count={}", self.pages.len()),
        ];
        if self.pages.is_empty() {
            lines.push("page_ids=none".to_string());
        } else {
            let page_ids = self.pages.keys().cloned().collect::<Vec<_>>().join(", ");
            lines.push(format!("page_ids={page_ids}"));
            for page in self.pages.values() {
                lines.push(format!(
                    "page={} title={} url={}",
                    page.page_id, page.title, page.url
                ));
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
            purpose: BROWSER_USAGE_PURPOSE.to_string(),
            when_to_focus: BROWSER_WHEN_TO_FOCUS
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
        }
    }

    fn how_to_use(&self) -> AppHowToUse {
        AppHowToUse {
            lines: BROWSER_HOW_TO_USE_LINES
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
        }
    }

    fn skills(&self) -> Vec<AppSkillSummary> {
        vec![
            AppSkillSummary {
                id: BROWSER_SKILL_DEEP_RESEARCH_ID.to_string(),
                name: "深度调查".to_string(),
                when_to_use: vec![
                    "需要跨多个页面和来源逐步查证，而不是停在单个搜索结果或单篇网页时。"
                        .to_string(),
                    "任务要求你综合搜索、阅读、交叉比对、逐步收敛结论时。".to_string(),
                ],
            },
            AppSkillSummary {
                id: BROWSER_SKILL_SOURCE_VERIFICATION_ID.to_string(),
                name: "来源查证".to_string(),
                when_to_use: vec![
                    "需要确认某条说法是否真的出现在网页来源中时。".to_string(),
                    "需要避免只凭搜索结果摘要就下结论时。".to_string(),
                ],
            },
            AppSkillSummary {
                id: BROWSER_SKILL_ARTICLE_READING_ID.to_string(),
                name: "长文阅读与提炼".to_string(),
                when_to_use: vec![
                    "需要阅读文章、报告、博客或新闻，并从中提炼主要论点、事实和出处时。"
                        .to_string(),
                    "任务重点是读懂一个较长网页并做总结，而不是做跨来源交叉查证时。".to_string(),
                ],
            },
        ]
    }

    fn read_skill(&self, id: &str) -> Result<AppSkillContent> {
        let (title, body) = match id {
            BROWSER_SKILL_DEEP_RESEARCH_ID => (
                "Browser Skill: 深度调查",
                "适用时机：当你需要跨多个网页逐步调查、交叉验证并收敛结论时。\n\n做法：\n1. 先打开一个起始页或搜索入口，但不要把第一页当成终点。\n2. 通过 `browser_snapshot` 识别关键链接、候选来源和下一步应进入的页面。\n3. 在多个页面之间建立调查链：进入来源页、阅读、返回、继续打开新候选。\n4. 只在已读过足够多的目标页、并完成必要交叉验证后再总结；不要把搜索结果摘要或单一页的站点简介当成调查结论。",
            ),
            BROWSER_SKILL_SOURCE_VERIFICATION_ID => (
                "Browser Skill: 来源查证",
                "适用时机：当用户要你确认某条事实、说法或网页内容是否成立时。\n\n做法：\n1. 先打开候选来源。\n2. 用 `browser_snapshot` 完整阅读页面语义结构，直接定位相关段落、标题、链接或控件引用。\n3. 不要只凭搜索结果摘要或零散片段下结论；要结合页面整体上下文确认信息位置。\n4. 只有在目标页上拿到足够明确的内容后，才可向用户下结论；搜索结果页摘要通常只算线索，不算最终查证。",
            ),
            BROWSER_SKILL_ARTICLE_READING_ID => (
                "Browser Skill: 长文阅读与提炼",
                "适用时机：当你需要阅读单篇文章、报告、博客或新闻并总结其内容时。\n\n做法：\n1. 先 `browser_wait`，避免在页头、导航或未稳定状态下误判内容缺失。\n2. 使用 `browser_snapshot` 阅读整页语义快照，识别标题、正文段落、引用和相关链接。\n3. 如果已经拿到标题、摘要或正文片段，应基于已确认内容总结，并明确哪些部分已经确认、哪些只是部分可见。\n4. 如果当前页仍不足以支持结论，再继续进入页内相关链接或返回上游来源；不要因为刚看到导航块就立刻失败。",
            ),
            _ => return Err(miette!("unknown Browser skill `{id}`")),
        };

        Ok(AppSkillContent {
            id: id.to_string(),
            title: title.to_string(),
            body: body.to_string(),
        })
    }

    fn focused_tool_scopes(&self) -> &'static [AppToolScope] {
        BROWSER_TOOL_SCOPES
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
