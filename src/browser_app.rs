use std::fmt::Write as _;
use std::{
    collections::{BTreeMap, HashMap},
    future::IntoFuture,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    thread,
    time::Duration,
};

use async_trait::async_trait;
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use serde_json::json;
use viewpoint_core::{
    AriaSnapshot, Browser, BrowserContext, DocumentLoadState, Page, network::Route,
};

use crate::{
    actors::ToolPermissionRequest,
    app::{App, AppDocs, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    context_budget::truncate_text_to_token_budget,
    core::{
        BrowserBackArgs, BrowserClickArgs, BrowserClosePageArgs, BrowserFillArgs,
        BrowserForwardArgs, BrowserOpenArgs, BrowserReloadArgs, BrowserSnapshotArgs,
        BrowserWaitArgs,
    },
    daat_locus_paths::daat_locus_paths_sync,
    model_protocol::AgentToolCall,
    schema_utils::model_schema_for,
};

const BROWSER_SNAPSHOT_MAX_DEPTH: usize = 6;
const BROWSER_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
const BROWSER_ACTION_TIMEOUT: Duration = Duration::from_secs(15);
const BROWSER_OPERATION_TIMEOUT_GRACE: Duration = Duration::from_secs(2);
const BROWSER_STATE_TIMEOUT: Duration = Duration::from_secs(3);
// Browser launch can overflow the already-deep agent turn stack on Windows.
// Keep Viewpoint's runtime alive on a dedicated stack until the browser drops.
const BROWSER_RUNTIME_THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

/// Browser navigation never confers local filesystem or privileged protocol
/// access, and does not consume credentials from a URL.
pub fn validate_browser_url(value: &str) -> Result<()> {
    if value.len() > 8192 {
        return Err(miette!("browser URL exceeds the size limit"));
    }
    let url = url::Url::parse(value)
        .map_err(|_| miette!("browser URL must be an absolute http or https URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(miette!("browser URL must use http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(miette!("browser URLs cannot contain credentials"));
    }
    Ok(())
}

/// Read-only browsing uses HTTP GET with no browser engine, cookies, scripts,
/// service workers, ambient proxy credentials, or automatic redirects.
pub struct ReadOnlyBrowserApp {
    pages: HashMap<String, ReadOnlyPage>,
}

#[daat_locus_macros::model_schema]
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadUrlArgs {
    url: String,
    refresh: bool,
}

#[derive(Clone, Serialize)]
struct ReadOnlyPage {
    requested_url: String,
    url: String,
    status: u16,
    media_type: String,
    fetched_at_ms: u64,
    body: String,
}

const MAX_READ_URL_BYTES: usize = 1024 * 1024;
const MAX_READ_URL_REDIRECTS: usize = 5;
const MAX_READ_URL_CACHE: usize = 8;

impl Default for ReadOnlyBrowserApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadOnlyBrowserApp {
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
        }
    }

    async fn read_url(&mut self, args: &ReadUrlArgs) -> Result<ReadOnlyPage> {
        validate_public_url(&args.url)?;
        if !args.refresh
            && let Some(page) = self.pages.get(&args.url)
        {
            return Ok(page.clone());
        }
        let page = tokio::time::timeout(Duration::from_secs(30), Self::fetch_public_url(args))
            .await
            .map_err(|_| miette!("HTTP GET deadline exceeded"))??;
        if self.pages.len() >= MAX_READ_URL_CACHE
            && !self.pages.contains_key(&args.url)
            && let Some(key) = self.pages.keys().next().cloned()
        {
            self.pages.remove(&key);
        }
        self.pages.insert(args.url.clone(), page.clone());
        Ok(page)
    }

    async fn fetch_public_url(args: &ReadUrlArgs) -> Result<ReadOnlyPage> {
        let mut current = validate_public_url(&args.url)?;
        for hop in 0..=MAX_READ_URL_REDIRECTS {
            let addresses = resolve_public_addresses(&current).await?;
            let host = current
                .host_str()
                .ok_or_else(|| miette!("HTTP host is missing"))?;
            // Each hop has a fresh resolver containing only addresses already
            // validated as public. DNS is never repeated during the connection.
            // No cookie provider is enabled; no ambient proxy is consulted.
            let client = reqwest::Client::builder()
                .no_proxy()
                .http1_only()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(host, &addresses)
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|_| miette!("read-only HTTP client is unavailable"))?;
            let mut response = client
                .get(current.clone())
                .send()
                .await
                .map_err(|_| miette!("HTTP GET failed"))?;
            if response.status().is_redirection() {
                if hop == MAX_READ_URL_REDIRECTS {
                    return Err(miette!("HTTP redirect limit exceeded"));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| miette!("HTTP redirect has no valid location"))?;
                current = safe_redirect_url(&current, location)?;
                continue;
            }
            let status = response.status().as_u16();
            let media_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("text/plain")
                .to_owned();
            if response
                .content_length()
                .is_some_and(|length| length > MAX_READ_URL_BYTES as u64)
            {
                return Err(miette!("HTTP body exceeds the size limit"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| miette!("unable to read HTTP body"))?
            {
                if bytes.len().saturating_add(chunk.len()) > MAX_READ_URL_BYTES {
                    return Err(miette!("HTTP body exceeds the size limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            let body =
                String::from_utf8(bytes).map_err(|_| miette!("HTTP body is not UTF-8 text"))?;
            let page = ReadOnlyPage {
                requested_url: args.url.clone(),
                url: current.into(),
                status,
                media_type,
                fetched_at_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                body,
            };
            return Ok(page);
        }
        Err(miette!("HTTP redirect limit exceeded"))
    }
}

fn safe_redirect_url(current: &url::Url, location: &str) -> Result<url::Url> {
    let target = current
        .join(location)
        .map_err(|_| miette!("invalid HTTP redirect URL"))?;
    validate_public_url(target.as_str())
}

fn validate_public_url(value: &str) -> Result<url::Url> {
    validate_browser_url(value)?;
    let target = url::Url::parse(value).map_err(|_| miette!("invalid HTTP URL"))?;
    match target
        .host()
        .ok_or_else(|| miette!("HTTP host is missing"))?
    {
        url::Host::Domain(host) => {
            let name = host.trim_end_matches('.').to_ascii_lowercase();
            if !name.contains('.')
                || [
                    "localhost",
                    "local",
                    "localdomain",
                    "internal",
                    "home.arpa",
                    "test",
                    "invalid",
                    "example",
                    "onion",
                ]
                .iter()
                .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")))
            {
                return Err(miette!(
                    "read-only Browser permits public Internet hosts only"
                ));
            }
        }
        url::Host::Ipv4(ip) => ensure_public_ip(IpAddr::V4(ip))?,
        url::Host::Ipv6(ip) => ensure_public_ip(IpAddr::V6(ip))?,
    }
    Ok(target)
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !matches!(a, 0 | 10 | 127)
        && a < 224
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 169 && b == 254)
        && !(a == 172 && (16..=31).contains(&b))
        && !(a == 192 && ((b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99) || b == 168))
        && !(a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
        && !(a == 203 && b == 0 && c == 113)
}

fn public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return public_ipv4(ipv4);
    }
    let segments = ip.segments();
    // Conservatively allow ordinary global unicast. Transition/NAT64 ranges
    // and special allocations can encode another address or route internally.
    segments[0] & 0xe000 == 0x2000
        && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
        && !matches!(segments[0], 0x2002 | 0x3ffe | 0x3fff)
}

fn ensure_public_ip(ip: IpAddr) -> Result<()> {
    if match ip {
        IpAddr::V4(ip) => public_ipv4(ip),
        IpAddr::V6(ip) => public_ipv6(ip),
    } {
        Ok(())
    } else {
        Err(miette!(
            "read-only Browser cannot access nonpublic network addresses"
        ))
    }
}

fn validate_public_addresses(mut addresses: Vec<SocketAddr>) -> Result<Vec<SocketAddr>> {
    if addresses.is_empty() || addresses.len() > 32 {
        return Err(miette!("HTTP host has no usable bounded address set"));
    }
    for address in &addresses {
        ensure_public_ip(address.ip())?;
    }
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

async fn resolve_public_addresses(target: &url::Url) -> Result<Vec<SocketAddr>> {
    validate_public_url(target.as_str())?;
    let port = target
        .port_or_known_default()
        .ok_or_else(|| miette!("HTTP port is missing"))?;
    let addresses = match target
        .host()
        .ok_or_else(|| miette!("HTTP host is missing"))?
    {
        url::Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        url::Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        url::Host::Domain(host) => tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| miette!("HTTP DNS lookup timed out"))?
        .map_err(|_| miette!("HTTP DNS lookup failed"))?
        .take(33)
        .collect(),
    };
    validate_public_addresses(addresses)
}

#[async_trait]
impl App for ReadOnlyBrowserApp {
    fn can_passivate(&self) -> bool {
        // This is only a GET cache; tool observations already live in history.
        true
    }
    fn id(&self) -> AppId {
        AppId::browser()
    }
    fn docs(&self) -> AppDocs {
        AppDocs::from_markdown(include_str!("app_instructions/browser_read_only.md"))
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![AppToolSpec { name: "read_url".into(), description: "Read URL source with HTTP GET, returning the final URL, status, media type, and original body.".into(), input_schema: model_schema_for::<ReadUrlArgs>() }]
    }
    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        _: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        if call.name != "read_url" {
            return Err(miette!("unknown read-only Browser tool"));
        }
        let args: ReadUrlArgs = serde_json::from_value(call.arguments.clone())
            .map_err(|_| miette!("invalid read_url arguments"))?;
        let page = self.read_url(&args).await?;
        let model_content = serde_json::to_string(&page)
            .map_err(|_| miette!("HTTP result serialization failed"))?;
        Ok(AppToolExecutionResult::new(
            format!("read {}", page.url),
            serde_json::to_value(page).map_err(|_| miette!("HTTP result serialization failed"))?,
            Some(model_content),
        ))
    }
    async fn shutdown(&mut self) -> Result<()> {
        self.pages.clear();
        Ok(())
    }
}

pub struct BrowserApp {
    context: Option<BrowserContext>,
    backend: Option<BrowserBackend>,
    pages: BTreeMap<String, BrowserPageState>,
    live_pages: HashMap<String, Page>,
    snapshot_targets: HashMap<String, CachedBrowserSnapshot>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct BrowserTargetFacts {
    role: Option<String>,
    name: Option<String>,
    description: Option<String>,
}

#[derive(Clone, Debug)]
struct CachedBrowserSnapshot {
    page: BrowserPageState,
    targets: BTreeMap<String, BrowserTargetFacts>,
}

fn snapshot_target_facts(snapshot: &AriaSnapshot) -> Result<BTreeMap<String, BrowserTargetFacts>> {
    let mut pending = vec![snapshot];
    let mut targets = BTreeMap::new();
    let mut nodes = 0usize;
    while let Some(node) = pending.pop() {
        nodes += 1;
        if nodes > 32_768 {
            return Err(miette!(
                "Browser snapshot exceeds the target metadata limit"
            ));
        }
        if let Some(reference) = &node.node_ref {
            let facts = BrowserTargetFacts {
                role: node.role.clone(),
                name: node.name.clone(),
                description: node.description.clone(),
            };
            if let Some(previous) = targets.insert(reference.clone(), facts.clone())
                && previous != facts
            {
                return Err(miette!(
                    "Browser snapshot contains conflicting element references"
                ));
            }
        }
        pending.extend(node.children.iter().rev());
    }
    Ok(targets)
}

fn validate_snapshot_target(
    cached: &CachedBrowserSnapshot,
    current_page: &BrowserPageState,
    current: &AriaSnapshot,
    reference: &str,
) -> Result<()> {
    let previous = cached.targets.get(reference).ok_or_else(|| {
        miette!("Element reference is not in the last snapshot; refresh browser_snapshot")
    })?;
    let now = snapshot_target_facts(current)?;
    if cached.page.page_id != current_page.page_id
        || cached.page.url != current_page.url
        || cached.page.title != current_page.title
        || now.get(reference) != Some(previous)
    {
        return Err(miette!(
            "Browser target changed after the snapshot; refresh browser_snapshot before acting"
        ));
    }
    Ok(())
}

struct BrowserBackend {
    browser: Option<Browser>,
    runtime_guard: Option<BrowserRuntimeGuard>,
}

impl BrowserBackend {
    const fn new(browser: Browser, runtime_guard: BrowserRuntimeGuard) -> Self {
        Self {
            browser: Some(browser),
            runtime_guard: Some(runtime_guard),
        }
    }

    const fn browser(&self) -> &Browser {
        self.browser
            .as_ref()
            .expect("browser backend should own a browser while alive")
    }
}

impl Drop for BrowserBackend {
    fn drop(&mut self) {
        drop(self.browser.take());
        drop(self.runtime_guard.take());
    }
}

struct BrowserRuntimeGuard {
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl Drop for BrowserRuntimeGuard {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
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

impl Default for BrowserApp {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserApp {
    pub fn new() -> Self {
        Self {
            context: None,
            backend: None,
            pages: BTreeMap::new(),
            live_pages: HashMap::new(),
            snapshot_targets: HashMap::new(),
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
            return Err(miette!(reason));
        }
        let backend = match launch_browser_backend(executable).await {
            Ok(backend) => backend,
            Err(err) => {
                let reason = err.to_string();
                return Err(miette!(reason));
            }
        };
        let context = match browser_operation_timeout(
            "failed to create browser context",
            BROWSER_STATE_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            backend.browser().new_context(),
        )
        .await
        {
            Ok(context) => context,
            Err(err) => {
                let reason = err.to_string();
                return Err(miette!(reason));
            }
        };
        // Interception is installed before the first page, and applies to
        // redirects and subresources as well as explicit model navigation.
        context
            .route("**/*", move |route: Route| async move {
                let request = route.request();
                if validate_browser_url(request.url()).is_err() {
                    route.abort().await
                } else {
                    route.continue_().await
                }
            })
            .await
            .map_err(|_| miette!("browser request isolation could not be installed"))?;
        self.context = Some(context);
        self.backend = Some(backend);
        self.pages.clear();
        self.live_pages.clear();
        self.snapshot_targets.clear();
        Ok(())
    }

    fn context_ref(&self) -> Result<&BrowserContext> {
        self.context
            .as_ref()
            .ok_or_else(|| miette!("browser backend is not ready"))
    }

    async fn find_page(&mut self, page_id: &str) -> Result<Page> {
        self.ensure_ready().await?;
        let page = if let Some(page) = self.live_pages.remove(page_id) {
            page
        } else {
            let mut indexed = self.refresh_pages().await?;
            let page = indexed
                .remove(page_id)
                .ok_or_else(|| miette!("unknown browser page `{page_id}`"))?;
            self.live_pages = indexed;
            page
        };
        let url = browser_operation_timeout(
            "failed to authorize browser page URL",
            BROWSER_STATE_TIMEOUT,
            page.url(),
        )
        .await?;
        validate_browser_url(&url)?;
        Ok(page)
    }

    async fn capture_page_state(&self, page: &Page) -> Result<BrowserPageState> {
        let url = browser_operation_timeout(
            "failed to authorize browser page URL",
            BROWSER_STATE_TIMEOUT,
            page.url(),
        )
        .await?;
        validate_browser_url(&url)?;
        let title = browser_operation_timeout(
            "failed to read browser page title",
            BROWSER_STATE_TIMEOUT,
            page.title(),
        )
        .await
        .unwrap_or_else(|_| "(untitled)".to_string());
        Ok(BrowserPageState {
            page_id: page.target_id().to_string(),
            title,
            url,
        })
    }

    async fn replace_page_state(&mut self, page: &Page) -> Result<BrowserPageState> {
        let state = self.capture_page_state(page).await?;
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(state)
    }

    fn action_details(&self, page_id: &str, reference: &str) -> Result<serde_json::Value> {
        let cached = self
            .snapshot_targets
            .get(page_id)
            .ok_or_else(|| miette!("Inspect this page with browser_snapshot before acting"))?;
        let page = self
            .pages
            .get(page_id)
            .ok_or_else(|| miette!("Unknown browser page; refresh browser_snapshot"))?;
        if page.url != cached.page.url || page.title != cached.page.title {
            return Err(miette!(
                "Browser page changed; refresh browser_snapshot before acting"
            ));
        }
        let target = cached.targets.get(reference).ok_or_else(|| {
            miette!("Unknown element reference; refresh browser_snapshot before acting")
        })?;
        Ok(json!({"page":page,"element_ref":reference,"target":target}))
    }

    async fn validate_action_target(
        &self,
        page: &Page,
        page_id: &str,
        reference: &str,
    ) -> Result<()> {
        let cached = self
            .snapshot_targets
            .get(page_id)
            .ok_or_else(|| miette!("Inspect this page with browser_snapshot before acting"))?;
        let snapshot = browser_operation_timeout(
            "Failed to revalidate browser target",
            BROWSER_ACTION_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            page.aria_snapshot(),
        )
        .await?;
        let current_page = self.capture_page_state(page).await?;
        validate_snapshot_target(cached, &current_page, &snapshot, reference)
    }

    fn normalize_wait_state(state: Option<&str>) -> Result<(&'static str, &'static str)> {
        match state
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("" | "load" | "complete") => {
                Ok(("load", "() => document.readyState === 'complete'"))
            }
            Some("dom" | "interactive" | "domcontentloaded") => {
                Ok(("dom", "() => document.readyState !== 'loading'"))
            }
            Some(other) => Err(miette!(
                "unsupported browser wait state `{other}`; use `dom` or `load`"
            )),
        }
    }

    async fn refresh_pages(&mut self) -> Result<HashMap<String, Page>> {
        if self.context.is_none() {
            return Ok(HashMap::new());
        }
        let pages = list_browser_pages(self.context_ref()?).await?;
        self.pages.clear();
        self.live_pages.clear();
        let mut updated = BTreeMap::new();
        let mut indexed = HashMap::with_capacity(pages.len());
        for page in pages {
            let Ok(state) = self.capture_page_state(&page).await else {
                continue;
            };
            indexed.insert(state.page_id.clone(), page);
            updated.insert(state.page_id.clone(), state);
        }
        self.pages = updated;
        Ok(indexed)
    }

    pub async fn open_page(&mut self, url: &str) -> Result<BrowserOpenResult> {
        validate_browser_url(url)?;
        self.ensure_ready().await?;
        let mut page = browser_operation_timeout(
            "failed to create browser page",
            BROWSER_STATE_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            self.context_ref()?.new_page(),
        )
        .await?;
        if let Err(err) = navigate_page_to(&page, url).await {
            let _ = browser_operation_timeout(
                "failed to close browser page after open failure",
                BROWSER_STATE_TIMEOUT,
                page.close(),
            )
            .await;
            return Err(err);
        }
        let state = self.capture_page_state(&page).await?;
        validate_browser_url(&state.url)?;
        self.live_pages.insert(state.page_id.clone(), page);
        self.pages.insert(state.page_id.clone(), state.clone());
        Ok(BrowserOpenResult { page: state })
    }

    pub async fn snapshot_page(&mut self, page_id: &str) -> Result<BrowserSnapshotResult> {
        let page = self.find_page(page_id).await?;
        let action = format!("failed to inspect page `{page_id}`");
        let snapshot = browser_operation_timeout(
            &action,
            BROWSER_ACTION_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            page.aria_snapshot(),
        )
        .await?;
        let page_state = self.replace_page_state(&page).await?;
        validate_browser_url(&page_state.url)?;
        self.snapshot_targets.insert(
            page_state.page_id.clone(),
            CachedBrowserSnapshot {
                page: page_state.clone(),
                targets: snapshot_target_facts(&snapshot)?,
            },
        );
        self.live_pages.insert(page_state.page_id.clone(), page);
        let (snapshot, stats) = compact_browser_snapshot(&snapshot);
        Ok(BrowserSnapshotResult {
            page: page_state,
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
        let action = format!("failed to wait for `{wait_state}` on page `{page_id}`");
        browser_operation_timeout(
            &action,
            timeout + BROWSER_OPERATION_TIMEOUT_GRACE,
            page.wait_for_function(expression).timeout(timeout).wait(),
        )
        .await?;
        let state = self.replace_page_state(&page).await?;
        validate_browser_url(&state.url)?;
        self.live_pages.insert(state.page_id.clone(), page);
        Ok(BrowserWaitResult {
            page: state,
            wait_state: wait_state.to_string(),
        })
    }

    pub async fn click(&mut self, page_id: &str, element_ref: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        if let Err(error) = self
            .validate_action_target(&page, page_id, element_ref)
            .await
        {
            self.live_pages.insert(page_id.to_owned(), page);
            return Err(error);
        }
        let action = format!("failed to click `{element_ref}` on page `{page_id}`");
        browser_operation_timeout(
            &action,
            BROWSER_ACTION_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            page.locator_from_ref(element_ref).click(),
        )
        .await?;
        let state = self.capture_page_state(&page).await?;
        validate_browser_url(&state.url)?;
        self.pages.insert(state.page_id.clone(), state.clone());
        self.live_pages.insert(state.page_id.clone(), page);
        Ok(BrowserActionResult { page: state })
    }

    pub async fn fill(
        &mut self,
        page_id: &str,
        element_ref: &str,
        value: &str,
    ) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        if let Err(error) = self
            .validate_action_target(&page, page_id, element_ref)
            .await
        {
            self.live_pages.insert(page_id.to_owned(), page);
            return Err(error);
        }
        let action = format!("failed to fill `{element_ref}` on page `{page_id}`");
        browser_operation_timeout(
            &action,
            BROWSER_ACTION_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            page.locator_from_ref(element_ref).fill(value),
        )
        .await?;
        let state = self.capture_page_state(&page).await?;
        validate_browser_url(&state.url)?;
        self.pages.insert(state.page_id.clone(), state.clone());
        self.live_pages.insert(state.page_id.clone(), page);
        Ok(BrowserActionResult { page: state })
    }

    pub async fn go_back(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        let action = format!("failed to go back on page `{page_id}`");
        browser_operation_timeout(
            &action,
            BROWSER_ACTION_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            page.go_back(),
        )
        .await?;
        let state = self.capture_page_state(&page).await?;
        validate_browser_url(&state.url)?;
        self.pages.insert(state.page_id.clone(), state.clone());
        self.live_pages.insert(state.page_id.clone(), page);
        Ok(BrowserActionResult { page: state })
    }

    pub async fn go_forward(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        let action = format!("failed to go forward on page `{page_id}`");
        browser_operation_timeout(
            &action,
            BROWSER_ACTION_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            page.go_forward(),
        )
        .await?;
        let state = self.capture_page_state(&page).await?;
        validate_browser_url(&state.url)?;
        self.pages.insert(state.page_id.clone(), state.clone());
        self.live_pages.insert(state.page_id.clone(), page);
        Ok(BrowserActionResult { page: state })
    }

    pub async fn reload(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let page = self.find_page(page_id).await?;
        let action = format!("failed to reload page `{page_id}`");
        browser_operation_timeout(
            &action,
            BROWSER_ACTION_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            page.reload(),
        )
        .await?;
        let state = self.capture_page_state(&page).await?;
        validate_browser_url(&state.url)?;
        self.pages.insert(state.page_id.clone(), state.clone());
        self.live_pages.insert(state.page_id.clone(), page);
        Ok(BrowserActionResult { page: state })
    }

    pub async fn close_page(&mut self, page_id: &str) -> Result<BrowserActionResult> {
        let mut page = self.find_page(page_id).await?;
        let state = self
            .pages
            .get(page_id)
            .cloned()
            .ok_or_else(|| miette!("unknown browser page `{page_id}`"))?;
        let action = format!("failed to close page `{page_id}`");
        browser_operation_timeout(&action, BROWSER_STATE_TIMEOUT, page.close()).await?;
        self.pages.remove(page_id);
        self.live_pages.remove(page_id);
        self.snapshot_targets.remove(page_id);
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
        .map_or_else(|| "generic".to_string(), str::to_ascii_lowercase)
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

#[derive(Debug, Clone)]
struct PreparedSnapshotNode {
    role: String,
    name: Option<String>,
    children: Vec<PreparedSnapshotNode>,
}

fn prepare_snapshot_node(node: &AriaSnapshot) -> PreparedSnapshotNode {
    PreparedSnapshotNode {
        role: normalize_snapshot_role(node),
        name: normalized_snapshot_text(node.name.as_deref()),
        children: node.children.iter().map(prepare_snapshot_node).collect(),
    }
}

fn collect_snapshot_ref_duplicate_counts(
    node: &AriaSnapshot,
    prepared: &PreparedSnapshotNode,
    counts: &mut HashMap<String, usize>,
) {
    if node.node_ref.is_some()
        && should_create_snapshot_ref(&prepared.role, prepared.name.as_deref())
    {
        let key = format!(
            "{}:{}",
            prepared.role,
            prepared.name.as_deref().unwrap_or("")
        );
        *counts.entry(key).or_insert(0) += 1;
    }
    for (child, prepared_child) in node.children.iter().zip(&prepared.children) {
        collect_snapshot_ref_duplicate_counts(child, prepared_child, counts);
    }
}

fn build_snapshot_line(
    node: &AriaSnapshot,
    prepared: &PreparedSnapshotNode,
    depth: usize,
    duplicate_counts: &HashMap<String, usize>,
    duplicate_seen: &mut HashMap<String, usize>,
    stats: &mut CompactSnapshotStats,
) -> String {
    let role = prepared.role.as_str();
    let name = prepared.name.as_deref();
    let indent = "  ".repeat(depth);
    let mut line = format!("{indent}- {role}");
    if let Some(name) = name {
        let _ = write!(
            line,
            " \"{}\"",
            escape_snapshot_value(&compact_snapshot_text(name, 160))
        );
    }

    if let Some(node_ref) = node.node_ref.as_deref()
        && should_create_snapshot_ref(role, name)
    {
        let key = format!("{role}:{}", name.unwrap_or(""));
        let nth = duplicate_seen.entry(key.clone()).or_insert(0);
        let current_nth = *nth;
        *nth += 1;
        let _ = write!(line, " [ref={node_ref}]");
        if duplicate_counts.get(&key).copied().unwrap_or(0) > 1 && current_nth > 0 {
            let _ = write!(line, " [nth={current_nth}]");
        }
        stats.ref_count += 1;
        if is_interactive_role(role) {
            stats.interactive_ref_count += 1;
        }
    }

    if let Some(value_text) = normalized_snapshot_text(node.value_text.as_deref()) {
        let _ = write!(
            line,
            " value=\"{}\"",
            escape_snapshot_value(&compact_snapshot_text(&value_text, 120))
        );
    } else if let Some(value_now) = node.value_now {
        let _ = write!(line, " value={value_now}");
    }

    if let Some(description) = normalized_snapshot_text(node.description.as_deref()) {
        let _ = write!(
            line,
            " description=\"{}\"",
            escape_snapshot_value(&compact_snapshot_text(&description, 120))
        );
    }

    if let Some(level) = node.level {
        let _ = write!(line, " level={level}");
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
        let _ = write!(line, " checked={checked:?}");
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
    prepared: &PreparedSnapshotNode,
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

    let role = prepared.role.as_str();
    let name = prepared.name.as_deref();

    let mut child_lines = Vec::new();
    let mut child_relevant = false;
    for (child, prepared_child) in node.children.iter().zip(&prepared.children) {
        let rendered = render_compact_snapshot_lines(
            child,
            prepared_child,
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

    let has_ref = node.node_ref.is_some() && should_create_snapshot_ref(role, name);
    let has_meaningful_text = name.is_some()
        || node
            .value_text
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || node
            .description
            .as_deref()
            .is_some_and(|description| !description.trim().is_empty());
    let include_node = should_include_snapshot_node(role, name);
    let include_line = include_node
        && (!is_structural_role(role) || has_ref || has_meaningful_text || child_relevant);

    let mut lines = Vec::new();
    if include_line {
        lines.push(build_snapshot_line(
            node,
            prepared,
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
    let prepared_tree = prepare_snapshot_node(snapshot);
    let (snapshot_root, prepared_root) =
        preferred_snapshot_root(snapshot, &prepared_tree).unwrap_or((snapshot, &prepared_tree));
    let mut duplicate_counts = HashMap::new();
    collect_snapshot_ref_duplicate_counts(snapshot_root, prepared_root, &mut duplicate_counts);

    let mut duplicate_seen = HashMap::new();
    let mut stats = CompactSnapshotStats::default();
    let rendered = render_compact_snapshot_lines(
        snapshot_root,
        prepared_root,
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

fn preferred_snapshot_root<'a>(
    snapshot: &'a AriaSnapshot,
    prepared: &'a PreparedSnapshotNode,
) -> Option<(&'a AriaSnapshot, &'a PreparedSnapshotNode)> {
    let mut best_main = None;
    let mut best_article = None;
    collect_preferred_snapshot_root(snapshot, prepared, &mut best_main, &mut best_article);
    best_main.or(best_article)
}

fn collect_preferred_snapshot_root<'a>(
    node: &'a AriaSnapshot,
    prepared: &'a PreparedSnapshotNode,
    best_main: &mut Option<(&'a AriaSnapshot, &'a PreparedSnapshotNode)>,
    best_article: &mut Option<(&'a AriaSnapshot, &'a PreparedSnapshotNode)>,
) {
    if prepared.role == "main" && best_main.is_none() {
        *best_main = Some((node, prepared));
    } else if prepared.role == "article" && best_article.is_none() {
        *best_article = Some((node, prepared));
    }

    if best_main.is_some() && best_article.is_some() {
        return;
    }

    for (child, prepared_child) in node.children.iter().zip(&prepared.children) {
        collect_preferred_snapshot_root(child, prepared_child, best_main, best_article);
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

async fn navigate_page_to(page: &Page, url: &str) -> Result<()> {
    let action = format!("failed to open `{url}`");
    browser_operation_timeout(
        &action,
        BROWSER_OPEN_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
        page.goto(url)
            .wait_until(DocumentLoadState::DomContentLoaded)
            .timeout(BROWSER_OPEN_TIMEOUT)
            .goto(),
    )
    .await?;
    Ok(())
}

async fn list_browser_pages(context: &BrowserContext) -> Result<Vec<Page>> {
    browser_operation_timeout(
        "failed to list browser pages",
        BROWSER_STATE_TIMEOUT,
        context.pages(),
    )
    .await
}

async fn browser_operation_timeout<T, E, Op>(
    action: &str,
    timeout: Duration,
    operation: Op,
) -> Result<T>
where
    Op: IntoFuture<Output = std::result::Result<T, E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(timeout, operation.into_future()).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(miette!("{action}: {err}")),
        Err(_) => Err(miette!(
            "{action} timed out after {}ms",
            timeout.as_millis()
        )),
    }
}

async fn launch_browser_backend(executable: PathBuf) -> Result<BrowserBackend> {
    let (result_tx, result_rx) =
        std::sync::mpsc::sync_channel::<std::result::Result<Browser, String>>(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let join_handle = thread::Builder::new()
        .name("daat-browser-runtime".to_string())
        .stack_size(BROWSER_RUNTIME_THREAD_STACK_BYTES)
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = result_tx.send(Err(format!("failed to build browser runtime: {err}")));
                    return;
                }
            };

            let launch_result = runtime.block_on(async {
                browser_operation_timeout(
                    "failed to launch browser backend",
                    BROWSER_OPEN_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
                    Browser::launch()
                        .executable_path(executable)
                        .headless(true)
                        .launch(),
                )
                .await
                .map_err(|err| err.to_string())
            });
            let launch_succeeded = launch_result.is_ok();
            if result_tx.send(launch_result).is_err() {
                return;
            }
            if launch_succeeded {
                runtime.block_on(async {
                    let _ = shutdown_rx.await;
                });
            }
        })
        .map_err(|err| miette!("spawn browser runtime thread failed: {err}"))?;

    let launch_result = tokio::task::spawn_blocking(move || result_rx.recv())
        .await
        .map_err(|err| miette!("join browser launch receiver failed: {err}"))?
        .map_err(|err| miette!("browser runtime thread ended before launch result: {err}"))?;

    match launch_result {
        Ok(browser) => Ok(BrowserBackend::new(
            browser,
            BrowserRuntimeGuard {
                shutdown_tx: Some(shutdown_tx),
                join_handle: Some(join_handle),
            },
        )),
        Err(err) => {
            let _ = join_handle.join();
            Err(miette!(err))
        }
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
    title: &str,
    result: &BrowserActionResult,
    extra_lines: Vec<String>,
    max_tokens: usize,
) -> AppToolExecutionResult {
    let model_content = browser_action_model_content(title, &result.page, &extra_lines, max_tokens);
    AppToolExecutionResult::new(
        title.to_string(),
        json!({ "page": result.page }),
        Some(model_content),
    )
}

fn browser_wait_result(result: &BrowserWaitResult, max_tokens: usize) -> AppToolExecutionResult {
    let extra_lines = vec![format!("wait_state={}", result.wait_state)];
    let model_content = browser_action_model_content(
        "waited for browser page",
        &result.page,
        &extra_lines,
        max_tokens,
    );
    AppToolExecutionResult::new(
        "waited for browser page".to_string(),
        json!({
            "page": result.page,
            "wait_state": result.wait_state,
        }),
        Some(model_content),
    )
}

#[async_trait]
impl App for BrowserApp {
    fn can_passivate(&self) -> bool {
        // Even with no tabs, the browser context can own cookies/storage.
        self.backend.is_none() && self.context.is_none() && self.live_pages.is_empty()
    }
    fn id(&self) -> AppId {
        AppId::browser()
    }

    fn docs(&self) -> AppDocs {
        AppDocs::from_markdown(include_str!("app_instructions/browser.md"))
    }

    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![
            AppToolSpec {
                name: "browser_open_page".to_string(),
                description: "Create a browser page, open the specified URL, and return the new `page_id`.".to_string(),
                input_schema: model_schema_for::<BrowserOpenArgs>(),
            },
            AppToolSpec {
                name: "browser_snapshot".to_string(),
                description: "Read a compact semantic snapshot of the specified page, preserving high-value nodes and interactable element refs first."
                    .to_string(),
                input_schema: model_schema_for::<BrowserSnapshotArgs>(),
            },
            AppToolSpec {
                name: "browser_wait".to_string(),
                description: "Wait until the specified page reaches a stable state. `state` may be `dom` or `load`.".to_string(),
                input_schema: model_schema_for::<BrowserWaitArgs>(),
            },
            AppToolSpec {
                name: "browser_click".to_string(),
                description: "Click a page element by `element_ref`; if page changes made the ref stale, the tool fails directly.".to_string(),
                input_schema: model_schema_for::<BrowserClickArgs>(),
            },
            AppToolSpec {
                name: "browser_fill".to_string(),
                description: "Fill an input by `element_ref`; if page changes made the ref stale, the tool fails directly.".to_string(),
                input_schema: model_schema_for::<BrowserFillArgs>(),
            },
            AppToolSpec {
                name: "browser_back".to_string(),
                description: "Navigate the specified page backward.".to_string(),
                input_schema: model_schema_for::<BrowserBackArgs>(),
            },
            AppToolSpec {
                name: "browser_forward".to_string(),
                description: "Navigate the specified page forward.".to_string(),
                input_schema: model_schema_for::<BrowserForwardArgs>(),
            },
            AppToolSpec {
                name: "browser_reload".to_string(),
                description: "Reload the specified page.".to_string(),
                input_schema: model_schema_for::<BrowserReloadArgs>(),
            },
            AppToolSpec {
                name: "browser_close_page".to_string(),
                description: "Close the specified browser page. Close pages that are no longer needed to save memory.".to_string(),
                input_schema: model_schema_for::<BrowserClosePageArgs>(),
            },
        ]
    }

    fn permission_request(
        &self,
        call: &AgentToolCall,
        _: &AppToolExecutionContext,
    ) -> Result<ToolPermissionRequest> {
        let operation_details = match call.name.as_str() {
            "browser_click" => {
                let args: BrowserClickArgs = parse_browser_tool_args(call)?;
                self.action_details(&args.page_id, &args.element_ref)?
            }
            "browser_fill" => {
                let args: BrowserFillArgs = parse_browser_tool_args(call)?;
                self.action_details(&args.page_id, &args.element_ref)?
            }
            _ => serde_json::Value::Null,
        };
        Ok(ToolPermissionRequest {
            operation_details,
            ..Default::default()
        })
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
                Ok(AppToolExecutionResult::new(
                    summary,
                    json!({ "page": result.page }),
                    Some(model_content),
                ))
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
                Ok(AppToolExecutionResult::new(
                    summary,
                    json!({
                        "page": result.page,
                        "snapshot": result.snapshot,
                        "line_count": result.line_count,
                        "ref_count": result.ref_count,
                        "interactive_ref_count": result.interactive_ref_count,
                    }),
                    Some(model_content),
                ))
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
                    "closed browser page",
                    &result,
                    Vec::new(),
                    context.tool_output_max_tokens,
                ))
            }
            _ => Err(miette!("unknown browser tool `{}`", call.name)),
        }
    }

    async fn shutdown(&mut self) -> Result<()> {
        if let Some(context) = self.context.as_mut() {
            let _ = browser_operation_timeout(
                "failed to close browser context",
                BROWSER_STATE_TIMEOUT,
                context.close(),
            )
            .await;
        }
        if let Some(backend) = self.backend.as_ref() {
            let _ = browser_operation_timeout(
                "failed to close browser backend",
                BROWSER_STATE_TIMEOUT,
                backend.browser().close(),
            )
            .await;
        }
        self.context = None;
        self.backend = None;
        self.snapshot_targets.clear();
        self.pages.clear();
        self.live_pages.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn semantic_fixture() -> (BrowserPageState, AriaSnapshot) {
        let page = BrowserPageState {
            page_id: "page-1".into(),
            title: "Account settings".into(),
            url: "https://example.com/account".into(),
        };
        let mut root = AriaSnapshot::with_role("document");
        let mut target = AriaSnapshot::with_role("button");
        target.node_ref = Some("e7".into());
        target.name = Some("Send \"report\"\nnow".into());
        target.description = Some("Send the report to Finance".into());
        root.children.push(target);
        (page, root)
    }

    #[test]
    fn browser_target_approval_contains_cached_typed_facts_without_browser_effects() {
        let (page, snapshot) = semantic_fixture();
        let mut browser = BrowserApp::new();
        let targets = snapshot_target_facts(&snapshot).unwrap();
        browser.pages.insert(page.page_id.clone(), page.clone());
        browser.snapshot_targets.insert(
            page.page_id.clone(),
            CachedBrowserSnapshot {
                page: page.clone(),
                targets: targets.clone(),
            },
        );
        let context = AppToolExecutionContext {
            execution_cwd: std::env::temp_dir(),
            sandbox_policy: crate::sandbox::RuntimeSandboxPolicy::disabled(),
            tool_output_max_tokens: 1000,
            turn_epoch: 1,
            actor_call: None,
        };
        for (name, arguments) in [
            (
                "browser_click",
                json!({"page_id":"page-1","element_ref":"e7"}),
            ),
            (
                "browser_fill",
                json!({"page_id":"page-1","element_ref":"e7","value":"draft"}),
            ),
        ] {
            let call = AgentToolCall {
                id: "call".into(),
                name: name.into(),
                arguments,
            };
            let planned = browser.permission_request(&call, &context).unwrap();
            assert_eq!(planned.operation_details["page"]["url"], page.url);
            assert_eq!(planned.operation_details["page"]["title"], page.title);
            assert_eq!(planned.operation_details["target"]["role"], "button");
            assert_eq!(
                planned.operation_details["target"]["name"],
                "Send \"report\"\nnow"
            );
            assert_eq!(
                planned.operation_details["target"]["description"],
                "Send the report to Finance"
            );
            assert!(planned.readable_roots.is_empty() && planned.writable_roots.is_empty());
        }
        assert!(
            browser.context.is_none() && browser.backend.is_none() && browser.live_pages.is_empty()
        );
        assert_eq!(browser.snapshot_targets[&page.page_id].targets, targets);
    }

    #[test]
    fn browser_target_changed_meaning_or_page_is_rejected_before_action() {
        let (page, snapshot) = semantic_fixture();
        let cached = CachedBrowserSnapshot {
            page: page.clone(),
            targets: snapshot_target_facts(&snapshot).unwrap(),
        };
        validate_snapshot_target(&cached, &page, &snapshot, "e7").unwrap();
        let mut renamed = snapshot.clone();
        renamed.children[0].name = Some("Delete account".into());
        assert!(
            validate_snapshot_target(&cached, &page, &renamed, "e7")
                .unwrap_err()
                .to_string()
                .contains("refresh browser_snapshot")
        );
        let mut changed_role = snapshot.clone();
        changed_role.children[0].role = Some("textbox".into());
        assert!(validate_snapshot_target(&cached, &page, &changed_role, "e7").is_err());
        let mut changed_description = snapshot.clone();
        changed_description.children[0].description = Some("Transfer all funds".into());
        assert!(validate_snapshot_target(&cached, &page, &changed_description, "e7").is_err());
        let mut reused_reference = snapshot.clone();
        reused_reference.children[0].node_ref = Some("e8".into());
        assert!(validate_snapshot_target(&cached, &page, &reused_reference, "e7").is_err());
        let mut navigated = page.clone();
        navigated.url = "https://example.com/pay".into();
        assert!(validate_snapshot_target(&cached, &navigated, &snapshot, "e7").is_err());
        assert!(validate_snapshot_target(&cached, &page, &snapshot, "unknown").is_err());
    }

    #[test]
    fn navigation_rejects_privileged_schemes_and_embedded_credentials() {
        for url in [
            "file:///C:/secret.txt",
            "data:text/plain,secret",
            "javascript:alert(1)",
            "chrome://settings",
            "https://user:password@example.com",
            "//example.com",
        ] {
            assert!(validate_browser_url(url).is_err(), "{url}");
        }
        assert!(validate_browser_url("https://example.com/document").is_ok());
    }

    #[tokio::test]
    async fn readonly_http_rejects_privileged_targets_before_dns_lookup() {
        let mut browser = ReadOnlyBrowserApp::new();
        let args = ReadUrlArgs {
            url: "file:///C:/secret.txt".into(),
            refresh: true,
        };
        assert!(browser.read_url(&args).await.is_err());
        assert!(browser.pages.is_empty());
        assert_eq!(browser.tool_specs()[0].name, "read_url");
        crate::schema_utils::validate_model_facing_schema(&browser.tool_specs()[0].input_schema)
            .unwrap();
    }

    #[test]
    fn readonly_redirects_validate_every_target() {
        let current = url::Url::parse("https://example.com/a/b").unwrap();
        assert_eq!(
            safe_redirect_url(&current, "../c").unwrap().as_str(),
            "https://example.com/c"
        );
        for location in [
            "file:///secret",
            "data:text/plain,secret",
            "https://name:secret@example.com",
            "http://127.1/private",
            "http://169.254.169.254/metadata",
            "http://[::ffff:192.168.1.1]/",
            "http://router.local/path",
        ] {
            assert!(safe_redirect_url(&current, location).is_err());
        }
    }

    #[tokio::test]
    async fn readonly_cache_is_local_and_preserves_source() {
        let mut browser = ReadOnlyBrowserApp::new();
        let args = ReadUrlArgs {
            url: "https://example.com/a".into(),
            refresh: false,
        };
        browser.pages.insert(
            args.url.clone(),
            ReadOnlyPage {
                requested_url: args.url.clone(),
                url: "https://example.com/b".into(),
                status: 200,
                media_type: "text/html".into(),
                fetched_at_ms: 1,
                body: "<script>write()</script>original".into(),
            },
        );
        let page = browser.read_url(&args).await.unwrap();
        assert_eq!(page.url, "https://example.com/b");
        assert_eq!(page.body, "<script>write()</script>original");
        assert!(ReadOnlyBrowserApp::new().pages.is_empty());
    }

    #[test]
    fn readonly_public_targets_reject_private_reserved_and_mapped_addresses() {
        for target in [
            "http://127.0.0.1",
            "http://127.1",
            "http://2130706433",
            "http://0x7f000001",
            "http://0177.0.0.1",
            "http://0.1.2.3",
            "http://10.0.0.1",
            "http://100.64.0.1",
            "http://169.254.169.254",
            "http://172.31.0.1",
            "http://192.168.1.1",
            "http://192.0.0.1",
            "http://192.0.2.1",
            "http://192.88.99.1",
            "http://198.18.0.1",
            "http://198.51.100.1",
            "http://203.0.113.1",
            "http://224.0.0.1",
            "http://240.0.0.1",
            "http://255.255.255.255",
            "http://[::]",
            "http://[::1]",
            "http://[::ffff:127.0.0.1]",
            "http://[::ffff:c0a8:1]",
            "http://[fe80::1]",
            "http://[fc00::1]",
            "http://[ff02::1]",
            "http://[2001:db8::1]",
            "http://[2001::1]",
            "http://[2002:a00:1::]",
            "http://[64:ff9b::a00:1]",
            "http://[3fff::1]",
            "http://localhost",
            "http://LOCALHOST.",
            "http://a.localhost",
            "http://router.local",
            "http://corp.internal",
            "http://host.home.arpa",
        ] {
            assert!(validate_public_url(target).is_err(), "{target}");
        }
        for target in [
            "https://example.com",
            "https://8.8.8.8",
            "https://[2606:4700:4700::1111]",
            "https://[::ffff:8.8.8.8]",
        ] {
            assert!(validate_public_url(target).is_ok(), "{target}");
        }
    }

    #[test]
    fn readonly_dns_address_set_rejects_any_private_answer_and_bounds_count() {
        let public = "8.8.8.8:443".parse::<SocketAddr>().unwrap();
        let private = "127.0.0.1:443".parse::<SocketAddr>().unwrap();
        assert!(validate_public_addresses(vec![public, private]).is_err());
        assert!(validate_public_addresses(vec![]).is_err());
        assert!(validate_public_addresses(vec![public; 33]).is_err());
        assert_eq!(
            validate_public_addresses(vec![public, public]).unwrap(),
            [public]
        );
    }

    #[tokio::test]
    async fn browser_operation_timeout_returns_instead_of_hanging() {
        let started = Instant::now();
        let err = browser_operation_timeout(
            "test browser operation",
            Duration::from_millis(10),
            std::future::pending::<std::result::Result<(), &'static str>>(),
        )
        .await
        .expect_err("pending operation should time out");

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(err.to_string().contains("timed out after"));
    }

    #[tokio::test]
    async fn browser_operation_timeout_preserves_operation_errors() {
        let err = browser_operation_timeout(
            "test browser operation",
            Duration::from_secs(1),
            std::future::ready(std::result::Result::<(), _>::Err("boom")),
        )
        .await
        .expect_err("operation error should propagate");

        assert!(err.to_string().contains("test browser operation: boom"));
    }

    #[tokio::test]
    #[ignore = "requires DAAT_LOCUS_TEST_BROWSER pointing to a Chromium executable"]
    async fn launch_browser_backend_keeps_connection_runtime_alive() {
        let executable = std::path::PathBuf::from(
            std::env::var_os("DAAT_LOCUS_TEST_BROWSER")
                .expect("set DAAT_LOCUS_TEST_BROWSER to an isolated test Chromium executable"),
        );
        assert!(executable.is_absolute() && executable.is_file());

        let backend = launch_browser_backend(executable)
            .await
            .expect("launch browser backend");
        let context = browser_operation_timeout(
            "test browser context creation",
            BROWSER_STATE_TIMEOUT + BROWSER_OPERATION_TIMEOUT_GRACE,
            backend.browser().new_context(),
        )
        .await
        .expect("create browser context");

        drop(context);
        drop(backend);
    }
}
