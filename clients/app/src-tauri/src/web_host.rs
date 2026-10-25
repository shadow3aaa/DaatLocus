//! Native WebView hosting. No commands from third-party page JavaScript.
use daat_locus_client_protocol::web::{
    CAPTURE_SCRIPT, Capture, MAX_CAPTURE_BYTES, Operation, Reply, Request,
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{Manager, WebviewBuilder, WebviewUrl, WindowBuilder};
use tokio::sync::oneshot;

pub(crate) type AppliedBounds = (String, i32, i32, u32, u32);

struct Loading {
    id: String,
    started: bool,
    full: bool,
    reply: oneshot::Sender<Result<Capture, String>>,
}
#[derive(Clone)]
pub struct NativeHost {
    pub(crate) app: tauri::AppHandle,
    pub(crate) root: PathBuf,
    pub(crate) preferences: Arc<Mutex<crate::browser_preferences::Preferences>>,
    pub(crate) targets: Arc<Mutex<HashMap<String, String>>>,
    pub(crate) icons: Arc<Mutex<HashMap<String, String>>>,
    pub(crate) library: Arc<Mutex<crate::browser_data::Library>>,
    pub(crate) zooms: Arc<Mutex<HashMap<String, f64>>>,
    pub(crate) loading: Arc<Mutex<HashMap<String, bool>>>,
    forbidden_origin: String,
    pending: Arc<Mutex<HashMap<String, Loading>>>,
    visible: bool,
    pub(crate) panel: Arc<Mutex<Option<daat_locus_client_protocol::web::BrowserBounds>>>,
    pub(crate) panel_insets: Arc<Mutex<(f64, f64)>>,
    pub(crate) applied_bounds: Arc<Mutex<HashMap<String, AppliedBounds>>>,
    pub(crate) foreground: Arc<Mutex<Vec<daat_locus_client_protocol::web::BrowserHitRegion>>>,
    pub(crate) probe_visible: bool,
    pub(crate) tab_order: Arc<Mutex<Vec<String>>>,
    pub(crate) titles: Arc<Mutex<HashMap<String, String>>>,
    pub(crate) selected: Arc<Mutex<Option<String>>>,
    pub(crate) fullscreen: Arc<Mutex<Option<String>>>,
    pub(crate) browser_frames:
        Arc<Mutex<HashMap<String, VecDeque<crate::browser_use::ImageFrame>>>>,
}
pub(crate) fn allowed_url(value: &url::Url, forbidden: &str) -> bool {
    matches!(value.scheme(), "http" | "https")
        && value.host_str().is_some()
        && value.username().is_empty()
        && value.password().is_none()
        && value.origin().ascii_serialization() != forbidden
}
impl NativeHost {
    pub(crate) fn changed(&self) {
        use tauri::Emitter;
        if let Some(main) = self.app.get_webview("main") {
            let _ = main.emit("dl-browser-changed", ());
        }
    }

    pub fn new(app: tauri::AppHandle, root: PathBuf, forbidden_origin: String) -> Self {
        let library = crate::browser_data::Library::load(&root);
        Self {
            preferences: Arc::new(Mutex::new(crate::browser_preferences::Preferences::load(
                &root,
            ))),
            targets: Default::default(),
            icons: Default::default(),
            app,
            root,
            library: Arc::new(Mutex::new(library)),
            zooms: Default::default(),
            loading: Default::default(),
            forbidden_origin,
            pending: Arc::new(Mutex::new(HashMap::new())),
            visible: false,
            browser_frames: Default::default(),
            panel: Default::default(),
            panel_insets: Default::default(),
            applied_bounds: Default::default(),
            foreground: Default::default(),
            probe_visible: false,
            tab_order: Default::default(),
            titles: Default::default(),
            selected: Default::default(),
            fullscreen: Default::default(),
        }
    }
    pub(crate) fn navigation_allowed(&self, url: &url::Url) -> bool {
        allowed_url(url, &self.forbidden_origin)
            && !self
                .app
                .try_state::<crate::transport::Transport>()
                .is_some_and(|transport| transport.is_service_origin(url))
    }
    pub async fn execute(&self, request: Request) -> Reply {
        let id = request.id.clone();
        if let Operation::Browser { command } = &request.operation {
            let result = if request.validate() {
                self.browser_command(&request.profile, command.clone(), &id)
                    .await
            } else {
                Err("Invalid native browser request".into())
            };
            return Reply {
                id,
                capture: None,
                browser: result.as_ref().ok().cloned(),
                error: result.err(),
            };
        }
        let result = if request.validate() {
            self.dispatch(request).await
        } else {
            Err("Invalid native Web request".into())
        };
        match result {
            Ok(capture) => Reply {
                id,
                capture,
                error: None,
                browser: None,
            },
            Err(error) => Reply {
                id,
                capture: None,
                error: Some(error),
                browser: None,
            },
        }
    }
    async fn dispatch(&self, request: Request) -> Result<Option<Capture>, String> {
        let label = format!("web-{}", request.profile);
        self.load(request, label, true).await
    }
    pub(crate) async fn load(
        &self,
        request: Request,
        label: String,
        full: bool,
    ) -> Result<Option<Capture>, String> {
        if matches!(request.operation, Operation::Close) {
            self.pending
                .lock()
                .map_err(|_| "Web host unavailable")?
                .remove(&label);
            self.close_page(&label)?;
            return Ok(None);
        }
        let Operation::Load { url } = request.operation else {
            unreachable!()
        };
        let url = url::Url::parse(&url).map_err(|_| "Invalid Web URL")?;
        if !self.navigation_allowed(&url) {
            return Err("Web navigation target is not allowed".into());
        }
        let (reply, response) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| "Web host unavailable")?
            .insert(
                label.clone(),
                Loading {
                    id: request.id,
                    started: false,
                    full,
                    reply,
                },
            );
        let host = self.clone();
        let key = label.clone();
        self.app
            .run_on_main_thread(move || {
                if let Err(error) = host.create_page(&request.profile, &key, url, !full) {
                    host.fail(&key, &error);
                }
            })
            .map_err(|_| "Native Web event loop unavailable")?;
        match tokio::time::timeout(Duration::from_secs(25), response).await {
            Ok(Ok(result)) => result.map(Some),
            _ => {
                self.pending
                    .lock()
                    .map_err(|_| "Web host unavailable")?
                    .remove(&label);
                let _ = self.close_page(&label);
                Err("Web loading or capture timed out; navigation may have occurred".into())
            }
        }
    }
    pub(crate) fn create_page(
        &self,
        profile: &str,
        key: &str,
        url: url::Url,
        shared: bool,
    ) -> Result<(), String> {
        if let Some(view) = self.app.get_webview(key) {
            return view
                .navigate(url)
                .map_err(|_| "Web navigation failed".into());
        }
        let window = if shared {
            crate::browser_ui::parking_window(self, key)?
        } else {
            WindowBuilder::new(&self.app, key)
                .title("Web")
                .inner_size(1100.0, 800.0)
                .visible(self.visible)
                .focused(false)
                .build()
                .map_err(|_| "Web window unavailable")?
        };
        let navigation_host = self.clone();
        let loading_host = self.clone();
        let loading_key = key.to_owned();
        let title_host = self.clone();
        let title_key = key.to_owned();
        let download_host = self.clone();
        let popup_host = self.clone();
        let requested_url = url.clone();
        if shared && url.as_str() != "about:blank" {
            self.targets
                .lock()
                .map_err(|_| "Browser unavailable")?
                .insert(key.into(), url.to_string());
            self.loading
                .lock()
                .map_err(|_| "Browser unavailable")?
                .insert(key.into(), true);
        }
        let initial_url = if shared {
            url::Url::parse("about:blank").unwrap()
        } else {
            url
        };
        let mut builder = WebviewBuilder::new(key, WebviewUrl::External(initial_url))
            .focused(false)
            .on_navigation(move |url| {
                navigation_host.navigation_allowed(url) || shared && url.as_str() == "about:blank"
            })
            .on_new_window(move |url, _| {
                if shared && popup_host.navigation_allowed(&url) {
                    let host = popup_host.clone();
                    let profile =
                        daat_locus_client_protocol::web::SHARED_BROWSER_PROFILE.to_owned();
                    tauri::async_runtime::spawn(async move {
                        let key = format!("web-{profile}-{}", uuid::Uuid::new_v4().simple());
                        let _ = host
                            .load(
                                Request {
                                    id: uuid::Uuid::new_v4().simple().to_string(),
                                    profile,
                                    operation: Operation::Load { url: url.into() },
                                },
                                key,
                                false,
                            )
                            .await;
                    });
                }
                tauri::webview::NewWindowResponse::Deny
            })
            .on_download(move |_, event| {
                if !shared {
                    return false;
                }
                download_host.download(event)
            })
            .on_permission_request(|_, _| tauri::webview::PermissionResponse::Deny)
            .on_document_title_changed(move |view, title| {
                title_host.changed();
                title_host
                    .titles
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(title_key.clone(), title.clone());
                if !shared {
                    let _ = view.window().set_title(&title);
                } else if let Ok(url) = view.url() {
                    let mut library = title_host.library.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(visit) = library.history.last_mut()
                        && visit.url == url.as_str()
                    {
                        visit.title = title;
                        let _ = library.save(&title_host.root);
                    }
                }
            })
            .on_page_load(move |view, payload| {
                if shared
                    && payload.url().as_str() == "about:blank"
                    && loading_host
                        .targets
                        .lock()
                        .is_ok_and(|targets| targets.contains_key(&loading_key))
                {
                    return;
                }
                loading_host
                    .loading
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        loading_key.clone(),
                        matches!(payload.event(), tauri::webview::PageLoadEvent::Started),
                    );
                loading_host.changed();
                if shared
                    && payload.url().as_str() != "about:blank"
                    && matches!(payload.event(), tauri::webview::PageLoadEvent::Finished)
                {
                    loading_host
                        .targets
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&loading_key);
                }
                if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) {
                    if let Ok(mut pending) = loading_host.pending.lock()
                        && let Some(loading) = pending.get_mut(&loading_key)
                    {
                        loading.started = true;
                    }
                    return;
                }
                if shared && loading_host.navigation_allowed(payload.url()) {
                    let metadata = loading_host.clone();
                    let key = loading_key.clone();
                    let view = view.clone();
                    tauri::async_runtime::spawn(async move {
                        metadata.capture_icon(&view, &key).await;
                    });
                    let title = loading_host
                        .titles
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&loading_key)
                        .cloned()
                        .unwrap_or_default();
                    let mut library = loading_host
                        .library
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    library.visit(payload.url().as_str(), &title);
                    let _ = library.save(&loading_host.root);
                }
                let request = loading_host.pending.lock().ok().and_then(|pending| {
                    pending
                        .get(&loading_key)
                        .filter(|loading| loading.started)
                        .map(|loading| (loading.id.clone(), loading.full))
                });
                let Some((request_id, full)) = request else {
                    return;
                };
                let host = loading_host.clone();
                let key = loading_key.clone();
                let script = if full {
                    CAPTURE_SCRIPT
                } else {
                    "({url:location.href,title:document.title,html:'',notices:[]})"
                };
                if view
                    .eval_with_callback(script, move |value| {
                        host.complete(&key, &request_id, decode_capture(&value))
                    })
                    .is_err()
                {
                    loading_host.fail(&loading_key, "Web capture could not start");
                }
            });
        #[cfg(windows)]
        {
            // Retain the existing browser store while correcting the malformed
            // wire namespace. This directory name is local storage, not an ID.
            let directory = if profile == daat_locus_client_protocol::web::SHARED_BROWSER_PROFILE {
                format!("a{profile}")
            } else {
                profile.to_owned()
            };
            builder =
                builder.data_directory(self.root.join("runtime/web-profiles").join(directory));
        }
        #[cfg(target_os = "macos")]
        {
            builder =
                builder.with_webview_configuration(crate::browser_use::configuration(profile)?);
        }
        let offset = 0.0;
        let view = window
            .add_child(
                builder,
                tauri::LogicalPosition::new(0.0, offset),
                tauri::LogicalSize::new(1100.0, 800.0),
            )
            .map_err(|error| format!("Independent WebView could not start: {error}"))?;
        if shared {
            #[cfg(any(windows, target_os = "macos"))]
            crate::browser_use::shortcuts(&view, self.app.clone());
            #[cfg(windows)]
            crate::browser_ui::watch_fullscreen(self, &view, key);
            self.tab_order
                .lock()
                .map_err(|_| "Browser unavailable")?
                .push(key.to_owned());
            let preferences = self
                .preferences
                .lock()
                .map_err(|_| "Browser unavailable")?
                .clone();
            view.set_zoom(preferences.default_zoom)
                .map_err(|e| e.to_string())?;
            self.zooms
                .lock()
                .map_err(|_| "Browser unavailable")?
                .insert(key.into(), preferences.default_zoom);
            view.show().map_err(|error| error.to_string())?;
            if requested_url.as_str() != "about:blank" {
                let host = self.clone();
                let key = key.to_owned();
                let view = view.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = async {
                        crate::browser_use::configure(&view, &preferences).await?;
                        crate::browser_use::navigate(&view, requested_url, &preferences).await
                    }
                    .await
                    {
                        host.fail(&key, &error);
                        host.loading
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(key, false);
                        host.changed();
                    }
                });
            }
            self.changed();
        } else {
            let _ = view.set_auto_resize(true);
            let host = self.clone();
            let key = key.to_owned();
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    host.fail(&key, "Web page closed");
                    host.titles
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&key);
                }
            });
        }
        #[cfg(target_os = "macos")]
        crate::browser_use::prepare(&view, shared);
        Ok(())
    }
    pub(crate) fn close_page(&self, label: &str) -> Result<(), String> {
        // Closing the fullscreen page restores the shell before it goes away.
        if self
            .fullscreen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            == Some(label)
        {
            crate::browser_ui::clear_page_fullscreen(self);
        }
        self.browser_frames
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(label);
        self.targets
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(label);
        self.icons
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(label);
        self.loading
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(label);
        self.zooms
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(label);
        self.applied_bounds
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(label);
        #[cfg(target_os = "macos")]
        {
            let label = label.to_owned();
            let _ = self
                .app
                .run_on_main_thread(move || crate::browser_use::remove_shortcuts(&label));
        }
        let adjacent = {
            let mut order = self.tab_order.lock().map_err(|_| "Browser unavailable")?;
            let index = order.iter().position(|key| key == label);
            order.retain(|key| key != label);
            index
                .and_then(|index| order.get(index.min(order.len().saturating_sub(1))))
                .cloned()
        };
        self.titles
            .lock()
            .map_err(|_| "Browser unavailable")?
            .remove(label);
        if let Some(view) = self.app.get_webview(label) {
            let window = view.window();
            if window.label() == label {
                window.close().map_err(|_| "Web window could not close")?;
            } else {
                view.close().map_err(|_| "Browser page could not close")?;
            }
        }
        if let Some(parking) = self.app.get_window(label) {
            parking
                .close()
                .map_err(|_| "Browser page window could not close")?;
        }
        let was_selected = self
            .selected
            .lock()
            .map_err(|_| "Browser unavailable")?
            .as_deref()
            == Some(label);
        if was_selected {
            *self.selected.lock().map_err(|_| "Browser unavailable")? = None;
            if let Some(next) = adjacent {
                self.select_page(&next, false)?;
            }
        }
        Ok(())
    }
    pub(crate) fn is_browser_page(&self, key: &str) -> bool {
        self.tab_order
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|id| id == key)
    }
    pub(crate) fn select_page(&self, label: &str, focus: bool) -> Result<(), String> {
        let view = self.app.get_webview(label).ok_or("Browser page closed")?;
        if self
            .selected
            .lock()
            .map_err(|_| "Browser unavailable")?
            .as_deref()
            == Some(label)
        {
            if focus {
                view.set_focus()
                    .map_err(|_| "Browser page could not focus")?;
            }
            return Ok(());
        }
        // Switching pages while another is fullscreen restores the shell first.
        let displaced = {
            let current = self
                .fullscreen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            current.filter(|key| key != label)
        };
        if let Some(key) = displaced {
            crate::browser_ui::apply_page_fullscreen(self, &key, false);
        }
        *self.selected.lock().map_err(|_| "Browser unavailable")? = Some(label.into());
        crate::browser_ui::present_selected(self)?;
        if focus {
            view.set_focus()
                .map_err(|_| "Browser page could not focus")?;
        }
        self.changed();
        Ok(())
    }
    pub(crate) async fn capture_icon(&self, view: &tauri::Webview, key: &str) {
        let Ok(url) = view.url() else { return };
        let Some(icon) = crate::browser_use::favicon(view).await else {
            return;
        };
        if view.url().ok().as_ref() != Some(&url) || !self.is_browser_page(key) {
            return;
        }
        self.icons
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.into(), icon.clone());
        let mut library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        for visit in &mut library.history {
            if url::Url::parse(&visit.url).is_ok_and(|old| old.origin() == url.origin()) {
                visit.favicon = Some(icon.clone());
            }
        }
        let _ = library.save(&self.root);
        drop(library);
        self.changed();
    }
    fn complete(&self, label: &str, id: &str, result: Result<Capture, String>) {
        if let Ok(mut pending) = self.pending.lock()
            && pending.get(label).is_some_and(|loading| loading.id == id)
            && let Some(loading) = pending.remove(label)
        {
            let _ = loading.reply.send(result);
        }
    }
    fn fail(&self, label: &str, error: &str) {
        if let Ok(mut pending) = self.pending.lock()
            && let Some(loading) = pending.remove(label)
        {
            let _ = loading.reply.send(Err(error.into()));
        }
    }
    #[cfg(feature = "web-contract")]
    pub fn close_all(&self) {
        for (label, _) in self.app.webviews() {
            if label.starts_with("web-") {
                let _ = self.close_page(&label);
            }
        }
    }
}
fn decode_capture(value: &str) -> Result<Capture, String> {
    if value.len() > MAX_CAPTURE_BYTES * 3 {
        return Err("Web capture exceeds its size limit".into());
    }
    let mut data: serde_json::Value =
        serde_json::from_str(value).map_err(|_| "Invalid Web capture result")?;
    if let Some(encoded) = data.as_str() {
        data = serde_json::from_str(encoded).map_err(|_| "Invalid encoded Web capture")?;
    }
    if let Some(error) = data.get("error").and_then(|error| error.as_str()) {
        return Err(error.into());
    }
    let capture: Capture =
        serde_json::from_value(data).map_err(|_| "Web capture did not contain a document")?;
    if capture.html.len() > MAX_CAPTURE_BYTES {
        return Err("Web document exceeds its size limit".into());
    }
    Ok(capture)
}

