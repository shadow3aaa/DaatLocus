use crate::backend::{self, Backend, Options};
use std::sync::{Arc, RwLock};
#[cfg(target_os = "macos")]
use tauri::menu::{Menu, MenuItem};
use tauri::{
    Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    image::Image,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::NewWindowResponse,
};
use url::Url;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app.png"));
#[cfg(target_os = "macos")]
const TRAY_ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tray.png"));
#[cfg(not(target_os = "macos"))]
const TRAY_ICON: &[u8] = ICON;
const OPEN: &str = "open-client";
const QUIT: &str = "quit-client";
const HIDE: &str = "hide-client";

pub(crate) struct Client {
    pub(crate) backend: Backend,
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
    window: tauri::Webview,
    action: WindowAction,
) -> std::result::Result<WindowState, String> {
    crate::transport::authorize(&window)?;
    let window = window.window();
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
    window: tauri::Webview,
    state: tauri::State<'_, Client>,
    input: daat_locus_client_protocol::BootstrapRequest,
) -> std::result::Result<daat_locus_client_protocol::BootstrapReply, String> {
    crate::transport::authorize(&window)?;
    state
        .backend
        .bootstrap(input)
        .await
        .map_err(|_| "Account request failed".into())
}

struct Labels {
    open: &'static str,
    quit: &'static str,
    hide: &'static str,
}
fn labels() -> Labels {
    if sys_locale::get_locale().is_some_and(|locale| locale.to_ascii_lowercase().starts_with("zh"))
    {
        Labels {
            open: "打开 Daat Locus",
            quit: "退出 Daat Locus",
            hide: "最小化到托盘",
        }
    } else {
        Labels {
            open: "Open Daat Locus",
            quit: "Quit Daat Locus",
            hide: "Minimize to tray",
        }
    }
}
fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_window("main") {
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
        HIDE => {
            if let Some(window) = app.get_window("main") {
                let _ = window.hide();
            }
        }
        _ => {}
    }
}
pub(crate) fn context<R: tauri::Runtime>() -> tauri::Context<R> {
    tauri::generate_context!()
}

