use daat_locus::mobile::MobileService;
use std::sync::Arc;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

struct Client(Arc<MobileService>);
#[tauri::command]
async fn desktop_account(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Client>,
    input: daat_locus_client_protocol::BootstrapRequest,
) -> Result<daat_locus_client_protocol::BootstrapReply, String> {
    let current = window.url().map_err(|_| "Window unavailable")?;
    let expected = url::Url::parse(&state.0.origin).map_err(|_| "Service unavailable")?;
    if window.label() != "main"
        || current.origin() != expected.origin()
        || current.path().starts_with("/v1")
    {
        return Err("Account operation unavailable".into());
    }
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
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![desktop_account])
        .setup(|app| {
            platform_tls()?;
            let service = tauri::async_runtime::block_on(MobileService::start(app.path().app_data_dir()?))
                .map_err(|_| "Cannot start Daat Locus")?;
            let origin = url::Url::parse(&service.origin)?;
            let capability = serde_json::json!({
                "identifier":"mobile-account", "windows":["main"], "local":false,
                "remote":{"urls":[format!("{}/",service.origin),format!("{}/actors/*",service.origin),format!("{}/stages/*",service.origin)]},
                "permissions":["allow-desktop-account"]
            });
            app.add_capability(capability.to_string())?;
            app.manage(Client(service));
            let navigation_origin = origin.clone();
            let handle = app.handle().clone();
            let popup_handle = handle.clone();
            WebviewWindowBuilder::new(app,"main",WebviewUrl::External(origin))
                .title("Daat Locus")
                .initialization_script("Object.defineProperty(window,'__DAAT_LOCUS_MOBILE__',{value:'android'});")
                .on_navigation(move |url| {
                    if url.origin() == navigation_origin.origin() { return true; }
                    if matches!(url.scheme(),"http"|"https") { let _ = handle.opener().open_url(url.as_str(),None::<&str>); }
                    false
                })
                .on_new_window(move |url,_| {
                    if matches!(url.scheme(),"http"|"https") { let _ = popup_handle.opener().open_url(url.as_str(),None::<&str>); }
                    tauri::webview::NewWindowResponse::Deny
                })
                .build()?;
            Ok(())
        }).build(tauri::generate_context!()).expect("Cannot initialize Daat Locus");
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
