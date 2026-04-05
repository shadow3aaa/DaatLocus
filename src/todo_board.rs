use std::{collections::HashMap, fmt::Display};

use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::get_spinova_home;

const TODO_BOARD_FILE_NAME: &str = "todo_board";

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct TodoBoard {
    items: HashMap<Uuid, TodoItem>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TodoItem {
    pub title: String,
    pub origin: TodoOrigin,
    pub done_criteria: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at_ms: i64,
    pub last_updated_at_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum TodoOrigin {
    SelfInitiated,
    Telegram,
    Terminal,
    System,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, JsonSchema)]
pub enum TodoStatus {
    Active,
    Blocked,
    Completed,
    Dropped,
}

impl TodoBoard {
    pub async fn new() -> Self {
        let persistence_path = get_spinova_home().await.join(TODO_BOARD_FILE_NAME);
        tokio::fs::read(&persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_else(Self::default)
    }

    pub fn add(
        &mut self,
        title: String,
        origin: TodoOrigin,
        done_criteria: String,
        notes: Option<String>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let now = Utc::now().timestamp_millis();
        self.items.insert(
            id,
            TodoItem {
                title,
                origin,
                done_criteria,
                status: TodoStatus::Active,
                notes,
                created_at_ms: now,
                last_updated_at_ms: now,
            },
        );
        id
    }

    pub fn update(
        &mut self,
        id: Uuid,
        title: Option<String>,
        done_criteria: Option<String>,
        notes: Option<Option<String>>,
        status: Option<TodoStatus>,
    ) -> bool {
        let Some(item) = self.items.get_mut(&id) else {
            return false;
        };

        let mut changed = false;
        if let Some(title) = title
            && item.title != title
        {
            item.title = title;
            changed = true;
        }
        if let Some(done_criteria) = done_criteria
            && item.done_criteria != done_criteria
        {
            item.done_criteria = done_criteria;
            changed = true;
        }
        if let Some(notes) = notes
            && item.notes != notes
        {
            item.notes = notes;
            changed = true;
        }
        if let Some(status) = status
            && item.status != status
        {
            item.status = status;
            changed = true;
        }
        if changed {
            item.last_updated_at_ms = Utc::now().timestamp_millis();
        }
        changed
    }

    pub fn get(&self, id: Uuid) -> Option<&TodoItem> {
        self.items.get(&id)
    }

    pub fn items(&self) -> impl Iterator<Item = (Uuid, &TodoItem)> {
        self.items.iter().map(|(id, item)| (*id, item))
    }

    pub fn active_items(&self) -> impl Iterator<Item = (Uuid, &TodoItem)> {
        self.items()
            .filter(|(_, item)| !matches!(item.status, TodoStatus::Completed | TodoStatus::Dropped))
    }

    pub async fn shutdown(self) {
        let persistence_path = get_spinova_home().await.join(TODO_BOARD_FILE_NAME);
        let data = postcard::to_allocvec(&self).unwrap();
        tokio::fs::write(persistence_path, data).await.unwrap();
    }
}

impl Display for TodoBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut items = self.items().collect::<Vec<_>>();
        if items.is_empty() {
            return write!(f, "当前没有 todo。");
        }

        items.sort_by_key(|(id, _)| id.to_string());
        for (index, (id, item)) in items.into_iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            writeln!(
                f,
                "- {id}. [{} / {}] {}",
                item.status, item.origin, item.title
            )?;
            writeln!(f, "  完成标准：{}", item.done_criteria)?;
            if let Some(notes) = item.notes.as_deref()
                && !notes.trim().is_empty()
            {
                writeln!(f, "  备注：{notes}")?;
            }
        }
        Ok(())
    }
}

impl Display for TodoOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelfInitiated => write!(f, "Self"),
            Self::Telegram => write!(f, "Telegram"),
            Self::Terminal => write!(f, "Terminal"),
            Self::System => write!(f, "System"),
        }
    }
}

impl Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "Active"),
            Self::Blocked => write!(f, "Blocked"),
            Self::Completed => write!(f, "Completed"),
            Self::Dropped => write!(f, "Dropped"),
        }
    }
}
