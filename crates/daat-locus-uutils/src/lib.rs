//! Portable commands linked into a multicall host and exposed through real PATH entries.
//! Dispatch must run before the host initializes its runtime or parses its own CLI.
use std::{ffi::OsString, path::Path};

mod command_path;
mod commands;
pub use command_path::CommandPath;

// Keep the upstream CLI modules at the crate root to preserve their internal paths.
#[allow(clippy::all)]
#[rustfmt::skip]
#[macro_use]
#[path = "../vendor/ripgrep/messages.rs"]
mod messages;
#[allow(clippy::all)]
#[rustfmt::skip]
#[path = "../vendor/ripgrep/flags/mod.rs"]
mod flags;
#[allow(clippy::all)]
#[rustfmt::skip]
#[path = "../vendor/ripgrep/haystack.rs"]
mod haystack;
#[allow(clippy::all)]
#[rustfmt::skip]
#[path = "../vendor/ripgrep/logger.rs"]
mod logger;
#[allow(clippy::all)]
#[rustfmt::skip]
#[path = "../vendor/ripgrep/search.rs"]
mod search;
#[allow(clippy::all)]
#[rustfmt::skip]
#[path = "../vendor/ripgrep/main.rs"]
mod ripgrep;

pub fn names() -> impl Iterator<Item = &'static str> {
    commands::NAMES
        .iter()
        .copied()
        .chain(["rg", "grep", "find", "xargs"])
}

/// Recognize only exact applet basenames. Environment variables never select a
/// privileged host mode; applets inherit the ordinary process sandbox.
pub fn try_run() -> Option<std::process::ExitCode> {
    let program = std::env::args_os().next()?;
    let name = Path::new(&program)
        .file_name()
        .and_then(|name| name.to_str())?;
    #[cfg(windows)]
    let name = name.to_ascii_lowercase();
    #[cfg(windows)]
    let name = name.as_str();
    let name = name
        .strip_suffix(std::env::consts::EXE_SUFFIX)
        .unwrap_or(name);
    if matches!(name, "find" | "xargs") {
        uucore::panic::mute_sigpipe_panic();
        let args = match std::env::args_os()
            .map(OsString::into_string)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(args) => args,
            Err(argument) => {
                eprintln!(
                    "{name}: invalid non-UTF-8 argument: {}",
                    argument.to_string_lossy()
                );
                std::process::exit(1);
            }
        };
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        let code = if name == "find" {
            findutils::find::find_main(&args, &findutils::find::StandardDependencies::new())
        } else {
            findutils::xargs::xargs_main(&args)
        };
        std::process::exit(code);
    }
    if matches!(name, "rg" | "grep") {
        // The upstream CLI owns process-global logging and streams; invoke it
        // once, in the child process, with its original argument parser.
        return Some(ripgrep::main());
    }
    if let Some(code) = commands::run(name, std::env::args_os().collect::<Vec<OsString>>()) {
        std::process::exit(code);
    }
    None
}
