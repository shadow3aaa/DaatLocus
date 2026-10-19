#[cfg(target_os = "android")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
mod phone_use;
#[cfg(target_os = "android")]
mod transport;
#[cfg(target_os = "android")]
#[tauri::mobile_entry_point]
pub fn run() {
    android::run();
}
