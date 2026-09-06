use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use daat_locus_macros::model_schema;
use miette::{Context as _, IntoDiagnostic, Result};
use rusqlite::{Connection, OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use crate::{
    daat_locus_paths::DaatLocusPaths,
    dashboard::SessionActivityEvent,
    reasoning::runtime::{AgentMessage, HistoryMessage},
};

const DASHBOARD_ACTIVITY_HISTORY_DB_FILE: &str = "dashboard_activity.sqlite3";
const DASHBOARD_ACTIVITY_HISTORY_LIMIT_MAX: usize = 200;
pub const DASHBOARD_ACTIVITY_HISTORY_INITIAL_LIMIT: usize = 80;

#[derive(Clone)]
pub struct DashboardActivityHistoryStore {
    db_path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

#[model_schema]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryArchiveQueryMode {
    Recent,
    Range,
    Search,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryArchiveItem {
    pub seq: i64,
    pub role: String,
    pub tool_name: Option<String>,
    pub content: String,
}

/// How many most-recent compaction archive batches are retained per session.
pub const HISTORY_ARCHIVE_BATCH_KEEP_LIMIT: usize = 32;
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DashboardActivityHistoryWindow {
    pub items: Vec<DashboardActivityHistoryItem>,
    pub oldest_cursor: Option<i64>,
    pub newest_cursor: Option<i64>,
    pub has_more_before: bool,
}

impl DashboardActivityHistoryWindow {
    pub fn merge_new_items(&mut self, incoming: Vec<DashboardActivityHistoryItem>) {
        if incoming.is_empty() {
            return;
        }

        let mut items = std::mem::take(&mut self.items);
        for mut item in incoming {
            normalize_window_explored_item(&mut item, &items);
            items.push(item);
        }
        self.items = dedupe_activity_items_keep_latest(items);
        if self.items.len() > DASHBOARD_ACTIVITY_HISTORY_INITIAL_LIMIT {
            let drop_count = self.items.len() - DASHBOARD_ACTIVITY_HISTORY_INITIAL_LIMIT;
            self.items.drain(0..drop_count);
            self.has_more_before = true;
        }
        self.newest_cursor = None;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DashboardActivityHistoryCount {
    pub matching_items: usize,
    pub total_items: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DashboardActivityHistoryPage {
    pub items: Vec<DashboardActivityHistoryItem>,
    pub oldest_cursor: Option<i64>,
    pub newest_cursor: Option<i64>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

pub const WORKFLOW_WORKER_ACTIVITY_INITIAL_LIMIT: usize = 80;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowWorkerActivityItem {
    pub cursor: i64,
    pub event: SessionActivityEvent,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowWorkerActivityPage {
    pub run_id: String,
    pub worker_id: String,
    pub items: Vec<WorkflowWorkerActivityItem>,
    pub oldest_cursor: Option<i64>,
    pub newest_cursor: Option<i64>,
    pub has_more_before: bool,
    pub has_more_after: bool,
    pub activity_count: usize,
    pub revision: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkflowWorkerActivityAppendResult {
    pub activity_count: usize,
    pub revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardActivityHistoryItem {
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub event: SessionActivityEvent,
}

impl DashboardActivityHistoryItem {
    pub fn from_event_with_id(event: &SessionActivityEvent, id: &str) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: id.to_string(),
            created_at: now,
            updated_at: now,
            event: event.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardInputHistory {
    pub entries: Vec<String>,
}

impl DashboardActivityHistoryStore {
    pub fn with_session(session_id: &str) -> Result<Self> {
        let paths = DaatLocusPaths::for_session(session_id);
        Self::open_at_path(paths.memory_file(DASHBOARD_ACTIVITY_HISTORY_DB_FILE))
    }

    fn open_at_path(db_path: PathBuf) -> Result<Self> {
        let store = Self {
            db_path,
            write_lock: Arc::new(Mutex::new(())),
        };
        store.initialize()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn open_at_path_for_test(db_path: PathBuf) -> Result<Self> {
        Self::open_at_path(db_path)
    }

    pub fn empty_window() -> DashboardActivityHistoryWindow {
        DashboardActivityHistoryWindow::default()
    }

    pub fn load_initial_window(&self) -> DashboardActivityHistoryWindow {
        match self.query_before(None, DASHBOARD_ACTIVITY_HISTORY_INITIAL_LIMIT) {
            Ok(page) => DashboardActivityHistoryWindow {
                items: page.items,
                oldest_cursor: page.oldest_cursor,
                newest_cursor: page.newest_cursor,
                has_more_before: page.has_more_before,
            },
            Err(err) => {
                tracing::warn!("load dashboard activity history initial window failed: {err:?}");
                Self::empty_window()
            }
        }
    }

    pub fn append_items(&self, items: &[DashboardActivityHistoryItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        self.try_append_items(items)
    }

    pub fn register_workflow_worker(&self, run_id: &str, worker_id: &str) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| miette::miette!("dashboard activity history lock poisoned"))?;
        let conn = self.open_connection()?;
        register_workflow_worker_state(&conn, run_id, worker_id)
    }

    pub fn append_workflow_worker_activity(
        &self,
        run_id: &str,
        worker_id: &str,
        event: &SessionActivityEvent,
    ) -> Result<WorkflowWorkerActivityAppendResult> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| miette::miette!("dashboard activity history lock poisoned"))?;
        let mut conn = self.open_connection()?;
        let transaction = conn
            .transaction()
            .into_diagnostic()
            .wrap_err("begin workflow worker activity transaction failed")?;
        let result =
            append_workflow_worker_activity_in_transaction(&transaction, run_id, worker_id, event)?;
        transaction
            .commit()
            .into_diagnostic()
            .wrap_err("commit workflow worker activity transaction failed")?;
        Ok(result)
    }

    pub fn query_workflow_worker_activity(
        &self,
        run_id: &str,
        worker_id: &str,
        before: Option<i64>,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Option<WorkflowWorkerActivityPage>> {
        if before.is_some() && after.is_some() {
            return Err(miette::miette!(
                "workflow worker activity query accepts either before or after, not both"
            ));
        }
        let limit = i64::try_from(clamp_history_limit(limit))
            .expect("workflow worker activity limit is clamped below i64::MAX");
        let conn = self.open_connection()?;
        let Some((activity_count, revision)) =
            workflow_worker_activity_state(&conn, run_id, worker_id)?
        else {
            return Ok(None);
        };

        let rows = if let Some(after) = after {
            let mut statement = conn
                .prepare(
                    "SELECT seq, event_json FROM workflow_worker_activity
                     WHERE run_id = ?1 AND worker_id = ?2 AND seq > ?3
                     ORDER BY seq ASC
                     LIMIT ?4",
                )
                .into_diagnostic()
                .wrap_err("prepare workflow worker activity after query failed")?;
            statement
                .query_map(
                    params![run_id, worker_id, after, limit],
                    decode_worker_activity_row,
                )
                .into_diagnostic()
                .wrap_err("query workflow worker activity after failed")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .into_diagnostic()
                .wrap_err("decode workflow worker activity after failed")?
        } else {
            let mut statement = if before.is_some() {
                conn.prepare(
                    "SELECT seq, event_json FROM workflow_worker_activity
                     WHERE run_id = ?1 AND worker_id = ?2 AND seq < ?3
                     ORDER BY seq DESC
                     LIMIT ?4",
                )
            } else {
                conn.prepare(
                    "SELECT seq, event_json FROM workflow_worker_activity
                     WHERE run_id = ?1 AND worker_id = ?2
                     ORDER BY seq DESC
                     LIMIT ?3",
                )
            }
            .into_diagnostic()
            .wrap_err("prepare workflow worker activity before query failed")?;
            let mapped = if let Some(before) = before {
                statement.query_map(
                    params![run_id, worker_id, before, limit],
                    decode_worker_activity_row,
                )
            } else {
                statement.query_map(
                    params![run_id, worker_id, limit],
                    decode_worker_activity_row,
                )
            }
            .into_diagnostic()
            .wrap_err("query workflow worker activity before failed")?;
            let mut rows = mapped
                .collect::<rusqlite::Result<Vec<_>>>()
                .into_diagnostic()
                .wrap_err("decode workflow worker activity before failed")?;
            rows.reverse();
            rows
        };

        let oldest_cursor = rows.first().map(|(cursor, _)| *cursor);
        let newest_cursor = rows.last().map(|(cursor, _)| *cursor);
        Ok(Some(WorkflowWorkerActivityPage {
            run_id: run_id.to_string(),
            worker_id: worker_id.to_string(),
            items: rows
                .into_iter()
                .map(|(cursor, event)| WorkflowWorkerActivityItem { cursor, event })
                .collect(),
            oldest_cursor,
            newest_cursor,
            has_more_before: workflow_worker_activity_exists_before(
                &conn,
                run_id,
                worker_id,
                oldest_cursor,
            )?,
            has_more_after: workflow_worker_activity_exists_after(
                &conn,
                run_id,
                worker_id,
                newest_cursor,
            )?,
            activity_count,
            revision,
        }))
    }

    pub fn clear_all(&self) -> Result<usize> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| miette::miette!("dashboard activity history lock poisoned"))?;
        let mut conn = self.open_connection()?;
        let transaction = conn
            .transaction()
            .into_diagnostic()
            .wrap_err("begin dashboard activity history clear transaction failed")?;
        let cleared = transaction
            .execute("DELETE FROM dashboard_activity", [])
            .into_diagnostic()
            .wrap_err("clear dashboard activity history failed")?;
        transaction
            .execute("DELETE FROM workflow_worker_activity", [])
            .into_diagnostic()
            .wrap_err("clear workflow worker activity failed")?;
        transaction
            .execute("DELETE FROM workflow_worker_activity_state", [])
            .into_diagnostic()
            .wrap_err("clear workflow worker activity state failed")?;
        transaction
            .commit()
            .into_diagnostic()
            .wrap_err("commit dashboard activity history clear failed")?;
        Ok(cleared)
    }

    pub fn archive_history_messages(
        &self,
        batch_id: &str,
        messages: &[HistoryMessage],
    ) -> Result<usize> {
        if messages.is_empty() {
            return Ok(0);
        }
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| miette::miette!("dashboard activity history lock poisoned"))?;
        let mut conn = self.open_connection()?;
        let transaction = conn
            .transaction()
            .into_diagnostic()
            .wrap_err("begin history archive transaction failed")?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut statement = transaction
            .prepare(
                "INSERT INTO history_archive
                    (batch_id, created_at_ms, role, tool_name, content_text, message_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .into_diagnostic()
            .wrap_err("prepare history archive insert failed")?;
        for message in messages {
            let message_json = serde_json::to_string(message)
                .into_diagnostic()
                .wrap_err("encode history archive message failed")?;
            let tool_name = match &message.message {
                AgentMessage::Tool { name, .. } => Some(name.clone()),
                _ => None,
            };
            statement
                .execute(params![
                    batch_id,
                    now_ms,
                    message.role_name(),
                    tool_name,
                    message.text_content().unwrap_or_default(),
                    message_json,
                ])
                .into_diagnostic()
                .wrap_err("insert history archive message failed")?;
        }
        drop(statement);
        transaction
            .commit()
            .into_diagnostic()
            .wrap_err("commit history archive transaction failed")?;
        Ok(messages.len())
    }

    pub fn query_history_archive(
        &self,
        mode: HistoryArchiveQueryMode,
        limit: usize,
        before_seq: Option<i64>,
        start_seq: Option<i64>,
        query: &str,
    ) -> Result<Vec<HistoryArchiveItem>> {
        let limit = i64::try_from(clamp_history_limit(limit))
            .expect("history archive query limit is clamped below i64::MAX");
        let conn = self.open_connection()?;
        let null_value = rusqlite::types::Value::Null;
        let (sql, params): (String, Vec<rusqlite::types::Value>) = match mode {
            HistoryArchiveQueryMode::Recent => (
                "SELECT seq, role, tool_name, content_text FROM history_archive
                 WHERE (?1 IS NULL OR seq < ?1)
                   AND (?2 = '' OR content_text LIKE '%' || ?2 || '%')
                 ORDER BY seq DESC
                 LIMIT ?3"
                    .to_string(),
                vec![
                    before_seq.map(i64::into).unwrap_or(null_value),
                    rusqlite::types::Value::Text(query.to_string()),
                    limit.into(),
                ],
            ),
            HistoryArchiveQueryMode::Range => (
                "SELECT seq, role, tool_name, content_text FROM history_archive
                 WHERE seq >= ?1
                   AND (?2 = '' OR content_text LIKE '%' || ?2 || '%')
                 ORDER BY seq ASC
                 LIMIT ?3"
                    .to_string(),
                vec![
                    start_seq.unwrap_or(1).into(),
                    rusqlite::types::Value::Text(query.to_string()),
                    limit.into(),
                ],
            ),
            HistoryArchiveQueryMode::Search => (
                "SELECT seq, role, tool_name, content_text FROM history_archive
                 WHERE content_text LIKE '%' || ?1 || '%'
                   AND (?2 IS NULL OR seq < ?2)
                 ORDER BY seq DESC
                 LIMIT ?3"
                    .to_string(),
                vec![
                    rusqlite::types::Value::Text(query.to_string()),
                    before_seq.map(i64::into).unwrap_or(null_value),
                    limit.into(),
                ],
            ),
        };
        let mut statement = conn
            .prepare(&sql)
            .into_diagnostic()
            .wrap_err("prepare history archive query failed")?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(params.iter()), decode_history_archive_row)
            .into_diagnostic()
            .wrap_err("query history archive failed")?;
        let mut items = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()
            .wrap_err("decode history archive rows failed")?;
        if matches!(
            mode,
            HistoryArchiveQueryMode::Recent | HistoryArchiveQueryMode::Search
        ) {
            items.reverse();
        }
        Ok(items)
    }

    pub fn count_history_archive(&self, mode: HistoryArchiveQueryMode, query: &str) -> Result<usize> {
        let conn = self.open_connection()?;
        let sql = match mode {
            HistoryArchiveQueryMode::Recent | HistoryArchiveQueryMode::Range => {
                "SELECT COUNT(*) FROM history_archive
                 WHERE (?1 = '' OR content_text LIKE '%' || ?1 || '%')"
                    .to_string()
            }
            HistoryArchiveQueryMode::Search => {
                "SELECT COUNT(*) FROM history_archive
                 WHERE content_text LIKE '%' || ?1 || '%'"
                    .to_string()
            }
        };
        let count = conn
            .query_row(&sql, params![query], |row| row.get::<_, i64>(0))
            .into_diagnostic()
            .wrap_err("count history archive failed")?;
        Ok(count.max(0) as usize)
    }

    pub fn prune_history_archive(&self, keep_batch_count: usize) -> Result<usize> {
        if keep_batch_count == 0 {
            return Ok(0);
        }
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| miette::miette!("dashboard activity history lock poisoned"))?;
        let conn = self.open_connection()?;
        let deleted = conn
            .execute(
                "DELETE FROM history_archive WHERE batch_id IN (
                    SELECT batch_id FROM (
                        SELECT batch_id, MAX(seq) AS max_seq
                        FROM history_archive
                        GROUP BY batch_id
                        ORDER BY max_seq DESC
                        LIMIT -1 OFFSET ?1
                    )
                )",
                params![keep_batch_count as i64],
            )
            .into_diagnostic()
            .wrap_err("prune history archive failed")?;
        Ok(deleted)
    }
    pub fn query_before(
        &self,
        before: Option<i64>,
        limit: usize,
    ) -> Result<DashboardActivityHistoryPage> {
        let limit = i64::try_from(clamp_history_limit(limit))
            .expect("history query limit is clamped below i64::MAX");
        let conn = self.open_connection()?;
        let mut statement = if before.is_some() {
            conn.prepare(
                "SELECT seq, item_json FROM dashboard_activity
                 WHERE seq < ?1
                 ORDER BY seq DESC
                 LIMIT ?2",
            )
        } else {
            conn.prepare(
                "SELECT seq, item_json FROM dashboard_activity
                 ORDER BY seq DESC
                 LIMIT ?1",
            )
        }
        .into_diagnostic()
        .wrap_err("prepare dashboard activity history before query failed")?;

        let rows = if let Some(before) = before {
            statement
                .query_map(params![before, limit], decode_history_row)
                .into_diagnostic()
        } else {
            statement
                .query_map(params![limit], decode_history_row)
                .into_diagnostic()
        }
        .wrap_err("query dashboard activity history before failed")?;

        let mut rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()
            .wrap_err("decode dashboard activity history before failed")?;
        rows.reverse();
        self.page_from_rows(rows)
    }

    pub fn query_after(
        &self,
        after: Option<i64>,
        limit: usize,
    ) -> Result<DashboardActivityHistoryPage> {
        let Some(after) = after else {
            return self.query_before(None, limit);
        };

        let limit = i64::try_from(clamp_history_limit(limit))
            .expect("history query limit is clamped below i64::MAX");
        let conn = self.open_connection()?;
        let mut statement = conn
            .prepare(
                "SELECT seq, item_json FROM dashboard_activity
                 WHERE seq > ?1
                 ORDER BY seq ASC
                 LIMIT ?2",
            )
            .into_diagnostic()
            .wrap_err("prepare dashboard activity history after query failed")?;
        let rows = statement
            .query_map(params![after, limit], decode_history_row)
            .into_diagnostic()
            .wrap_err("query dashboard activity history after failed")?;
        let rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()
            .wrap_err("decode dashboard activity history after failed")?;
        self.page_from_rows(rows)
    }

    pub fn query_user_input_count(&self) -> Result<DashboardActivityHistoryCount> {
        let conn = self.open_connection()?;
        let mut statement = conn
            .prepare("SELECT item_json FROM dashboard_activity")
            .into_diagnostic()
            .wrap_err("prepare dashboard activity history count query failed")?;
        let rows = statement
            .query_map([], |row| {
                let item_json: String = row.get(0)?;
                Ok(serde_json::from_str::<DashboardActivityHistoryItem>(&item_json).ok())
            })
            .into_diagnostic()
            .wrap_err("query dashboard activity history count failed")?;

        let mut matching_items = 0;
        let mut total_items = 0;
        for item in rows {
            if let Some(item) = item.into_diagnostic()? {
                total_items += 1;
                if history_item_is_user_input(&item) {
                    matching_items += 1;
                }
            }
        }

        Ok(DashboardActivityHistoryCount {
            matching_items,
            total_items,
        })
    }

    pub fn query_recent_user_inputs(&self, limit: usize) -> Result<DashboardInputHistory> {
        let limit = clamp_history_limit(limit);
        let conn = self.open_connection()?;
        let mut statement = conn
            .prepare("SELECT item_json FROM dashboard_activity ORDER BY seq DESC")
            .into_diagnostic()
            .wrap_err("prepare dashboard input history query failed")?;
        let mut rows = statement
            .query([])
            .into_diagnostic()
            .wrap_err("query dashboard input history failed")?;
        let mut entries = Vec::new();

        while let Some(row) = rows
            .next()
            .into_diagnostic()
            .wrap_err("read dashboard input history row failed")?
        {
            let item_json: String = row
                .get(0)
                .into_diagnostic()
                .wrap_err("read dashboard input history item json failed")?;
            let Some(item) = serde_json::from_str::<DashboardActivityHistoryItem>(&item_json).ok()
            else {
                continue;
            };
            let Some(text) = history_item_user_input_text(&item) else {
                continue;
            };
            if entries.last().is_some_and(|previous| previous == &text) {
                continue;
            }
            entries.push(text);
            if entries.len() >= limit {
                break;
            }
        }

        entries.reverse();
        Ok(DashboardInputHistory { entries })
    }

    fn initialize(&self) -> Result<()> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .into_diagnostic()
                .wrap_err_with(|| {
                    format!(
                        "failed to create dashboard activity history directory {}",
                        parent.display()
                    )
                })?;
        }
        let conn = self.open_connection()?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS dashboard_activity (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 item_id TEXT NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 item_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_dashboard_activity_created_at
                 ON dashboard_activity(created_at_ms);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_dashboard_activity_item_id
                 ON dashboard_activity(item_id);
             CREATE TABLE IF NOT EXISTS workflow_worker_activity (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 run_id TEXT NOT NULL,
                 worker_id TEXT NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 event_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_workflow_worker_activity_lookup
                 ON workflow_worker_activity(run_id, worker_id, seq);
             CREATE TABLE IF NOT EXISTS workflow_worker_activity_state (
                 run_id TEXT NOT NULL,
                 worker_id TEXT NOT NULL,
                 activity_count INTEGER NOT NULL DEFAULT 0,
                 revision INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(run_id, worker_id)
             );
             CREATE TABLE IF NOT EXISTS history_archive (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 batch_id TEXT NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 role TEXT NOT NULL,
                 tool_name TEXT,
                 content_text TEXT NOT NULL DEFAULT '',
                 message_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_history_archive_batch
                 ON history_archive(batch_id, seq);",
        )
        .into_diagnostic()
        .wrap_err("initialize dashboard activity history sqlite failed")?;
        Ok(())
    }

    fn try_append_items(&self, items: &[DashboardActivityHistoryItem]) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| miette::miette!("dashboard activity history lock poisoned"))?;
        let mut conn = self.open_connection()?;
        let transaction = conn
            .transaction()
            .into_diagnostic()
            .wrap_err("begin dashboard activity history transaction failed")?;
        let mut existing_items = load_all_history_items(&transaction)?;
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO dashboard_activity
                        (item_id, created_at_ms, updated_at_ms, item_json)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(item_id) DO UPDATE SET
                        updated_at_ms = excluded.updated_at_ms,
                        item_json = excluded.item_json",
                )
                .into_diagnostic()
                .wrap_err("prepare dashboard activity history insert failed")?;

            for item in items {
                let mut item = item.clone();
                normalize_legacy_workflow_worker_activity(&transaction, &mut item)?;
                normalize_window_explored_item(&mut item, &existing_items);
                let item_json = serde_json::to_string(&item)
                    .into_diagnostic()
                    .wrap_err("encode dashboard activity item failed")?;
                statement
                    .execute(params![
                        &item.id,
                        item.created_at,
                        item.updated_at,
                        item_json
                    ])
                    .into_diagnostic()
                    .wrap_err("insert dashboard activity item failed")?;
                if let Some(existing) = existing_items
                    .iter_mut()
                    .find(|existing| existing.id == item.id)
                {
                    *existing = item;
                } else {
                    existing_items.push(item);
                }
            }
        }
        transaction
            .commit()
            .into_diagnostic()
            .wrap_err("commit dashboard activity history transaction failed")?;
        Ok(())
    }

    fn open_connection(&self) -> Result<Connection> {
        Connection::open(&self.db_path)
            .into_diagnostic()
            .wrap_err_with(|| {
                format!(
                    "open dashboard activity history sqlite {} failed",
                    self.db_path.display()
                )
            })
    }

    fn page_from_rows(
        &self,
        rows: Vec<(i64, DashboardActivityHistoryItem)>,
    ) -> Result<DashboardActivityHistoryPage> {
        let oldest_cursor = rows.first().map(|(seq, _)| *seq);
        let newest_cursor = rows.last().map(|(seq, _)| *seq);
        let items = rows
            .into_iter()
            .map(|(seq, mut item)| {
                item.id = format!("history-{seq}");
                item
            })
            .collect();

        Ok(DashboardActivityHistoryPage {
            items,
            oldest_cursor,
            newest_cursor,
            has_more_before: self.has_record_before(oldest_cursor)?,
            has_more_after: self.has_record_after(newest_cursor)?,
        })
    }

    fn has_record_before(&self, cursor: Option<i64>) -> Result<bool> {
        let Some(cursor) = cursor else {
            return Ok(false);
        };
        let conn = self.open_connection()?;
        let value = conn
            .query_row(
                "SELECT 1 FROM dashboard_activity WHERE seq < ?1 LIMIT 1",
                params![cursor],
                |_| Ok(()),
            )
            .optional()
            .into_diagnostic()
            .wrap_err("query older dashboard activity existence failed")?;
        Ok(value.is_some())
    }

    fn has_record_after(&self, cursor: Option<i64>) -> Result<bool> {
        let Some(cursor) = cursor else {
            return Ok(false);
        };
        let conn = self.open_connection()?;
        let value = conn
            .query_row(
                "SELECT 1 FROM dashboard_activity WHERE seq > ?1 LIMIT 1",
                params![cursor],
                |_| Ok(()),
            )
            .optional()
            .into_diagnostic()
            .wrap_err("query newer dashboard activity existence failed")?;
        Ok(value.is_some())
    }
}

fn normalize_legacy_workflow_worker_activity(
    transaction: &rusqlite::Transaction<'_>,
    item: &mut DashboardActivityHistoryItem,
) -> Result<()> {
    let SessionActivityEvent::Workflow(workflow) = &mut item.event else {
        return Ok(());
    };
    let Some(snapshot) = workflow.snapshot.as_mut() else {
        return Ok(());
    };
    for worker in &mut snapshot.workers {
        if worker.activity.is_empty() {
            continue;
        }
        let legacy = worker.activity.clone();
        let legacy_activity_count = legacy.len();
        let persisted_state =
            workflow_worker_activity_state(transaction, &snapshot.run_id, &worker.worker_id)?;
        let stream_was_empty =
            persisted_state.is_none_or(|(activity_count, _)| activity_count == 0);
        register_workflow_worker_state(transaction, &snapshot.run_id, &worker.worker_id)?;
        if let Some((activity_count, revision)) = persisted_state {
            worker.activity_count = worker.activity_count.max(activity_count);
            worker.activity_revision = worker.activity_revision.max(revision);
        }
        if stream_was_empty {
            for event in legacy {
                let persisted = append_workflow_worker_activity_in_transaction(
                    transaction,
                    &snapshot.run_id,
                    &worker.worker_id,
                    &event,
                )?;
                worker.activity_count = worker.activity_count.max(persisted.activity_count);
                worker.activity_revision = worker.activity_revision.max(persisted.revision);
            }
        }
        if !stream_was_empty {
            worker.activity_count = worker.activity_count.max(legacy_activity_count);
            if worker.activity_revision == 0 {
                worker.activity_revision = i64::try_from(legacy_activity_count).unwrap_or(i64::MAX);
            }
        }
        transaction
            .execute(
                "UPDATE workflow_worker_activity_state
                 SET activity_count = MAX(activity_count, ?1),
                     revision = MAX(revision, ?2)
                 WHERE run_id = ?3 AND worker_id = ?4",
                params![
                    i64::try_from(worker.activity_count).unwrap_or(i64::MAX),
                    worker.activity_revision,
                    &snapshot.run_id,
                    &worker.worker_id,
                ],
            )
            .into_diagnostic()
            .wrap_err("synchronize legacy workflow worker activity state failed")?;
        worker.activity =
            crate::workflow::tail_worker_activity(std::mem::take(&mut worker.activity));
    }
    Ok(())
}

fn register_workflow_worker_state(conn: &Connection, run_id: &str, worker_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO workflow_worker_activity_state
             (run_id, worker_id, activity_count, revision)
         VALUES (?1, ?2, 0, 0)
         ON CONFLICT(run_id, worker_id) DO NOTHING",
        params![run_id, worker_id],
    )
    .into_diagnostic()
    .wrap_err("register workflow worker activity state failed")?;
    Ok(())
}

