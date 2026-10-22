/// Coordinate native HANDLE_LIST spawns with ordinary Rust process creation.
/// Rust's private creation lock cannot protect handles prepared by our native broker.
#[cfg(windows)]
pub fn creation_lock() -> std::io::Result<std::sync::MutexGuard<'static, ()>> {
    static CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    CREATION
        .lock()
        .map_err(|_| std::io::Error::other("Process creation lock is unavailable"))
}
