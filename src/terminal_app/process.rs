use std::{
    collections::VecDeque,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
};

use crate::sandbox::RuntimeSandboxPolicy;

pub const DEFAULT_OUTPUT_BUFFER_CAPACITY_BYTES: usize = 4 * 1024 * 1024;

pub struct TerminalProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    last_update: Arc<Mutex<Instant>>,
    output: Arc<Mutex<BoundedOutputBuffer>>,
}

#[derive(Clone, Debug)]
pub struct TerminalOutputChunk {
    pub text: String,
    pub next_offset: usize,
    pub missed_bytes: usize,
    pub stats: TerminalOutputStats,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TerminalOutputStats {
    pub buffer_capacity: usize,
    pub total_written_bytes: usize,
    pub retained_bytes: usize,
    pub dropped_bytes: usize,
}

#[derive(Debug)]
struct BoundedOutputBuffer {
    bytes: VecDeque<u8>,
    capacity: usize,
    total_written: usize,
}

impl TerminalProcess {
    pub fn spawn(
        command: &str,
        workdir: Option<&str>,
        sandbox_policy: &RuntimeSandboxPolicy,
    ) -> std::io::Result<Self> {
        Self::spawn_with_output_capacity(
            command,
            workdir,
            sandbox_policy,
            DEFAULT_OUTPUT_BUFFER_CAPACITY_BYTES,
        )
    }

    pub fn spawn_with_output_capacity(
        command: &str,
        workdir: Option<&str>,
        sandbox_policy: &RuntimeSandboxPolicy,
        output_buffer_capacity: usize,
    ) -> std::io::Result<Self> {
        let (shell_program, shell_args) = shell_invocation(command);
        let spawn_spec = sandbox_policy
            .shell_spawn_spec(shell_program, shell_args)
            .map_err(std::io::Error::other)?;
        let mut process = Command::new(&spawn_spec.program);
        process.args(&spawn_spec.args);
        for (name, _) in std::env::vars_os() {
            if name
                .to_str()
                .is_some_and(|name| sandbox_policy.is_env_var_protected(name))
            {
                process.env_remove(&name);
            }
        }
        if let Some(workdir) = workdir {
            process.current_dir(workdir);
        }

        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = process.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let last_update = Arc::new(Mutex::new(Instant::now()));
        let output = Arc::new(Mutex::new(BoundedOutputBuffer::new(output_buffer_capacity)));

        if let Some(stdout) = stdout {
            spawn_reader(stdout, output.clone(), last_update.clone());
        }
        if let Some(stderr) = stderr {
            spawn_reader(stderr, output.clone(), last_update.clone());
        }

        Ok(Self {
            child,
            stdin,
            last_update,
            output,
        })
    }

    pub async fn write(&mut self, data: &str) -> std::io::Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            stdin.write_all(data.as_bytes()).await?;
            stdin.flush().await?;
        }
        Ok(())
    }

    pub fn start_kill(&mut self) -> std::io::Result<()> {
        self.child.start_kill()
    }

    pub async fn wait_until_silent(&self, silence_duration: Duration, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            let last = *self.last_update.lock();
            if last.elapsed() >= silence_duration {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    pub fn output_len(&self) -> usize {
        self.output.lock().end_offset()
    }

    pub fn output_since(&self, offset: usize) -> TerminalOutputChunk {
        self.output.lock().output_since(offset)
    }

    pub fn output_tail(&self, max_chars: usize) -> String {
        let text = self.output.lock().retained_text();
        let chars = text.chars().collect::<Vec<_>>();
        if chars.len() <= max_chars {
            text
        } else {
            chars[chars.len().saturating_sub(max_chars)..]
                .iter()
                .collect::<String>()
        }
    }

    pub fn output_stats(&self) -> TerminalOutputStats {
        self.output.lock().stats()
    }
}

impl BoundedOutputBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            capacity,
            total_written: 0,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        self.total_written = self.total_written.saturating_add(bytes.len());
        if self.capacity == 0 {
            self.bytes.clear();
            return;
        }
        if bytes.len() >= self.capacity {
            self.bytes.clear();
            self.bytes.extend(
                bytes[bytes.len().saturating_sub(self.capacity)..]
                    .iter()
                    .copied(),
            );
            return;
        }
        self.bytes.extend(bytes.iter().copied());
        let overflow = self.bytes.len().saturating_sub(self.capacity);
        if overflow > 0 {
            self.bytes.drain(0..overflow);
        }
    }

    fn base_offset(&self) -> usize {
        self.total_written.saturating_sub(self.bytes.len())
    }

    fn end_offset(&self) -> usize {
        self.total_written
    }

    fn output_since(&self, offset: usize) -> TerminalOutputChunk {
        let base_offset = self.base_offset();
        let end_offset = self.end_offset();
        let missed_bytes = base_offset.saturating_sub(offset);
        let start_offset = offset.max(base_offset).min(end_offset);
        let local_start = start_offset.saturating_sub(base_offset);
        let bytes = self
            .bytes
            .iter()
            .skip(local_start)
            .copied()
            .collect::<Vec<_>>();
        TerminalOutputChunk {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            next_offset: end_offset,
            missed_bytes,
            stats: self.stats(),
        }
    }

    fn retained_text(&self) -> String {
        let bytes = self.bytes.iter().copied().collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn stats(&self) -> TerminalOutputStats {
        TerminalOutputStats {
            buffer_capacity: self.capacity,
            total_written_bytes: self.total_written,
            retained_bytes: self.bytes.len(),
            dropped_bytes: self.base_offset(),
        }
    }
}

fn spawn_reader<R>(
    mut reader: R,
    output: Arc<Mutex<BoundedOutputBuffer>>,
    last_update: Arc<Mutex<Instant>>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    output.lock().append(&buffer[..n]);
                    *last_update.lock() = Instant::now();
                }
                Err(_) => break,
            }
        }
    });
}

fn shell_invocation(command: &str) -> (&'static str, Vec<String>) {
    let shell_command = shell_command(command);
    if cfg!(windows) {
        (
            "powershell.exe",
            vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-Command".to_string(),
                shell_command,
            ],
        )
    } else {
        ("bash", vec!["-lc".to_string(), shell_command])
    }
}

fn shell_command(command: &str) -> String {
    command.to_string()
}

#[cfg(test)]
mod tests {
    use super::{BoundedOutputBuffer, shell_command};

    #[test]
    fn shell_command_does_not_wrap_command_in_cd() {
        assert_eq!(shell_command("pwd"), "pwd");
    }

    #[test]
    fn bounded_output_buffer_drops_oldest_bytes_and_reports_missed_offsets() {
        let mut buffer = BoundedOutputBuffer::new(8);
        buffer.append(b"abcdef");
        buffer.append(b"ghijkl");

        let stats = buffer.stats();
        assert_eq!(stats.total_written_bytes, 12);
        assert_eq!(stats.retained_bytes, 8);
        assert_eq!(stats.dropped_bytes, 4);

        let chunk = buffer.output_since(0);
        assert_eq!(chunk.missed_bytes, 4);
        assert_eq!(chunk.text, "efghijkl");
        assert_eq!(chunk.next_offset, 12);

        let recent = buffer.output_since(10);
        assert_eq!(recent.missed_bytes, 0);
        assert_eq!(recent.text, "kl");
    }
}