fn append_workflow_worker_activity_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    run_id: &str,
    worker_id: &str,
    event: &SessionActivityEvent,
) -> Result<WorkflowWorkerActivityAppendResult> {
    register_workflow_worker_state(transaction, run_id, worker_id)?;
    let mut normalized = normalize_workflow_worker_activity_event(event);
    let last = transaction
        .query_row(
            "SELECT seq, event_json FROM workflow_worker_activity
             WHERE run_id = ?1 AND worker_id = ?2
             ORDER BY seq DESC LIMIT 1",
            params![run_id, worker_id],
            decode_worker_activity_row,
        )
        .optional()
        .into_diagnostic()
        .wrap_err("load latest workflow worker activity failed")?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut activity_count_delta = 1_i64;
    if let Some((seq, previous)) = last {
        let mut coalesced =
            crate::dashboard::coalesce_activity_events(vec![previous, normalized.clone()]);
        if coalesced.len() == 1 {
            normalized = coalesced.pop().expect("coalesced worker activity item");
            let event_json = serde_json::to_string(&normalized)
                .into_diagnostic()
                .wrap_err("encode coalesced workflow worker activity failed")?;
            transaction
                .execute(
                    "UPDATE workflow_worker_activity
                     SET updated_at_ms = ?1, event_json = ?2
                     WHERE seq = ?3",
                    params![now, event_json, seq],
                )
                .into_diagnostic()
                .wrap_err("update workflow worker activity failed")?;
            activity_count_delta = 0;
        }
    }
    if activity_count_delta == 1 {
        let event_json = serde_json::to_string(&normalized)
            .into_diagnostic()
            .wrap_err("encode workflow worker activity failed")?;
        transaction
            .execute(
                "INSERT INTO workflow_worker_activity
                     (run_id, worker_id, created_at_ms, updated_at_ms, event_json)
                 VALUES (?1, ?2, ?3, ?3, ?4)",
                params![run_id, worker_id, now, event_json],
            )
            .into_diagnostic()
            .wrap_err("insert workflow worker activity failed")?;
    }
    transaction
        .execute(
            "UPDATE workflow_worker_activity_state
             SET activity_count = activity_count + ?1,
                 revision = revision + 1
             WHERE run_id = ?2 AND worker_id = ?3",
            params![activity_count_delta, run_id, worker_id],
        )
        .into_diagnostic()
        .wrap_err("update workflow worker activity state failed")?;
    let (activity_count, revision) =
        workflow_worker_activity_state(transaction, run_id, worker_id)?
            .expect("workflow worker activity state was registered");
    Ok(WorkflowWorkerActivityAppendResult {
        activity_count,
        revision,
    })
}

