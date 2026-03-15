//! 本模块定义tui面板显示相关的内容

use std::{cmp::Reverse, collections::HashMap, sync::Arc, time::Duration};

use crossterm::event::{Event, KeyCode};
use parking_lot::Mutex;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tui_term::widget::PseudoTerminal;
use uuid::Uuid;

use crate::{device::DeviceId, telegram_acl::TelegramAclHandle};

pub struct DashboardState {
    pub pty_parser: Arc<Mutex<vt100::Parser>>,
    pub focused_device: Option<DeviceId>,
    pub focused_title: Option<String>,
    pub focused_content: Option<String>,
    pub obligations: Vec<String>,
    pub projects: Vec<String>,
    pub tasks: HashMap<Uuid, DashboardTaskEntry>,
    pub working_task: Option<Uuid>,
    pub latest_trail: Option<String>,
    pub last_cycle_elapsed_ms: Option<u128>,
}

pub struct DashboardTaskEntry {
    pub display: String,
    pub last_touched_at_ms: Option<i64>,
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
                    Constraint::Percentage(12),
                    Constraint::Percentage(14),
                    Constraint::Percentage(16),
                    Constraint::Percentage(18),
                    Constraint::Percentage(40),
                ])
                .split(chunks[1]);

            render_main_pane(f, chunks[0], &state);

            let obligations_display = if state.obligations.is_empty() {
                "No obligations.".to_string()
            } else {
                state.obligations.join("\n")
            };
            let obligations_widget = Paragraph::new(obligations_display)
                .wrap(Wrap { trim: true })
                .block(Block::default().title("Obligations").borders(Borders::ALL));
            f.render_widget(obligations_widget, right_chunks[0]);

            let projects_display = if state.projects.is_empty() {
                "No projects.".to_string()
            } else {
                state.projects.join("\n")
            };
            let projects_widget = Paragraph::new(projects_display)
                .wrap(Wrap { trim: true })
                .block(Block::default().title("Projects").borders(Borders::ALL));
            f.render_widget(projects_widget, right_chunks[1]);

            // 渲染任务
            let mut task_items = state.tasks.iter().collect::<Vec<_>>();
            task_items.sort_by_key(|(id, desc)| {
                (
                    Some(**id) != state.working_task,
                    Reverse(desc.last_touched_at_ms.unwrap_or(0)),
                    id.to_string(),
                )
            });
            let tasks_display = task_items
                .into_iter()
                .map(|(id, desc)| {
                    if Some(*id) == state.working_task {
                        format!("> {}", desc.display)
                    } else {
                        desc.display.clone()
                    }
                })
                .collect::<Vec<String>>()
                .join("\n");
            let tasks_widget = Paragraph::new(tasks_display)
                .wrap(Wrap { trim: true })
                .block(Block::default().title("Next Actions").borders(Borders::ALL));
            f.render_widget(tasks_widget, right_chunks[2]);

            let access_display = render_pending_requests(&pending_requests, pending_index);
            let access_widget = Paragraph::new(access_display)
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .title("Telegram Access (Up/Down, a=approve, r=reject)")
                        .borders(Borders::ALL),
                );
            f.render_widget(access_widget, right_chunks[3]);

            // 渲染最近的行动轨迹
            let trail_display =
                render_latest_trail(state.latest_trail.as_deref(), state.last_cycle_elapsed_ms);
            let trail_widget = Paragraph::new(trail_display)
                .wrap(Wrap { trim: true })
                .block(Block::default().title("Trail").borders(Borders::ALL));
            f.render_widget(trail_widget, right_chunks[4]);
        })?;
    }

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    Ok(())
}

fn render_main_pane(f: &mut Frame, area: Rect, state: &DashboardState) {
    match state.focused_device {
        Some(DeviceId::Terminal) => {
            let screen = state.pty_parser.lock().screen().clone();
            let title = state.focused_title.as_deref().unwrap_or("Terminal");
            let pty_widget = PseudoTerminal::new(&screen)
                .block(Block::default().title(title).borders(Borders::ALL));
            f.render_widget(pty_widget, area);
        }
        Some(DeviceId::Telegram) => render_telegram_pane(
            f,
            area,
            state.focused_title.as_deref().unwrap_or("Telegram"),
            state.focused_content.as_deref().unwrap_or(""),
        ),
        None => {
            let widget = Paragraph::new("当前没有前景设备。")
                .wrap(Wrap { trim: true })
                .block(Block::default().title("No Device").borders(Borders::ALL));
            f.render_widget(widget, area);
        }
    }
}

fn render_telegram_pane(f: &mut Frame, area: Rect, title: &str, content: &str) {
    let (list_text, chat_text, footer_text) = split_telegram_view(content);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(32),
            Constraint::Percentage(58),
            Constraint::Percentage(10),
        ])
        .split(area);

    let list_widget = Paragraph::new(list_text).wrap(Wrap { trim: false }).block(
        Block::default()
            .title(format!("{title} / Chats"))
            .borders(Borders::ALL),
    );
    f.render_widget(list_widget, chunks[0]);

    let chat_widget = Paragraph::new(chat_text).wrap(Wrap { trim: false }).block(
        Block::default()
            .title(format!("{title} / Current Chat"))
            .borders(Borders::ALL),
    );
    f.render_widget(chat_widget, chunks[1]);

    let footer_widget = Paragraph::new(footer_text).wrap(Wrap { trim: true }).block(
        Block::default()
            .title(format!("{title} / Tips"))
            .borders(Borders::ALL),
    );
    f.render_widget(footer_widget, chunks[2]);
}

fn split_telegram_view(content: &str) -> (String, String, String) {
    let list_title = "聊天列表页";
    let chat_title = "当前会话页";
    let tips_marker = "如果要发送消息，请使用 `DeviceAction` -> `TelegramSendMessage`。";

    let list_text = extract_section(content, list_title)
        .unwrap_or_else(|| "当前没有可显示的聊天列表。".to_string());
    let chat_text = extract_section(content, chat_title)
        .unwrap_or_else(|| "当前没有打开任何会话。".to_string());
    let footer_text = content
        .find(tips_marker)
        .map(|start| content[start..].trim().to_string())
        .unwrap_or_else(|| "No tips.".to_string());

    (list_text, chat_text, footer_text)
}

fn extract_section(content: &str, title: &str) -> Option<String> {
    let start = content.find(title)?;
    let rest = &content[start..];
    let next_title = rest
        .char_indices()
        .skip(1)
        .find_map(|(idx, _)| rest[idx..].starts_with("当前会话页").then_some(idx))
        .or_else(|| {
            rest.char_indices()
                .skip(1)
                .find_map(|(idx, _)| rest[idx..].starts_with("如果要发送消息").then_some(idx))
        })
        .unwrap_or(rest.len());
    Some(rest[..next_title].trim().to_string())
}

fn render_latest_trail(latest_trail: Option<&str>, last_cycle_elapsed_ms: Option<u128>) -> String {
    let elapsed = last_cycle_elapsed_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "未知".to_string());

    match latest_trail {
        Some(trail) if !trail.trim().is_empty() => {
            format!("最近事件：\n{}\n\n处理耗时：{}", trail.trim(), elapsed)
        }
        _ => format!("No recent events.\n\n处理耗时：{}", elapsed),
    }
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
