//! GUI operation is confined to our own isolated native pages. No OS-wide input.
use crate::web_host::NativeHost;
use daat_locus_client_protocol::web::{
    BrowserCommand, BrowserInput, BrowserObservation, BrowserPage, BrowserReply, Operation, Request,
};
use serde_json::Value;
use std::time::Duration;
use tauri::{Manager, Webview};

#[cfg(windows)]
#[path = "browser_use_windows.rs"]
mod platform;
#[cfg(target_os = "macos")]
pub(crate) use platform::configuration;
#[cfg(target_os = "macos")]
pub(crate) use platform::remove_shortcuts;
#[cfg(any(windows, target_os = "macos"))]
pub(crate) use platform::shortcuts;
#[cfg(target_os = "macos")]
#[path = "browser_use_macos.rs"]
mod platform;

pub(crate) struct Receipt {
    pub state: String,
    pub signature: String,
    pub width: u32,
    pub height: u32,
    pub viewport_width: f64,
    pub viewport_height: f64,
}
// DOM changes invalidate coordinates too. This fixed host script exposes no
// native capability and accepts no model-provided JavaScript.
const METRICS: &str = daat_locus_client_protocol::web::BROWSER_METRICS_SCRIPT;
pub(crate) async fn navigation(view: &Webview) -> Result<(bool, bool), String> {
    platform::navigation(view).await
}
pub(crate) async fn occlude(
    view: &Webview,
    bounds: Option<daat_locus_client_protocol::web::BrowserBounds>,
    cuts: Vec<daat_locus_client_protocol::web::BrowserOcclusion>,
) -> Result<(), String> {
    platform::occlude(view, bounds, cuts).await
}
#[cfg(all(windows, feature = "web-contract"))]
pub(crate) async fn occlusion_probe(view: &Webview) -> Result<Value, String> {
    platform::occlusion_probe(view).await
}
pub(crate) async fn page_screenshot(view: &Webview) -> Result<String, String> {
    cursor_visibility(view, "suspend").await;
    let image = platform::screenshot(view).await;
    cursor_visibility(view, "resume").await;
    image
}
pub(crate) async fn favicon(view: &Webview) -> Option<String> {
    #[cfg(windows)]
    {
        platform::favicon(view).await.ok()
    }
    #[cfg(target_os = "macos")]
    {
        // The ordinary page fetch boundary applies; no privileged shell fetch.
        let script = "(()=>{const key=Symbol.for('dl.favicon');window[key]=null;const icon=document.querySelector('link[rel~=icon]')?.href||new URL('/favicon.ico',location.href).href;const image=new Image();image.crossOrigin='anonymous';image.onload=()=>{try{const canvas=document.createElement('canvas');canvas.width=32;canvas.height=32;canvas.getContext('2d').drawImage(image,0,0,32,32);window[key]=canvas.toDataURL('image/png')}catch{window[key]=''}};image.onerror=()=>window[key]='';image.src=icon;return true})()";
        evaluate(view, script).await.ok()?;
        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let value = evaluate(view, "window[Symbol.for('dl.favicon')]")
                .await
                .ok()?;
            if let Some(icon) = value.as_str() {
                return (icon.starts_with("data:image/png;base64,") && icon.len() < 48_000)
                    .then(|| icon.into());
            }
        }
        None
    }
}
pub(crate) async fn find(view: &Webview, action: &str, text: &str) -> Result<Value, String> {
    #[cfg(windows)]
    {
        platform::find(view, action, text).await
    }
    #[cfg(target_os = "macos")]
    {
        let clear = action == "find_close" || text.is_empty();
        let text = serde_json::to_string(text).map_err(|_| "Invalid search")?;
        let script = if clear {
            "(()=>{getSelection()?.removeAllRanges();return {found:false,count:0,index:0}})()"
                .into()
        } else {
            format!(
                "(()=>{{if({}){{const range=document.createRange();range.selectNodeContents(document.body);range.collapse(true);getSelection()?.removeAllRanges();getSelection()?.addRange(range)}}return {{found:window.find({text},false,{},true,false,true,false),count:null,index:null}}}})()",
                action == "find",
                action == "find_previous"
            )
        };
        evaluate(view, &script).await
    }
}
pub(crate) async fn evaluate(window: &Webview, script: &str) -> Result<Value, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = std::sync::Mutex::new(Some(sender));
    window
        .eval_with_callback(script, move |value| {
            if let Some(sender) = sender.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(value);
            }
        })
        .map_err(|_| "Browser inspection could not start")?;
    let value = tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .map_err(|_| "Browser inspection timed out")?
        .map_err(|_| "Browser inspection interrupted")?;
    let mut value: Value =
        serde_json::from_str(&value).map_err(|_| "Invalid browser inspection")?;
    if let Some(encoded) = value.as_str() {
        value = serde_json::from_str(encoded).map_err(|_| "Invalid encoded browser inspection")?;
    }
    Ok(value)
}
fn label(profile: &str, tab: &str) -> String {
    format!("web-{profile}-{tab}")
}
fn page(window: &Webview, tab: &str) -> Result<BrowserPage, String> {
    Ok(BrowserPage {
        tab: tab.into(),
        url: window.url().map_err(|_| "Browser URL unavailable")?.into(),
        title: String::new(),
    })
}
impl NativeHost {
    pub(crate) fn pages(&self, profile: &str) -> Vec<BrowserPage> {
        let prefix = format!("web-{profile}-");
        let mut pages = Vec::new();
        for (key, window) in self.app.webviews() {
            if let Some(tab) = key.strip_prefix(&prefix)
                && let Ok(mut page) = page(&window, tab)
                && (page.url == "about:blank"
                    || url::Url::parse(&page.url).is_ok_and(|url| self.navigation_allowed(&url)))
            {
                if page.url == "about:blank"
                    && let Some(target) = self
                        .targets
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&key)
                {
                    page.url = target.clone();
                }
                pages.push(BrowserPage {
                    title: if page.url == "about:blank" {
                        String::new()
                    } else {
                        self.titles
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(&key)
                            .cloned()
                            .unwrap_or_default()
                    },
                    ..page
                });
            }
        }
        let order = self.tab_order.lock().unwrap_or_else(|e| e.into_inner());
        pages.sort_by_key(|page| {
            order
                .iter()
                .position(|key| key == &label(profile, &page.tab))
                .unwrap_or(usize::MAX)
        });
        pages
    }
    pub(crate) async fn browser_command(
        &self,
        profile: &str,
        command: BrowserCommand,
        id: &str,
    ) -> Result<BrowserReply, String> {
        let mut observation = None;
        match command {
            BrowserCommand::List => {}
            BrowserCommand::Open { tab, url } => {
                let key = label(profile, &tab);
                if self.app.get_webview(&key).is_some() {
                    return Err(
                        "Browser page already exists; observe it instead of replaying open".into(),
                    );
                }
                self.load(
                    Request {
                        id: id.into(),
                        profile: profile.into(),
                        operation: Operation::Load { url },
                    },
                    key.clone(),
                    false,
                )
                .await?;
                observation = Some(self.browser_observe(&key, &tab, id).await?);
            }
            BrowserCommand::Close { tab } => {
                let key = label(profile, &tab);
                self.browser_states
                    .lock()
                    .map_err(|_| "Browser unavailable")?
                    .remove(&key);
                self.close_page(&key)?;
            }
            BrowserCommand::Observe { tab } => {
                observation = Some(
                    self.browser_observe(&label(profile, &tab), &tab, id)
                        .await?,
                );
            }
            BrowserCommand::Input {
                tab,
                state_id,
                input,
            } => {
                let key = label(profile, &tab);
                let window = self.app.get_webview(&key).ok_or("Browser page is closed")?;
                let receipt = self
                    .browser_states
                    .lock()
                    .map_err(|_| "Browser unavailable")?
                    .remove(&key)
                    .ok_or("Observe this page before input")?;
                if receipt.state != state_id {
                    return Err("Stale browser observation; observe again".into());
                }
                let metrics = evaluate(&window, METRICS).await?;
                if signature(&window, &metrics)? != receipt.signature {
                    return Err("Browser page changed; observe again before input".into());
                }
                let map = |x: f64, y: f64| -> Result<(f64, f64), String> {
                    if x < 0.0
                        || y < 0.0
                        || x >= f64::from(receipt.width)
                        || y >= f64::from(receipt.height)
                    {
                        return Err("Input point is outside the observed image".into());
                    }
                    Ok((
                        x / f64::from(receipt.width) * receipt.viewport_width,
                        y / f64::from(receipt.height) * receipt.viewport_height,
                    ))
                };
                let input = match input {
                    BrowserInput::Move { x, y } => {
                        let (x, y) = map(x, y)?;
                        BrowserInput::Move { x, y }
                    }
                    BrowserInput::Drag { x, y, to_x, to_y } => {
                        let (x, y) = map(x, y)?;
                        let (to_x, to_y) = map(to_x, to_y)?;
                        BrowserInput::Drag { x, y, to_x, to_y }
                    }
                    BrowserInput::Click {
                        x,
                        y,
                        button,
                        count,
                    } => {
                        let (x, y) = map(x, y)?;
                        BrowserInput::Click {
                            x,
                            y,
                            button,
                            count,
                        }
                    }
                    BrowserInput::Scroll {
                        x,
                        y,
                        delta_x,
                        delta_y,
                    } => {
                        let (x, y) = map(x, y)?;
                        BrowserInput::Scroll {
                            x,
                            y,
                            delta_x,
                            delta_y,
                        }
                    }
                    other => other,
                };
                // Decoration follows accepted, mapped input; rejected/stale calls
                // cannot move it. It never dispatches input or moves the OS pointer.
                show_cursor(&window, &input).await;
                // Owner input remains live during inspection/decoration awaits.
                // The prepared closed shadow keeps our animation out of this signature.
                if signature(&window, &evaluate(&window, METRICS).await?)? != receipt.signature {
                    hide_cursor(&window);
                    return Err("Browser page changed before input; observe again".into());
                }
                if matches!(
                    &input,
                    BrowserInput::Back | BrowserInput::Forward | BrowserInput::Reload
                ) {
                    let preferences = self
                        .preferences
                        .lock()
                        .map_err(|_| "Browser unavailable")?
                        .clone();
                    configure(&window, &preferences).await?;
                }
                if let BrowserInput::Navigate { url } = &input {
                    let url = url::Url::parse(url).map_err(|_| "Invalid browser URL")?;
                    if !self.navigation_allowed(&url) {
                        return Err("Browser target is not allowed".into());
                    }
                    let preferences = self
                        .preferences
                        .lock()
                        .map_err(|_| "Browser unavailable")?
                        .clone();
                    configure(&window, &preferences).await?;
                    navigate(&window, url, &preferences).await?;
                } else {
                    platform::input(&window, input).await?;
                }
                // Give native input/layout and normal navigation a chance to settle.
                tokio::time::sleep(Duration::from_millis(180)).await;
                for _ in 0..40 {
                    if evaluate(&window, METRICS)
                        .await?
                        .get("ready")
                        .and_then(Value::as_str)
                        == Some("complete")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                observation = Some(self.browser_observe(&key, &tab, id).await?);
            }
        }
        Ok(BrowserReply {
            pages: self.pages(profile),
            observation,
        })
    }
    async fn browser_observe(
        &self,
        key: &str,
        tab: &str,
        id: &str,
    ) -> Result<BrowserObservation, String> {
        self.browser_states
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(key);
        let window = self.app.get_webview(key).ok_or("Browser page is closed")?;
        cursor_visibility(&window, "prepare").await;
        self.select_page(key, false)?;
        if !window
            .url()
            .is_ok_and(|url| url.as_str() == "about:blank" || self.navigation_allowed(&url))
        {
            return Err("Browser page is still loading or its URL is not allowed".into());
        }
        // Retake only the observation when a transient layout/pointer event
        // races capture. Native input is never repeated here.
        let (before_signature, after, png) = tokio::time::timeout(Duration::from_secs(8), async {
            for _ in 0..3 {
                let before = evaluate(&window, METRICS).await?;
                let prior = signature(&window, &before)?;
                cursor_visibility(&window, "suspend").await;
                let captured = platform::screenshot(&window).await;
                cursor_visibility(&window, "resume").await;
                let png = captured?;
                let after = evaluate(&window, METRICS).await?;
                if prior == signature(&window, &after)? {
                    return Ok((prior, after, png));
                }
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
            Err::<_, String>("Browser changed during capture; observe again".into())
        })
        .await
        .map_err(|_| "Browser capture timed out; observe again, do not replay input")??;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&png)
            .map_err(|_| "Invalid browser screenshot")?;
        if bytes.len() < 24 || bytes[..8] != *b"\x89PNG\r\n\x1a\n" || bytes.len() > 7 * 1024 * 1024
        {
            return Err("Browser screenshot exceeds its image budget".into());
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        let viewport_width = after["width"]
            .as_f64()
            .filter(|v| *v > 0.0)
            .ok_or("Browser viewport unavailable")?;
        let viewport_height = after["height"]
            .as_f64()
            .filter(|v| *v > 0.0)
            .ok_or("Browser viewport unavailable")?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 8_000_000 {
            return Err("Browser image geometry exceeds its budget".into());
        }
        let mut page = page(&window, tab)?;
        page.title = self
            .titles
            .lock()
            .map_err(|_| "Browser unavailable")?
            .get(key)
            .cloned()
            .unwrap_or_default();
        self.browser_states
            .lock()
            .map_err(|_| "Browser unavailable")?
            .insert(
                key.into(),
                Receipt {
                    state: id.into(),
                    signature: before_signature,
                    width,
                    height,
                    viewport_width,
                    viewport_height,
                },
            );
        Ok(BrowserObservation {
            page,
            state_id: id.into(),
            width,
            height,
            png_base64: png,
        })
    }
}
fn signature(window: &Webview, metrics: &Value) -> Result<String, String> {
    Ok(format!(
        "{}:{}:{}",
        window.url().map_err(|_| "Browser URL unavailable")?,
        serde_json::to_string(metrics).map_err(|_| "Browser geometry unavailable")?,
        window
            .size()
            .map_err(|_| "Browser geometry unavailable")?
            .width
    ))
}

impl NativeHost {
    pub(crate) async fn owner_input(&self, key: &str, input: BrowserInput) -> Result<(), String> {
        self.browser_states
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(key);
        let view = self.app.get_webview(key).ok_or("Browser page closed")?;
        hide_cursor(&view);
        if let BrowserInput::Navigate { url } = input {
            let url = url::Url::parse(&url).map_err(|_| "Invalid browser URL")?;
            if !self.navigation_allowed(&url) {
                return Err("Browser target is not allowed".into());
            }
            let preferences = self
                .preferences
                .lock()
                .map_err(|_| "Browser unavailable")?
                .clone();
            configure(&view, &preferences).await?;
            self.targets
                .lock()
                .map_err(|_| "Browser unavailable")?
                .insert(key.into(), url.to_string());
            self.loading
                .lock()
                .map_err(|_| "Browser unavailable")?
                .insert(key.into(), true);
            self.changed();
            navigate(&view, url, &preferences).await
        } else {
            if matches!(
                &input,
                BrowserInput::Back | BrowserInput::Forward | BrowserInput::Reload
            ) {
                let preferences = self
                    .preferences
                    .lock()
                    .map_err(|_| "Browser unavailable")?
                    .clone();
                configure(&view, &preferences).await?;
            }
            platform::input(&view, input).await
        }
    }
}

fn cursor_script(value: &Value) -> String {
    format!(
        "({})({})",
        daat_locus_client_protocol::web::BROWSER_CURSOR_SCRIPT,
        value
    )
}
async fn cursor_visibility(window: &Webview, kind: &str) {
    let _ = evaluate(window, &cursor_script(&serde_json::json!({"kind":kind}))).await;
}
async fn show_cursor(window: &Webview, input: &BrowserInput) {
    use serde_json::json;
    let event = match input {
        BrowserInput::Move { x, y } => json!({"kind":"move","x":x,"y":y}),
        BrowserInput::Drag { x, y, to_x, to_y } => {
            json!({"kind":"drag","x":x,"y":y,"to_x":to_x,"to_y":to_y})
        }
        BrowserInput::Click { x, y, count, .. } => {
            json!({"kind":"click","x":x,"y":y,"count":count})
        }
        BrowserInput::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        } => json!({"kind":"scroll","x":x,"y":y,"delta_x":delta_x,"delta_y":delta_y}),
        BrowserInput::Type { .. } => json!({"kind":"type"}),
        BrowserInput::Key { .. } => json!({"kind":"key"}),
        _ => json!({"kind":"hide"}),
    };
    let _ = evaluate(window, &cursor_script(&event)).await;
}
pub(crate) fn hide_cursor(window: &Webview) {
    let _ = window.eval(cursor_script(&serde_json::json!({"kind":"hide"})));
}
#[cfg(feature = "web-contract")]
impl NativeHost {
    pub(crate) async fn cursor_probe(&self, key: &str) -> Option<Value> {
        let view = self.app.get_webview(key)?;
        let state = evaluate(
            &view,
            &cursor_script(&serde_json::json!({"kind":"inspect"})),
        )
        .await
        .ok()?;
        let png = platform::screenshot(&view).await.ok()?;
        Some(serde_json::json!({"state":state,"png":png}))
    }
}

pub(crate) async fn configure(
    view: &Webview,
    preferences: &crate::browser_preferences::Preferences,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        platform::configure(view, &preferences.locale()).await
    }
    #[cfg(target_os = "macos")]
    {
        platform::configure(view, &preferences.language_script()).await
    }
}

pub(crate) async fn navigate(
    view: &Webview,
    url: url::Url,
    _preferences: &crate::browser_preferences::Preferences,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        view.navigate(url)
            .map_err(|_| "Browser navigation failed".into())
    }
    #[cfg(target_os = "macos")]
    {
        platform::navigate(view, &url, &_preferences.locale()).await
    }
}
