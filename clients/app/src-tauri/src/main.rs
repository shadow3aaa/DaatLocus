#![cfg_attr(windows, windows_subsystem = "windows")]
mod backend;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod browser_data;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod browser_preferences;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod browser_ui;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod browser_use;
#[cfg(target_os = "macos")]
mod computer_use;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod desktop;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod transport;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod web_host;
#[cfg(windows)]
mod windows_tray;

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    #[cfg(all(target_os = "macos", feature = "native-contract"))]
    if let Some(output) = std::env::var_os("DAAT_LOCUS_NATIVE_CONTRACT") {
        let output = std::path::PathBuf::from(output);
        let _ = std::fs::write(&output, serde_json::json!({"phase":"starting"}).to_string());
        std::panic::set_hook(Box::new(move |panic| {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&output) {
                let _ = writeln!(
                    file,
                    "\n{}\n{}",
                    panic,
                    std::backtrace::Backtrace::force_capture()
                );
            }
        }));
    }
    use clap::Parser;
    let options = backend::Options::parse();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if let Err(error) = desktop::run(options) {
        eprintln!("Daat Locus: {error}");
        #[cfg(all(target_os = "macos", feature = "native-contract"))]
        if let Some(output) = std::env::var_os("DAAT_LOCUS_NATIVE_CONTRACT") {
            let _ = std::fs::write(
                output,
                serde_json::json!({"passed":false,"startup_error":error.to_string()}).to_string(),
            );
        }
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
