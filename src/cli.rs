use std::{io::Read, sync::Arc};

use crate::{
    commands::reset::{run_compile_reset, run_memory_reset, run_reset_all, run_state_reset},
    config::load_config,
    daemon::{
        CreatedDaemonToken, DaemonClient, DaemonTokenListEntry, connect_daemon_status,
        connect_existing_daemon, connect_or_start_daemon, create_daemon_token, list_daemon_tokens,
        revoke_daemon_token, rotate_daemon_token, spawn_detached_daemon_process, status_summary,
        wait_for_daemon_ready, wait_for_daemon_restarted, wait_for_daemon_shutdown,
    },
    dashboard::run_tui_dashboard,
    i18n::Locale,
    logging::init_logging,
};
use crate::{config, config_wizard};
#[cfg(feature = "tui-perf-cmd")]
use clap::Args;
use clap::{Parser, Subcommand};
use miette::{IntoDiagnostic, Result, miette};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{ListItem, ListState},
};
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
    /// Developer-only commands.
    #[cfg(feature = "tui-perf-cmd")]
    #[command(name = "dev", hide = true)]
    Dev {
        #[command(subcommand)]
        target: DevTarget,
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

#[cfg(feature = "tui-perf-cmd")]
#[derive(Debug, Subcommand)]
enum DevTarget {
    /// Run deterministic TUI render performance scenarios.
    #[command(name = "tui-perf", hide = true)]
    TuiPerf(TuiPerfCliArgs),
}

#[cfg(feature = "tui-perf-cmd")]
#[derive(Debug, Args)]
struct TuiPerfCliArgs {
    /// Scenario to render: mixed, long-history, live-activity, command-panels.
    #[arg(long, default_value = "mixed")]
    scenario: String,
    /// Measured frame count after warmup.
    #[arg(long, default_value_t = 120)]
    frames: usize,
    /// Warmup frames excluded from timing aggregates.
    #[arg(long, default_value_t = 10)]
    warmup: usize,
    /// Mock terminal width.
    #[arg(long, default_value_t = 120)]
    width: u16,
    /// Mock terminal height.
    #[arg(long, default_value_t = 40)]
    height: u16,
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
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
        #[cfg(feature = "tui-perf-cmd")]
        Some(DaatLocusCommand::Dev {
            target: DevTarget::TuiPerf(args),
        }) => {
            crate::dashboard::tui_perf::run_tui_perf_command(
                crate::dashboard::tui_perf::TuiPerfCommand {
                    scenario: args.scenario.clone(),
                    frames: args.frames,
                    warmup: args.warmup,
                    width: args.width,
                    height: args.height,
                    json: args.json,
                },
            )?;
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

    let mut sessions = selector_sessions(&client, project_dir.as_deref()).await;
    let mut rows = build_session_tree_rows(&sessions, project_dir.as_deref());
    let mut state = ListState::default();
    select_first_action_row(&mut state, &rows);

    let mut terminal: Option<DefaultTerminal> = None;
    let mut last_refresh = std::time::Instant::now();
    let refresh_interval = std::time::Duration::from_secs(2);

    let result: Result<()> = async {
        loop {
            let now = std::time::Instant::now();
            if now.duration_since(last_refresh) >= refresh_interval {
                let selected_id = selected_session(&sessions, &rows, state.selected())
                    .map(|session| session.session_id.as_str().to_string());
                sessions = selector_sessions(&client, project_dir.as_deref()).await;
                rows = build_session_tree_rows(&sessions, project_dir.as_deref());
                last_refresh = now;
                restore_session_tree_selection(
                    &mut state,
                    &sessions,
                    &rows,
                    selected_id.as_deref(),
                );
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

                if rows.is_empty() {
                    let p =
                        Paragraph::new("(no sessions)").style(Style::default().fg(Color::DarkGray));
                    frame.render_widget(p, layout[0]);
                } else {
                    let items = session_tree_list_items(&sessions, &rows);

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

                let help = Paragraph::new("Enter attach/create  d delete  t title  q quit")
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
                KeyCode::Char('d') => {
                    if let Some(session_id) = selected_session(&sessions, &rows, state.selected())
                        .map(|session| session.session_id.as_str().to_string())
                        && client.delete_session(&session_id).await.is_ok()
                    {
                        sessions = selector_sessions(&client, project_dir.as_deref()).await;
                        rows = build_session_tree_rows(&sessions, project_dir.as_deref());
                        restore_session_tree_selection(&mut state, &sessions, &rows, None);
                    }
                }
                KeyCode::Char('t') => {
                    if let Some(s) = selected_session(&sessions, &rows, state.selected()).cloned() {
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
                        sessions = selector_sessions(&client, project_dir.as_deref()).await;
                        rows = build_session_tree_rows(&sessions, project_dir.as_deref());
                        restore_session_tree_selection(
                            &mut state,
                            &sessions,
                            &rows,
                            Some(session_id.as_str()),
                        );
                        terminal = Some(
                            ratatui::try_init()
                                .map_err(|e| miette!("failed to reinit terminal: {e}"))?,
                        );
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    select_previous_session_row(&mut state, &rows);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    select_next_session_row(&mut state, &rows);
                }
                KeyCode::Enter => {
                    if let Some(target) = selected_create_target(&rows, state.selected()).cloned() {
                        if let Ok(session) = create_selector_session(&client, &target).await {
                            let session_id = session.session_id.as_str().to_string();
                            sessions = selector_sessions(&client, project_dir.as_deref()).await;
                            rows = build_session_tree_rows(&sessions, project_dir.as_deref());
                            restore_session_tree_selection(
                                &mut state,
                                &sessions,
                                &rows,
                                Some(session_id.as_str()),
                            );
                        }
                    } else if let Some(s) = selected_session(&sessions, &rows, state.selected()) {
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

async fn selector_sessions(
    client: &DaemonClient,
    project_dir: Option<&std::path::Path>,
) -> Vec<crate::daemon::session::SessionSummary> {
    let Ok(sessions) = client.list_sessions().await else {
        return Vec::new();
    };
    let mut sessions = sessions
        .into_iter()
        .filter(|session| match (&session.scope, project_dir) {
            (_, None) => true,
            (
                crate::daemon::session::SessionScope::Project {
                    project_dir: stored,
                },
                Some(project_dir),
            ) => stored == project_dir,
            _ => false,
        })
        .collect::<Vec<_>>();
    sort_selector_sessions(&mut sessions);
    sessions
}

async fn create_selector_session(
    client: &DaemonClient,
    target: &SessionCreateTarget,
) -> Result<crate::daemon::session::SessionSummary> {
    match target {
        SessionCreateTarget::General => client.create_session(None, None).await,
        SessionCreateTarget::Project { project_dir } => {
            let title = project_label(project_dir);
            client
                .create_session(Some(project_dir.as_path()), Some(title.as_str()))
                .await
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionCreateTarget {
    General,
    Project { project_dir: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionTreeRow {
    Section {
        label: String,
        count: usize,
    },
    Project {
        label: String,
        path: String,
        count: usize,
    },
    Create {
        target: SessionCreateTarget,
        label: String,
        depth: usize,
    },
    Session {
        session_index: usize,
        depth: usize,
    },
}

fn build_session_tree_rows(
    sessions: &[crate::daemon::session::SessionSummary],
    project_dir_filter: Option<&std::path::Path>,
) -> Vec<SessionTreeRow> {
    use std::collections::BTreeMap;

    let mut general = Vec::new();
    let mut projects: BTreeMap<String, (std::path::PathBuf, Vec<usize>)> = BTreeMap::new();

    for (index, session) in sessions.iter().enumerate() {
        match &session.scope {
            crate::daemon::session::SessionScope::General => general.push(index),
            crate::daemon::session::SessionScope::Project { project_dir } => {
                projects
                    .entry(project_dir.display().to_string())
                    .or_insert_with(|| (project_dir.clone(), Vec::new()))
                    .1
                    .push(index);
            }
        }
    }

    if let Some(project_dir) = project_dir_filter {
        projects
            .entry(project_dir.display().to_string())
            .or_insert_with(|| (project_dir.to_path_buf(), Vec::new()));
    }

    let mut rows = Vec::new();
    if project_dir_filter.is_none() {
        rows.push(SessionTreeRow::Section {
            label: "General".to_string(),
            count: general.len(),
        });
        rows.push(SessionTreeRow::Create {
            target: SessionCreateTarget::General,
            label: "New general session".to_string(),
            depth: 1,
        });
        rows.extend(
            general
                .into_iter()
                .map(|session_index| SessionTreeRow::Session {
                    session_index,
                    depth: 1,
                }),
        );
    }

    let coding_count = projects.values().map(|(_, sessions)| sessions.len()).sum();
    if coding_count > 0 || project_dir_filter.is_some() {
        rows.push(SessionTreeRow::Section {
            label: "Coding".to_string(),
            count: coding_count,
        });
        for (_, (project_dir, project_sessions)) in projects {
            rows.push(SessionTreeRow::Project {
                label: project_label(&project_dir),
                path: project_dir.display().to_string(),
                count: project_sessions.len(),
            });
            rows.push(SessionTreeRow::Create {
                target: SessionCreateTarget::Project {
                    project_dir: project_dir.clone(),
                },
                label: "New coding session".to_string(),
                depth: 2,
            });
            rows.extend(project_sessions.into_iter().map(|session_index| {
                SessionTreeRow::Session {
                    session_index,
                    depth: 2,
                }
            }));
        }
    }

    rows
}

fn session_tree_list_items(
    sessions: &[crate::daemon::session::SessionSummary],
    rows: &[SessionTreeRow],
) -> Vec<ListItem<'static>> {
    rows.iter()
        .map(|row| match row {
            SessionTreeRow::Section { label, count } => ListItem::new(Line::from(vec![
                Span::styled(
                    label.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" ({count})"), Style::default().fg(Color::DarkGray)),
            ])),
            SessionTreeRow::Project { label, path, count } => ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    label.clone(),
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" ({count})"), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("  {path}"), Style::default().fg(Color::DarkGray)),
            ])),
            SessionTreeRow::Create { label, depth, .. } => {
                let indent = "  ".repeat(*depth);
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled("+ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(label.clone(), Style::default().fg(Color::LightGreen)),
                ]))
            }
            SessionTreeRow::Session {
                session_index,
                depth,
            } => {
                let session = &sessions[*session_index];
                let indent = "  ".repeat(*depth);
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled("- ", Style::default().fg(Color::DarkGray)),
                    Span::styled(session_title(session), Style::default().fg(Color::White)),
                    Span::styled(
                        format!("  {}", short_session_id(session.session_id.as_str())),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            }
        })
        .collect()
}

fn sort_selector_sessions(sessions: &mut [crate::daemon::session::SessionSummary]) {
    sessions.sort_by_key(|session| {
        let scope_order = match &session.scope {
            crate::daemon::session::SessionScope::General => 0,
            crate::daemon::session::SessionScope::Project { .. } => 1,
        };
        (
            scope_order,
            session_project_sort_key(session),
            session_title(session).to_ascii_lowercase(),
            session.started_at_ms,
            session.session_id.as_str().to_string(),
        )
    });
}

fn session_project_sort_key(session: &crate::daemon::session::SessionSummary) -> String {
    match &session.scope {
        crate::daemon::session::SessionScope::General => String::new(),
        crate::daemon::session::SessionScope::Project { project_dir } => {
            project_dir.display().to_string()
        }
    }
}

fn project_label(project_dir: &std::path::Path) -> String {
    project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| project_dir.display().to_string())
}

fn session_title(session: &crate::daemon::session::SessionSummary) -> String {
    session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Untitled session")
        .to_string()
}

fn short_session_id(session_id: &str) -> &str {
    session_id.get(..8).unwrap_or(session_id)
}

fn selected_session<'a>(
    sessions: &'a [crate::daemon::session::SessionSummary],
    rows: &[SessionTreeRow],
    selected_row: Option<usize>,
) -> Option<&'a crate::daemon::session::SessionSummary> {
    let session_index = selected_row
        .and_then(|row_index| rows.get(row_index))
        .and_then(|row| match row {
            SessionTreeRow::Session { session_index, .. } => Some(*session_index),
            SessionTreeRow::Section { .. }
            | SessionTreeRow::Project { .. }
            | SessionTreeRow::Create { .. } => None,
        })?;
    sessions.get(session_index)
}

fn selected_create_target(
    rows: &[SessionTreeRow],
    selected_row: Option<usize>,
) -> Option<&SessionCreateTarget> {
    selected_row
        .and_then(|row_index| rows.get(row_index))
        .and_then(|row| match row {
            SessionTreeRow::Create { target, .. } => Some(target),
            SessionTreeRow::Section { .. }
            | SessionTreeRow::Project { .. }
            | SessionTreeRow::Session { .. } => None,
        })
}

fn select_first_action_row(state: &mut ListState, rows: &[SessionTreeRow]) {
    state.select(first_action_row(rows));
}

fn restore_session_tree_selection(
    state: &mut ListState,
    sessions: &[crate::daemon::session::SessionSummary],
    rows: &[SessionTreeRow],
    preferred_session_id: Option<&str>,
) {
    if let Some(session_id) = preferred_session_id
        && select_session_row_by_id(state, sessions, rows, session_id)
    {
        return;
    }

    normalize_session_tree_selection(state, rows);
}

fn normalize_session_tree_selection(state: &mut ListState, rows: &[SessionTreeRow]) {
    let Some(selected) = state.selected() else {
        select_first_action_row(state, rows);
        return;
    };
    if is_action_row(rows, selected) {
        return;
    }

    let next = (selected..rows.len())
        .find(|row_index| is_action_row(rows, *row_index))
        .or_else(|| {
            (0..selected)
                .rev()
                .find(|row_index| is_action_row(rows, *row_index))
        });
    state.select(next);
}

fn select_session_row_by_id(
    state: &mut ListState,
    sessions: &[crate::daemon::session::SessionSummary],
    rows: &[SessionTreeRow],
    session_id: &str,
) -> bool {
    let Some(row_index) = rows.iter().position(|row| match row {
        SessionTreeRow::Session { session_index, .. } => sessions
            .get(*session_index)
            .is_some_and(|session| session.session_id.as_str() == session_id),
        SessionTreeRow::Section { .. }
        | SessionTreeRow::Project { .. }
        | SessionTreeRow::Create { .. } => false,
    }) else {
        return false;
    };
    state.select(Some(row_index));
    true
}

fn first_action_row(rows: &[SessionTreeRow]) -> Option<usize> {
    rows.iter().position(|row| {
        matches!(
            row,
            SessionTreeRow::Create { .. } | SessionTreeRow::Session { .. }
        )
    })
}

fn is_action_row(rows: &[SessionTreeRow], row_index: usize) -> bool {
    matches!(
        rows.get(row_index),
        Some(SessionTreeRow::Create { .. } | SessionTreeRow::Session { .. })
    )
}

fn select_next_session_row(state: &mut ListState, rows: &[SessionTreeRow]) {
    let Some(first) = first_action_row(rows) else {
        state.select(None);
        return;
    };
    let current = state.selected().unwrap_or(first);
    let next = ((current + 1)..rows.len())
        .find(|row_index| is_action_row(rows, *row_index))
        .unwrap_or(first);
    state.select(Some(next));
}

fn select_previous_session_row(state: &mut ListState, rows: &[SessionTreeRow]) {
    let Some(first) = first_action_row(rows) else {
        state.select(None);
        return;
    };
    let current = state.selected().unwrap_or(first);
    let previous = (0..current)
        .rev()
        .find(|row_index| is_action_row(rows, *row_index))
        .or_else(|| {
            (first..rows.len())
                .rev()
                .find(|row_index| is_action_row(rows, *row_index))
        })
        .unwrap_or(first);
    state.select(Some(previous));
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
    let status = if let Ok(client) = connect_existing_daemon().await {
        let previous = client.status().await?;
        client.restart().await?;
        wait_for_daemon_restarted(&previous).await?
    } else {
        spawn_detached_daemon_process().await?;
        wait_for_daemon_ready().await?
    };
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

#[cfg(test)]
mod session_selector_tests {
    use super::*;
    use crate::daemon::session::{SessionId, SessionScope, SessionSummary};
    use std::path::PathBuf;

    #[test]
    fn session_tree_groups_general_and_coding_projects() {
        let project_a = PathBuf::from("/tmp/project-a");
        let project_b = PathBuf::from("/tmp/project-b");
        let sessions = vec![
            summary("general-1", SessionScope::General, "General"),
            summary(
                "project-a-1",
                SessionScope::Project {
                    project_dir: project_a.clone(),
                },
                "Project A",
            ),
            summary(
                "project-b-1",
                SessionScope::Project {
                    project_dir: project_b.clone(),
                },
                "Project B",
            ),
        ];

        let rows = build_session_tree_rows(&sessions, None);

        assert_eq!(
            rows,
            vec![
                SessionTreeRow::Section {
                    label: "General".to_string(),
                    count: 1,
                },
                SessionTreeRow::Create {
                    target: SessionCreateTarget::General,
                    label: "New general session".to_string(),
                    depth: 1,
                },
                SessionTreeRow::Session {
                    session_index: 0,
                    depth: 1,
                },
                SessionTreeRow::Section {
                    label: "Coding".to_string(),
                    count: 2,
                },
                SessionTreeRow::Project {
                    label: "project-a".to_string(),
                    path: project_a.display().to_string(),
                    count: 1,
                },
                SessionTreeRow::Create {
                    target: SessionCreateTarget::Project {
                        project_dir: project_a.clone(),
                    },
                    label: "New coding session".to_string(),
                    depth: 2,
                },
                SessionTreeRow::Session {
                    session_index: 1,
                    depth: 2,
                },
                SessionTreeRow::Project {
                    label: "project-b".to_string(),
                    path: project_b.display().to_string(),
                    count: 1,
                },
                SessionTreeRow::Create {
                    target: SessionCreateTarget::Project {
                        project_dir: project_b.clone(),
                    },
                    label: "New coding session".to_string(),
                    depth: 2,
                },
                SessionTreeRow::Session {
                    session_index: 2,
                    depth: 2,
                },
            ]
        );
    }

    #[test]
    fn code_session_tree_starts_at_coding_project_hierarchy() {
        let project = PathBuf::from("/tmp/project-a");
        let sessions = vec![summary(
            "project-a-1",
            SessionScope::Project {
                project_dir: project.clone(),
            },
            "Project A",
        )];

        let rows = build_session_tree_rows(&sessions, Some(project.as_path()));

        assert_eq!(
            rows,
            vec![
                SessionTreeRow::Section {
                    label: "Coding".to_string(),
                    count: 1,
                },
                SessionTreeRow::Project {
                    label: "project-a".to_string(),
                    path: project.display().to_string(),
                    count: 1,
                },
                SessionTreeRow::Create {
                    target: SessionCreateTarget::Project {
                        project_dir: project.clone(),
                    },
                    label: "New coding session".to_string(),
                    depth: 2,
                },
                SessionTreeRow::Session {
                    session_index: 0,
                    depth: 2,
                },
            ]
        );
    }

    #[test]
    fn code_session_tree_keeps_create_row_when_project_has_no_sessions() {
        let project = PathBuf::from("/tmp/project-a");

        let rows = build_session_tree_rows(&[], Some(project.as_path()));

        assert_eq!(
            rows,
            vec![
                SessionTreeRow::Section {
                    label: "Coding".to_string(),
                    count: 0,
                },
                SessionTreeRow::Project {
                    label: "project-a".to_string(),
                    path: project.display().to_string(),
                    count: 0,
                },
                SessionTreeRow::Create {
                    target: SessionCreateTarget::Project {
                        project_dir: project.clone(),
                    },
                    label: "New coding session".to_string(),
                    depth: 2,
                },
            ]
        );
    }

    fn summary(id: &str, scope: SessionScope, title: &str) -> SessionSummary {
        let project_dir = match &scope {
            SessionScope::General => None,
            SessionScope::Project { project_dir } => Some(project_dir.clone()),
        };
        SessionSummary {
            session_id: SessionId::from_string(id.to_string()).expect("test session id"),
            scope,
            project_dir,
            title: Some(title.to_string()),
            started_at_ms: 0,
            last_seen_at_ms: None,
        }
    }
}
