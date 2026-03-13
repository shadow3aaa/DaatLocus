use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use miette::Result;
use parking_lot::Mutex;

use crate::{
    device::{
        AttentionLevel, Device, DeviceAction, DeviceId, FocusedRender, PeripheralRender,
    },
    pty::Pty,
};

pub struct TerminalDevice {
    pty: Pty,
}

impl TerminalDevice {
    pub fn new() -> Self {
        Self { pty: Pty::new() }
    }

    pub fn parser(&self) -> Arc<Mutex<vt100::Parser>> {
        self.pty.parser()
    }
}

#[async_trait]
impl Device for TerminalDevice {
    fn id(&self) -> DeviceId {
        DeviceId::Terminal
    }

    fn render_peripheral(&self, is_focused: bool) -> PeripheralRender {
        let summary = if is_focused {
            "设备在前景，正在显示终端内容。".to_string()
        } else {
            "设备在后台，没有外围提醒。".to_string()
        };
        PeripheralRender {
            title: "Terminal".to_string(),
            summary,
            attention: AttentionLevel::Quiet,
            is_focused,
            interactive: true,
        }
    }

    fn render_focused(&self) -> FocusedRender {
        FocusedRender {
            title: "Terminal".to_string(),
            content: render_terminal_screen(&self.pty),
            interactive: true,
        }
    }

    async fn wait_until_settled(&self, silence_duration: Duration, timeout: Duration) -> bool {
        self.pty.wait_until_silent(silence_duration, timeout).await
    }

    async fn execute(&mut self, action: DeviceAction) -> Result<()> {
        match action {
            DeviceAction::TerminalInput { text } => {
                self.pty.write(&text);
                Ok(())
            }
        }
    }
}

fn render_terminal_screen(pty: &Pty) -> String {
    let screen = pty.screen_text();
    let cursor_pos = pty.cursor_pos();
    let screen = insert_cursor_marker(&screen, cursor_pos, "<CURSOR>");
    format!(
        "终端光标位置为<CURSOR>\n光标位置：({}, {})\n--- 终端显示 ---\n{}\n--- 终端显示 ---",
        cursor_pos.0, cursor_pos.1, screen
    )
}

fn insert_cursor_marker(screen: &str, cursor_pos: (u16, u16), marker: &str) -> String {
    let (cursor_row, cursor_col) = cursor_pos;
    let cursor_row = cursor_row as usize;
    let cursor_col = cursor_col as usize;

    let mut lines: Vec<String> = screen.lines().map(|s| s.to_string()).collect();

    if cursor_row < lines.len() {
        let line = &lines[cursor_row];
        let chars: Vec<char> = line.chars().collect();
        let col = cursor_col.min(chars.len());

        let before: String = chars[..col].iter().collect();
        let after: String = chars[col..].iter().collect();
        lines[cursor_row] = format!("{before}{marker}{after}");
    } else {
        while lines.len() <= cursor_row {
            lines.push(String::new());
        }
        lines[cursor_row] = marker.to_string();
    }

    lines.join("\n")
}
