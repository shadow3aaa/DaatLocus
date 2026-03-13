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

use crate::snapshot::insert_cursor_marker;

pub struct DashboardState {
    pub pty_parser: Arc<Mutex<vt100::Parser>>,
    pub tasks: HashMap<Uuid, String>,
    pub working_task: Option<Uuid>,
    pub trail: Vec<String>,
}

pub async fn run_tui_dashboard(
    rx: &mut tokio::sync::watch::Receiver<DashboardState>,
) -> Result<(), std::io::Error> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        if crossterm::event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.code == KeyCode::Char('q') {
                    break;
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
                .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
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

            // 渲染最近的行动轨迹
            let trail_display = state.trail.join("\n");
            let trail_widget = Paragraph::new(trail_display)
                .wrap(Wrap { trim: true })
                .block(Block::default().title("Trail").borders(Borders::ALL));
            f.render_widget(trail_widget, right_chunks[1]);
        })?;
    }

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    Ok(())
}
