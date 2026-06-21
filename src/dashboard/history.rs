use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use miette::{Context as _, IntoDiagnostic, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    daat_locus_paths::{DaatLocusPaths, daat_locus_paths},
    dashboard::{
        ActivityCell, WebActivityActor, WebActivityItem, WebActivityKind,
        default_web_activity_version, web_activity_item_from_cell,
    },
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
    pub items: Vec<WebActivityItem>,
    pub oldest_cursor: Option<i64>,
    pub newest_cursor: Option<i64>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardInputHistory {
    pub entries: Vec<String>,
}

impl DashboardActivityHistoryStore {
    #[allow(dead_code)]
    pub async fn new() -> Result<Self> {
        let paths = daat_locus_paths().await;
        Self::open_at_path(paths.memory_file(DASHBOARD_ACTIVITY_HISTORY_DB_FILE))
    }

    pub async fn with_session(session_id: &str) -> Result<Self> {
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

    pub fn clear_all(&self) -> Result<usize> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| miette::miette!("dashboard activity history lock poisoned"))?;
        let conn = self.open_connection()?;
        conn.execute("DELETE FROM dashboard_activity", [])
            .into_diagnostic()
            .wrap_err("clear dashboard activity history failed")
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

    pub fn query_user_input_count(&self) -> Result<DashboardActivityHistoryCount> {
        let conn = self.open_connection()?;
        let mut statement = conn
            .prepare("SELECT item_json FROM dashboard_activity")
            .into_diagnostic()
            .wrap_err("prepare dashboard activity history count query failed")?;
        let rows = statement
            .query_map([], |row| {
                let item_json: String = row.get(0)?;
                Ok(serde_json::from_str::<WebActivityItem>(&item_json).ok())
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
            let Some(item) = serde_json::from_str::<WebActivityItem>(&item_json).ok() else {
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

fn history_item_is_user_input(item: &WebActivityItem) -> bool {
    item.kind == WebActivityKind::Message
        && matches!(
            item.actor,
            Some(WebActivityActor::User | WebActivityActor::Telegram)
        )
        && !matches!(
            item.cell,
            Some(
                ActivityCell::Assistant(_)
                    | ActivityCell::Reply(_)
                    | ActivityCell::Thinking(_)
                    | ActivityCell::FinalMessageSeparator(_)
            )
        )
        && item.ui_hint.as_deref() != Some("final-message-separator")
}

fn history_item_user_input_text(item: &WebActivityItem) -> Option<String> {
    if item.kind != WebActivityKind::Message
        || !matches!(item.actor.as_ref(), Some(WebActivityActor::User))
    {
        return None;
    }
    let Some(ActivityCell::User(cell)) = item.cell.as_ref() else {
        return None;
    };
    let text = cell
        .full_body
        .clone()
        .unwrap_or_else(|| {
            std::iter::once(cell.title.as_str())
                .chain(cell.body_lines.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
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

fn load_all_history_items(transaction: &rusqlite::Transaction<'_>) -> Result<Vec<WebActivityItem>> {
    let mut statement = transaction
        .prepare("SELECT item_json FROM dashboard_activity ORDER BY seq ASC")
        .into_diagnostic()
        .wrap_err("prepare dashboard activity history scan failed")?;
    let rows = statement
        .query_map([], |row| {
            let item_json: String = row.get(0)?;
            Ok(serde_json::from_str::<WebActivityItem>(&item_json).ok())
        })
        .into_diagnostic()
        .wrap_err("query dashboard activity history scan failed")?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .into_diagnostic()
        .wrap_err("decode dashboard activity history scan failed")
        .map(|items| items.into_iter().flatten().collect())
}

fn normalize_window_explored_item(item: &mut WebActivityItem, existing_items: &[WebActivityItem]) {
    let Some(group_stable_id) = explored_stable_id(item).map(str::to_owned) else {
        return;
    };

    if let Some(active_group_item) = existing_items.last().and_then(|item| {
        (explored_stable_id(item) == Some(group_stable_id.as_str())).then_some(item)
    }) {
        item.id = active_group_item.id.clone();
        if let (
            Some(ActivityCell::Explored(active_group)),
            Some(ActivityCell::Explored(incoming_group)),
        ) = (active_group_item.cell.as_ref(), item.cell.as_mut())
        {
            let mut calls = active_group.calls.clone();
            calls.extend(incoming_group.calls.clone());
            incoming_group.calls = calls;
            refresh_web_activity_item_from_cell(item);
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

fn refresh_web_activity_item_from_cell(item: &mut WebActivityItem) {
    let Some(cell) = item.cell.clone() else {
        return;
    };
    let id = item.id.clone();
    let status = item.status.clone();
    let created_at = item.created_at;
    let updated_at = item.updated_at;
    let source = item.source.clone();
    let metadata = item.metadata.clone();

    let mut refreshed = web_activity_item_from_cell(&cell, &id, false);
    refreshed.status = status;
    refreshed.created_at = created_at;
    refreshed.updated_at = updated_at;
    refreshed.source = source;
    refreshed.metadata = metadata;
    *item = refreshed;
}

fn explored_stable_id(item: &WebActivityItem) -> Option<&str> {
    match item.cell.as_ref()? {
        crate::dashboard::ActivityCell::Explored(group) => Some(group.stable_id.as_str()),
        _ => None,
    }
}

fn explored_segment(item_id: &str) -> Option<usize> {
    item_id
        .rsplit_once("-segment-")
        .and_then(|(_, segment)| segment.parse::<usize>().ok())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::cells::{ActivityCell, WebActivityKind, WebActivityStatus};
    use crate::reasoning::runtime::HistoryMessage;
    use crate::tool_ui::{ExploredCallUiData, ExploredUiData};

    fn activity_item(id: &str, cell: Option<ActivityCell>) -> WebActivityItem {
        WebActivityItem {
            web_activity_version: default_web_activity_version(),
            id: id.to_string(),
            kind: WebActivityKind::Unknown,
            status: WebActivityStatus::Completed,
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
            cell,
        }
    }

    fn user_input_item(id: &str, text: &str) -> WebActivityItem {
        let cell = crate::dashboard::render_activity_from_messages(vec![HistoryMessage::user(
            text.to_string(),
        )])
        .into_iter()
        .next()
        .expect("user activity cell");
        web_activity_item_from_cell(&cell, id, false)
    }

    fn explored_group(stable_id: &str, summary: &str) -> ActivityCell {
        explored_group_with_summaries(stable_id, &[summary])
    }

    fn explored_group_with_summaries(stable_id: &str, summaries: &[&str]) -> ActivityCell {
        ActivityCell::Explored(
            ExploredUiData {
                stable_id: stable_id.to_string(),
                title: "Explored".to_string(),
                calls: summaries
                    .iter()
                    .map(|summary| ExploredCallUiData {
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
    fn recent_user_input_query_returns_chronological_command_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store =
            DashboardActivityHistoryStore::open_at_path(temp.path().join("history.sqlite3"))
                .expect("history store");
        let items = vec![
            user_input_item("user-1", "first"),
            activity_item("non-user", None),
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
            Some(explored_group("explored", "first")),
        )]);
        window.merge_new_items(vec![activity_item(
            "activity-explored",
            Some(explored_group("explored", "second")),
        )]);

        assert_eq!(window.items.len(), 1);
        assert_eq!(window.items[0].id, "activity-explored");
        let ActivityCell::Explored(group) = window.items[0].cell.as_ref().unwrap() else {
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

        window.merge_new_items(vec![activity_item("activity-boundary", None)]);
        window.merge_new_items(vec![activity_item(
            "activity-explored",
            Some(explored_group("explored", "third")),
        )]);

        assert_eq!(window.items.len(), 3);
        assert_eq!(window.items[0].id, "activity-explored");
        assert_eq!(window.items[1].id, "activity-boundary");
        assert_eq!(window.items[2].id, "activity-explored-segment-1");
    }

    #[test]
    fn explored_active_segment_appends_calls_and_refreshes_preview() {
        let stable_id = "explored";
        let item_id = "activity-explored";
        let mut window = DashboardActivityHistoryWindow::default();
        window.merge_new_items(vec![web_activity_item_from_cell(
            &explored_group(stable_id, "first"),
            item_id,
            false,
        )]);
        window.merge_new_items(vec![web_activity_item_from_cell(
            &explored_group(stable_id, "second"),
            item_id,
            false,
        )]);

        assert_eq!(window.items.len(), 1);
        assert_eq!(window.items[0].id, item_id);
        let ActivityCell::Explored(group) = window.items[0].cell.as_ref().unwrap() else {
            panic!("expected explored group");
        };
        assert_eq!(group.calls.len(), 2);
        assert_eq!(group.calls[0].summary, "first");
        assert_eq!(group.calls[1].summary, "second");
        assert_eq!(
            window.items[0]
                .tool
                .as_ref()
                .and_then(|tool| tool.input_preview.as_deref()),
            Some("2 call(s)")
        );
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

        window.merge_new_items(vec![web_activity_item_from_cell(
            &explored_group_with_summaries(stable_id, &first_refs),
            item_id,
            false,
        )]);
        window.merge_new_items(vec![web_activity_item_from_cell(
            &explored_group_with_summaries(stable_id, &second_refs),
            item_id,
            false,
        )]);

        assert_eq!(window.items.len(), 1);
        let ActivityCell::Explored(group) = window.items[0].cell.as_ref().unwrap() else {
            panic!("expected explored group");
        };
        assert_eq!(group.calls.len(), 32);
        assert_eq!(group.calls[0].summary, "call-00");
        assert_eq!(group.calls[31].summary, "call-31");
        assert_eq!(
            window.items[0]
                .tool
                .as_ref()
                .and_then(|tool| tool.input_preview.as_deref()),
            Some("32 call(s)")
        );
    }
}