fn normalize_workflow_worker_activity_event(event: &SessionActivityEvent) -> SessionActivityEvent {
    let mut event = event.clone();
    if let SessionActivityEvent::Workflow(workflow) = &mut event {
        workflow.snapshot = None;
    }
    event
}

fn workflow_worker_activity_state(
    conn: &Connection,
    run_id: &str,
    worker_id: &str,
) -> Result<Option<(usize, i64)>> {
    conn.query_row(
        "SELECT activity_count, revision FROM workflow_worker_activity_state
         WHERE run_id = ?1 AND worker_id = ?2",
        params![run_id, worker_id],
        |row| {
            let activity_count: i64 = row.get(0)?;
            let revision: i64 = row.get(1)?;
            Ok((
                usize::try_from(activity_count).unwrap_or(usize::MAX),
                revision,
            ))
        },
    )
    .optional()
    .into_diagnostic()
    .wrap_err("query workflow worker activity state failed")
}

fn decode_worker_activity_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(i64, SessionActivityEvent)> {
    let seq: i64 = row.get(0)?;
    let event_json: String = row.get(1)?;
    let event = serde_json::from_str::<SessionActivityEvent>(&event_json).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(err))
    })?;
    Ok((seq, event))
}

fn workflow_worker_activity_exists_before(
    conn: &Connection,
    run_id: &str,
    worker_id: &str,
    cursor: Option<i64>,
) -> Result<bool> {
    let Some(cursor) = cursor else {
        return Ok(false);
    };
    conn.query_row(
        "SELECT 1 FROM workflow_worker_activity
         WHERE run_id = ?1 AND worker_id = ?2 AND seq < ?3 LIMIT 1",
        params![run_id, worker_id, cursor],
        |_| Ok(()),
    )
    .optional()
    .into_diagnostic()
    .wrap_err("query older workflow worker activity existence failed")
    .map(|value| value.is_some())
}

