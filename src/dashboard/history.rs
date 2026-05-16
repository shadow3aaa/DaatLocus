use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use miette::{Context as _, IntoDiagnostic, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    daat_locus_paths::daat_locus_paths,
    dashboard::{
        ActivityCell, WebActivityItem, default_web_activity_version, web_activity_item_from_cell,
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
            normalize_window_coding_tool_group_item(&mut item, &items);
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
                normalize_window_coding_tool_group_item(&mut item, &existing_items);
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

fn normalize_window_coding_tool_group_item(
    item: &mut WebActivityItem,
    existing_items: &[WebActivityItem],
) {
    let Some(group_stable_id) = coding_tool_group_stable_id(item).map(str::to_owned) else {
        return;
    };

    let baseline_end = if let Some(active_group_item) = existing_items.last().and_then(|item| {
        (coding_tool_group_stable_id(item) == Some(group_stable_id.as_str())).then_some(item)
    }) {
        item.id = active_group_item.id.clone();
        existing_items.len().saturating_sub(1)
    } else {
        if !existing_items
            .iter()
            .any(|item| coding_tool_group_stable_id(item) == Some(group_stable_id.as_str()))
        {
            return;
        }

        let segment = existing_items
            .iter()
            .filter(|item| coding_tool_group_stable_id(item) == Some(group_stable_id.as_str()))
            .filter_map(|item| coding_tool_group_segment(&item.id))
            .max()
            .unwrap_or(0)
            + 1;
        item.id = format!("{}-segment-{segment}", item.id);
        existing_items.len()
    };

    let baseline_calls = existing_items[..baseline_end]
        .iter()
        .filter_map(|item| match item.cell.as_ref()? {
            ActivityCell::CodingToolGroup(group) if group.stable_id == group_stable_id => {
                Some(group.calls.iter().cloned())
            }
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();

    let mut changed = false;
    if let Some(ActivityCell::CodingToolGroup(group)) = item.cell.as_mut() {
        let overlap = suffix_prefix_overlap(&baseline_calls, &group.calls);
        if overlap > 0 {
            group.calls.drain(0..overlap);
            changed = true;
        }
    }
    if changed {
        refresh_web_activity_item_from_cell(item);
    }
}

fn suffix_prefix_overlap<T: PartialEq>(baseline: &[T], incoming: &[T]) -> usize {
    let max_overlap = baseline.len().min(incoming.len());
    (1..=max_overlap)
        .rev()
        .find(|&len| baseline[baseline.len() - len..] == incoming[..len])
        .unwrap_or(0)
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

fn coding_tool_group_stable_id(item: &WebActivityItem) -> Option<&str> {
    match item.cell.as_ref()? {
        crate::dashboard::ActivityCell::CodingToolGroup(group) => Some(group.stable_id.as_str()),
        _ => None,
    }
}

fn coding_tool_group_segment(item_id: &str) -> Option<usize> {
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
    use crate::tool_ui::{CodingToolCallUiData, CodingToolGroupUiData};

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

    fn coding_group(stable_id: &str, summary: &str) -> ActivityCell {
        coding_group_with_summaries(stable_id, &[summary])
    }

    fn coding_group_with_summaries(stable_id: &str, summaries: &[&str]) -> ActivityCell {
        ActivityCell::CodingToolGroup(
            CodingToolGroupUiData {
                stable_id: stable_id.to_string(),
                title: "Explored".to_string(),
                calls: summaries
                    .iter()
                    .map(|summary| CodingToolCallUiData {
                        tool_name: "grep".to_string(),
                        summary: summary.to_string(),
                        detail_lines: Vec::new(),
                    })
                    .collect(),
            }
            .into(),
        )
    }

    #[test]
    fn coding_tool_group_item_ids_follow_contiguous_segments() {
        let mut window = DashboardActivityHistoryWindow::default();
        window.merge_new_items(vec![activity_item(
            "activity-coding-tools-project",
            Some(coding_group("coding-tools-project", "first")),
        )]);
        window.merge_new_items(vec![activity_item(
            "activity-coding-tools-project",
            Some(coding_group("coding-tools-project", "second")),
        )]);

        assert_eq!(window.items.len(), 1);
        assert_eq!(window.items[0].id, "activity-coding-tools-project");

        window.merge_new_items(vec![activity_item("activity-boundary", None)]);
        window.merge_new_items(vec![activity_item(
            "activity-coding-tools-project",
            Some(coding_group("coding-tools-project", "third")),
        )]);

        assert_eq!(window.items.len(), 3);
        assert_eq!(window.items[0].id, "activity-coding-tools-project");
        assert_eq!(window.items[1].id, "activity-boundary");
        assert_eq!(
            window.items[2].id,
            "activity-coding-tools-project-segment-1"
        );
    }

    #[test]
    fn coding_tool_group_segments_trim_already_rendered_calls() {
        let stable_id = "coding-tools-project";
        let item_id = "activity-coding-tools-project";
        let mut window = DashboardActivityHistoryWindow::default();
        window.merge_new_items(vec![web_activity_item_from_cell(
            &coding_group_with_summaries(stable_id, &["first", "second"]),
            item_id,
            false,
        )]);
        window.merge_new_items(vec![activity_item("activity-boundary", None)]);
        window.merge_new_items(vec![web_activity_item_from_cell(
            &coding_group_with_summaries(stable_id, &["first", "second", "third"]),
            item_id,
            false,
        )]);

        assert_eq!(window.items.len(), 3);
        assert_eq!(window.items[2].id, format!("{item_id}-segment-1"));
        let ActivityCell::CodingToolGroup(group) = window.items[2].cell.as_ref().unwrap() else {
            panic!("expected coding group");
        };
        assert_eq!(group.calls.len(), 1);
        assert_eq!(group.calls[0].summary, "third");
        assert_eq!(
            window.items[2]
                .tool
                .as_ref()
                .and_then(|tool| tool.input_preview.as_deref()),
            Some("1 call(s)")
        );
    }
}
