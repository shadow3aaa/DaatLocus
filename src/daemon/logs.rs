use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
};

use crate::daat_locus_paths::daat_locus_paths;

use super::{DAEMON_MAIN_LOG, SESSION_LOG, ServerState};

const DEFAULT_LOG_LINE_LIMIT: usize = 500;
const MAX_LOG_LINE_LIMIT: usize = 2_000;
const LOG_READ_CHUNK_SIZE: usize = 64 * 1024;
const LOG_TAIL_MAX_BYTES: usize = 1024 * 1024;
const LOG_FORWARD_MAX_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(super) struct LogSourceEntry {
    pub id: String,
    pub label: String,
    pub description: String,
    pub path: String,
    pub exists: bool,
    pub size_bytes: u64,
    pub modified_at_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(super) struct LogSourcesResponse {
    pub sources: Vec<LogSourceEntry>,
}

#[derive(Debug, Serialize)]
pub(super) struct LogReadResponse {
    pub source: LogSourceEntry,
    pub lines: Vec<String>,
    pub next_cursor: u64,
    pub file_size_bytes: u64,
    pub truncated_start: bool,
    pub has_more: bool,
    pub reset: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct LogsReadQuery {
    source: String,
    cursor: Option<u64>,
    limit: Option<usize>,
}

struct LogFileRead {
    lines: Vec<String>,
    next_cursor: u64,
    file_size_bytes: u64,
    truncated_start: bool,
    has_more: bool,
    reset: bool,
}

enum ReadLogError {
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

pub(super) async fn sources_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    Json(LogSourcesResponse {
        sources: log_sources().await,
    })
    .into_response()
}

pub(super) async fn read_handler(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<LogsReadQuery>,
) -> impl IntoResponse {
    if !state.auth_registry.authorize_headers(&headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Some(source) = log_sources()
        .await
        .into_iter()
        .find(|source| source.id == query.source)
    else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown log source `{}`", query.source),
        )
            .into_response();
    };

    match read_log_path(Path::new(&source.path), &query).await {
        Ok(read) => Json(LogReadResponse {
            source,
            lines: read.lines,
            next_cursor: read.next_cursor,
            file_size_bytes: read.file_size_bytes,
            truncated_start: read.truncated_start,
            has_more: read.has_more,
            reset: read.reset,
        })
        .into_response(),
        Err(ReadLogError::NotFound) => (
            StatusCode::NOT_FOUND,
            format!("log source `{}` does not exist yet", query.source),
        )
            .into_response(),
        Err(ReadLogError::Io(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read log source `{}`: {error}", query.source),
        )
            .into_response(),
    }
}

async fn log_sources() -> Vec<LogSourceEntry> {
    let paths = daat_locus_paths().await;
    let mut sources = vec![
        log_source_entry(
            "daemon-main",
            "Daemon log",
            "Daemon tracing plus stdout/stderr output.",
            paths.logs_file(DAEMON_MAIN_LOG),
        )
        .await,
    ];
    sources.extend(session_log_sources(paths.sessions_dir()).await);
    sources
}

async fn session_log_sources(sessions_dir: PathBuf) -> Vec<LogSourceEntry> {
    let Ok(mut entries) = tokio::fs::read_dir(sessions_dir).await else {
        return Vec::new();
    };
    let mut sources = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let session_dir = entry.path();
        let Some(session_id) = session_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let path = session_dir.join("logs").join(SESSION_LOG);
        let Ok(metadata) = tokio::fs::metadata(&path).await else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        sources.push(
            log_source_entry(
                format!("session-log-{session_id}"),
                format!("Session {session_id} log"),
                "Session process tracing plus stdout/stderr output.",
                path,
            )
            .await,
        );
    }
    sources.sort_by(|left, right| left.label.cmp(&right.label));
    sources
}

async fn log_source_entry(
    id: impl Into<String>,
    label: impl Into<String>,
    description: impl Into<String>,
    path: PathBuf,
) -> LogSourceEntry {
    let metadata = tokio::fs::metadata(&path).await.ok();
    let modified_at_ms = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok());

    LogSourceEntry {
        id: id.into(),
        label: label.into(),
        description: description.into(),
        path: path.display().to_string(),
        exists: metadata.as_ref().is_some_and(|metadata| metadata.is_file()),
        size_bytes: metadata
            .as_ref()
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        modified_at_ms,
    }
}

async fn read_log_path(path: &Path, query: &LogsReadQuery) -> Result<LogFileRead, ReadLogError> {
    let metadata = tokio::fs::metadata(path).await?;
    if !metadata.is_file() {
        return Err(ReadLogError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "log source path is not a file",
        )));
    }

    let file_size = metadata.len();
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
        buffered_bytes += chunk.len();
        chunks.push(chunk);

        let newline_count = chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .filter(|byte| **byte == b'\n')
            .count();
        if newline_count > limit {
            break;
        }
    }

    chunks.reverse();
    let mut buffer = Vec::with_capacity(buffered_bytes);
    for chunk in chunks {
        buffer.extend(chunk);
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
    let mut truncated_start = false;
    if lines.len() > limit {
        let keep_from = lines.len() - limit;
        lines.drain(0..keep_from);
        truncated_start = true;
    }

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

    text.lines().map(|line| line.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_log_sources_lists_per_session_logs_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = temp.path().to_path_buf();
        let session_log_dir = sessions_dir.join("abc").join("logs");
        tokio::fs::create_dir_all(&session_log_dir)
            .await
            .expect("create session log dir");
        tokio::fs::write(session_log_dir.join("session.log"), b"panic\n")
            .await
            .expect("write session log");
        tokio::fs::write(session_log_dir.join("session-abc-stdio.log"), b"old\n")
            .await
            .expect("write old log");
        tokio::fs::write(sessions_dir.join("daat-locus.log"), b"main\n")
            .await
            .expect("write main log");

        let sources = session_log_sources(sessions_dir).await;

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, "session-log-abc");
        assert_eq!(sources[0].label, "Session abc log");
        assert!(sources[0].exists);
        assert!(
            PathBuf::from(&sources[0].path)
                .ends_with(Path::new("abc").join("logs").join("session.log"))
        );
    }
}
