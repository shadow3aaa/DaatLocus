rust_i18n::i18n!("locales", fallback = "en-US");

mod app;
mod apply_patch;
mod browser_app;
mod browser_install;
mod cli;
mod commands;
mod config;
mod config_wizard;
mod context;
mod context_budget;
mod core;
mod daat_locus_paths;
mod daemon;
mod dashboard;
mod dsml_repair;
mod events;
mod hindsight;
mod i18n;
mod live_progress;
mod logging;
mod memory;
mod model_catalog;
mod pending_work;
mod persistence;
mod plan;
mod preturn_state;
mod providers;
mod reasoning;
mod runtime;
mod runtime_context;
mod runtime_tools;
mod sandbox;
mod schema_utils;
mod sleep_status;
mod system_info;
mod telegram_acl;
mod telegram_transport;
mod terminal_app;
mod tool_ui;
mod workflow;
mod workspace_app;

pub(crate) use runtime::bootstrap::{DaatLocusHomeOverride, build_eval_context_with_compiled};
pub(crate) use runtime::patch_tool::execute_apply_patch_tool;
pub(crate) use runtime::runtime_loop::{AgentLoopStepOutput, execute_agent_loop_step};

fn main() {
    let cli = cli::parse_args();

    if let Err(err) = crate::daemon::daemonize_current_process_if_requested() {
        eprintln!("{err:?}");
        std::process::exit(1);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    if let Err(err) = runtime.block_on(cli::async_main(cli)) {
        eprintln!("{err:?}");
        std::process::exit(1);
    }
}
