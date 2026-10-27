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
pub(crate) use platform::prepare;
#[cfg(target_os = "macos")]
pub(crate) use platform::remove_shortcuts;
#[cfg(any(windows, target_os = "macos"))]
pub(crate) use platform::shortcuts;
#[cfg(all(target_os = "macos", feature = "web-contract"))]
pub(crate) use platform::ui_probe_visibility;
#[cfg(target_os = "macos")]
#[path = "browser_use_macos.rs"]
mod platform;

#[derive(Clone)]
pub(crate) struct ImageFrame {
    pub state: String,
    pub width: u32,
    pub height: u32,
}
// Fixed viewport measurements only. Page mutations never gate native input.
const METRICS: &str = daat_locus_client_protocol::web::BROWSER_METRICS_SCRIPT;
pub(crate) async fn navigation(view: &Webview) -> Result<(bool, bool), String> {
    platform::navigation(view).await
}
pub(crate) async fn compose(
    view: &Webview,
    bounds: Option<daat_locus_client_protocol::web::BrowserBounds>,
    cuts: Vec<daat_locus_client_protocol::web::BrowserHitRegion>,
) -> Result<(), String> {
    platform::compose(view, bounds, cuts).await
}
#[cfg(all(windows, feature = "web-contract"))]
pub(crate) use platform::{owner_click, owner_text};
#[cfg(all(windows, feature = "web-contract"))]
pub(crate) async fn composition_probe(view: &Webview) -> Result<Value, String> {
    let mut value = platform::composition_probe(view).await?;
    value["background"] = evaluate(view, include_str!("browser-background.js")).await?;
    Ok(value)
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
        platform::find(view, action, text).await
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
    let value: Value = serde_json::from_str(&value).map_err(|_| "Invalid browser inspection")?;
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
                // A reference supplies coordinate dimensions, not permission or freshness.
                // Keep old frames usable while the page and the Owner remain live.
                let frame = self
                    .browser_frames
                    .lock()
                    .map_err(|_| "Browser unavailable")?
                    .get(&key)
                    .and_then(|frames| {
                        frames
                            .iter()
                            .find(|frame| frame.state == state_id)
                            .or_else(|| frames.back())
                    })
                    .cloned();
                let needs_point = matches!(
                    &input,
                    BrowserInput::Move { .. }
                        | BrowserInput::Drag { .. }
                        | BrowserInput::Click { .. }
                        | BrowserInput::Scroll { .. }
                );
                let viewport = if needs_point {
                    Some(evaluate(&window, METRICS).await?)
                } else {
                    None
                };
                let size = window.size().map_err(|_| "Browser viewport unavailable")?;
                let (image_width, image_height) = frame
                    .map(|frame| (frame.width, frame.height))
                    .unwrap_or((size.width.max(1), size.height.max(1)));
                let map = |x: f64, y: f64| -> Result<(f64, f64), String> {
                    if !x.is_finite()
                        || !y.is_finite()
                        || x < 0.0
                        || y < 0.0
                        || x >= f64::from(image_width)
                        || y >= f64::from(image_height)
                    {
                        return Err("Input point is outside the coordinate image".into());
                    }
                    let viewport = viewport.as_ref().ok_or("Browser viewport unavailable")?;
                    let width = viewport["width"]
                        .as_f64()
                        .filter(|width| *width > 0.0)
                        .ok_or("Browser viewport unavailable")?;
                    let height = viewport["height"]
                        .as_f64()
                        .filter(|height| *height > 0.0)
                        .ok_or("Browser viewport unavailable")?;
                    Ok((
                        x / f64::from(image_width) * width,
                        y / f64::from(image_height) * height,
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
                // The cursor is decoration; Owner interaction can continue during it.
                show_cursor(&window, &input).await;
                if self.app.get_webview(&key).is_none() {
                    return Err("Browser page is closed".into());
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
                    // Canvas/native UI engines commonly route wheel input using
                    // their last pointer-move position, not the wheel's own x/y.
                    // Match physical input without moving the OS pointer/focus.
                    if let BrowserInput::Scroll { x, y, .. } = &input {
                        platform::input(&window, BrowserInput::Move { x: *x, y: *y }).await?;
                    }
                    platform::input(&window, input).await?;
                }
                // Give native input/layout and normal navigation a chance to settle.
                tokio::time::sleep(Duration::from_millis(180)).await;
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
        let window = self.app.get_webview(key).ok_or("Browser page is closed")?;
        cursor_visibility(&window, "prepare").await;
        if !window
            .url()
            .is_ok_and(|url| url.as_str() == "about:blank" || self.navigation_allowed(&url))
        {
            return Err("Browser page is still loading or its URL is not allowed".into());
        }
        // Capture the live page once; animations, scrolling and DOM updates are normal.
        cursor_visibility(&window, "suspend").await;
        let captured = platform::screenshot(&window).await;
        cursor_visibility(&window, "resume").await;
        let png = captured?;
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
        {
            let mut pages = self
                .browser_frames
                .lock()
                .map_err(|_| "Browser unavailable")?;
            let frames = pages.entry(key.into()).or_default();
            frames.push_back(ImageFrame {
                state: id.into(),
                width,
                height,
            });
            // Bounded geometry cache only. Evicted references fall back to the latest
            // dimensions and never turn a live page into a stale-state error.
            while frames.len() > 128 {
                frames.pop_front();
            }
        }
        Ok(BrowserObservation {
            page,
            state_id: id.into(),
            width,
            height,
            png_base64: png,
        })
    }
}
impl NativeHost {
    pub(crate) async fn owner_input(&self, key: &str, input: BrowserInput) -> Result<(), String> {
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

#[cfg(feature = "web-contract")]
pub(crate) async fn input_focus(app: &tauri::AppHandle) -> Result<Value, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(platform::input_focus());
    })
    .map_err(|error| error.to_string())?;
    rx.await.map_err(|error| error.to_string())
}