fn workflow_worker_activity_exists_after(
    conn: &Connection,
    run_id: &str,
    worker_id: &str,
    cursor: Option<i64>,
) -> Result<bool> {
    let Some(cursor) = cursor else {
        return Ok(false);
    };
    conn.query_row(
        "SELECT 1 FROM workflow_worker_activity
         WHERE run_id = ?1 AND worker_id = ?2 AND seq > ?3 LIMIT 1",
        params![run_id, worker_id, cursor],
        |_| Ok(()),
    )
    .optional()
    .into_diagnostic()
    .wrap_err("query newer workflow worker activity existence failed")
    .map(|value| value.is_some())
}

fn clamp_history_limit(limit: usize) -> usize {
    limit.clamp(1, DASHBOARD_ACTIVITY_HISTORY_LIMIT_MAX)
}

const fn history_item_is_user_input(item: &DashboardActivityHistoryItem) -> bool {
    matches!(item.event, SessionActivityEvent::User(_))
}

fn history_item_user_input_text(item: &DashboardActivityHistoryItem) -> Option<String> {
    let SessionActivityEvent::User(cell) = &item.event else {
        return None;
    };
    let text = cell.content.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn decode_history_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(i64, DashboardActivityHistoryItem)> {
    let seq: i64 = row.get(0)?;
    let item_json: String = row.get(1)?;
    let item = serde_json::from_str::<DashboardActivityHistoryItem>(&item_json).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(err))
    })?;
    Ok((seq, item))
}

