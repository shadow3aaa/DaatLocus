use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use async_trait::async_trait;
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use viewpoint_core::{AriaSnapshot, Browser, BrowserContext, DocumentLoadState, Page};

use crate::{
    device::{Device, DeviceHowToUse, DeviceId, DeviceStateRender, DeviceToolScope, DeviceUsage},
    spinova_paths::spinova_paths_sync,
};

const BROWSER_USAGE_PURPOSE: &str =
    "Browser 是网页查看与交互界面，适合读取页面语义快照、点击元素、填写表单和跟踪多标签页。";
const BROWSER_WHEN_TO_FOCUS: &[&str] = &[
    "需要查看网页当前可见内容、文章、搜索结果或表单时。",
    "需要点击页面元素、填写输入框、前进后退或刷新页面时。",
    "需要在多个标签页之间保留网页会话，并基于最新页面快照继续操作时。",
];
const BROWSER_HOW_TO_USE_LINES: &[&str] = &[
    "Browser 只通过 browser tools 操作；不要假设自己可以直接读取原始 HTML 或像人类一样机械导航。",
    "先用 `browser_open` 打开新页面，或在已知 `page_id` 上继续工作。",
    "读取页面时先调用 `browser_snapshot` 获取最新语义快照。快照会返回 `snapshot_id` 和元素 `element_ref`。",
    "如果页面可能仍在加载，先调用 `browser_wait`；如果要定位正文或某个关键词，优先调用 `browser_find_in_page`。",
    "一切页面交互都必须显式提供 `page_id + snapshot_id + element_ref`；不要猜测页面结构，也不要复用旧快照。",
    "点击、填写、前进、后退、刷新之后，旧 `snapshot_id` 会失效；继续之前应重新调用 `browser_snapshot`。",
    "不再需要的页面应调用 `browser_close_page` 关闭，避免标签页长期堆积并浪费内存。",
    "不要因为第一页快照主要是导航或页头就立刻宣告失败；应先等待、查找正文或关键信息，再决定是否无法完成。",
    "如果已经查到标题、摘要或正文片段，应基于已确认内容回答并明确范围；只有在关键内容确实不可得时才收尾为失败。",
    "Browser 只使用 Spinova 自带的独立浏览器 runtime，不会复用用户日常浏览器 profile；如果 runtime 未安装，浏览器工具会直接报错。",
];
const BROWSER_TOOL_SCOPES: &[DeviceToolScope] = &[DeviceToolScope::Browser];
pub struct BrowserDevice {
    browser: Option<Browser>,
    context: Option<BrowserContext>,
    pages: BTreeMap<String, BrowserPageState>,
    next_snapshot_index: usize,
    init_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserPageState {
    pub page_id: String,
    pub title: String,
    pub url: String,
    pub last_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserOpenResult {
    pub page: BrowserPageState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserSnapshotResult {
    pub page: BrowserPageState,
    pub snapshot_id: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserFindMatch {
    pub element_ref: Option<String>,
    pub role: Option<String>,
    pub name: Option<String>,
    pub text_snippet: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserFindResult {
    pub page: BrowserPageState,
    pub query: String,
    pub matches: Vec<BrowserFindMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserActionResult {
    pub page: BrowserPageState,
    pub invalidated_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserWaitResult {
    pub page: BrowserPageState,
    pub wait_state: String,
    pub invalidated_snapshot_id: Option<String>,
}

impl BrowserDevice {
    pub fn new() -> Self {
        Self {
            browser: None,
            context: None,
            pages: BTreeMap::new(),
            next_snapshot_index: 1,
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

    fn next_snapshot_id(&mut self) -> String {
        let value = self.next_snapshot_index;
        self.next_snapshot_index += 1;
        format!("snapshot-{value}")
    }

    fn require_current_snapshot(&self, page_id: &str, snapshot_id: &str) -> Result<()> {
        let page = self
            .pages
            .get(page_id)
            .ok_or_else(|| miette!("unknown browser page `{page_id}`"))?;
        match page.last_snapshot_id.as_deref() {
            Some(current) if current == snapshot_id => Ok(()),
            Some(current) => Err(miette!(
                "stale browser snapshot for page `{page_id}`: expected `{current}`, got `{snapshot_id}`; call `browser_snapshot` again"
            )),
            None => Err(miette!(
                "page `{page_id}` has no active snapshot; call `browser_snapshot` first"
            )),
        }
    }

    async fn capture_page_state(
        &self,
        page: &Page,
        last_snapshot_id: Option<String>,
    ) -> BrowserPageState {
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
            last_snapshot_id,
        }
    }

    async fn replace_page_state(
        &mut self,
        page: &Page,
        last_snapshot_id: Option<String>,
    ) -> BrowserPageState {
        let state = self.capture_page_state(page, last_snapshot_id).await;
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

    fn summarize_match_text(text: &str) -> String {
        const MAX_CHARS: usize = 220;
        let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut chars = compact.chars();
        let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
        if chars.next().is_some() {
            format!("{summary}...")
        } else {
            summary
        }
    }

    fn snapshot_search_blob(node: &AriaSnapshot) -> String {
        let mut fields = Vec::new();
        if let Some(role) = node.role.as_deref() {
            fields.push(role.to_string());
        }
        if let Some(name) = node.name.as_deref() {
            fields.push(name.to_string());
        }
        if let Some(description) = node.description.as_deref() {
            fields.push(description.to_string());
        }
        if let Some(value_text) = node.value_text.as_deref() {
            fields.push(value_text.to_string());
        }
        for child in &node.children {
            let child_blob = Self::snapshot_search_blob(child);
            if !child_blob.is_empty() {
                fields.push(child_blob);
            }
        }
        fields.join("\n")
    }

    fn collect_snapshot_matches(
        node: &AriaSnapshot,
        query_lower: &str,
        seen_refs: &mut BTreeSet<String>,
        matches: &mut Vec<BrowserFindMatch>,
        max_results: usize,
    ) {
        if matches.len() >= max_results {
            return;
        }

        let search_blob = Self::snapshot_search_blob(node);
        if !search_blob.is_empty() && search_blob.to_ascii_lowercase().contains(query_lower) {
            let node_ref = node.node_ref.clone();
            let is_new = match node_ref.as_deref() {
                Some(node_ref) => seen_refs.insert(node_ref.to_string()),
                None => true,
            };
            if is_new {
                let snippet_source = node
                    .name
                    .as_deref()
                    .or(node.description.as_deref())
                    .or(node.value_text.as_deref())
                    .unwrap_or(&search_blob);
                matches.push(BrowserFindMatch {
                    element_ref: node_ref,
                    role: node.role.clone(),
                    name: node.name.clone(),
                    text_snippet: Self::summarize_match_text(snippet_source),
                    snapshot: node.to_yaml(),
                });
            }
        }

        for child in &node.children {
            if matches.len() >= max_results {
                break;
            }
            Self::collect_snapshot_matches(child, query_lower, seen_refs, matches, max_results);
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
        let previous = std::mem::take(&mut self.pages);
        let mut updated = BTreeMap::new();
        for page in pages {
            let last_snapshot_id = previous
                .get(page.target_id())
                .and_then(|state| state.last_snapshot_id.clone());
            let state = self.capture_page_state(&page, last_snapshot_id).await;
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
        let state = self.capture_page_state(&page, None).await;
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
            .map_err(|err| miette!("failed to capture browser snapshot for `{page_id}`: {err}"))?;
        let snapshot_id = self.next_snapshot_id();
        let state = self
            .capture_page_state(&page, Some(snapshot_id.clone()))
            .await;
        let snapshot = snapshot.to_yaml();
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserSnapshotResult {
            page: state,
            snapshot_id,
            snapshot,
        })
    }

    pub async fn find_in_page(
        &mut self,
        page_id: &str,
        query: &str,
        max_results: usize,
    ) -> Result<BrowserFindResult> {
        let page = self.find_page(page_id).await?;
        let limit = max_results.clamp(1, 20);
        let query_lower = query.trim().to_ascii_lowercase();
        if query_lower.is_empty() {
            return Err(miette!("browser_find_in_page requires a non-empty query"));
        }
        let snapshot = page
            .aria_snapshot()
            .await
            .map_err(|err| miette!("failed to inspect page `{page_id}`: {err}"))?;
        let mut seen_refs = BTreeSet::new();
        let mut matches = Vec::new();
        Self::collect_snapshot_matches(
            &snapshot,
            &query_lower,
            &mut seen_refs,
            &mut matches,
            limit,
        );

        let last_snapshot_id = self
            .pages
            .get(page_id)
            .and_then(|state| state.last_snapshot_id.clone());
        let state = self.replace_page_state(&page, last_snapshot_id).await;
        Ok(BrowserFindResult {
            page: state,
            query: query.to_string(),
            matches,
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
        let invalidated_snapshot_id = self
            .pages
            .get(page_id)
            .and_then(|state| state.last_snapshot_id.clone());
        let state = self.replace_page_state(&page, None).await;
        Ok(BrowserWaitResult {
            page: state,
            wait_state: wait_state.to_string(),
            invalidated_snapshot_id,
        })
    }

    pub async fn click(
        &mut self,
        page_id: &str,
        snapshot_id: &str,
        element_ref: &str,
    ) -> Result<BrowserActionResult> {
        self.require_current_snapshot(page_id, snapshot_id)?;
        let page = self.find_page(page_id).await?;
        page.locator_from_ref(element_ref)
            .click()
            .await
            .map_err(|err| miette!("failed to click `{element_ref}` on page `{page_id}`: {err}"))?;
        let invalidated_snapshot_id = self
            .pages
            .get(page_id)
            .and_then(|state| state.last_snapshot_id.clone());
        let state = self.capture_page_state(&page, None).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult {
            page: state,
            invalidated_snapshot_id,
        })
    }

    pub async fn fill(
        &mut self,
        page_id: &str,
        snapshot_id: &str,
        element_ref: &str,
        value: &str,
    ) -> Result<BrowserActionResult> {
        self.require_current_snapshot(page_id, snapshot_id)?;
        let page = self.find_page(page_id).await?;
        page.locator_from_ref(element_ref)
            .fill(value)
            .await
            .map_err(|err| miette!("failed to fill `{element_ref}` on page `{page_id}`: {err}"))?;
        let invalidated_snapshot_id = self
            .pages
            .get(page_id)
            .and_then(|state| state.last_snapshot_id.clone());
        let state = self.capture_page_state(&page, None).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult {
            page: state,
            invalidated_snapshot_id,
        })
    }

    pub async fn go_back(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        page.go_back()
            .await
            .map_err(|err| miette!("failed to go back on page `{page_id}`: {err}"))?;
        let invalidated_snapshot_id = self
            .pages
            .get(page_id)
            .and_then(|state| state.last_snapshot_id.clone());
        let state = self.capture_page_state(&page, None).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult {
            page: state,
            invalidated_snapshot_id,
        })
    }

    pub async fn go_forward(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        page.go_forward()
            .await
            .map_err(|err| miette!("failed to go forward on page `{page_id}`: {err}"))?;
        let invalidated_snapshot_id = self
            .pages
            .get(page_id)
            .and_then(|state| state.last_snapshot_id.clone());
        let state = self.capture_page_state(&page, None).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult {
            page: state,
            invalidated_snapshot_id,
        })
    }

    pub async fn reload(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        page.reload()
            .await
            .map_err(|err| miette!("failed to reload page `{page_id}`: {err}"))?;
        let invalidated_snapshot_id = self
            .pages
            .get(page_id)
            .and_then(|state| state.last_snapshot_id.clone());
        let state = self.capture_page_state(&page, None).await;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserActionResult {
            page: state,
            invalidated_snapshot_id,
        })
    }

    pub async fn close_page(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let mut page = self.find_page(page_id).await?;
        let invalidated_snapshot_id = self
            .pages
            .get(page_id)
            .and_then(|state| state.last_snapshot_id.clone());
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
        Ok(BrowserActionResult {
            page: BrowserPageState {
                last_snapshot_id: None,
                ..state
            },
            invalidated_snapshot_id,
        })
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
impl Device for BrowserDevice {
    fn id(&self) -> DeviceId {
        DeviceId::Browser
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn render_state(&self) -> DeviceStateRender {
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
                    "page={} title={} url={} last_snapshot_id={}",
                    page.page_id,
                    page.title,
                    page.url,
                    page.last_snapshot_id.as_deref().unwrap_or("none")
                ));
            }
        }
        if let Some(err) = self.init_error.as_deref() {
            lines.push(format!("last_error={}", summarize_state_text(err)));
        }
        DeviceStateRender {
            title: "Browser".to_string(),
            lines,
        }
    }

    fn usage(&self) -> DeviceUsage {
        DeviceUsage {
            purpose: BROWSER_USAGE_PURPOSE.to_string(),
            when_to_focus: BROWSER_WHEN_TO_FOCUS
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
        }
    }

    fn how_to_use(&self) -> DeviceHowToUse {
        DeviceHowToUse {
            lines: BROWSER_HOW_TO_USE_LINES
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
        }
    }

    fn focused_tool_scopes(&self) -> &'static [DeviceToolScope] {
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
