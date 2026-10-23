use tauri::Manager;
#[tauri::command]
pub async fn computer_use_status(
    window: tauri::WebviewWindow,
) -> Result<daat_locus_computer_use::Permissions, String> {
    crate::transport::authorize_owner(&window).await?;
    let root = window
        .state::<crate::desktop::Client>()
        .backend
        .root
        .clone();
    tokio::task::spawn_blocking(move || {
        daat_locus_computer_use::permissions(&root).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "GUI permission check interrupted")?
}
#[tauri::command]
pub async fn computer_use_configure(
    window: tauri::WebviewWindow,
    enabled: bool,
) -> Result<daat_locus_computer_use::Permissions, String> {
    crate::transport::authorize_owner(&window).await?;
    let root = window
        .state::<crate::desktop::Client>()
        .backend
        .root
        .clone();
    let server = window
        .state::<std::sync::Arc<daat_locus_computer_use::Server>>()
        .inner()
        .clone();
    tokio::task::spawn_blocking(move || server.configure(&root, enabled).map_err(|e| e.to_string()))
        .await
        .map_err(|_| "GUI configuration interrupted")?
}
#[tauri::command]
pub async fn computer_use_permission(
    window: tauri::WebviewWindow,
    kind: String,
) -> Result<(), String> {
    crate::transport::authorize_owner(&window).await?;
    tokio::task::spawn_blocking(move || {
        daat_locus_computer_use::request_permission(&kind).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "GUI permission request interrupted")?
}

#[tauri::command]
pub async fn computer_use_display_configure(
    window: tauri::WebviewWindow,
    display: daat_locus_computer_use::DisplayConfig,
) -> Result<daat_locus_computer_use::Permissions, String> {
    crate::transport::authorize_owner(&window).await?;
    let root = window
        .state::<crate::desktop::Client>()
        .backend
        .root
        .clone();
    let server = window
        .state::<std::sync::Arc<daat_locus_computer_use::Server>>()
        .inner()
        .clone();
    tokio::task::spawn_blocking(move || {
        server
            .configure_display(&root, display)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "GUI display configuration interrupted")?
}
