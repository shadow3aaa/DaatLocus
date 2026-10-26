use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

use crate::{Child, Input, ProcessPolicy};
use daat_locus_sandbox::{SandboxProcessOptions, SandboxStdio};

pub const DEFAULT_OUTPUT_BUFFER_CAPACITY_BYTES: usize = 4 * 1024 * 1024;

pub struct TerminalProcess<P: ProcessPolicy> {
    child: P::Child,
    stdin: Option<Input>,
    output: Arc<Mutex<HeadTailOutputBuffer>>,
    active_readers: Arc<AtomicUsize>,
    output_drained: Arc<Notify>,
    readers: Vec<tokio::task::JoinHandle<()>>,
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
struct HeadTailOutputBuffer {
    capacity: usize,
    head_budget: usize,
    tail_budget: usize,
    head: VecDeque<OutputSegment>,
    tail: VecDeque<OutputSegment>,
    head_len: usize,
    tail_len: usize,
    total_written: usize,
}

#[derive(Clone, Debug)]
struct OutputSegment {
    start: usize,
    bytes: Vec<u8>,
}

impl<P: ProcessPolicy> TerminalProcess<P> {
    pub(crate) fn ensure_policy(&self, current: &P) -> miette::Result<()> {
        self.child.ensure_policy(current)
    }
    pub fn spawn(
        command: &str,
        workdir: Option<&str>,
        sandbox_policy: &P,
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
        sandbox_policy: &P,
        output_buffer_capacity: usize,
    ) -> std::io::Result<Self> {
        Self::spawn_cancellable(
            command,
            workdir,
            sandbox_policy,
            output_buffer_capacity,
            None,
        )
    }

