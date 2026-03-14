//! 本模块定义tui面板显示相关的内容

use std::{collections::HashMap, sync::Arc, time::Duration};

use crossterm::event::{Event, KeyCode};
use parking_lot::Mutex;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tui_term::widget::PseudoTerminal;
use uuid::Uuid;

use crate::telegram_acl::TelegramAclHandle;

pub struct DashboardState {
    pub pty_parser: Arc<Mutex<vt100::Parser>>,
    pub tasks: HashMap<Uuid, String>,
    pub working_task: Option<Uuid>,
    pub trail: Vec<String>,
}

pub async fn run_tui_dashboard(
    rx: &mut tokio::sync::watch::Receiver<DashboardState>,
    telegram_acl: TelegramAclHandle,
) -> Result<(), std::io::Error> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut pending_index = 0usize;

    loop {
        let pending_requests = telegram_acl.pending_requests();
        if pending_requests.is_empty() {
            pending_index = 0;
        } else if pending_index >= pending_requests.len() {
            pending_index = pending_requests.len().saturating_sub(1);
        }

        if crossterm::event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = crossterm::event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Up => {
                        pending_index = pending_index.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        if !pending_requests.is_empty() {
                            pending_index = (pending_index + 1).min(pending_requests.len() - 1);
                        }
                    }
                    KeyCode::Char('a') => {
                        if let Some(request) = pending_requests.get(pending_index) {
                            if let Err(err) = telegram_acl.approve(request.chat_id) {
                                eprintln!("approve telegram chat failed: {err:?}");
                            }
                        }
                    }
                    KeyCode::Char('r') => {
                        if let Some(request) = pending_requests.get(pending_index) {
                            if let Err(err) = telegram_acl.reject(request.chat_id) {
                                eprintln!("reject telegram chat failed: {err:?}");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let state = rx.borrow();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
                .split(f.area());

            let right_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(18),
                    Constraint::Percentage(28),
                    Constraint::Percentage(54),
                ])
                .split(chunks[1]);

            // 渲染虚拟终端
            let screen = state.pty_parser.lock().screen().clone();
            let pty_widget = PseudoTerminal::new(&screen)
                .block(Block::default().title("Terminal").borders(Borders::ALL));
            f.render_widget(pty_widget, chunks[0]);

            // 渲染任务
            let tasks_display = state
                .tasks
                .iter()
                .map(|(id, desc)| {
                    if Some(*id) == state.working_task {
                        format!("> {desc}")
                    } else {
                        desc.clone()
                    }
                })
                .collect::<Vec<String>>()
                .join("\n");
            let tasks_widget = Paragraph::new(tasks_display)
                .wrap(Wrap { trim: true })
                .block(Block::default().title("Tasks").borders(Borders::ALL));
            f.render_widget(tasks_widget, right_chunks[0]);

            let access_display = render_pending_requests(&pending_requests, pending_index);
            let access_widget = Paragraph::new(access_display)
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .title("Telegram Access (Up/Down, a=approve, r=reject)")
                        .borders(Borders::ALL),
                );
            f.render_widget(access_widget, right_chunks[1]);

            // 渲染最近的行动轨迹
            let trail_display = state.trail.join("\n");
            let trail_widget = Paragraph::new(trail_display)
                .wrap(Wrap { trim: true })
                .block(Block::default().title("Trail").borders(Borders::ALL));
            f.render_widget(trail_widget, right_chunks[2]);
        })?;
    }

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    Ok(())
}

fn render_pending_requests(
    requests: &[crate::telegram_acl::PendingAccessRequest],
    selected: usize,
) -> String {
    if requests.is_empty() {
        return "No pending requests.".to_string();
    }

    requests
        .iter()
        .enumerate()
        .map(|(index, request)| {
            let marker = if index == selected { ">" } else { " " };
            format!(
                "{marker} {title}\n  chat_id={chat_id}\n  sender={sender}\n  preview={preview}",
                title = request.title,
                chat_id = request.chat_id,
                sender = request.sender,
                preview = request.last_message_preview
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
