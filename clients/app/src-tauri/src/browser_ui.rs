//! Owner-only full-page browser surface within the main client WebView.
use crate::web_host::NativeHost;
use daat_locus_client_protocol::web::{
    BrowserBounds, BrowserHitRegion, BrowserInput, SHARED_BROWSER_PROFILE,
};
use tauri::{Manager, Webview, Window, WindowBuilder};

// Keep browser pages alive off-route. This window has no chrome or command grants.
pub(crate) fn ensure_window(host: &NativeHost) -> Result<Window, String> {
    if let Some(window) = host.app.get_window("browser") {
        return Ok(window);
    }
    WindowBuilder::new(&host.app, "browser")
        .title("Browser")
        .inner_size(1100.0, 800.0)
        .visible(host.probe_visible)
        .focused(false)
        .build()
        .map_err(|e| e.to_string())
}
pub(crate) fn page_window(host: &NativeHost) -> Result<Window, String> {
    if host
        .panel
        .lock()
        .map_err(|_| "Browser unavailable")?
        .is_some()
    {
        host.app
            .get_window("main")
            .ok_or("Client unavailable".into())
    } else {
        ensure_window(host)
    }
}
pub(crate) fn resize(app: &tauri::AppHandle) -> Result<(), String> {
    let Some(host) = app.try_state::<NativeHost>() else {
        return Ok(());
    };
    let panel = *host.panel.lock().unwrap_or_else(|e| e.into_inner());
    let Some(window) = app.get_window(if panel.is_some() { "main" } else { "browser" }) else {
        return Ok(());
    };
    let Ok(size) = window.inner_size() else {
        return Ok(());
    };
    let bounds = panel.map(|mut bounds| {
        let scale = window.scale_factor().unwrap_or(bounds.scale);
        let (right, bottom) = *host.panel_insets.lock().unwrap_or_else(|e| e.into_inner());
        bounds.width = (f64::from(size.width) / scale - bounds.x - right).max(1.0);
        bounds.height = (f64::from(size.height) / scale - bounds.y - bottom).max(1.0);
        bounds.scale = scale;
        *host.panel.lock().unwrap_or_else(|e| e.into_inner()) = Some(bounds);
        bounds
    });
    let (x, y, width, height) = bounds
        .map(|bounds| {
            (
                (bounds.x * bounds.scale).round() as i32,
                (bounds.y * bounds.scale).round() as i32,
                (bounds.width * bounds.scale).round() as u32,
                (bounds.height * bounds.scale).round() as u32,
            )
        })
        .unwrap_or((0, 0, size.width, size.height));
    let selected = host
        .selected
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(key) = selected
        && let Some(view) = app.get_webview(&key)
    {
        let geometry = (window.label().to_owned(), x, y, width, height);
        let mut applied = host
            .applied_bounds
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if applied.get(&key) == Some(&geometry) {
            return Ok(());
        }
        view.set_bounds(tauri::Rect {
            position: tauri::PhysicalPosition::new(x, y).into(),
            size: tauri::PhysicalSize::new(width, height).into(),
        })
        .map_err(|e| e.to_string())?;
        applied.insert(key, geometry);
    }
    let updating = host.inner().clone();
    tauri::async_runtime::spawn(async move {
        let _ = apply_input_regions(&updating).await;
    });
    Ok(())
}
pub(crate) async fn set_panel(
    host: &NativeHost,
    bounds: Option<BrowserBounds>,
) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let host = host.clone();
    let app = host.app.clone();
    app.run_on_main_thread(move || {
        let result = (|| {
            let previous = *host.panel.lock().map_err(|_| "Browser unavailable")?;
            if let Some(bounds) = bounds
                && let Some(window) = host.app.get_window("main")
            {
                let size = window.inner_size().map_err(|e| e.to_string())?;
                *host
                    .panel_insets
                    .lock()
                    .map_err(|_| "Browser unavailable")? = (
                    (f64::from(size.width) / bounds.scale - bounds.x - bounds.width).max(0.0),
                    (f64::from(size.height) / bounds.scale - bounds.y - bounds.height).max(0.0),
                );
            }
            *host.panel.lock().map_err(|_| "Browser unavailable")? = bounds;
            // Resizing an attached page never hides or reparents its native view.
            if previous.is_some() == bounds.is_some() {
                return resize(&host.app);
            }
            let order = host
                .tab_order
                .lock()
                .map_err(|_| "Browser unavailable")?
                .clone();
            if order.is_empty() {
                return Ok(());
            }
            let window = page_window(&host)?;
            let selected = host
                .selected
                .lock()
                .map_err(|_| "Browser unavailable")?
                .clone();
            for key in &order {
                if let Some(view) = host.app.get_webview(key) {
                    view.hide().map_err(|e| e.to_string())?;
                }
            }
            for key in &order {
                if let Some(view) = host.app.get_webview(key)
                    && view.window().label() != window.label()
                {
                    view.reparent(&window).map_err(|e| e.to_string())?;
                }
            }
            resize(&host.app)?;
            if let Some(view) = selected
                .as_deref()
                .and_then(|key| host.app.get_webview(key))
            {
                view.show().map_err(|e| e.to_string())?;
            }
            Ok::<_, String>(())
        })();
        let _ = tx.send(result);
    })
    .map_err(|e| e.to_string())?;
    rx.await.map_err(|_| "Browser unavailable")?
}

