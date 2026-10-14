#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
#[tauri::mobile_entry_point]
pub fn run() {
    android::run();
}
