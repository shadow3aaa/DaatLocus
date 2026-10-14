use crate::backend::{self, Backend, Options};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tauri::{
    Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::NewWindowResponse,
};
use tauri_plugin_dialog::DialogExt;
use url::Url;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app.png"));
const OPEN: &str = "open-client";
const QUIT: &str = "quit-client";
const STOP: &str = "stop-service";
static PREVIEW: AtomicU64 = AtomicU64::new(0);

struct Client {
    backend: Backend,
    origin: Arc<RwLock<Url>>,
    stopping: AtomicBool,
    stop_item: MenuItem<tauri::Wry>,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum WindowAction {
    State,
    Drag,
    Minimize,
    ToggleMaximize,
    Close,
}
#[derive(serde::Serialize)]
struct WindowState {
    maximized: bool,
    decorated: bool,
}
#[tauri::command]
fn desktop_window(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Client>,
    action: WindowAction,
) -> std::result::Result<WindowState, String> {
    let url = window.url().map_err(|_| "Window unavailable")?;
    if window.label() != "main"
        || !backend::same_origin(&state.origin.read().unwrap(), &url)
        || url.path().starts_with("/v1/")
    {
        return Err("Window action forbidden".into());
    }
    let result = match action {
        WindowAction::State => Ok(()),
        WindowAction::Drag => window.start_dragging(),
        WindowAction::Minimize => window.minimize(),
        WindowAction::ToggleMaximize => {
            if window.is_maximized().unwrap_or(false) {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }
        WindowAction::Close => window.close(),
    };
    result.map_err(|_| "Window action failed")?;
    Ok(WindowState {
        maximized: window.is_maximized().unwrap_or(false),
        decorated: window
            .is_decorated()
            .map_err(|_| "Window state unavailable")?,
    })
}
#[tauri::command]
async fn desktop_account(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Client>,
    input: daat_locus_client_protocol::BootstrapRequest,
) -> std::result::Result<daat_locus_client_protocol::BootstrapReply, String> {
    let url = window.url().map_err(|_| "Account request unavailable")?;
    if window.label() != "main"
        || !backend::same_origin(&state.origin.read().unwrap(), &url)
        || url.path().starts_with("/v1/")
    {
        return Err("Account request forbidden".into());
    }
    state
        .backend
        .bootstrap(input)
        .await
        .map_err(|_| "Account request failed".into())
}
fn bootstrap_capability(app: &tauri::AppHandle, origin: &Url) -> tauri::Result<()> {
    let base = origin.origin().ascii_serialization();
    app.add_capability(serde_json::json!({
        "identifier":format!("desktop-bootstrap-{}",origin.port_or_known_default().unwrap_or_default()),
        "windows":["main"],"local":false,
        "remote":{"urls":[format!("{base}/"),format!("{base}/actors/*"),format!("{base}/stages/*")]},
        "permissions":["allow-desktop-account","allow-desktop-window"]
    }).to_string())
}
struct Labels {
    open: &'static str,
    quit: &'static str,
    stop: &'static str,
    stopping: &'static str,
    failure: &'static str,
}
fn labels() -> Labels {
    if sys_locale::get_locale().is_some_and(|locale| locale.to_ascii_lowercase().starts_with("zh"))
    {
        Labels {
            open: "打开 Daat Locus",
            quit: "退出客户端",
            stop: "停止并退出",
            stopping: "正在停止…",
            failure: "无法停止服务，请重试。",
        }
    } else {
        Labels {
            open: "Open Daat Locus",
            quit: "Quit client",
            stop: "Stop and exit",
            stopping: "Stopping…",
            failure: "Unable to stop the service. Please try again.",
        }
    }
}
fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
fn external(url: &Url) {
    if matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
    {
        let _ = open::that(url.as_str());
    }
}
fn action(app: &tauri::AppHandle, id: &str) {
    match id {
        OPEN => show(app),
        QUIT => app.exit(0),
        STOP => {
            let state = app.state::<Client>();
            if state.stopping.swap(true, Ordering::AcqRel) {
                return;
            }
            let _ = state.stop_item.set_text(labels().stopping);
            let _ = state.stop_item.set_enabled(false);
            let backend = state.backend.clone();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if backend.stop().await.is_ok() {
                    app.exit(0);
                } else {
                    let state = app.state::<Client>();
                    state.stopping.store(false, Ordering::Release);
                    let _ = state.stop_item.set_text(labels().stop);
                    let _ = state.stop_item.set_enabled(true);
                    app.dialog()
                        .message(labels().failure)
                        .title("Daat Locus")
                        .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                        .show(|_| {});
                }
            });
        }
        _ => {}
    }
}
pub fn run(options: Options) -> Result<()> {
    let background = options.background;
    let backend = Backend::new(options)?;
    let status = tauri::async_runtime::block_on(backend.start())?;
    let origin = Arc::new(RwLock::new(backend::local_origin(&status.url)?));
    let mut context = tauri::generate_context!();
    // Single-instance identity is profile-local, including separately installed OS users.
    let profile = backend::profile_id(&backend.root);
    context.config_mut().identifier = format!("io.daat-locus.app.p{profile}");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().any(|arg| arg == "--background") {
                show(app);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![desktop_account, desktop_window])
        .setup(move |app| {
            let text = labels();
            let open = MenuItem::with_id(app, OPEN, text.open, true, None::<&str>)?;
            let quit = MenuItem::with_id(app, QUIT, text.quit, true, None::<&str>)?;
            let stop = MenuItem::with_id(app, STOP, text.stop, true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit, &stop])?;
            app.manage(Client {
                backend: backend.clone(),
                origin: origin.clone(),
                stopping: AtomicBool::new(false),
                stop_item: stop,
            });
            bootstrap_capability(app.handle(), &origin.read().unwrap())?;
            let navigation_origin = origin.clone();
            let popup_origin = origin.clone();
            let handle = app.handle().clone();
            let data = backend.root.join("runtime/client-webview");
            let mut builder = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(origin.read().unwrap().clone()),
            )
            .title("Daat Locus")
            .visible(!background)
            .inner_size(1100.0, 820.0)
            .min_inner_size(640.0, 480.0)
            .icon(Image::from_bytes(ICON)?)?
            .data_directory(data)
            .disable_drag_drop_handler()
            .initialization_script(if cfg!(windows) {
                "Object.defineProperty(window,'__DAAT_LOCUS_DESKTOP__',{value:'windows'});"
            } else {
                "Object.defineProperty(window,'__DAAT_LOCUS_DESKTOP__',{value:'macos'});"
            })
            .on_navigation(move |url| {
                if backend::same_origin(&navigation_origin.read().unwrap(), url) {
                    true
                } else {
                    external(url);
                    false
                }
            })
            .on_new_window(move |url, features| {
                let base = popup_origin.read().unwrap().clone();
                if !backend::resource(&base, &url) {
                    if !backend::same_origin(&base, &url) {
                        external(&url);
                    }
                    return NewWindowResponse::Deny;
                }
                if handle.webview_windows().len() >= 16 {
                    return NewWindowResponse::Deny;
                }
                let label = format!("resource-{}", PREVIEW.fetch_add(1, Ordering::Relaxed));
                let preview = WebviewWindowBuilder::new(
                    &handle,
                    label,
                    WebviewUrl::External("about:blank".parse().unwrap()),
                )
                .window_features(features)
                .title("Daat Locus")
                .inner_size(900.0, 700.0)
                .on_navigation(move |url| {
                    url.as_str() == "about:blank" || backend::resource(&base, url)
                })
                .build();
                match preview {
                    Ok(window) => NewWindowResponse::Create { window },
                    Err(_) => NewWindowResponse::Deny,
                }
            });
            #[cfg(target_os = "macos")]
            {
                builder = builder
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .hidden_title(true)
                    .traffic_light_position(tauri::LogicalPosition::new(14.0, 12.0));
                let mut id = [0u8; 16];
                for (index, byte) in id.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&profile[index * 2..index * 2 + 2], 16).unwrap();
                }
                builder = builder.data_store_identifier(id);
            }
            #[cfg(not(target_os = "macos"))]
            {
                builder = builder.decorations(false);
            }
            builder.build()?;
            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{PredefinedMenuItem, Submenu};
                let quit = MenuItem::with_id(app, QUIT, text.quit, true, Some("CmdOrCtrl+Q"))?;
                let application = Submenu::with_items(
                    app,
                    "Daat Locus",
                    true,
                    &[&PredefinedMenuItem::about(app, None, None)?, &quit],
                )?;
                let edit = Submenu::with_items(
                    app,
                    "Edit",
                    true,
                    &[
                        &PredefinedMenuItem::undo(app, None)?,
                        &PredefinedMenuItem::redo(app, None)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::cut(app, None)?,
                        &PredefinedMenuItem::copy(app, None)?,
                        &PredefinedMenuItem::paste(app, None)?,
                        &PredefinedMenuItem::select_all(app, None)?,
                    ],
                )?;
                app.set_menu(Menu::with_items(app, &[&application, &edit])?)?;
            }
            TrayIconBuilder::with_id("client")
                .icon(Image::from_bytes(ICON)?)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("Daat Locus")
                .menu(&menu)
                .show_menu_on_left_click(!cfg!(windows))
                .on_menu_event(|app, event| action(app, event.id.as_ref()))
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::DoubleClick {
                            button: MouseButton::Left,
                            ..
                        }
                    ) || cfg!(windows)
                        && matches!(
                            event,
                            TrayIconEvent::Click {
                                button: MouseButton::Left,
                                button_state: MouseButtonState::Up,
                                ..
                            }
                        )
                    {
                        show(tray.app_handle());
                    }
                })
                .build(app)?;
            let app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut missing = 0;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    if app.state::<Client>().stopping.load(Ordering::Acquire) {
                        continue;
                    }
                    match backend.status().await {
                        Ok(Some(status)) => {
                            missing = 0;
                            if let Ok(url) = backend::local_origin(&status.url) {
                                let changed = origin.read().unwrap().as_str() != url.as_str();
                                if changed {
                                    if bootstrap_capability(&app, &url).is_err() {
                                        continue;
                                    }
                                    *origin.write().unwrap() = url.clone();
                                    for (label, window) in app.webview_windows() {
                                        if label != "main" {
                                            let _ = window.close();
                                        }
                                    }
                                    if let Some(window) = app.get_webview_window("main") {
                                        let _ = window.navigate(url);
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            missing += 1;
                            if missing >= 3 {
                                app.exit(0);
                                break;
                            }
                        }
                        Err(_) => {}
                    }
                }
            });
            Ok(())
        })
        .on_menu_event(|app, event| action(app, event.id.as_ref()))
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(context)?;
    app.run(|_app, event| match event {
        RunEvent::ExitRequested {
            code: None, api, ..
        } => api.prevent_exit(),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show(_app),
        _ => {}
    });
    Ok(())
}
pub fn startup_error() {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let title: Vec<u16> = std::ffi::OsStr::new("Daat Locus")
            .encode_wide()
            .chain(Some(0))
            .collect();
        let message: Vec<u16> = std::ffi::OsStr::new("Unable to start Daat Locus.")
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("/usr/bin/osascript")
            .args([
                "-e",
                "display alert \"Daat Locus\" message \"Unable to start Daat Locus.\" as critical",
            ])
            .status();
    }
}
