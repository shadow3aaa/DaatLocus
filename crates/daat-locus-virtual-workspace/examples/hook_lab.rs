//! Disposable cross-process experiment. Both target windows share one GUI thread.
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dll: Option<PathBuf>,
    /// Exercise the IDD workspace API as well as the hook. Requires the installed driver.
    #[arg(long)]
    owned_display: bool,
    /// Keep the original no-driver, two-window diagnostic instead of the IDD preview.
    #[arg(long, conflicts_with = "owned_display")]
    physical_screen: bool,
    /// Start automatically and close the preview after verification.
    #[arg(long)]
    self_test: bool,
    /// Start automatically, keeping the preview open after verification.
    #[arg(long)]
    run_tests: bool,
    /// Measure genuine touch with default compatibility promotion (may move the cursor).
    #[arg(long)]
    native_touch: bool,
    #[arg(long, hide = true)]
    fixture: bool,
    #[arg(long, hide = true)]
    lease_target: Option<usize>,
    #[arg(long, hide = true, default_value_t = 500)]
    agent_x: i32,
    #[arg(long, hide = true, default_value_t = 100)]
    agent_y: i32,
    #[arg(long, hide = true)]
    preview_display: Option<String>,
}

#[cfg(windows)]
#[path = "hook_support/mod.rs"]
mod support;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    #[cfg(windows)]
    if args.fixture {
        support::fixture(args.agent_x, args.agent_y, args.preview_display.as_deref())
    } else if let Some(target) = args.lease_target {
        support::lease_owner(target, args.dll.ok_or("--dll required")?)
    } else {
        support::run(
            args.dll.unwrap_or(
                std::env::current_exe()?
                    .parent()
                    .ok_or("missing example directory")?
                    .join("../daat_locus_workspace_hook.dll"),
            ),
            !args.physical_screen,
            args.native_touch,
            args.self_test,
            args.self_test || args.run_tests,
        )
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Err("Windows interactive desktop required".into())
    }
}
