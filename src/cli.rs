use std::{io::Read, sync::Arc};

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
use miette::{IntoDiagnostic, Result, miette};
use std::path::PathBuf;

pub(crate) fn parse_args() -> Cli {
    Cli::parse()
}

#[derive(Debug, Parser)]
#[command(name = "daat-locus", about = "Daat Locus Agent")]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Option<DaatLocusCommand>,
    #[arg(long, hide = true)]
    session_id: Option<String>,
    #[arg(long, hide = true)]
    ipc_name: Option<PathBuf>,
    #[arg(long, hide = true)]
    ipc_token: Option<String>,
    #[arg(long, hide = true)]
    session_project_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum DaatLocusCommand {
    /// Start the foreground runtime flow.
    Run,
    /// Open a coding session tied to a project directory.
    #[command(name = "code")]
    Code {
        /// Project directory path.
        project_dir: PathBuf,
    },
    /// Attach to an already-running daemon.
    Attach,
    /// Send a one-shot message to the running agent and wait for its reply.
    Send {
        /// Print the raw final reply instead of terminal-rendered Markdown.
        #[arg(long)]
        raw: bool,
        /// Print JSON metadata; ignores --raw.
        #[arg(long)]
        json: bool,
        /// Prompt text. Multiple words are joined with spaces; stdin is used when omitted.
        prompt: Vec<String>,
    },
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
            run_session_selector(client, None).await?;
            return Ok(());
        }
        Some(DaatLocusCommand::Send { raw, json, prompt }) => {
            run_send_command(prompt, *raw, *json).await?;
            return Ok(());
        }
        _ => {}
    }

    if matches!(cli.command, None | Some(DaatLocusCommand::Run))
        && let Ok(client) = connect_existing_daemon().await
    {
        run_session_selector(client, None).await?;
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
        if let Some(session_id) = cli.session_id.clone() {
            let ipc_name = cli
                .ipc_name
                .clone()
                .ok_or_else(|| miette!("--ipc-name is required for session serve"))?;
            let ipc_token = cli
                .ipc_token
                .clone()
                .ok_or_else(|| miette!("--ipc-token is required for session serve"))?;
            crate::runtime::session_server::run_session_serve(
                config,
                crate::runtime::session_server::SessionServeArgs {
                    session_id,
                    ipc_name: ipc_name.display().to_string(),
                    ipc_token,
                    project_dir: cli.session_project_dir.clone(),
                },
            )
            .await?;
            return Ok(());
        }
        crate::runtime::daemon_server::run_daemon_serve(config).await?;
        return Ok(());
    }

    if let Some(DaatLocusCommand::Code { project_dir }) = cli.command.as_ref() {
        run_code_command(project_dir.clone()).await?;
        return Ok(());
    }

    if matches!(cli.command, None | Some(DaatLocusCommand::Run)) {
        let client = connect_or_start_daemon().await?;
        run_session_selector(client, None).await?;
        return Ok(());
    }
    Ok(())
}

async fn run_code_command(project_dir: PathBuf) -> Result<()> {
    let project_dir_abs = std::fs::canonicalize(&project_dir).map_err(|err| {
        miette!(
            "cannot resolve project directory {}: {err}",
            project_dir.display()
        )
    })?;
    let client = connect_or_start_daemon().await?;
    run_session_selector(client, Some(project_dir_abs)).await
}

async fn run_send_command(prompt: &[String], raw: bool, json: bool) -> Result<()> {
    let message = read_send_message(prompt)?;
    let client = connect_or_start_daemon().await?;
    let client = default_general_session_client(client).await?;
    let response = client.send_message(&message).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response)
                .map_err(|err| miette!("encode send response as JSON failed: {err}"))?
        );
        return Ok(());
    }

    let reply = response.reply_message.unwrap_or_default();
    if raw {
        println!("{reply}");
    } else {
        print_terminal_markdown(&reply);
    }
    Ok(())
}