    pub(crate) fn spawn_cancellable(
        command: &str,
        workdir: Option<&str>,
        sandbox_policy: &P,
        output_buffer_capacity: usize,
        spawn_cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> std::io::Result<Self> {
        let (shell_program, shell_args) =
            crate::shell_invocation(&sandbox_policy.shell_executable()?, command)?;
        let process_policy = sandbox_policy.clone();
        let mut child = process_policy.spawn(
            &shell_program,
            shell_args,
            SandboxProcessOptions {
                environment: crate::ENVIRONMENT
                    .iter()
                    .map(|(key, value)| ((*key).into(), (*value).into()))
                    .collect(),
                current_dir: workdir.map(PathBuf::from),
                stdin: SandboxStdio::Piped,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
                spawn_cancelled,
                // Terminal children record native denials when the host
                // supports deny-and-record; unsupported hosts ignore it.
                capture_denials: true,
            },
        )?;
        let stdin = child.take_stdin();
        let stdout = child.take_stdout();
        let stderr = child.take_stderr();
        let output = Arc::new(Mutex::new(HeadTailOutputBuffer::new(
            output_buffer_capacity,
        )));
        let reader_count = usize::from(stdout.is_some()) + usize::from(stderr.is_some());
        let active_readers = Arc::new(AtomicUsize::new(reader_count));
        let output_drained = Arc::new(Notify::new());

        let mut readers = Vec::new();
        if let Some(stdout) = stdout {
            readers.push(spawn_reader(
                stdout,
                output.clone(),
                active_readers.clone(),
                output_drained.clone(),
            ));
        }
        if let Some(stderr) = stderr {
            readers.push(spawn_reader(
                stderr,
                output.clone(),
                active_readers.clone(),
                output_drained.clone(),
            ));
        }

        Ok(Self {
            child,
            stdin,
            output,
            active_readers,
            output_drained,
            readers,
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

    pub async fn wait_for_output_drained(&self, timeout: Duration) -> bool {
        if self.active_readers.load(Ordering::Acquire) == 0 {
            return true;
        }

        tokio::time::timeout(timeout, async {
            loop {
                let drained = self.output_drained.notified();
                tokio::pin!(drained);
                drained.as_mut().enable();
                if self.active_readers.load(Ordering::Acquire) == 0 {
                    break;
                }
                drained.await;
            }
        })
        .await
        .is_ok()
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Native denial records captured for this process, when available.
    pub(crate) fn take_denial_report(&mut self) -> daat_locus_sandbox::DenialReport {
        self.child.take_denial_report()
    }

    pub fn output_len(&self) -> usize {
        self.output.lock().end_offset()
    }

    pub fn output_since(&self, offset: usize) -> TerminalOutputChunk {
        self.output.lock().output_since(offset)
    }

    pub fn output_tail(&self, max_chars: usize) -> String {
        self.output.lock().tail_chars(max_chars)
    }

    pub fn output_stats(&self) -> TerminalOutputStats {
        self.output.lock().stats()
    }
}

impl HeadTailOutputBuffer {
    const fn new(capacity: usize) -> Self {
        let head_budget = capacity / 2;
        let tail_budget = capacity.saturating_sub(head_budget);
        Self {
            capacity,
            head_budget,
            tail_budget,
            head: VecDeque::new(),
            tail: VecDeque::new(),
            head_len: 0,
            tail_len: 0,
            total_written: 0,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        let start = self.total_written;
        self.total_written = self.total_written.saturating_add(bytes.len());
        if self.capacity == 0 {
            return;
        }

        if self.head_len < self.head_budget {
            let remaining_head = self.head_budget.saturating_sub(self.head_len);
            if bytes.len() <= remaining_head {
                self.push_head(OutputSegment {
                    start,
                    bytes: bytes.to_vec(),
                });
                return;
            }

            let (head_part, tail_part) = bytes.split_at(remaining_head);
            if !head_part.is_empty() {
                self.push_head(OutputSegment {
                    start,
                    bytes: head_part.to_vec(),
                });
            }
            self.push_tail(start + remaining_head, tail_part);
            return;
        }

        self.push_tail(start, bytes);
    }

    fn push_tail(&mut self, start: usize, bytes: &[u8]) {
        if self.tail_budget == 0 || bytes.is_empty() {
            return;
        }

        if bytes.len() >= self.tail_budget {
            let keep_from = bytes.len().saturating_sub(self.tail_budget);
            self.tail.clear();
            self.tail_len = 0;
            self.push_tail_segment(OutputSegment {
                start: start + keep_from,
                bytes: bytes[keep_from..].to_vec(),
            });
            return;
        }

        self.push_tail_segment(OutputSegment {
            start,
            bytes: bytes.to_vec(),
        });
        self.trim_tail_to_budget();
    }

    fn push_head(&mut self, segment: OutputSegment) {
        self.head_len = self.head_len.saturating_add(segment.bytes.len());
        self.head.push_back(segment);
    }

    fn push_tail_segment(&mut self, segment: OutputSegment) {
        self.tail_len = self.tail_len.saturating_add(segment.bytes.len());
        self.tail.push_back(segment);
    }

    fn trim_tail_to_budget(&mut self) {
        let mut overflow = self.tail_len.saturating_sub(self.tail_budget);
        self.tail_len = self.tail_len.saturating_sub(overflow);
        while overflow > 0 {
            let Some(front) = self.tail.front_mut() else {
                break;
            };
            if overflow >= front.bytes.len() {
                overflow -= front.bytes.len();
                self.tail.pop_front();
                continue;
            }
            front.bytes.drain(..overflow);
            front.start += overflow;
            break;
        }
    }

    fn retained_bytes(&self) -> usize {
        self.head_len.saturating_add(self.tail_len)
    }

    const fn end_offset(&self) -> usize {
        self.total_written
    }

    fn output_since(&self, offset: usize) -> TerminalOutputChunk {
        let end_offset = self.end_offset();
        let mut cursor = offset.min(end_offset);
        let mut missed_bytes = 0usize;
        let mut text = String::new();

        for segment in self.iter_segments() {
            let segment_start = segment.start;
            let segment_end = segment.start.saturating_add(segment.bytes.len());
            if segment_end <= cursor {
                continue;
            }

            if cursor < segment_start {
                missed_bytes = missed_bytes.saturating_add(segment_start - cursor);
                cursor = segment_start;
            }

            let local_start = cursor.saturating_sub(segment_start);
            text.push_str(&String::from_utf8_lossy(&segment.bytes[local_start..]));
            cursor = segment_end;
        }

        if cursor < end_offset {
            missed_bytes = missed_bytes.saturating_add(end_offset - cursor);
        }

        TerminalOutputChunk {
            text,
            next_offset: end_offset,
            missed_bytes,
            stats: self.stats(),
        }
    }

    fn tail_chars(&self, max_chars: usize) -> String {
        if max_chars == 0 || self.retained_bytes() == 0 {
            return String::new();
        }
        let mut parts = Vec::new();
        let mut remaining = max_chars;
        for segment in self.tail.iter().rev().chain(self.head.iter().rev()) {
            let text = String::from_utf8_lossy(&segment.bytes);
            let char_count = text.chars().count();
            if char_count <= remaining {
                parts.push(text.into_owned());
                remaining -= char_count;
                if remaining == 0 {
                    break;
                }
                continue;
            }
            let skip = char_count - remaining;
            parts.push(text.chars().skip(skip).collect::<String>());
            break;
        }
        parts.reverse();
        parts.concat()
    }

    fn stats(&self) -> TerminalOutputStats {
        TerminalOutputStats {
            buffer_capacity: self.capacity,
            total_written_bytes: self.total_written,
            retained_bytes: self.retained_bytes(),
            dropped_bytes: self.total_written.saturating_sub(self.retained_bytes()),
        }
    }

    fn iter_segments(&self) -> impl Iterator<Item = &OutputSegment> {
        self.head.iter().chain(self.tail.iter())
    }
}

fn spawn_reader<R>(
    mut reader: R,
    output: Arc<Mutex<HeadTailOutputBuffer>>,
    active_readers: Arc<AtomicUsize>,
    output_drained: Arc<Notify>,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        let mut pending = Vec::<u8>::new();
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buffer[..n]);
                    while let Some(chunk) = take_valid_utf8_prefix(&mut pending) {
                        output.lock().append(&chunk);
                    }
                }
            }
        }
        if !pending.is_empty() {
            output.lock().append(&pending);
        }
        if active_readers.fetch_sub(1, Ordering::AcqRel) == 1 {
            output_drained.notify_waiters();
        }
    })
}

fn take_valid_utf8_prefix(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    if buffer.is_empty() {
        return None;
    }

    match std::str::from_utf8(buffer) {
        Ok(_) => Some(std::mem::take(buffer)),
        Err(err) => {
            let valid_up_to = err.valid_up_to();
            if valid_up_to > 0 {
                return Some(buffer.drain(..valid_up_to).collect());
            }
            err.error_len()
                .map(|len| buffer.drain(..len).collect::<Vec<u8>>())
        }
    }
}

impl<P: ProcessPolicy> Drop for TerminalProcess<P> {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        for reader in &self.readers {
            reader.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HeadTailOutputBuffer;

