use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use miette::{Context as _, IntoDiagnostic, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    daat_locus_paths::daat_locus_paths,
    dashboard::{WebActivityItem, default_web_activity_version},
};

const DASHBOARD_ACTIVITY_HISTORY_DB_FILE: &str = "dashboard_activity.sqlite3";
const DASHBOARD_ACTIVITY_HISTORY_LIMIT_MAX: usize = 200;
pub const DASHBOARD_ACTIVITY_HISTORY_INITIAL_LIMIT: usize = 80;

#[derive(Clone)]
pub struct DashboardActivityHistoryStore {
    db_path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DashboardActivityHistoryWindow {
    pub items: Vec<WebActivityItem>,
    pub oldest_cursor: Option<i64>,
    pub newest_cursor: Option<i64>,
    pub has_more_before: bool,
}

impl DashboardActivityHistoryWindow {
    pub fn merge_new_items(&mut self, incoming: Vec<WebActivityItem>) {
        if incoming.is_empty() {
            return;
        }

        let mut items = std::mem::take(&mut self.items);
        items.extend(incoming);
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
pub struct DashboardActivityHistoryPage {
    pub items: Vec<WebActivityItem>,
    pub oldest_cursor: Option<i64>,
    pub newest_cursor: Option<i64>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

impl DashboardActivityHistoryStore {
    pub async fn new() -> Result<Self> {
        let paths = daat_locus_paths().await;
        let db_path = paths.memory_file(DASHBOARD_ACTIVITY_HISTORY_DB_FILE);
        let store = Self {
            db_path,
            write_lock: Arc::new(Mutex::new(())),
        };
        store.initialize()?;
        Ok(store)
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

    pub fn append_items(&self, items: &[WebActivityItem]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        self.try_append_items(items)
    }

    pub fn query_before(
        &self,
        before: Option<i64>,
        limit: usize,
    ) -> Result<DashboardActivityHistoryPage> {
        let limit = clamp_history_limit(limit);
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
                .query_map(params![before, limit as i64], decode_history_row)
                .into_diagnostic()
        } else {
            statement
                .query_map(params![limit as i64], decode_history_row)
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

        let limit = clamp_history_limit(limit);
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
            .query_map(params![after, limit as i64], decode_history_row)
            .into_diagnostic()
            .wrap_err("query dashboard activity history after failed")?;
        let rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .into_diagnostic()
            .wrap_err("decode dashboard activity history after failed")?;
        self.page_from_rows(rows)
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
                 ON dashboard_activity(item_id);",
        )
        .into_diagnostic()
        .wrap_err("initialize dashboard activity history sqlite failed")?;
        Ok(())
    }

    fn try_append_items(&self, items: &[WebActivityItem]) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| miette::miette!("dashboard activity history lock poisoned"))?;
        let mut conn = self.open_connection()?;
        let transaction = conn
            .transaction()
            .into_diagnostic()
            .wrap_err("begin dashboard activity history transaction failed")?;
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
                let item_json = serde_json::to_string(item)
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
        rows: Vec<(i64, WebActivityItem)>,
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

fn clamp_history_limit(limit: usize) -> usize {
    limit.clamp(1, DASHBOARD_ACTIVITY_HISTORY_LIMIT_MAX)
}

fn decode_history_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, WebActivityItem)> {
    let seq: i64 = row.get(0)?;
    let item_json: String = row.get(1)?;
    let item = serde_json::from_str::<WebActivityItem>(&item_json).unwrap_or_else(|err| {
        tracing::warn!("decode dashboard activity item {seq} failed: {err}");
        WebActivityItem {
            web_activity_version: default_web_activity_version(),
            id: format!("history-{seq}"),
            kind: crate::dashboard::cells::WebActivityKind::Unknown,
            status: crate::dashboard::cells::WebActivityStatus::Unknown,
            ui_hint: None,
            title: "Activity".to_string(),
            actor: None,
            created_at: 0,
            updated_at: 0,
            source: None,
            tool: None,
            blocks: Vec::new(),
            detail_blocks: Vec::new(),
            error: None,
            metadata: None,
            cell: None,
        }
    });
    Ok((seq, item))
}

fn dedupe_activity_items_keep_latest(items: Vec<WebActivityItem>) -> Vec<WebActivityItem> {
    let mut deduped: Vec<WebActivityItem> = Vec::new();
    for item in items {
        if let Some(existing) = deduped.iter_mut().find(|existing| existing.id == item.id) {
            *existing = item;
        } else {
            deduped.push(item);
        }
    }
    deduped
}
