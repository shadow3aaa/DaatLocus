//! 本模块定义快照，即LLM应当看到的输入。

use std::fmt::{Display, format};

use crate::{context::Context, memory::Memory, pty::Pty, system_info::SystemInfo, tasks::Tasks};

/// 快照保存着当前agent的大脑状态
///
/// 这包括记忆、短期任务、感官输入。
pub struct Snapshot {
    sensory: Sensory,
    current_memory: CurrentMemory,
    tasks: Tasks,
    terminal: TerminalSnapshot,
}

impl Snapshot {
    pub async fn new(context: &mut Context) -> Self {
        Self {
            sensory: Sensory::new(),
            current_memory: CurrentMemory::new(&mut context.memory).await,
            tasks: context.tasks.clone(),
            terminal: TerminalSnapshot::new(&context.pty),
        }
    }
}

impl Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "感官：")?;
        writeln!(f, "{}", self.sensory)?;
        writeln!(f, "记忆：")?;
        writeln!(f, "{}", self.current_memory)?;
        writeln!(f, "任务列表：")?;
        writeln!(f, "{}", self.tasks)?;
        writeln!(f, "终端：")?;
        write!(f, "{}", self.terminal)
    }
}

struct Sensory {
    time: String,
    machine_status: SystemInfo,
}

impl Sensory {
    fn new() -> Self {
        let local = chrono::Local::now();
        let time = local.format("%Y-%m-%d %H:%M:%S %z").to_string();
        let machine_status = SystemInfo::sample();
        Self {
            time,
            machine_status,
        }
    }
}

impl Display for Sensory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "当前时间：{}", self.time)?;
        write!(f, "机器状态：\n{}", self.machine_status)
    }
}

struct CurrentMemory {
    current_doing: Option<String>,
    trail: Vec<String>,
    associated_memories: Vec<String>,
}

impl CurrentMemory {
    const EMPTY: Self = Self {
        current_doing: None,
        trail: Vec::new(),
        associated_memories: Vec::new(),
    };

    async fn new(memory: &mut Memory) -> Self {
        let Some(current_doing) = memory.current_doing() else {
            return Self::EMPTY;
        };
        let trail = memory.trail();
        let query = format!(
            "在【{}】时，发生：【{}】",
            current_doing,
            trail.last().unwrap()
        );
        let associated_memories = memory.search_mem(&query, 5).await;
        Self {
            current_doing: Some(current_doing),
            trail,
            associated_memories,
        }
    }
}

impl Display for CurrentMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(current_doing) = &self.current_doing {
            writeln!(f, "当前正在：{}", current_doing)?;
        } else {
            writeln!(f, "当前没有在干什么")?;
        }
        writeln!(f, "近期经历：")?;
        for event in self.trail.iter() {
            writeln!(f, "{event}")?;
            writeln!(f, "然后")?;
        }
        writeln!(f, "联想回忆：")?;
        let len = self.associated_memories.len();
        for (i, mem) in self.associated_memories.iter().enumerate() {
            if i != len - 1 {
                writeln!(f, "{mem}")?;
            } else {
                write!(f, "{mem}")?;
            }
        }
        Ok(())
    }
}

struct TerminalSnapshot {
    screen: String,
    cursor_pos: (u16, u16),
}

impl Display for TerminalSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "终端光标位置为<CURSOR>")?;
        writeln!(
            f,
            "光标位置：({}, {})",
            self.cursor_pos.0, self.cursor_pos.1
        )?;
        writeln!(f, "--- 终端显示 ---")?;
        let screen = insert_cursor_marker(&self.screen, self.cursor_pos);
        writeln!(f, "{screen}")?;
        write!(f, "--- 终端显示 ---")
    }
}

impl TerminalSnapshot {
    fn new(pty: &Pty) -> Self {
        let screen = pty.screen_text();
        let cursor_pos = pty.cursor_pos();
        Self { screen, cursor_pos }
    }
}

// 在屏幕文本中插入光标位置标记
fn insert_cursor_marker(screen: &str, cursor_pos: (u16, u16)) -> String {
    let (cursor_row, cursor_col) = cursor_pos;
    let cursor_row = cursor_row as usize;
    let cursor_col = cursor_col as usize;

    let mut lines: Vec<String> = screen.lines().map(|s| s.to_string()).collect();

    if cursor_row < lines.len() {
        let line = &lines[cursor_row];

        let chars: Vec<char> = line.chars().collect();
        let col = if cursor_col <= chars.len() {
            cursor_col
        } else {
            chars.len()
        };

        let before: String = chars[..col].iter().collect();
        let after: String = chars[col..].iter().collect();
        lines[cursor_row] = format!("{}<CURSOR>{}", before, after);
    } else {
        while lines.len() <= cursor_row {
            lines.push(String::new());
        }
        lines[cursor_row] = "<CURSOR>".to_string();
    }

    lines.join("\n")
}