    #[test]
    fn output_buffer_preserves_head_and_tail_and_reports_missed_offsets() {
        let mut buffer = HeadTailOutputBuffer::new(8);
        buffer.append(b"abcdef");
        buffer.append(b"ghijkl");

        let stats = buffer.stats();
        assert_eq!(stats.total_written_bytes, 12);
        assert_eq!(stats.retained_bytes, 8);
        assert_eq!(stats.dropped_bytes, 4);

        let chunk = buffer.output_since(0);
        assert_eq!(chunk.missed_bytes, 4);
        assert_eq!(chunk.text, "abcdijkl");
        assert_eq!(chunk.next_offset, 12);

        let recent = buffer.output_since(10);
        assert_eq!(recent.missed_bytes, 0);
        assert_eq!(recent.text, "kl");
    }

    #[test]
    fn output_buffer_keeps_incomplete_utf8_until_complete() {
        let mut pending = Vec::new();
        pending.extend_from_slice("中".as_bytes().split_at(2).0);
        assert!(super::take_valid_utf8_prefix(&mut pending).is_none());

        pending.push("中".as_bytes()[2]);
        assert_eq!(
            super::take_valid_utf8_prefix(&mut pending),
            Some("中".as_bytes().to_vec())
        );
        assert!(pending.is_empty());
    }
}
