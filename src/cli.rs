use crate::{
    commands::reset::{run_complite_reset, run_memory_reset, run_reset_all, run_state_reset},
    config::load_config,
    daemon::{
        DaemonClient, connect_existing_daemon, connect_or_start_daemon,
        spawn_detached_daemon_process, status_summary, wait_for_daemon_ready,
        wait_for_daemon_shutdown,
    },
    dashboard::run_tui_dashboard,
    i18n::Locale,
    logging::init_logging,
};
use crate::{config, config_wizard};
use clap::{Parser, Subcommand};
use miette::{Result, miette};

pub(crate) fn parse_args() -> Cli {
    Cli::parse()
}

#[derive(Debug, Parser)]
#[command(name = "daat-locus", about = "Daat Locus Agent")]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Option<DaatLocusCommand>,
}

#[derive(Debug, Subcommand)]
enum DaatLocusCommand {
    /// Start the foreground runtime flow.
    Run,
    /// Attach to an already-running daemon.
    Attach,
    /// Manage the background daemon process.
    Daemon {
        #[command(subcommand)]
        target: DaemonTarget,
    },
    /// Clear local state or cache data.
    Reset {
        #[command(subcommand)]
        target: ResetTarget,
    },
    /// Manage configuration; opens an interactive menu without a subcommand.
    Config {
        #[command(subcommand)]
        target: Option<ConfigTarget>,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigTarget {
    /// Show the current config summary with secrets masked.
    Show,
    /// Add a provider interactively.
    #[command(name = "add-provider")]
    AddProvider,
    /// Add a model interactively.
    #[command(name = "add-model")]
    AddModel,
    /// Change the main model.
    #[command(name = "set-main-model")]
    SetMainModel,
    /// Change the model used by hindsight.
    #[command(name = "set-hindsight-model")]
    SetHindsightModel,
}

#[derive(Debug, Subcommand)]
enum ResetTarget {
    /// Clear compiled prompt cache.
    #[command(name = "complite", alias = "compile")]
    Complite,
    /// Clear runtime state such as daemon locks and sockets.
    State,
    /// Clear conversation history, hindsight records, and reasoning traces.
    Memory,
    /// Clear state, memory, and complite data.
    All,
}

#[derive(Debug, Subcommand)]
enum DaemonTarget {
    /// Show daemon status.
    Status,
    /// Stop the background daemon.
    Stop,
    /// Restart the background daemon.
    Restart,
    /// Start the daemon in the foreground.
    Serve,
}

pub(crate) async fn async_main(cli: Cli) -> Result<()> {
    let _log_guard = init_logging().await;

    match cli.command.as_ref() {
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::Complite,
        }) => {
            run_complite_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::State,
        }) => {
            run_state_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::Memory,
        }) => {
            run_memory_reset().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::All,
        }) => {
            run_reset_all().await?;
            return Ok(());
        }
        // Config subcommands may run before a complete config exists.
        Some(DaatLocusCommand::Config { target }) => {
            return run_config_command(target.as_ref()).await;
        }
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Status,
        }) => {
            run_daemon_status_command().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Stop,
        }) => {
            run_daemon_stop_command().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Restart,
        }) => {
            run_daemon_restart_command().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Attach) => {
            let client = connect_existing_daemon().await?;
            attach_to_daemon(client).await?;
            return Ok(());
        }
        _ => {}
    }

    if matches!(cli.command, None | Some(DaatLocusCommand::Run))
        && let Ok(client) = connect_existing_daemon().await
    {
        attach_to_daemon(client).await?;
        return Ok(());
    }

    // First run starts the interactive setup when config.toml is missing.
    let config = if !config::config_file_exists().await {
        match config_wizard::run_first_time_setup().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "{}",
                    crate::tr!(
                        Locale::default(),
                        "cli.setup_failed",
                        error = format!("{e:?}")
                    )
                );
                std::process::exit(1);
            }
        }
    } else {
        match load_config().await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!("failed to load config: {e}");
                eprintln!(
                    "{}",
                    crate::tr!(
                        Locale::default(),
                        "common.config_load_failed",
                        error = format!("{e:?}")
                    )
                );
                std::process::exit(1);
            }
        }
    };

    match cli.command.as_ref() {
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Serve,
        }) => {
            crate::runtime::daemon_server::run_daemon_serve(config).await?;
            return Ok(());
        }
        _ => {}
    }

    if matches!(cli.command, None | Some(DaatLocusCommand::Run)) {
        let client = connect_or_start_daemon().await?;
        attach_to_daemon(client).await?;
        return Ok(());
    }
    Ok(())
}

/// Tail `~/.hindsight/profiles/<profile>.log` and forward new lines to stdout/tracing.
/// Designed to run concurrently with `HindsightManagedServer::start()` so the user

async fn run_config_command(target: Option<&ConfigTarget>) -> Result<()> {
    match target {
        None => config_wizard::run_config_menu().await,
        Some(ConfigTarget::Show) => config_wizard::show_config().await,
        Some(ConfigTarget::AddProvider) => config_wizard::run_add_provider().await,
        Some(ConfigTarget::AddModel) => config_wizard::run_add_model().await,
        Some(ConfigTarget::SetMainModel) => config_wizard::run_set_main_model().await,
        Some(ConfigTarget::SetHindsightModel) => config_wizard::run_set_hindsight_model().await,
    }
}

async fn run_daemon_status_command() -> Result<()> {
    let client = connect_existing_daemon().await?;
    let status = client.status().await?;
    println!("{}", status_summary(&status));
    Ok(())
}

async fn run_daemon_stop_command() -> Result<()> {
    let locale = configured_locale().await;
    let client = connect_existing_daemon().await?;
    let port = client.port();
    client.shutdown().await?;
    wait_for_daemon_shutdown(port).await?;
    println!("{}", crate::tr!(locale, "daemon.stopped"));
    Ok(())
}

async fn run_daemon_restart_command() -> Result<()> {
    let locale = configured_locale().await;
    if let Ok(client) = connect_existing_daemon().await {
        let port = client.port();
        client.shutdown().await?;
        wait_for_daemon_shutdown(port).await?;
    }
    spawn_detached_daemon_process().await?;
    let status = wait_for_daemon_ready().await?;
    println!(
        "{}",
        crate::tr!(locale, "daemon.restarted", status = status_summary(&status))
    );
    Ok(())
}

async fn configured_locale() -> Locale {
    load_config()
        .await
        .map(|config| config.locale)
        .unwrap_or_default()
}

async fn attach_to_daemon(client: DaemonClient) -> Result<()> {
    let initial = client.snapshot().await?;
    let (tx, mut rx) = tokio::sync::watch::channel(initial);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let stream_client = DaemonClient::new(client.port());
    let stream_task = tokio::spawn(async move {
        let _ = stream_client.stream_to(tx, stop_rx).await;
    });
    run_tui_dashboard(&mut rx, &client)
        .await
        .map_err(|err| miette!("dashboard attach failed: {err}"))?;
    let _ = stop_tx.send(());
    let _ = stream_task.await;
    Ok(())
}