pub fn run(options: Options) -> Result<()> {
    #[cfg(feature = "web-contract")]
    if let Some(manifest) = &options.web_probe {
        return crate::web_host::probe(manifest);
    }
    #[cfg(all(target_os = "macos", feature = "native-contract"))]
    let native_contract = options
        .native_gui_contract
        .clone()
        .or_else(|| std::env::var_os("DAAT_LOCUS_NATIVE_CONTRACT").map(std::path::PathBuf::from));
    let background = options.background;
    let backend = Backend::new(options)?;
    let status = tauri::async_runtime::block_on(backend.start())?;
    let origin = Arc::new(RwLock::new(backend::local_origin(&status.url)?));
    let mut context = context();
    // Single-instance identity is profile-local, including separately installed OS users.
    let profile = backend::profile_id(&backend.root);
    let identifier = format!("{}.p{profile}", context.config().identifier);
    context.config_mut().identifier = identifier;
    let app = crate::transport::resources(tauri::Builder::default())
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().any(|arg| arg == "--background") {
                show(app);
            }
        }))
        .invoke_handler(tauri::generate_handler![
            crate::browser_ui::browser_control,
            desktop_account,
            desktop_window,
            #[cfg(target_os = "macos")]
            crate::computer_use::computer_use_status,
            #[cfg(target_os = "macos")]
            crate::computer_use::computer_use_configure,
            #[cfg(target_os = "macos")]
            crate::computer_use::computer_use_display_configure,
            #[cfg(target_os = "macos")]
            crate::computer_use::computer_use_permission,
            crate::transport::native_http_start,
            crate::transport::native_http_write,
            crate::transport::native_http_send,
            crate::transport::native_http_read,
            crate::transport::native_http_cancel,
            crate::transport::native_resource_download
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            let text = labels();
            #[cfg(target_os = "macos")]
            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(app, OPEN, text.open, true, None::<&str>)?,
                    &MenuItem::with_id(app, HIDE, text.hide, true, None::<&str>)?,
                    &MenuItem::with_id(app, QUIT, text.quit, true, None::<&str>)?,
                ],
            )?;
            app.manage(Client {
                backend: backend.clone(),
            });
            app.manage(crate::transport::Transport::new(origin.clone())?);
            #[cfg(target_os = "macos")]
            app.manage(std::sync::Arc::new(daat_locus_computer_use::Server::start(
                &backend.root,
                &backend.executable,
            )?));
            #[cfg(all(target_os = "macos", feature = "native-contract"))]
            if let Some(output) = native_contract.clone() {
                let root = backend.root.clone();
                let server = app
                    .state::<std::sync::Arc<daat_locus_computer_use::Server>>()
                    .inner()
                    .clone();
                std::thread::spawn(move || {
                    let result = daat_locus_computer_use::run_native_contract(&root, &server);
                    let value = match result {
                        Ok(value) => serde_json::json!({"passed":true,"checks":value}),
                        Err(error) => serde_json::json!({"passed":false,"error":error.to_string()}),
                    };
                    let _ = std::fs::write(output, value.to_string());
                });
            }
            crate::transport::capability(app.handle())?;
            let navigation_app = app.handle().clone();
            let data = backend.root.join("runtime/client-webview");
            let mut builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
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
                    .on_page_load(|window, payload| {
                        if matches!(payload.event(), tauri::webview::PageLoadEvent::Started)
                            && crate::transport::navigation(window.app_handle(), payload.url())
                        {
                            window.state::<crate::transport::Transport>().clear();
                        }
                    })
                    .on_navigation(move |url| {
                        if crate::transport::navigation(&navigation_app, url)
                            || crate::transport::resource_url(url)
                        {
                            true
                        } else {
                            external(url);
                            false
                        }
                    })
                    .on_new_window(move |url, _| {
                        external(&url);
                        NewWindowResponse::Deny
                    });
            #[cfg(target_os = "macos")]
            {
                builder = builder
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .hidden_title(true)
                    // Tao's vertical inset sizes the titlebar container; it is
                    // not the button's distance from the top. Center the native
                    // controls in the WebUI's 52-point rail header.
                    .traffic_light_position(tauri::LogicalPosition::new(14.0, 28.0));
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
            crate::web_host::start(
                app.handle().clone(),
                backend.clone(),
                status.url.trim_end_matches('/').into(),
            );
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
            let tray = TrayIconBuilder::with_id("client")
                .icon(Image::from_bytes(TRAY_ICON)?)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("Daat Locus")
                .show_menu_on_left_click(!cfg!(windows))
                .on_tray_icon_event(|tray, event| {
                    #[cfg(windows)]
                    if let TrayIconEvent::Click {
                        button: MouseButton::Right,
                        button_state: MouseButtonState::Up,
                        position,
                        ..
                    } = event
                    {
                        let app = tray.app_handle().clone();
                        let handle = app.clone();
                        let _ = app.run_on_main_thread(move || {
                            if let Some(window) = handle.get_window("main") {
                                let visible = window.is_visible().unwrap_or(false)
                                    && !window.is_minimized().unwrap_or(false);
                                let text = labels();
                                if let Ok(owner) = window.hwnd() {
                                    let choice = crate::windows_tray::show(
                                        owner.0 as _,
                                        windows_sys::Win32::Foundation::POINT {
                                            x: position.x as i32,
                                            y: position.y as i32,
                                        },
                                        if visible { text.hide } else { text.open },
                                        text.quit,
                                        window.theme().ok() == Some(tauri::Theme::Dark),
                                    );
                                    match choice {
                                        Some(crate::windows_tray::Action::Toggle) => {
                                            action(&handle, if visible { HIDE } else { OPEN })
                                        }
                                        Some(crate::windows_tray::Action::Quit) => {
                                            action(&handle, QUIT)
                                        }
                                        None => {}
                                    }
                                }
                            }
                        });
                    }
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
                });
            #[cfg(target_os = "macos")]
            let tray = tray
                .menu(&menu)
                .on_menu_event(|app, event| action(app, event.id.as_ref()));
            tray.build(app)?;
            let app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut missing = 0;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    match backend.status().await {
                        Ok(Some(status)) => {
                            missing = 0;
                            if let Ok(url) = backend::local_origin(&status.url) {
                                let changed = origin.read().unwrap().as_str() != url.as_str();
                                if changed {
                                    app.state::<crate::transport::Transport>().set_origin(url);
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
            if matches!(window.label(), "main" | "browser")
                && matches!(
                    event,
                    WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
                )
            {
                let _ = crate::browser_ui::resize(window.app_handle());
            }
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
        RunEvent::Exit => {
            let _ = _app
                .state::<std::sync::Arc<daat_locus_computer_use::Server>>()
                .shutdown();
        }
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
