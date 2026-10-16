#![cfg_attr(windows, windows_subsystem = "windows")]
mod backend;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod desktop;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod transport;
#[cfg(windows)]
mod windows_tray;

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    use clap::Parser;
    let options = backend::Options::parse();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if let Err(error) = desktop::run(options) {
        eprintln!("Daat Locus: {error}");
        desktop::startup_error();
        std::process::exit(1);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = options;
        eprintln!("The desktop client currently supports Windows and macOS.");
        std::process::exit(1);
    }
}
