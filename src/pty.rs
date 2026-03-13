use std::{
    io::Write,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use serde::Deserialize;

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

#[derive(Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Action {
    /// 输入文本并换行
    Input(String),
    /// 发送控制键 (如 "Ctrl+C", "Tab", "Esc")
    Control(String),
}

pub struct Pty {
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: SharedWriter,
    last_update: Arc<Mutex<Instant>>,
}

impl Pty {
    const ROW: u16 = 40;
    const COL: u16 = 120;

    pub fn new() -> Self {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(portable_pty::PtySize {
                rows: Self::ROW,
                cols: Self::COL,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = if cfg!(windows) {
            portable_pty::CommandBuilder::new("powershell.exe")
        } else {
            portable_pty::CommandBuilder::new("bash")
        };
        if cfg!(windows) {
            cmd.arg("-NoLogo");
            cmd.arg("-NoProfile");
        }
        let child = pair.slave.spawn_command(cmd).unwrap();

        let parser = Arc::new(Mutex::new(vt100::Parser::new(Self::ROW, Self::COL, 0)));
        let master = pair.master;
        let writer = Arc::new(Mutex::new(master.take_writer().unwrap()));
        let last_update = Arc::new(Mutex::new(Instant::now()));

        // 启动一个线程来读取pty的输出
        let mut reader = master.try_clone_reader().unwrap();
        let parser_clone = parser.clone();
        let writer_clone = writer.clone();
        let last_update_clone = last_update.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let chunk = &buffer[..n];
                respond_to_cursor_query(chunk, &writer_clone); // windows上，如果不响应光标位置查询，powershell会一直等待
                parser_clone.lock().process(chunk);
                *last_update_clone.lock() = Instant::now();
            }
        });

        Self {
            master: Arc::new(Mutex::new(master)),
            parser,
            child,
            writer,
            last_update,
        }
    }

    pub fn write(&mut self, data: &str) {
        let mut writer = self.writer.lock();
        writer.write_all(data.as_bytes()).unwrap();
        writer.flush().unwrap();
    }

    pub async fn wait_until_silent(&self, silence_duration: Duration, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            let last = *self.last_update.lock();
            if last.elapsed() >= silence_duration {
                return true; // 足够安静
            }
            if start.elapsed() >= timeout {
                return false; // 等太久了，可能程序在持续输出（比如 top）
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn parser(&self) -> Arc<Mutex<vt100::Parser>> {
        self.parser.clone()
    }

    pub fn screen_text(&self) -> String {
        let lock = self.parser.lock();
        let screen = lock.screen();

        let mut output = String::new();
        let (rows, cols) = screen.size();

        for row in 0..rows {
            for col in 0..cols {
                let cell = screen.cell(row, col).unwrap();
                output.push(cell.contents().chars().next().unwrap_or(' '));
            }
            output.push('\n');
        }
        output
    }

    pub fn cursor_pos(&self) -> (u16, u16) {
        let lock = self.parser.lock();
        lock.screen().cursor_position()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[cfg(windows)]
fn respond_to_cursor_query(chunk: &[u8], writer: &SharedWriter) {
    if chunk.windows(4).any(|window| window == b"\x1b[6n") {
        let mut writer = writer.lock();
        writer.write_all(b"\x1b[1;1R").unwrap();
        writer.flush().unwrap();
    }
}

#[cfg(not(windows))]
fn respond_to_cursor_query(_chunk: &[u8], _writer: &SharedWriter) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn test_pty_interactive() {
        let mut pty = Pty::new();

        let initial_screen = wait_for(
            &pty,
            Duration::from_secs(5),
            |screen| !trim_screen(screen).is_empty(),
            "shell 没有在预期时间内输出初始内容",
        );
        println!("--- 初始屏幕快照 ---");
        println!("{}", trim_screen(&initial_screen));

        let cmd = if cfg!(windows) {
            "Write-Output ('hello ' + 'spinova')\r"
        } else {
            "printf '%s%s\\n' 'hello ' 'spinova'\n"
        };
        pty.write(cmd);

        let after_cmd_screen = wait_for(
            &pty,
            Duration::from_secs(5),
            |screen| trim_screen(screen).contains("hello spinova"),
            "屏幕应该包含 echo 的输出内容",
        );
        println!("--- 执行命令后的快照 ---");
        let formatted_screen = trim_screen(&after_cmd_screen);
        println!("{}", formatted_screen);

        let (row, col) = pty.cursor_pos();
        println!("当前光标位置: Row: {}, Col: {}", row, col);
        assert!(row > 0, "执行命令后光标应该移动到后续行");
    }

    // 辅助函数：去掉全黑屏幕末尾的大量空行，方便观察
    fn trim_screen(screen: &str) -> String {
        screen
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wait_for(
        pty: &Pty,
        timeout: Duration,
        predicate: impl Fn(&str) -> bool,
        message: &str,
    ) -> String {
        let deadline = Instant::now() + timeout;

        loop {
            let screen = pty.screen_text();
            if predicate(&screen) {
                return screen;
            }
            assert!(Instant::now() < deadline, "{message}");
            thread::sleep(Duration::from_millis(50));
        }
    }
}