#[tauri::command]
pub async fn browser_control(
    webview: Webview,
    action: String,
    tab: Option<String>,
    url: Option<String>,
    bounds: Option<BrowserBounds>,
    foreground: Option<Vec<BrowserHitRegion>>,
) -> Result<serde_json::Value, String> {
    let app = webview.app_handle();
    let current = webview.url().map_err(|_| "Browser unavailable")?;
    if webview.label() != "main" || !crate::transport::navigation(app, &current) {
        return Err("Browser control is restricted to the Owner interface".into());
    }
    let host = app.state::<NativeHost>().inner().clone();
    // Visual detachment must also work after logout, without revealing page facts.
    if action == "detach" {
        host.foreground
            .lock()
            .map_err(|_| "Browser unavailable")?
            .clear();
        set_panel(&host, None).await?;
        return Ok(serde_json::json!({}));
    }
    crate::transport::authorize_owner(&webview).await?;
    let foreground = foreground.unwrap_or_default();
    if foreground.len() > 512 || foreground.iter().any(|cut| !cut.validate()) {
        return Err("Invalid browser overlays".into());
    }
    let key = match tab {
        Some(tab)
            if tab.len() == 32
                && tab
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) =>
        {
            Some(format!("web-{SHARED_BROWSER_PROFILE}-{tab}"))
        }
        Some(_) => return Err("Invalid browser page".into()),
        None => host
            .selected
            .lock()
            .map_err(|_| "Browser unavailable")?
            .clone(),
    };
    match action.as_str() {
        "preferences" => {
            return serde_json::to_value(
                &*host.preferences.lock().map_err(|_| "Browser unavailable")?,
            )
            .map_err(|e| e.to_string());
        }
        "set_preferences" => {
            let patch = serde_json::from_str(&url.ok_or("Invalid browser settings")?)
                .map_err(|_| "Invalid browser settings")?;
            let mut preferences = host.preferences.lock().map_err(|_| "Browser unavailable")?;
            *preferences = preferences.patch(&host.root, patch)?;
            return serde_json::to_value(&*preferences).map_err(|e| e.to_string());
        }
        "mount" => {
            let bounds = bounds
                .filter(BrowserBounds::validate)
                .ok_or("Invalid browser bounds")?;
            *host.foreground.lock().map_err(|_| "Browser unavailable")? = foreground;
            set_panel(&host, Some(bounds)).await?;
            apply_input_regions(&host).await?;
            return Ok(serde_json::json!({}));
        }
        "list" => {}
        "move_tab" => {
            let before = url.map(|tab| format!("web-{SHARED_BROWSER_PROFILE}-{tab}"));
            move_tab(
                &host,
                key.as_deref().ok_or("Select a browser page")?,
                before.as_deref(),
            )?;
        }
        "new_tab" => {
            let key = format!(
                "web-{SHARED_BROWSER_PROFILE}-{}",
                uuid::Uuid::new_v4().simple()
            );
            let host = host.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            app.run_on_main_thread(move || {
                let _ = tx.send(host.create_page(
                    SHARED_BROWSER_PROFILE,
                    &key,
                    url::Url::parse("about:blank").unwrap(),
                    true,
                ));
            })
            .map_err(|e| e.to_string())?;
            rx.await.map_err(|_| "Browser unavailable")??;
        }
        "library" => {
            let library = host
                .library
                .lock()
                .map_err(|_| "Browser library unavailable")?;
            let offset = url
                .as_deref()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            return Ok(
                serde_json::json!({"history":library.history.iter().rev().skip(offset).take(100).collect::<Vec<_>>(),"downloads":library.downloads.iter().rev().skip(offset).take(100).collect::<Vec<_>>(),"next_offset":if library.history.len().max(library.downloads.len())>offset.saturating_add(100){Some(offset.saturating_add(100))}else{None}}),
            );
        }
        "clear_history" => {
            let mut library = host
                .library
                .lock()
                .map_err(|_| "Browser library unavailable")?;
            library.history.clear();
            library.save(&host.root)?;
        }
        "remove_history" => {
            let mut library = host
                .library
                .lock()
                .map_err(|_| "Browser library unavailable")?;
            library
                .history
                .retain(|entry| Some(&entry.id) != url.as_ref());
            library.save(&host.root)?;
        }
        "open_downloads" => {
            let directory = host.root.join("runtime/browser-downloads");
            std::fs::create_dir_all(&directory).map_err(|_| "Downloads unavailable")?;
            open::that(directory).map_err(|_| "Downloads could not be opened")?;
        }
        "reveal_download" => {
            let download = host
                .library
                .lock()
                .map_err(|_| "Browser library unavailable")?
                .downloads
                .iter()
                .find(|entry| Some(&entry.id) == url.as_ref() && entry.status == "complete")
                .cloned()
                .ok_or("Download unavailable")?;
            let path = crate::browser_data::download_path(&host.root, &download);
            if !path.is_file() {
                return Err("Download unavailable".into());
            }
            tauri_plugin_opener::reveal_item_in_dir(path)
                .map_err(|_| "Download could not be shown")?;
        }
        "save_download" => {
            use tauri_plugin_dialog::DialogExt;
            let download = host
                .library
                .lock()
                .map_err(|_| "Browser library unavailable")?
                .downloads
                .iter()
                .find(|entry| Some(&entry.id) == url.as_ref() && entry.status == "complete")
                .cloned()
                .ok_or("Download unavailable")?;
            let source = crate::browser_data::download_path(&host.root, &download);
            let app = app.clone();
            tokio::task::spawn_blocking(move || {
                if let Some(target) = app
                    .dialog()
                    .file()
                    .set_file_name(&download.name)
                    .blocking_save_file()
                {
                    let target = target.into_path().map_err(|_| "Invalid save destination")?;
                    std::fs::copy(source, target).map_err(|_| "Download could not be saved")?;
                }
                Ok::<_, String>(())
            })
            .await
            .map_err(|_| "Download could not be saved")??;
        }
        "remove_download" => {
            let mut library = host
                .library
                .lock()
                .map_err(|_| "Browser library unavailable")?;
            // Remove only the record; never cancel or delete an in-flight download.
            library
                .downloads
                .retain(|entry| Some(&entry.id) != url.as_ref() || entry.status == "downloading");
            library.save(&host.root)?;
        }
        "zoom" => {
            let key = key.as_deref().ok_or("Select a browser page")?;
            let zoom = url
                .as_deref()
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite() && (0.25..=5.0).contains(v))
                .ok_or("Invalid browser zoom")?;
            host.app
                .get_webview(key)
                .ok_or("Browser page closed")?
                .set_zoom(zoom)
                .map_err(|e| e.to_string())?;
            host.zooms
                .lock()
                .map_err(|_| "Browser unavailable")?
                .insert(key.into(), zoom);
        }
        "print" => {
            host.app
                .get_webview(key.as_deref().ok_or("Select a browser page")?)
                .ok_or("Browser page closed")?
                .print()
                .map_err(|e| e.to_string())?;
        }
        "screenshot" => {
            let view = host
                .app
                .get_webview(key.as_deref().ok_or("Select a browser page")?)
                .ok_or("Browser page closed")?;
            let png = crate::browser_use::page_screenshot(&view).await?;
            copy_screenshot(&png).await?;
            return Ok(serde_json::json!({"destination":"clipboard"}));
        }
        "find" | "find_next" | "find_previous" | "find_close" => {
            let view = host
                .app
                .get_webview(key.as_deref().ok_or("Select a browser page")?)
                .ok_or("Browser page closed")?;
            return crate::browser_use::find(&view, &action, &url.unwrap_or_default()).await;
        }
        "clear_data" => {
            let temporary = if !host
                .app
                .webviews()
                .keys()
                .any(|key| host.is_browser_page(key))
            {
                let key = format!(
                    "web-{SHARED_BROWSER_PROFILE}-{}",
                    uuid::Uuid::new_v4().simple()
                );
                let creating = host.clone();
                let created = key.clone();
                let (tx, rx) = tokio::sync::oneshot::channel();
                app.run_on_main_thread(move || {
                    let _ = tx.send(creating.create_page(
                        SHARED_BROWSER_PROFILE,
                        &created,
                        url::Url::parse("about:blank").unwrap(),
                        true,
                    ));
                })
                .map_err(|e| e.to_string())?;
                rx.await.map_err(|_| "Browser unavailable")??;
                Some(key)
            } else {
                None
            };
            for (_, view) in host.app.webviews() {
                if host.is_browser_page(view.label()) {
                    view.clear_all_browsing_data().map_err(|e| e.to_string())?;
                }
            }
            if let Some(key) = temporary {
                host.close_page(&key)?;
            }
        }
        "stop" => {
            if let Some(view) = key.as_deref().and_then(|key| host.app.get_webview(key)) {
                let _ = view.eval("window.stop()");
                host.loading
                    .lock()
                    .map_err(|_| "Browser unavailable")?
                    .insert(view.label().into(), false);
                host.changed();
            }
        }
        "select" => host.select_page(key.as_deref().ok_or("Select a browser page")?, true)?,
        "close" => host.close_page(key.as_deref().ok_or("Select a browser page")?)?,
        "open" => {
            let url =
                url::Url::parse(&url.ok_or("Enter a URL")?).map_err(|_| "Invalid browser URL")?;
            if !host.navigation_allowed(&url) {
                return Err("Browser URL is not allowed".into());
            }
            let key = format!(
                "web-{SHARED_BROWSER_PROFILE}-{}",
                uuid::Uuid::new_v4().simple()
            );
            let creating = host.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            app.run_on_main_thread(move || {
                let _ = tx.send(creating.create_page(SHARED_BROWSER_PROFILE, &key, url, true));
            })
            .map_err(|e| e.to_string())?;
            rx.await.map_err(|_| "Browser unavailable")??;
        }
        "navigate" | "back" | "forward" | "reload" => {
            let input = match action.as_str() {
                "navigate" => BrowserInput::Navigate {
                    url: url.ok_or("Enter a URL")?,
                },
                "back" => BrowserInput::Back,
                "forward" => BrowserInput::Forward,
                _ => BrowserInput::Reload,
            };
            host.owner_input(key.as_deref().ok_or("Select a browser page")?, input)
                .await?;
        }
        _ => return Err("Unknown browser control".into()),
    }
    let prefix = format!("web-{SHARED_BROWSER_PROFILE}-");
    let selected = host
        .selected
        .lock()
        .map_err(|_| "Browser unavailable")?
        .as_deref()
        .and_then(|v| v.strip_prefix(&prefix))
        .map(str::to_owned);
    let mut pages = serde_json::to_value(host.pages(SHARED_BROWSER_PROFILE))
        .map_err(|_| "Browser unavailable")?;
    if let Some(pages) = pages.as_array_mut() {
        for page in pages {
            if let Some(tab) = page["tab"].as_str().map(str::to_owned) {
                let key = format!("{prefix}{tab}");
                let icon = host
                    .icons
                    .lock()
                    .map_err(|_| "Browser unavailable")?
                    .get(&key)
                    .cloned();
                page["favicon"] = serde_json::json!(icon);
                if selected.as_deref() == Some(tab.as_str())
                    && let Some(view) = host.app.get_webview(&key)
                    && let Ok((back, forward)) = crate::browser_use::navigation(&view).await
                {
                    page["can_back"] = serde_json::json!(back);
                    page["can_forward"] = serde_json::json!(forward);
                    page["background"] = tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        crate::browser_use::evaluate(&view, include_str!("browser-background.js")),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(serde_json::Value::Null);
                }
                page["loading"] = serde_json::json!(
                    host.loading
                        .lock()
                        .map_err(|_| "Browser unavailable")?
                        .get(&key)
                        .copied()
                        .unwrap_or(false)
                );
                page["zoom"] = serde_json::json!(
                    host.zooms
                        .lock()
                        .map_err(|_| "Browser unavailable")?
                        .get(&key)
                        .copied()
                        .unwrap_or(1.0)
                );
            }
        }
    }
    Ok(serde_json::json!({"pages":pages,"selected":selected}))
}
pub(crate) async fn copy_screenshot(png: &str) -> Result<(), String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png)
        .map_err(|_| "Invalid screenshot")?;
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let image =
            tauri::image::Image::from_bytes(&bytes).map_err(|_| "Invalid screenshot image")?;
        let mut clipboard = arboard::Clipboard::new().map_err(|_| "Clipboard unavailable")?;
        clipboard
            .set_image(arboard::ImageData {
                width: image.width() as usize,
                height: image.height() as usize,
                bytes: std::borrow::Cow::Borrowed(image.rgba()),
            })
            .map_err(|_| "Could not copy screenshot".to_owned())
    })
    .await
    .map_err(|_| "Clipboard unavailable")?
}
pub(crate) fn move_tab(host: &NativeHost, key: &str, before: Option<&str>) -> Result<(), String> {
    let mut order = host.tab_order.lock().map_err(|_| "Browser unavailable")?;
    if !order.iter().any(|id| id == key)
        || before.is_some_and(|target| !order.iter().any(|id| id == target))
    {
        return Err("Browser page closed".into());
    }
    if before != Some(key) {
        order.retain(|id| id != key);
        let index = before
            .and_then(|target| order.iter().position(|id| id == target))
            .unwrap_or(order.len());
        order.insert(index, key.to_owned());
    }
    Ok(())
}
pub(crate) async fn apply_input_regions(host: &NativeHost) -> Result<(), String> {
    let bounds = *host.panel.lock().map_err(|_| "Browser unavailable")?;
    let foreground = host
        .foreground
        .lock()
        .map_err(|_| "Browser unavailable")?
        .clone();
    if let Some(shell) = host.app.get_webview("main") {
        crate::browser_use::compose(&shell, bounds, foreground).await?;
    }
    Ok(())
}

