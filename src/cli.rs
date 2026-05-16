use std::sync::Arc;

use crate::{
    commands::reset::{run_compile_reset, run_memory_reset, run_reset_all, run_state_reset},
    config::load_config,
    daemon::{
        CreatedDaemonToken, DaemonClient, DaemonTokenListEntry, connect_daemon_status,
        connect_existing_daemon, connect_or_start_daemon, create_daemon_token, list_daemon_tokens,
        revoke_daemon_token, rotate_daemon_token, spawn_detached_daemon_process, status_summary,
        wait_for_daemon_ready, wait_for_daemon_shutdown,
    },
    dashboard::run_tui_dashboard,
    i18n::Locale,
    logging::init_logging,
};
use crate::{config, config_wizard};
use clap::{Parser, Subcommand};
use miette::{Result, miette};
use std::path::PathBuf;

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
    /// Print the JSON Schema for config.toml.
    #[command(name = "config-schema")]
    ConfigSchema,
    /// Internal workspace app worker process.
    #[command(name = "workspace-app-worker", hide = true)]
    WorkspaceAppWorker {
        #[arg(long)]
        app_id: String,
        #[arg(long)]
        app_dir: PathBuf,
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        entry: String,
        #[arg(long)]
        connect_addr: String,
        #[arg(long)]
        token: String,
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
    /// Change the efficient model.
    #[command(name = "set-efficient-model")]
    SetEfficientModel,
    /// Configure Telegram transport.
    #[command(name = "set-telegram")]
    SetTelegram,
}

#[derive(Debug, Subcommand)]
enum ResetTarget {
    /// Clear compiled prompt cache.
    #[command(name = "compile", alias = "complite")]
    Compile,
    /// Clear runtime state such as daemon locks and sockets.
    State,
    /// Clear conversation history, runtime memory records, and reasoning traces.
    Memory,
    /// Clear state, memory, and compile data.
    All,
}

#[derive(Debug, Subcommand)]
enum DaemonTarget {
    /// Show daemon status.
    Status,
    /// Manage daemon access tokens.
    Token {
        #[command(subcommand)]
        target: DaemonTokenTarget,
    },
    /// Stop the background daemon.
    Stop,
    /// Restart the background daemon.
    Restart,
    /// Start the daemon in the foreground.
    Serve,
}

#[derive(Debug, Subcommand)]
enum DaemonTokenTarget {
    /// Create a new full-access daemon token.
    Create { name: String },
    /// List daemon token metadata without revealing token secrets.
    List,
    /// Revoke a daemon token by id or name.
    Revoke { selector: String },
    /// Rotate a daemon token by id or name and print the new secret once.
    Rotate { selector: String },
}

pub(crate) async fn async_main(cli: Cli) -> Result<()> {
    let _log_guard = init_logging().await;

    match cli.command.as_ref() {
        Some(DaatLocusCommand::WorkspaceAppWorker {
            app_id,
            app_dir,
            state_dir,
            entry,
            connect_addr,
            token,
        }) => {
            crate::workspace_app::worker::run_workspace_app_worker(
                crate::workspace_app::worker::WorkspaceAppWorkerArgs {
                    app_id: app_id.clone(),
                    app_dir: app_dir.clone(),
                    state_dir: state_dir.clone(),
                    entry: entry.clone(),
                    connect_addr: connect_addr.clone(),
                    token: token.clone(),
                },
            )?;
            return Ok(());
        }
        Some(DaatLocusCommand::Reset {
            target: ResetTarget::Compile,
        }) => {
            run_compile_reset().await?;
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
        Some(DaatLocusCommand::ConfigSchema) => {
            print_config_schema()?;
            return Ok(());
        }
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Status,
        }) => {
            run_daemon_status_command().await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Daemon {
            target: DaemonTarget::Token { target },
        }) => {
            run_daemon_token_command(target).await?;
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

    if let Some(DaatLocusCommand::Daemon {
        target: DaemonTarget::Serve,
    }) = cli.command.as_ref()
    {
        crate::runtime::daemon_server::run_daemon_serve(config).await?;
        return Ok(());
    }

    if matches!(cli.command, None | Some(DaatLocusCommand::Run)) {
        let client = connect_or_start_daemon().await?;
        attach_to_daemon(client).await?;
        return Ok(());
    }
    Ok(())
}

async fn run_config_command(target: Option<&ConfigTarget>) -> Result<()> {
    match target {
        None => config_wizard::run_config_menu().await,
        Some(ConfigTarget::Show) => config_wizard::show_config().await,
        Some(ConfigTarget::AddProvider) => config_wizard::run_add_provider().await,
        Some(ConfigTarget::AddModel) => config_wizard::run_add_model().await,
        Some(ConfigTarget::SetMainModel) => config_wizard::run_set_main_model().await,
        Some(ConfigTarget::SetEfficientModel) => config_wizard::run_set_efficient_model().await,
        Some(ConfigTarget::SetTelegram) => config_wizard::run_set_telegram().await,
    }
}

fn print_config_schema() -> Result<()> {
    let schema = schemars::schema_for!(crate::config::Config);
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).map_err(|e| miette!(e))?
    );
    Ok(())
}

async fn run_daemon_status_command() -> Result<()> {
    let client = connect_daemon_status().await?;
    let status = client.status().await?;
    println!("{}", status_summary(&status));
    Ok(())
}

async fn run_daemon_token_command(target: &DaemonTokenTarget) -> Result<()> {
    match target {
        DaemonTokenTarget::Create { name } => {
            let token = create_daemon_token(name).await?;
            print_created_daemon_token("Created daemon token", &token);
        }
        DaemonTokenTarget::List => {
            let tokens = list_daemon_tokens().await?;
            print_daemon_token_list(&tokens);
        }
        DaemonTokenTarget::Revoke { selector } => {
            let token = revoke_daemon_token(selector).await?;
            println!("Revoked daemon token {} ({})", token.name, token.id);
        }
        DaemonTokenTarget::Rotate { selector } => {
            let token = rotate_daemon_token(selector).await?;
            print_created_daemon_token("Rotated daemon token", &token);
        }
    }
    Ok(())
}

fn print_created_daemon_token(label: &str, token: &CreatedDaemonToken) {
    println!("{label}:");
    println!("  id: {}", token.id);
    println!("  name: {}", token.name);
    println!("  token: {}", token.token);
    println!("Store this token now; it will not be shown again.");
}

fn print_daemon_token_list(tokens: &[DaemonTokenListEntry]) {
    println!("{:<36}  {:<20}  {:<25}  LAST USED", "ID", "NAME", "CREATED");
    for token in tokens {
        println!(
            "{:<36}  {:<20}  {:<25}  {}",
            token.id,
            token.name,
            format_timestamp_ms(token.created_at_ms),
            token
                .last_used_at_ms
                .map(format_timestamp_ms)
                .unwrap_or_else(|| "-".to_string())
        );
    }
}

fn format_timestamp_ms(timestamp_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms)
        .map(|timestamp| timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| timestamp_ms.to_string())
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
        client.restart().await?;
        wait_for_daemon_shutdown(port).await?;
    } else {
        spawn_detached_daemon_process().await?;
    }
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
    let stream_client = client.clone();
    let stream_task = tokio::spawn(async move {
        let _ = stream_client.stream_to(tx, stop_rx).await;
    });
    run_tui_dashboard(&mut rx, &client, Some(Arc::new(client.clone())))
        .await
        .map_err(|err| miette!("dashboard attach failed: {err}"))?;
    let _ = stop_tx.send(());
    let _ = stream_task.await;
    Ok(())
}