pub fn start(app: tauri::AppHandle, backend: crate::backend::Backend, origin: String) {
    let host = NativeHost::new(app.clone(), backend.root.clone(), origin);
    app.manage(host.clone());
    tauri::async_runtime::spawn(async move {
        loop {
            let _ = bridge(&host, &backend).await;
            for (key, _view) in host.app.webviews() {
                if key.starts_with("web-") && !host.is_browser_page(&key) {
                    let _ = host.close_page(&key);
                }
            }
            // Reconnect the infrastructure only; an interrupted navigation is
            // never replayed, and existing model calls receive their failure.
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
}
async fn bridge(host: &NativeHost, backend: &crate::backend::Backend) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut child = backend
        .web_host()
        .map_err(|_| "Web host bridge could not start")?;
    let mut stdout = child.stdout.take().ok_or("Web host bridge has no output")?;
    let mut stdin = child.stdin.take().ok_or("Web host bridge has no input")?;
    loop {
        let size = stdout
            .read_u32()
            .await
            .map_err(|_| "Web host bridge disconnected")? as usize;
        if size == 0 || size > 64 * 1024 {
            return Err("Invalid Web host command size".into());
        }
        let mut bytes = vec![0; size];
        stdout
            .read_exact(&mut bytes)
            .await
            .map_err(|_| "Web host bridge command incomplete")?;
        let request: Request =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid Web host command")?;
        let reply = host.execute(request).await;
        let bytes = serde_json::to_vec(&reply).map_err(|_| "Cannot encode Web capture")?;
        stdin
            .write_u32(bytes.len() as u32)
            .await
            .map_err(|_| "Web host bridge disconnected")?;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|_| "Web host reply incomplete")?;
        stdin
            .flush()
            .await
            .map_err(|_| "Web host reply incomplete")?;
    }
}

/// Opt-in native integration probe. Absent from distribution builds.
#[cfg(feature = "web-contract")]
pub fn probe(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Manifest {
        #[serde(default)]
        urls: Vec<String>,
        #[serde(default)]
        shell_url: Option<url::Url>,
        #[serde(default)]
        owner_clicks: HashMap<usize, Vec<(f64, f64)>>,
        #[serde(default)]
        owner_text: HashMap<usize, String>,
        output: PathBuf,
        #[serde(default)]
        browser_steps: Vec<daat_locus_client_protocol::web::BrowserCommand>,
        #[serde(default)]
        relative_points: bool,
        #[serde(default)]
        hidden_browser: bool,
        #[serde(default)]
        panel_steps: HashMap<usize, Option<daat_locus_client_protocol::web::BrowserBounds>>,
        #[serde(default)]
        owner_select: HashMap<usize, String>,
        #[serde(default)]
        inspect_tabs: bool,
        #[serde(default)]
        owner_steps:
            std::collections::HashMap<usize, daat_locus_client_protocol::web::BrowserInput>,
        #[serde(default)]
        ui_review: bool,
        #[serde(default)]
        ui_targets: HashMap<usize, String>,
        #[serde(default)]
        cursor_frames: Vec<usize>,
        #[serde(default)]
        owner_find: HashMap<usize, Vec<(String, String)>>,
        #[serde(default)]
        owner_screenshots: Vec<usize>,
        #[serde(default)]
        owner_moves: HashMap<usize, Vec<(String, Option<String>)>>,
        #[serde(default)]
        owner_foreground: HashMap<usize, Vec<daat_locus_client_protocol::web::BrowserHitRegion>>,
        #[serde(default)]
        delays: HashMap<usize, u64>,
        #[serde(default)]
        resize_steps: HashMap<usize, Vec<(u32, u32)>>,
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() > 64 * 1024 {
        return Err("Web probe manifest exceeds its limit".into());
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if (manifest.urls.is_empty() && manifest.browser_steps.is_empty())
        || manifest.urls.len() > 16
        || manifest.browser_steps.len() > 100
        || !manifest.output.is_absolute()
    {
        return Err("Invalid Web probe manifest".into());
    }
    let root = path
        .parent()
        .ok_or("Web probe directory unavailable")?
        .to_owned();
    let app = tauri::Builder::default()
        .on_window_event(|window,event|{if matches!(window.label(),"main"|"browser")&&matches!(event,tauri::WindowEvent::Resized(_)|tauri::WindowEvent::ScaleFactorChanged{..}){let _=crate::browser_ui::resize(window.app_handle());}})
        .invoke_handler(tauri::generate_handler![crate::browser_ui::browser_control])
        .setup(move |app| {
            let app = app.handle().clone();
            let mut host = NativeHost::new(app.clone(), root.clone(), "http://127.0.0.1:1".into());
            host.visible = !manifest.browser_steps.is_empty();
            host.probe_visible = !manifest.hidden_browser;
            app.manage(host.clone());
            crate::transport::capability(&app)?;
            if !manifest.panel_steps.is_empty(){
                let window=WindowBuilder::new(&app,"main").inner_size(1280.0,1000.0).visible(!manifest.hidden_browser).focused(false).build()?;
                if let Some(url)=&manifest.shell_url {
                    if url.scheme()!="http"||url.host_str()!=Some("127.0.0.1"){return Err("Probe shell must use a local fixture".into());}
                    window.add_child(WebviewBuilder::new("main",WebviewUrl::External(url.clone())).background_color(tauri::webview::Color(0,0,0,0)).auto_resize().disable_drag_drop_handler(),tauri::LogicalPosition::new(0.0,0.0),tauri::LogicalSize::new(1280.0,1000.0))?;
                }
            }
            tauri::async_runtime::spawn(async move {
                use sha2::{Digest, Sha256};
                let mut results = Vec::new();
                for url in manifest.urls {
                    let request = Request {
                        id: hex::encode(&Sha256::digest(url.as_bytes())[..16]),
                        profile: hex::encode(Sha256::digest(url.as_bytes())),
                        operation: Operation::Load { url: url.clone() },
                    };
                    let reply = host.execute(request).await;
                    results.push(serde_json::json!({"url":url,"reply":reply}));
                }
                let profile =
                    hex::encode(Sha256::digest(format!("browser-probe:{}", root.display())));
                let mut observations =
                    HashMap::<String, daat_locus_client_protocol::web::BrowserObservation>::new();
                let mut first = HashMap::<String, String>::new();
                for (index, mut command) in manifest.browser_steps.into_iter().enumerate() {
                    if let Some(delay)=manifest.delays.get(&index){tokio::time::sleep(Duration::from_millis((*delay).min(3000))).await;}
                    if let Some(bounds)=manifest.panel_steps.get(&index){
                        let bounds=bounds.map(|mut bounds|{bounds.scale=host.app.get_window("main").unwrap().scale_factor().unwrap();bounds});
                        crate::browser_ui::set_panel(&host,bounds).await.unwrap();
                    }
                    if let Some(tab) = manifest.owner_select.get(&index) {
                        host.select_page(&format!("web-{profile}-{tab}"), false).unwrap();
                    }
                    if let Some(input) = manifest.owner_steps.get(&index) {
                        let key=host.selected.lock().unwrap().clone().unwrap();
                        host.owner_input(&key,input.clone()).await.unwrap();
                    }
                    // UI fixtures include background app state. Inspect immediately
                    // before each input; never replay an input after an uncertain result.
                    if manifest.ui_review
                        && let daat_locus_client_protocol::web::BrowserCommand::Input{tab,..}=&command
                        && let Ok(reply)=host.browser_command(&profile,daat_locus_client_protocol::web::BrowserCommand::Observe{tab:tab.clone()},&uuid::Uuid::new_v4().simple().to_string()).await
                        && let Some(observation)=reply.observation {
                        observations.insert(tab.clone(),observation);
                    }
                    if let Some(target)=manifest.ui_targets.get(&index) && let daat_locus_client_protocol::web::BrowserCommand::Input{tab,input,..}=&mut command {
                        let key=format!("web-{profile}-{tab}");let view=host.app.get_webview(&key).unwrap();
                        let selector=serde_json::to_string(target).unwrap();
                        let script=format!("(()=>{{const target={selector};const root=document.querySelector('.dl-browser-page')||document;const nodes=[...root.querySelectorAll('button,input'),...document.querySelectorAll('[role=menuitem], [data-slot=popover-content] button')];const node=nodes.find(node=>{{const rect=node.getBoundingClientRect();return !node.disabled&&rect.width>0&&rect.height>0&&(target.startsWith('aria:')?node.getAttribute('aria-label')===target.slice(5):target.startsWith('tab:')?node.getAttribute('role')==='tab'&&node.textContent.trim()===target.slice(4):target.startsWith('menu:')?node.getAttribute('role')==='menuitem'&&node.textContent.trim().startsWith(target.slice(5)):node.textContent.trim().startsWith(target.slice(5)))}});if(!node)return {{error:'Target missing'}};const rect=node.getBoundingClientRect();return {{x:rect.x+rect.width/2,y:rect.y+rect.height/2,width:innerWidth,height:innerHeight}}}})()");
                        let target=crate::browser_use::evaluate(&view,&script).await.unwrap();
                        if target.get("error").is_some(){let _=std::fs::write(&manifest.output,format!("Missing UI target at step {index}: {target:?}"));host.close_all();host.app.exit(1);return;}
                        if let daat_locus_client_protocol::web::BrowserInput::Click{x,y,..}=input {
                            let receipt=observations.get(tab).unwrap();
                            *x=target["x"].as_f64().unwrap()/target["width"].as_f64().unwrap();*y=target["y"].as_f64().unwrap()/target["height"].as_f64().unwrap();
                            if !manifest.relative_points {*x=target["x"].as_f64().unwrap()*f64::from(receipt.width)/target["width"].as_f64().unwrap();*y=target["y"].as_f64().unwrap()*f64::from(receipt.height)/target["height"].as_f64().unwrap();}
                        }
                    }
                    if let daat_locus_client_protocol::web::BrowserCommand::Input {
                        tab,
                        state_id,
                        input,
                    } = &mut command
                        && let Some(observation) = observations.get(tab)
                    {
                        if state_id == "latest" {
                            *state_id = observation.state_id.clone();
                        }
                        if state_id == "first" {
                            *state_id = first.get(tab).unwrap().clone();
                        }
                        if manifest.relative_points {
                            match input {
                                daat_locus_client_protocol::web::BrowserInput::Click {
                                    x,
                                    y,
                                    ..
                                }
                                | daat_locus_client_protocol::web::BrowserInput::Scroll {
                                    x,
                                    y,
                                    ..
                                }
                                | daat_locus_client_protocol::web::BrowserInput::Move { x, y } => {
                                    *x *= f64::from(observation.width);
                                    *y *= f64::from(observation.height);
                                }
                                daat_locus_client_protocol::web::BrowserInput::Drag {
                                    x,
                                    y,
                                    to_x,
                                    to_y,
                                } => {
                                    *x *= f64::from(observation.width);
                                    *y *= f64::from(observation.height);
                                    *to_x *= f64::from(observation.width);
                                    *to_y *= f64::from(observation.height);
                                }
                                _ => {}
                            }
                        }
                    }
                    let request = Request {
                        id: hex::encode(&Sha256::digest(format!("{profile}:{index}"))[..16]),
                        profile: profile.clone(),
                        operation: Operation::Browser {
                            command: command.clone(),
                        },
                    };
                    let focus_before = crate::browser_use::input_focus(&host.app).await;
                    let reply = host.execute(request).await;
                    let focus_after = crate::browser_use::input_focus(&host.app).await;
                    if let Some(observation) = reply
                        .browser
                        .as_ref()
                        .and_then(|browser| browser.observation.as_ref())
                    {
                        first
                            .entry(observation.page.tab.clone())
                            .or_insert(observation.state_id.clone());
                        observations.insert(observation.page.tab.clone(), observation.clone());
                    }
                    let selected=host.selected.lock().unwrap().clone();
                    if let Some(moves)=manifest.owner_moves.get(&index){
                        for (tab,before) in moves{
                            let key=format!("web-{profile}-{tab}");let before=before.as_ref().map(|tab|format!("web-{profile}-{tab}"));
                            if crate::browser_ui::move_tab(&host,&key,before.as_deref()).is_err(){app.exit(1);return;}
                        }
                    }
                    if let Some(cuts)=manifest.owner_foreground.get(&index){
                        *host.foreground.lock().unwrap()=cuts.clone();
                        if crate::browser_ui::apply_input_regions(&host).await.is_err(){app.exit(1);return;}
                    }
                    if manifest.shell_url.is_some()&&let Some(shell)=host.app.get_webview("main") {
                        let mut value=serde_json::Value::Null;
                        for _ in 0..50 {
                            value=crate::browser_use::evaluate(&shell,"window.DLComposition?.browserForeground(document.querySelector('#viewport'))").await.unwrap();
                            if value.is_array(){break;}tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        if !value.is_array(){let _=std::fs::write(&manifest.output,"Production DOM collector did not load");app.exit(1);return;}
                        *host.foreground.lock().unwrap()=serde_json::from_value(value).unwrap();
                        crate::browser_ui::apply_input_regions(&host).await.unwrap();
                    }
                    #[cfg(windows)]
                    if let Some(points)=manifest.owner_clicks.get(&index) {
                        for &(x,y) in points{crate::browser_use::owner_click(&host.app,x,y).await.unwrap();}
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    #[cfg(windows)]
                    if let Some(text)=manifest.owner_text.get(&index){crate::browser_use::owner_text(&host.app,text.clone()).await.unwrap();tokio::time::sleep(Duration::from_millis(100)).await;}
                    #[cfg(windows)]
                    let layer=if let Some(key)=&selected&&let Some(view)=host.app.get_webview(key){Some(crate::browser_use::composition_probe(&view).await.unwrap_or_else(|error|serde_json::json!({"error":error})))}else{None};
                    #[cfg(not(windows))]
                    let layer:Option<serde_json::Value>=None;
                    let shell=if manifest.shell_url.is_some()&&let Some(shell)=host.app.get_webview("main") {crate::browser_use::evaluate(&shell,"({clicks:window.clicks||0,tips:window.tips||0,regions:window.DLComposition?.browserForeground(document.querySelector('#viewport')),width:innerWidth,height:innerHeight})").await.ok()}else{None};
                    let owner_workspace=serde_json::json!({"pages":host.pages(&profile),"selected":selected.as_deref().and_then(|key|key.strip_prefix(&format!("web-{profile}-")))});
                    let view=selected.as_deref().and_then(|key|host.app.get_webview(key));
                    #[cfg(target_os = "macos")]
                    if manifest.ui_review && matches!(&command,daat_locus_client_protocol::web::BrowserCommand::Open{..}) && let Some(view)=&view {crate::browser_use::ui_probe_visibility(view);}
                    let parent=view.as_ref().map(|view|view.window().label().to_owned());
                    let mut owner_find=Vec::new();
                    if let Some(queries)=manifest.owner_find.get(&index) && let Some(view)=&view {
                        for (action,text) in queries {
                            if !matches!(action.as_str(),"find"|"find_next"|"find_previous"|"find_close") {app.exit(1);return;}
                            owner_find.push(crate::browser_use::find(view,action,text).await);
                        }
                    }
                    let screenshot=if manifest.owner_screenshots.contains(&index) && let Some(view)=&view {
                        let captured=crate::browser_use::page_screenshot(view).await;
                        match captured {
                            Ok(png)=>{
                                let copied=crate::browser_ui::copy_screenshot(&png).await;
                                let clipboard=if copied.is_ok(){arboard::Clipboard::new().and_then(|mut clipboard|clipboard.get_image()).ok().map(|image|serde_json::json!({"width":image.width,"height":image.height,"pixel":image.bytes.get(..4)}))}else{None};
                                Some(serde_json::json!({"png":png,"copied":copied,"clipboard":clipboard}))
                            }
                            Err(error)=>Some(serde_json::json!({"error":error})),
                        }
                    }else{None};
                    let native_control=if let Some(view)=&view{crate::browser_use::evaluate(view,"({status:window.nativeControlResult || null})").await.ok()}else{None};
                    let cursor=if let Some(key)=&selected {host.cursor_probe(key).await}else{None};
                    let ui=if manifest.ui_review {if let Some(view)=selected.as_deref().and_then(|key|host.app.get_webview(key)) {
                        crate::browser_use::evaluate(&view,"({text:[...document.querySelectorAll('.dl-browser-page,[data-slot=popover-content]')].map(node=>node.innerText).join('\\n'),downloadPopup:(()=>{const node=document.querySelector('.dl-browser-downloads');if(!node)return null;const rect=node.getBoundingClientRect();const list=node.querySelector('.dl-browser-download-scroll');return {x:rect.x,y:rect.y,width:rect.width,height:rect.height,scrollTop:list.scrollTop,scrollHeight:list.scrollHeight,clientHeight:list.clientHeight}})(),tabs:Array.from(document.querySelectorAll('[role=tab]')).map(node=>({text:node.textContent,selected:node.getAttribute('aria-selected')==='true',draggable:node.parentElement.draggable})),surface:{live:[...document.querySelectorAll('.dl-browser-viewport>iframe')].some(node=>getComputedStyle(node).display!=='none'),clipped:[...document.querySelectorAll('.dl-browser-viewport>iframe')].some(node=>getComputedStyle(node).maskImage!=='none')},language:document.documentElement.lang,width:innerWidth,height:innerHeight,scrollWidth:document.documentElement.scrollWidth,inputs:Array.from(document.querySelectorAll('input')).map(input=>({label:input.getAttribute('aria-label'),value:input.value})),buttons:Array.from(document.querySelectorAll('button')).map(button=>({label:button.getAttribute('aria-label')||button.innerText,disabled:button.disabled,rect:{x:button.getBoundingClientRect().x,y:button.getBoundingClientRect().y,width:button.getBoundingClientRect().width,height:button.getBoundingClientRect().height}}))})").await.ok()
                    }else{None}}else{None};
                    let mut cursor_frames=Vec::new();
                    if manifest.cursor_frames.contains(&index) && let Some(key)=&selected {for _ in 0..2{tokio::time::sleep(Duration::from_millis(100)).await;cursor_frames.push(host.cursor_probe(key).await);}}
                    let mut resize_checks=Vec::new();
                    if let Some(sizes)=manifest.resize_steps.get(&index) && let Some(window)=host.app.get_window("main") {
                        for (width,height) in sizes {
                            let start=std::time::Instant::now();
                            window.set_size(tauri::LogicalSize::new(*width,*height)).unwrap();
                            // Read through the real page loop after native resize delivery.
                            tokio::time::sleep(Duration::from_millis(40)).await;
                            let dimensions=if let Some(view)=&view {crate::browser_use::evaluate(view,"({width:innerWidth,height:innerHeight,language:navigator.language,languages:navigator.languages,locale:Intl.DateTimeFormat().resolvedOptions().locale})").await.ok()} else {None};
                            resize_checks.push(serde_json::json!({"requested":[width,height],"page":dimensions,"elapsed_ms":start.elapsed().as_millis(),"panel":*host.panel.lock().unwrap()}));
                        }
                    }
                    let metadata=if let Some(view)=&view {crate::browser_use::evaluate(view,"({language:navigator.language,languages:navigator.languages,locale:Intl.DateTimeFormat().resolvedOptions().locale,header:document.querySelector('#language')?.textContent})").await.ok()}else{None};
                    let mut tab_states=serde_json::Map::new();
                    if manifest.inspect_tabs {
                        for page in host.pages(&profile) {
                            if let Some(view)=host.app.get_webview(&format!("web-{profile}-{}",page.tab)) {
                                let state=crate::browser_use::evaluate(&view,"({value:document.querySelector('input')?.value,active:document.activeElement?.tagName,focus:document.hasFocus(),scrollY,width:innerWidth,height:innerHeight})").await.unwrap();
                                tab_states.insert(page.tab,state);
                            }
                        }
                    }
                    let icons=host.icons.lock().unwrap().clone();
                    let library=if manifest.ui_review {serde_json::to_value(host.library.lock().unwrap().clone()).ok()}else{None};
                    results.push(serde_json::json!({"command":command,"reply":reply,"parent":parent,"native_control":native_control,"cursor":cursor,"cursor_frames":cursor_frames,"ui":ui,"library":library,"owner_find":owner_find,"screenshot":screenshot,"layer":layer,"owner_workspace":owner_workspace,"shell":shell,"metadata":metadata,"icons":icons,"resize_checks":resize_checks,"focus_before":focus_before,"focus_after":focus_after,"tab_states":tab_states}));
                }
                let result = serde_json::to_vec(&results)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| {
                        std::fs::write(manifest.output, bytes).map_err(|error| error.to_string())
                    });
                host.close_all();
                app.exit(if result.is_ok() { 0 } else { 1 });
            });
            Ok(())
        })
        .build(crate::desktop::context())?;
    app.run(|_, event| {
        if let tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
    Ok(())
}