impl NativeHost {
    pub(crate) fn download(&self, event: tauri::webview::DownloadEvent<'_>) -> bool {
        use tauri::webview::DownloadEvent;
        let mut library = self.library.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            DownloadEvent::Requested { url, destination } => {
                if !self.navigation_allowed(&url) && url.scheme() != "blob" {
                    return false;
                }
                let entry = crate::browser_data::Download {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    url: url.into(),
                    name: crate::browser_data::safe_name(destination),
                    time: crate::browser_data::now(),
                    status: "downloading".into(),
                    bytes: 0,
                };
                let path = crate::browser_data::download_path(&self.root, &entry);
                if std::fs::create_dir_all(path.parent().unwrap()).is_err() {
                    return false;
                }
                *destination = path;
                library.downloads.push(entry);
            }
            DownloadEvent::Finished { url, path, success } => {
                if let Some(entry) = library.downloads.iter_mut().find(|entry| {
                    entry.status == "downloading"
                        && entry.url == url.as_str()
                        && path.as_ref().is_none_or(|p| {
                            std::fs::canonicalize(p)
                                .ok()
                                .zip(
                                    std::fs::canonicalize(crate::browser_data::download_path(
                                        &self.root, entry,
                                    ))
                                    .ok(),
                                )
                                .is_some_and(|(actual, expected)| actual == expected)
                        })
                }) {
                    entry.status = if success { "complete" } else { "failed" }.into();
                    entry.bytes =
                        std::fs::metadata(crate::browser_data::download_path(&self.root, entry))
                            .map(|m| m.len())
                            .unwrap_or(0);
                }
            }
            _ => return false,
        }
        library.save(&self.root).is_ok()
    }
}
