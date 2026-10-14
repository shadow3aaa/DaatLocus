//! Bounded, cursor-based file log reads. Callers own source selection and access checks.
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
};

const DEFAULT_LOG_LINE_LIMIT: usize = 500;
const MAX_LOG_LINE_LIMIT: usize = 2_000;
const LOG_READ_CHUNK_SIZE: usize = 64 * 1024;
const LOG_TAIL_MAX_BYTES: usize = 1024 * 1024;
const LOG_FORWARD_MAX_BYTES: usize = 512 * 1024;

#[derive(Debug, Deserialize)]
pub struct LogReadQuery {
    pub cursor: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LogFileRead {
    pub lines: Vec<String>,
    pub next_cursor: u64,
    pub file_size_bytes: u64,
    pub truncated_start: bool,
    pub has_more: bool,
    pub reset: bool,
}

#[derive(Debug)]
pub enum ReadLogError {
    NotFound,
    Io(std::io::Error),
}

impl From<std::io::Error> for ReadLogError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            Self::NotFound
        } else {
            Self::Io(error)
        }
    }
}

pub async fn read_log_path(path: &Path, query: &LogReadQuery) -> Result<LogFileRead, ReadLogError> {
    let mut file = File::open(path).await?;
    let file_size = file.seek(std::io::SeekFrom::End(0)).await?;
    drop(file);
    let limit = query
        .limit
        .unwrap_or(DEFAULT_LOG_LINE_LIMIT)
        .clamp(1, MAX_LOG_LINE_LIMIT);
    let reset = query.cursor.is_some_and(|cursor| cursor > file_size);

    if let Some(cursor) = query.cursor
        && !reset
    {
        return read_forward(path, cursor, file_size, limit).await;
    }

    read_tail(path, file_size, limit, reset).await
}

async fn read_tail(
    path: &Path,
    file_size: u64,
    limit: usize,
    reset: bool,
) -> Result<LogFileRead, ReadLogError> {
    if file_size == 0 {
        return Ok(LogFileRead {
            lines: Vec::new(),
            next_cursor: 0,
            file_size_bytes: 0,
            truncated_start: false,
            has_more: false,
            reset,
        });
    }

    let mut file = File::open(path).await?;
    let mut position = file_size;
    let mut buffered_bytes = 0usize;
    let mut newline_count = 0usize;
    let mut chunks = Vec::new();

    while position > 0 && buffered_bytes < LOG_TAIL_MAX_BYTES {
        let remaining_budget = LOG_TAIL_MAX_BYTES - buffered_bytes;
        let read_size = usize::try_from(position.min(LOG_READ_CHUNK_SIZE as u64))
            .unwrap_or(LOG_READ_CHUNK_SIZE)
            .min(remaining_budget);
        position = position.saturating_sub(read_size as u64);
        let mut chunk = vec![0; read_size];
        file.seek(std::io::SeekFrom::Start(position)).await?;
        file.read_exact(&mut chunk).await?;
        newline_count += chunk.iter().filter(|byte| **byte == b'\n').count();
        buffered_bytes += chunk.len();
        chunks.push(chunk);
        if newline_count > limit {
            break;
        }
    }

    let mut buffer = Vec::with_capacity(buffered_bytes);
    for chunk in chunks.iter().rev() {
        buffer.extend_from_slice(chunk);
    }

    let mut truncated_start = position > 0;
    let mut lines = lines_from_bytes(&buffer, truncated_start);
    if lines.len() > limit {
        let keep_from = lines.len() - limit;
        lines.drain(0..keep_from);
        truncated_start = true;
    }

    Ok(LogFileRead {
        lines,
        next_cursor: file_size,
        file_size_bytes: file_size,
        truncated_start,
        has_more: false,
        reset,
    })
}

async fn read_forward(
    path: &Path,
    cursor: u64,
    file_size: u64,
    limit: usize,
) -> Result<LogFileRead, ReadLogError> {
    let start = cursor.min(file_size);
    if start == file_size {
        return Ok(LogFileRead {
            lines: Vec::new(),
            next_cursor: file_size,
            file_size_bytes: file_size,
            truncated_start: false,
            has_more: false,
            reset: false,
        });
    }

    let read_len = usize::try_from((file_size - start).min(LOG_FORWARD_MAX_BYTES as u64))
        .unwrap_or(LOG_FORWARD_MAX_BYTES);
    let mut file = File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(start)).await?;

    let mut buffer = vec![0; read_len];
    let bytes_read = file.read(&mut buffer).await?;
    buffer.truncate(bytes_read);

    let mut next_cursor = start + bytes_read as u64;
    let mut has_more = next_cursor < file_size;
    if has_more && let Some(last_newline_index) = buffer.iter().rposition(|byte| *byte == b'\n') {
        let keep_len = last_newline_index + 1;
        let dropped_bytes = buffer.len() - keep_len;
        buffer.truncate(keep_len);
        next_cursor = next_cursor.saturating_sub(dropped_bytes as u64);
        has_more = next_cursor < file_size;
    }

    let mut lines = lines_from_bytes(&buffer, false);
    let truncated_start = if lines.len() > limit {
        let keep_from = lines.len() - limit;
        lines.drain(0..keep_from);
        true
    } else {
        false
    };

    Ok(LogFileRead {
        lines,
        next_cursor,
        file_size_bytes: file_size,
        truncated_start,
        has_more,
        reset: false,
    })
}

fn lines_from_bytes(buffer: &[u8], starts_mid_line: bool) -> Vec<String> {
    let text = String::from_utf8_lossy(buffer);
    let text = if starts_mid_line {
        text.split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or_default()
    } else {
        text.as_ref()
    };

    text.lines().map(std::string::ToString::to_string).collect()
}
