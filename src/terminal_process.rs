use std::{
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
};

pub struct TerminalProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    last_update: Arc<Mutex<Instant>>,
    raw_output: Arc<Mutex<Vec<u8>>>,
}

impl TerminalProcess {
    pub fn spawn(command: &str, workdir: Option<&str>) -> std::io::Result<Self> {
        let mut process = if cfg!(windows) {
            let mut cmd = Command::new("powershell.exe");
            cmd.arg("-NoLogo").arg("-NoProfile").arg("-Command");
            cmd.arg(shell_command(command, workdir));
            cmd
        } else {
            let mut cmd = Command::new("bash");
            cmd.arg("-lc").arg(shell_command(command, workdir));
            cmd
        };

        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = process.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let last_update = Arc::new(Mutex::new(Instant::now()));
        let raw_output = Arc::new(Mutex::new(Vec::new()));

        if let Some(stdout) = stdout {
            spawn_reader(stdout, raw_output.clone(), last_update.clone());
        }
        if let Some(stderr) = stderr {
            spawn_reader(stderr, raw_output.clone(), last_update.clone());
        }

        Ok(Self {
            child,
            stdin,
            last_update,
            raw_output,
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
        self.raw_output.lock().len()
    }

    pub fn output_since(&self, offset: usize) -> (String, usize) {
        let output = self.raw_output.lock();
        let next_offset = output.len();
        let slice = if offset < next_offset {
            &output[offset..]
        } else {
            &[]
        };
        (String::from_utf8_lossy(slice).into_owned(), next_offset)
    }

    pub fn output_tail(&self, max_chars: usize) -> String {
        let output = self.raw_output.lock();
        let text = String::from_utf8_lossy(&output).into_owned();
        let chars = text.chars().collect::<Vec<_>>();
        if chars.len() <= max_chars {
            text
        } else {
            chars[chars.len().saturating_sub(max_chars)..]
                .iter()
                .collect::<String>()
        }
    }
}

fn spawn_reader<R>(mut reader: R, raw_output: Arc<Mutex<Vec<u8>>>, last_update: Arc<Mutex<Instant>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    raw_output.lock().extend_from_slice(&buffer[..n]);
                    *last_update.lock() = Instant::now();
                }
                Err(_) => break,
            }
        }
    });
}

fn shell_command(command: &str, workdir: Option<&str>) -> String {
    if let Some(workdir) = workdir {
        if cfg!(windows) {
            let escaped = workdir.replace('\'', "''");
            format!("Set-Location -LiteralPath '{escaped}'; {command}")
        } else {
            let escaped = workdir.replace('\'', "'\"'\"'");
            format!("cd -- '{escaped}' && {command}")
        }
    } else {
        command.to_string()
    }
}