fn decode_history_archive_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryArchiveItem> {
    Ok(HistoryArchiveItem {
        seq: row.get(0)?,
        role: row.get(1)?,
        tool_name: row.get(2)?,
        content: row.get(3)?,
    })
}
fn load_all_history_items(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<Vec<DashboardActivityHistoryItem>> {
    let mut statement = transaction
        .prepare("SELECT item_json FROM dashboard_activity ORDER BY seq ASC")
        .into_diagnostic()
        .wrap_err("prepare dashboard activity history scan failed")?;
    let rows = statement
        .query_map([], |row| {
            let item_json: String = row.get(0)?;
            Ok(serde_json::from_str::<DashboardActivityHistoryItem>(&item_json).ok())
        })
        .into_diagnostic()
        .wrap_err("query dashboard activity history scan failed")?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()
        .wrap_err("decode dashboard activity history scan failed")
        .map(|items| items.into_iter().flatten().collect())
}

fn normalize_window_explored_item(
    item: &mut DashboardActivityHistoryItem,
    existing_items: &[DashboardActivityHistoryItem],
) {
    let Some(group_stable_id) = explored_stable_id(item).map(str::to_owned) else {
        return;
    };

    if let Some(active_group_item) = existing_items.last().and_then(|item| {
        (explored_stable_id(item) == Some(group_stable_id.as_str())).then_some(item)
    }) {
        item.id.clone_from(&active_group_item.id);
        if let (
            SessionActivityEvent::Explored(active_group),
            SessionActivityEvent::Explored(incoming_group),
        ) = (&active_group_item.event, &mut item.event)
        {
            let mut calls = active_group.calls.clone();
            calls.extend(incoming_group.calls.clone());
            incoming_group.calls = calls;
        }
        return;
    }

    if !existing_items
        .iter()
        .any(|item| explored_stable_id(item) == Some(group_stable_id.as_str()))
    {
        return;
    }

    let segment = existing_items
        .iter()
        .filter(|item| explored_stable_id(item) == Some(group_stable_id.as_str()))
        .filter_map(|item| explored_segment(&item.id))
        .max()
        .unwrap_or(0)
        + 1;
    item.id = format!("{}-segment-{segment}", item.id);
}

const fn explored_stable_id(item: &DashboardActivityHistoryItem) -> Option<&str> {
    match &item.event {
        SessionActivityEvent::Explored(group) => Some(group.stable_id.as_str()),
        _ => None,
    }
}

fn explored_segment(item_id: &str) -> Option<usize> {
    item_id
        .rsplit_once("-segment-")
        .and_then(|(_, segment)| segment.parse::<usize>().ok())
}

fn dedupe_activity_items_keep_latest(
    items: Vec<DashboardActivityHistoryItem>,
) -> Vec<DashboardActivityHistoryItem> {
    let mut deduped: Vec<DashboardActivityHistoryItem> = Vec::new();
    for item in items {
        if let Some(existing) = deduped.iter_mut().find(|existing| existing.id == item.id) {
            *existing = item;
        } else {
            deduped.push(item);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity_event::{ExploredActivityDescriptor, ExploredCallActivityDescriptor};
    use crate::dashboard::cells::SessionActivityEvent;
    use crate::reasoning::runtime::HistoryMessage;

    fn activity_item(id: &str, cell: &SessionActivityEvent) -> DashboardActivityHistoryItem {
        DashboardActivityHistoryItem::from_event_with_id(cell, id)
    }

    fn non_user_item(id: &str) -> DashboardActivityHistoryItem {
        activity_item(
            id,
            &crate::dashboard::assistant_activity_cell("non-user")
                .expect("assistant activity cell"),
        )
    }

    fn user_input_item(id: &str, text: &str) -> DashboardActivityHistoryItem {
        let cell = crate::dashboard::render_activity_from_messages(vec![HistoryMessage::user(
            text.to_string(),
        )])
        .into_iter()
        .next()
        .expect("user activity cell");
        activity_item(id, &cell)
    }

    #[test]
    fn history_archive_round_trip_recent_range_search_and_prune() {
        let temp = tempfile::tempdir().expect("test temp dir");
        let store = DashboardActivityHistoryStore::open_at_path_for_test(
            temp.path().join("archive.sqlite3"),
        )
        .expect("store");
        let messages = vec![
            HistoryMessage::user("user one"),
            HistoryMessage::assistant("assistant one"),
            HistoryMessage::tool("call-1", "read_file", "tool output one", None),
            HistoryMessage::user("user two with needle"),
            HistoryMessage::assistant("assistant two"),
        ];
        assert_eq!(
            store
                .archive_history_messages("batch-1", &messages)
                .expect("archive batch 1"),
            5
        );
        assert_eq!(
            store
                .archive_history_messages("batch-2", &[HistoryMessage::user("batch two only")])
                .expect("archive batch 2"),
            1
        );

        let recent = store
            .query_history_archive(HistoryArchiveQueryMode::Recent, 10, None, None, "")
            .expect("recent query");
        assert_eq!(recent.len(), 6);
        assert_eq!(recent[0].seq, 1);
        assert_eq!(recent[5].seq, 6);
        assert_eq!(recent[5].role, "user");
        assert!(recent[5].content.contains("batch two only"));

        let paged = store
            .query_history_archive(HistoryArchiveQueryMode::Recent, 2, None, None, "")
            .expect("paged query");
        assert_eq!(paged.len(), 2);
        assert_eq!(paged[0].seq, 5);
        assert_eq!(paged[1].seq, 6);
        let older = store
            .query_history_archive(
                HistoryArchiveQueryMode::Recent,
                2,
                Some(paged[0].seq),
                None,
                "",
            )
            .expect("older page");
        assert_eq!(older.len(), 2);
        assert!(older.iter().all(|item| item.seq < paged[0].seq));
        assert_eq!(older[0].seq, 3);
        assert_eq!(older[1].seq, 4);

        let range = store
            .query_history_archive(HistoryArchiveQueryMode::Range, 3, None, Some(3), "")
            .expect("range query");
        assert_eq!(range.len(), 3);
        assert_eq!(range[0].seq, 3);
        assert_eq!(range[2].seq, 5);

        let search = store
            .query_history_archive(HistoryArchiveQueryMode::Search, 10, None, None, "needle")
            .expect("search query");
        assert_eq!(search.len(), 1);
        assert_eq!(search[0].seq, 4);
        assert_eq!(search[0].role, "user");
        assert!(search[0].content.contains("needle"));
        assert_eq!(
            store
                .count_history_archive(HistoryArchiveQueryMode::Search, "needle")
                .expect("search count"),
            1
        );

        let tool = store
            .query_history_archive(HistoryArchiveQueryMode::Recent, 10, None, None, "")
            .expect("tool query");
        assert_eq!(tool[2].role, "tool");
        assert_eq!(tool[2].tool_name.as_deref(), Some("read_file"));
        assert!(tool[2].content.contains("tool output one"));

        let pruned = store.prune_history_archive(1).expect("prune to one batch");
        assert_eq!(pruned, 5);
        let remaining = store
            .query_history_archive(HistoryArchiveQueryMode::Recent, 10, None, None, "")
            .expect("remaining query");
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].content.contains("batch two only"));
    }
    fn explored_group(stable_id: &str, summary: &str) -> SessionActivityEvent {
        explored_group_with_summaries(stable_id, &[summary])
    }

    fn explored_group_with_summaries(stable_id: &str, summaries: &[&str]) -> SessionActivityEvent {
        SessionActivityEvent::Explored(
            ExploredActivityDescriptor {
                stable_id: stable_id.to_string(),
                title: "Explored".to_string(),
                calls: summaries
                    .iter()
                    .map(|summary| ExploredCallActivityDescriptor {
                        tool_name: "grep".to_string(),
                        action: None,
                        target: None,
                        secondary_target: None,
                        summary: summary.to_string(),
                        detail_lines: Vec::new(),
                    })
                    .collect(),
            }
            .into(),
        )
    }

    #[test]
    fn workflow_worker_history_is_bounded_in_transport_and_pageable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DashboardActivityHistoryStore::open_at_path_for_test(
            temp.path().join("history.sqlite3"),
        )
        .expect("history store");
        let run_id = "run-session";
        let worker_id = "worker-1";
        store
            .register_workflow_worker(run_id, worker_id)
            .expect("register worker");
        for index in 0..100 {
            store
                .append_workflow_worker_activity(
                    run_id,
                    worker_id,
                    &crate::dashboard::assistant_activity_cell(&format!("activity-{index:03}"))
                        .expect("activity event"),
                )
                .expect("append worker activity");
        }
        let page = store
            .query_workflow_worker_activity(run_id, worker_id, None, None, 20)
            .expect("query worker activity")
            .expect("worker stream");
        assert_eq!(page.items.len(), 20);
        assert_eq!(page.activity_count, 100);
        assert!(page.has_more_before);
        assert_eq!(page.items.first().map(|item| item.cursor), Some(81));
        assert_eq!(page.items.last().map(|item| item.cursor), Some(100));
        let older = store
            .query_workflow_worker_activity(run_id, worker_id, page.oldest_cursor, None, 20)
            .expect("query older worker activity")
            .expect("older worker page");
        assert_eq!(older.items.first().map(|item| item.cursor), Some(61));
    }

    #[test]
    fn recent_user_input_query_returns_chronological_command_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store =
            DashboardActivityHistoryStore::open_at_path(temp.path().join("history.sqlite3"))
                .expect("history store");
        let items = vec![
            user_input_item("user-1", "first"),
            non_user_item("non-user"),
            user_input_item("user-2", "second"),
            user_input_item("user-2-duplicate", "second"),
            user_input_item("user-3", "third"),
        ];
        store.append_items(&items).expect("append history items");

        let history = store
            .query_recent_user_inputs(3)
            .expect("recent user inputs");

        assert_eq!(history.entries, vec!["first", "second", "third"]);
    }

    #[test]
    fn explored_item_ids_follow_contiguous_segments() {
        let mut window = DashboardActivityHistoryWindow::default();
        window.merge_new_items(vec![activity_item(
            "activity-explored",
            &explored_group("explored", "first"),
        )]);
        window.merge_new_items(vec![activity_item(
            "activity-explored",
            &explored_group("explored", "second"),
        )]);

        assert_eq!(window.items.len(), 1);
        assert_eq!(window.items[0].id, "activity-explored");
        let SessionActivityEvent::Explored(group) = &window.items[0].event else {
            panic!("expected explored group");
        };
        assert_eq!(
            group
                .calls
                .iter()
                .map(|call| call.summary.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );

        window.merge_new_items(vec![non_user_item("activity-boundary")]);
        window.merge_new_items(vec![activity_item(
            "activity-explored",
            &explored_group("explored", "third"),
        )]);

        assert_eq!(window.items.len(), 3);
        assert_eq!(window.items[0].id, "activity-explored");
        assert_eq!(window.items[1].id, "activity-boundary");
        assert_eq!(window.items[2].id, "activity-explored-segment-1");
    }

    #[test]
    fn explored_active_segment_appends_calls() {
        let stable_id = "explored";
        let item_id = "activity-explored";
        let mut window = DashboardActivityHistoryWindow::default();
        window.merge_new_items(vec![activity_item(
            item_id,
            &explored_group(stable_id, "first"),
        )]);
        window.merge_new_items(vec![activity_item(
            item_id,
            &explored_group(stable_id, "second"),
        )]);

        assert_eq!(window.items.len(), 1);
        assert_eq!(window.items[0].id, item_id);
        let SessionActivityEvent::Explored(group) = &window.items[0].event else {
            panic!("expected explored group");
        };
        assert_eq!(group.calls.len(), 2);
        assert_eq!(group.calls[0].summary, "first");
        assert_eq!(group.calls[1].summary, "second");
    }

    #[test]
    fn explored_active_segment_preserves_all_calls() {
        let stable_id = "explored";
        let item_id = "activity-explored";
        let first_batch = (0..20)
            .map(|index| format!("call-{index:02}"))
            .collect::<Vec<_>>();
        let second_batch = (20..32)
            .map(|index| format!("call-{index:02}"))
            .collect::<Vec<_>>();
        let first_refs = first_batch.iter().map(String::as_str).collect::<Vec<_>>();
        let second_refs = second_batch.iter().map(String::as_str).collect::<Vec<_>>();
        let mut window = DashboardActivityHistoryWindow::default();

        window.merge_new_items(vec![activity_item(
            item_id,
            &explored_group_with_summaries(stable_id, &first_refs),
        )]);
        window.merge_new_items(vec![activity_item(
            item_id,
            &explored_group_with_summaries(stable_id, &second_refs),
        )]);

        assert_eq!(window.items.len(), 1);
        let SessionActivityEvent::Explored(group) = &window.items[0].event else {
            panic!("expected explored group");
        };
        assert_eq!(group.calls.len(), 32);
        assert_eq!(group.calls[0].summary, "call-00");
        assert_eq!(group.calls[31].summary, "call-31");
    }

    #[test]
    fn workflow_worker_activity_is_normalized_coalesced_and_paged() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store =
            DashboardActivityHistoryStore::open_at_path(temp.path().join("history.sqlite3"))
                .expect("history store");
        let run_id = "run-1";
        let worker_id = "worker-1";
        store
            .register_workflow_worker(run_id, worker_id)
            .expect("register worker");
        store
            .append_workflow_worker_activity(
                run_id,
                worker_id,
                &explored_group("worker-explored", "first"),
            )
            .expect("append first explored");
        let coalesced = store
            .append_workflow_worker_activity(
                run_id,
                worker_id,
                &explored_group("worker-explored", "second"),
            )
            .expect("append second explored");
        assert_eq!(coalesced.activity_count, 1);
        assert_eq!(coalesced.revision, 2);
        let after_page = store
            .query_workflow_worker_activity(run_id, worker_id, None, Some(0), 2)
            .expect("query after cursor")
            .expect("known worker");
        assert_eq!(after_page.items.len(), 1);
        assert_eq!(after_page.oldest_cursor, after_page.newest_cursor);
        assert!(!after_page.has_more_after);

        let nested_snapshot = crate::workflow::WorkflowRunSnapshot {
            run_id: run_id.to_string(),
            workflow_id: "nested".to_string(),
            status: crate::workflow::WorkflowNodeStatus::Running,
            started_at_ms: 1,
            completed_at_ms: None,
            input: serde_json::json!({}),
            output: None,
            error: None,
            await_groups: Vec::new(),
            transitions: Vec::new(),
            workers: Vec::new(),
        };
        let workflow_event =
            SessionActivityEvent::Workflow(crate::dashboard::WorkflowActivityData {
                workflow_id: "nested".to_string(),
                status: crate::workflow::WorkflowInvocationStatus::Running,
                output: None,
                message: "nested workflow".to_string(),
                snapshot: Some(nested_snapshot),
            });
        store
            .append_workflow_worker_activity(run_id, worker_id, &workflow_event)
            .expect("append normalized workflow");
        store
            .append_workflow_worker_activity(
                run_id,
                worker_id,
                &crate::dashboard::assistant_activity_cell("boundary").expect("assistant event"),
            )
            .expect("append boundary");

        let newest = store
            .query_workflow_worker_activity(run_id, worker_id, None, None, 2)
            .expect("query newest")
            .expect("known worker");
        assert_eq!(newest.activity_count, 3);
        assert_eq!(newest.items.len(), 2);
        assert!(newest.has_more_before);
        let SessionActivityEvent::Workflow(workflow) = &newest.items[0].event else {
            panic!("expected workflow worker event");
        };
        assert!(workflow.snapshot.is_none());

        let older = store
            .query_workflow_worker_activity(run_id, worker_id, newest.oldest_cursor, None, 2)
            .expect("query older")
            .expect("known worker");
        assert_eq!(older.items.len(), 1);
        assert!(!older.has_more_before);
        assert!(older.has_more_after);
        let SessionActivityEvent::Explored(group) = &older.items[0].event else {
            panic!("expected coalesced explored event");
        };
        assert_eq!(group.calls.len(), 2);
    }

    #[test]
    fn workflow_worker_activity_rejects_unknown_worker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store =
            DashboardActivityHistoryStore::open_at_path(temp.path().join("history.sqlite3"))
                .expect("history store");
        assert!(
            store
                .query_workflow_worker_activity("missing-run", "missing-worker", None, None, 80)
                .expect("query unknown worker")
                .is_none()
        );
        assert!(
            store
                .query_workflow_worker_activity(
                    "missing-run",
                    "missing-worker",
                    Some(2),
                    Some(1),
                    80,
                )
                .is_err()
        );
    }

    #[test]
    fn workflow_snapshot_persists_and_restores_from_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store =
            DashboardActivityHistoryStore::open_at_path(temp.path().join("history.sqlite3"))
                .expect("history store");
        let snapshot = crate::workflow::WorkflowRunSnapshot {
            run_id: "workflow-run-1".to_string(),
            workflow_id: "research".to_string(),
            status: crate::workflow::WorkflowNodeStatus::Completed,
            started_at_ms: 1,
            completed_at_ms: Some(3),
            input: serde_json::json!({ "topic": "persistence" }),
            output: Some(serde_json::json!({ "summary": "restored" })),
            error: None,
            await_groups: vec![crate::workflow::WorkflowAwaitGroupSnapshot {
                group_id: "await-1".to_string(),
                sequence: 1,
                status: crate::workflow::WorkflowNodeStatus::Completed,
                started_at_ms: 1,
                completed_at_ms: Some(3),
                worker_ids: vec!["worker-1".to_string()],
            }],
            transitions: Vec::new(),
            workers: vec![crate::workflow::WorkflowWorkerSnapshot {
                worker_id: "worker-1".to_string(),
                actor_id: "history-actor-1".to_string(),
                await_group_id: "await-1".to_string(),
                role: "agent".to_string(),
                model: "main".to_string(),
                status: crate::workflow::WorkflowNodeStatus::Completed,
                started_at_ms: 1,
                completed_at_ms: Some(2),
                agent_run_time_ms: 1_000,
                input: serde_json::json!({ "query": "persist" }),
                output: Some(serde_json::json!({ "answer": "yes" })),
                error: None,
                activity_count: 1,
                activity_revision: 1,
                activity: vec![
                    crate::dashboard::thinking_activity_cell("checking history")
                        .expect("thinking event"),
                ],
            }],
        };
        let event = SessionActivityEvent::Workflow(crate::dashboard::WorkflowActivityData {
            workflow_id: snapshot.workflow_id.clone(),
            status: crate::workflow::WorkflowInvocationStatus::Completed,
            output: snapshot.output.clone(),
            message: "workflow completed".to_string(),
            snapshot: Some(snapshot.clone()),
        });
        store
            .append_items(&[activity_item("activity-workflow-run-1", &event)])
            .expect("persist workflow activity");

        let window = store.load_initial_window();
        let SessionActivityEvent::Workflow(workflow) = &window.items[0].event else {
            panic!("expected persisted workflow activity");
        };
        assert_eq!(workflow.snapshot.as_ref(), Some(&snapshot));
        let restored = workflow.snapshot.as_ref().expect("restored snapshot");
        assert_eq!(restored.workers[0].activity_count, 1);
        assert_eq!(restored.workers[0].activity.len(), 1);
        let worker_page = store
            .query_workflow_worker_activity("workflow-run-1", "worker-1", None, None, 80)
            .expect("query restored worker activity")
            .expect("registered worker stream");
        assert_eq!(worker_page.activity_count, 1);
        assert_eq!(worker_page.items.len(), 1);
    }
}