async fn default_general_session_client(client: DaemonClient) -> Result<DaemonClient> {
    let sessions = client.list_sessions().await?;
    if let Some(session) = sessions
        .iter()
        .find(|session| matches!(session.scope, crate::daemon::session::SessionScope::General))
    {
        return Ok(client.with_session(session.session_id.as_str().to_string()));
    }

    let session = client.create_session(None, Some("CLI Send")).await?;
    Ok(client.with_session(session.session_id.as_str().to_string()))
}

fn read_send_message(prompt: &[String]) -> Result<String> {
    let message = if prompt.is_empty() {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .into_diagnostic()
            .map_err(|err| miette!("read send prompt from stdin failed: {err}"))?;
        buffer
    } else {
        prompt.join(" ")
    };
    let trimmed = message.trim().to_string();
    if trimmed.is_empty() {
        return Err(miette!("send prompt is empty"));
    }
    Ok(trimmed)
}

fn print_terminal_markdown(markdown: &str) {
    let lines =
        crate::dashboard::cells::markdown::render_markdown(markdown, ratatui::style::Color::White);
    if lines.is_empty() {
        println!();
        return;
    }
    for line in lines {
        let text = line
            .spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect::<String>();
        println!("{text}");
    }
}

async fn run_session_selector(client: DaemonClient, project_dir: Option<PathBuf>) -> Result<()> {
    use crossterm::event::{Event, KeyCode, KeyEventKind};
    use ratatui::{
        DefaultTerminal,
        layout::{Constraint, Layout},
        style::{Color, Modifier, Style, Stylize},
        text::Line,
        widgets::{Block, Borders, List, ListState, Paragraph},
    };

    let mut sessions = filtered_sessions(&client, project_dir.as_deref()).await;
    let mut state = ListState::default();
    if !sessions.is_empty() {
        state.select(Some(0));
    }

    let mut terminal: Option<DefaultTerminal> = None;
    let mut last_refresh = std::time::Instant::now();
    let refresh_interval = std::time::Duration::from_secs(2);

    let result: Result<()> = async {
        loop {
            let now = std::time::Instant::now();
            if now.duration_since(last_refresh) >= refresh_interval {
                sessions = filtered_sessions(&client, project_dir.as_deref()).await;
                last_refresh = now;
                if state.selected().unwrap_or(0) >= sessions.len() && !sessions.is_empty() {
                    state.select(Some(sessions.len() - 1));
                }
            }
            if terminal.is_none() {
                let _ = ratatui::try_restore();
                terminal =
                    Some(ratatui::try_init().map_err(|e| miette!("failed to init terminal: {e}"))?);
                while crossterm::event::poll(std::time::Duration::from_millis(0)).unwrap_or(false) {
                    let _ = crossterm::event::read();
                }
            }
            let t = terminal.as_mut().unwrap();
            let title = if let Some(project_dir) = project_dir.as_ref() {
                format!(" Daat Locus Code Sessions: {} ", project_dir.display())
            } else {
                " Daat Locus Sessions ".to_string()
            };

            t.draw(|frame| {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(Line::from(vec![title.as_str().bold().cyan()]));
                let inner = block.inner(frame.area());
                frame.render_widget(block, frame.area());

                let layout = Layout::vertical([
                    Constraint::Min(3),
                    Constraint::Length(1),
                    Constraint::Length(2),
                ])
                .split(inner);

                if sessions.is_empty() {
                    let p =
                        Paragraph::new("(no sessions)").style(Style::default().fg(Color::DarkGray));
                    frame.render_widget(p, layout[0]);
                } else {
                    let items: Vec<String> = sessions
                        .iter()
                        .map(|s| {
                            let name = s
                                .title
                                .as_deref()
                                .filter(|title| !title.trim().is_empty())
                                .unwrap_or("Untitled session");
                            match &s.scope {
                                crate::daemon::session::SessionScope::General => name.to_string(),
                                crate::daemon::session::SessionScope::Project { project_dir } => {
                                    format!("{name}  [{}]", project_dir.display())
                                }
                            }
                        })
                        .collect();

                    let list = List::new(items)
                        .block(Block::default())
                        .highlight_symbol("> ")
                        .highlight_style(
                            Style::default()
                                .fg(Color::White)
                                .bg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        )
                        .repeat_highlight_symbol(true);
                    frame.render_stateful_widget(list, layout[0], &mut state);
                }

                let help = Paragraph::new("n new  d delete  t title  Enter attach  q quit")
                    .style(Style::default().fg(Color::DarkGray));
                frame.render_widget(help, layout[2]);
            })
            .map_err(|e| miette!("render error: {e}"))?;

            let ev = crossterm::event::read().map_err(|e| miette!("input error: {e}"))?;
            let key = match ev {
                Event::Key(k) if k.kind == KeyEventKind::Press => k,
                _ => continue,
            };

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('n') => {
                    let title = project_dir
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .and_then(|name| name.to_str());
                    if client
                        .create_session(project_dir.as_deref(), title)
                        .await
                        .is_ok()
                    {
                        sessions = filtered_sessions(&client, project_dir.as_deref()).await;
                        if !sessions.is_empty() {
                            state.select(Some(sessions.len() - 1));
                        }
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(idx) = state.selected()
                        && let Some(s) = sessions.get(idx)
                        && client.delete_session(s.session_id.as_str()).await.is_ok()
                    {
                        sessions = filtered_sessions(&client, project_dir.as_deref()).await;
                        if state.selected().unwrap_or(0) >= sessions.len() && !sessions.is_empty() {
                            state.select(Some(sessions.len() - 1));
                        }
                    }
                }
                KeyCode::Char('t') => {
                    if let Some(idx) = state.selected()
                        && let Some(s) = sessions.get(idx)
                    {
                        let session_id = s.session_id.as_str().to_string();
                        let current_title = s.title.clone().unwrap_or_default();
                        drop(terminal.take());
                        let _ = ratatui::try_restore();
                        print!(
                            "Title [{}]: ",
                            if current_title.is_empty() {
                                "untitled"
                            } else {
                                current_title.as_str()
                            }
                        );
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                        let mut input = String::new();
                        if std::io::stdin().read_line(&mut input).is_ok() {
                            let title = input.trim().to_string();
                            if !title.is_empty() {
                                let _ = client.set_session_title(&session_id, &title).await;
                            }
                        }
                        sessions = filtered_sessions(&client, project_dir.as_deref()).await;
                        terminal = Some(
                            ratatui::try_init()
                                .map_err(|e| miette!("failed to reinit terminal: {e}"))?,
                        );
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let current = state.selected().unwrap_or(0);
                    let next = if current == 0 {
                        sessions.len().saturating_sub(1)
                    } else {
                        current - 1
                    };
                    state.select(if sessions.is_empty() {
                        None
                    } else {
                        Some(next)
                    });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let current = state.selected().unwrap_or(0);
                    let next = if current + 1 >= sessions.len() {
                        0
                    } else {
                        current + 1
                    };
                    state.select(if sessions.is_empty() {
                        None
                    } else {
                        Some(next)
                    });
                }
                KeyCode::Enter => {
                    if let Some(idx) = state.selected()
                        && let Some(s) = sessions.get(idx)
                    {
                        let session_id = s.session_id.as_str().to_string();
                        drop(terminal.take());
                        let _ = ratatui::try_restore();
                        return attach_to_daemon(client.clone().with_session(session_id)).await;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    .await;

    let _ = ratatui::try_restore();
    result
}

async fn filtered_sessions(
    client: &DaemonClient,
    project_dir: Option<&std::path::Path>,
) -> Vec<crate::daemon::session::SessionSummary> {
    let Ok(sessions) = client.list_sessions().await else {
        return Vec::new();
    };
    sessions
        .into_iter()
        .filter(|session| match (&session.scope, project_dir) {
            (crate::daemon::session::SessionScope::General, None) => true,
            (
                crate::daemon::session::SessionScope::Project {
                    project_dir: stored,
                },
                Some(project_dir),
            ) => stored == project_dir,
            _ => false,
        })
        .collect()
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
