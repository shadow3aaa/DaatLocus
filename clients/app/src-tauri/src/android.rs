use daat_locus::mobile::MobileService;
use std::sync::Arc;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
static SERVICE: std::sync::OnceLock<std::sync::Weak<MobileService>> = std::sync::OnceLock::new();
static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();
static SAFE_AREA: std::sync::Mutex<[f32; 4]> = std::sync::Mutex::new([0.0; 4]);

fn apply_safe_area(window: &tauri::WebviewWindow) {
    let values = *SAFE_AREA.lock().unwrap_or_else(|e| e.into_inner());
    // OS geometry only, with no JS-to-native capability. Explicit values also
    // support WebViews older than CSS system-bar inset forwarding (M136).
    let script = format!(
        "document.documentElement.dataset.mobilePlatform='android';\
         ['top','right','bottom','left'].forEach((side,i)=>\
         document.documentElement.style.setProperty('--dl-safe-'+side,{}[i]+'px'));",
        serde_json::json!(values)
    );
    let _ = window.eval(script);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_shadow3aaa_daatlocus_MainActivity_nativeWindowInsetsChanged(
    _env: *mut jni::sys::JNIEnv,
    _activity: jni::sys::jobject,
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
) {
    let values = [top, right, bottom, left];
    if !values.iter().all(|v| v.is_finite() && *v >= 0.0) {
        return;
    }
    *SAFE_AREA.lock().unwrap_or_else(|e| e.into_inner()) = values;
    if let Some(window) = APP.get().and_then(|app| app.get_webview_window("main")) {
        apply_safe_area(&window);
    }
}

// An OS connectivity hint only. No WebView/JavaScript command or authority is
// introduced, and callbacks before service initialization are harmless.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_shadow3aaa_daatlocus_MainActivity_nativeNetworkChanged(
    _env: *mut jni::sys::JNIEnv,
    _activity: jni::sys::jobject,
) {
    if let Some(service) = SERVICE.get().and_then(std::sync::Weak::upgrade) {
        tauri::async_runtime::spawn(async move {
            service.connectivity_changed();
        });
    }
}

struct Client(Arc<MobileService>);
#[tauri::command]
async fn desktop_account(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Client>,
    input: daat_locus_client_protocol::BootstrapRequest,
) -> Result<daat_locus_client_protocol::BootstrapReply, String> {
    crate::transport::authorize(&window)?;
    state
        .0
        .bootstrap(input)
        .await
        .map_err(|_| "Account operation unavailable".into())
}

fn platform_tls() -> Result<(), Box<dyn std::error::Error>> {
    let context = ndk_context::android_context();
    // Android owns both pointers for the app process lifetime. The verifier
    // takes global references and validates HTTPS with the system trust store.
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
    vm.attach_current_thread(|env| {
        let object = unsafe { jni::objects::JObject::from_raw(env, context.context().cast()) };
        rustls_platform_verifier::android::init_with_env(env, object)
    })?;
    Ok(())
}

pub fn run() {
    let app = crate::transport::resources(tauri::Builder::default())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            desktop_account,
            crate::phone_use::phone_use_status,
            crate::phone_use::phone_use_configure,
            crate::phone_use::phone_use_display_configure,
            crate::transport::native_http_start,
            crate::transport::native_http_write,
            crate::transport::native_http_send,
            crate::transport::native_http_read,
            crate::transport::native_http_cancel,
            crate::transport::native_resource_download
        ])
        .setup(|app| {
            let _ = APP.set(app.handle().clone());
            platform_tls()?;
            crate::phone_use::initialize()?;
            let service =
                tauri::async_runtime::block_on(MobileService::start(app.path().app_data_dir()?))
                    .map_err(|_| "Cannot start Daat Locus")?;
            let origin = url::Url::parse(&service.origin)?;
            let _ = SERVICE.set(Arc::downgrade(&service));
            crate::transport::capability(app.handle())?;
            app.manage(crate::transport::Transport::new(Arc::new(
                std::sync::RwLock::new(origin),
            ))?);
            app.manage(Client(service));
            let handle = app.handle().clone();
            let popup_handle = handle.clone();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Daat Locus")
                .initialization_script(
                    "Object.defineProperty(window,'__DAAT_LOCUS_MOBILE__',{value:'android'});",
                )
                .on_page_load(|window, payload| {
                    if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished)
                        && crate::transport::navigation(window.app_handle(), payload.url())
                    {
                        apply_safe_area(&window);
                    }
                    if matches!(payload.event(), tauri::webview::PageLoadEvent::Started)
                        && crate::transport::navigation(window.app_handle(), payload.url())
                    {
                        window.state::<crate::transport::Transport>().clear();
                    }
                })
                .on_navigation(move |url| {
                    if crate::transport::navigation(&handle, url)
                        || crate::transport::resource_url(url)
                    {
                        return true;
                    }
                    if matches!(url.scheme(), "http" | "https") {
                        let _ = handle.opener().open_url(url.as_str(), None::<&str>);
                    }
                    false
                })
                .on_new_window(move |url, _| {
                    if matches!(url.scheme(), "http" | "https") {
                        let _ = popup_handle.opener().open_url(url.as_str(), None::<&str>);
                    }
                    tauri::webview::NewWindowResponse::Deny
                })
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Cannot initialize Daat Locus");
    app.run(|handle, event| {
        let active = match event {
            tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::Suspended,
                ..
            } => Some(false),
            tauri::RunEvent::Resumed => Some(true),
            _ => None,
        };
        if let Some(active) = active
            && let Some(client) = handle.try_state::<Client>()
        {
            let service = client.0.clone();
            service.set_foreground(active);
            tauri::async_runtime::spawn(async move {
                if service.apply_foreground().await.is_err() {
                    tracing::warn!("Mobile execution lifecycle transition failed");
                }
            });
        }
    });
}
