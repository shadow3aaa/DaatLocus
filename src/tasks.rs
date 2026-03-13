//! 本模块定义任务列表

use std::{collections::HashMap, fmt::Display};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::get_spinova_home;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Tasks {
    working_task: Option<Uuid>,
    tasks: HashMap<Uuid, Task>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Task {
    description: String,
}

impl Tasks {
    pub async fn new() -> Self {
        let tasks_persistence_path = get_spinova_home().await.join("tasks");
        tokio::fs::read(tasks_persistence_path)
            .await
            .ok()
            .and_then(|data| postcard::from_bytes::<Self>(&data).ok())
            .unwrap_or_else(|| Self::default())
    }

    pub fn select_working_task(&mut self, id: Uuid) -> Option<&Task> {
        if self.tasks.contains_key(&id) {
            self.working_task = Some(id);
            self.tasks.get(&id)
        } else {
            None
        }
    }

    pub fn add_task(&mut self, description: String) -> Uuid {
        let id = Uuid::new_v4();
        self.tasks.insert(id, Task { description });
        id
    }

    pub fn delete_task(&mut self, id: Uuid) -> Option<Task> {
        if self.working_task == Some(id) {
            self.working_task = None;
        }
        self.tasks.remove(&id)
    }

    pub async fn shutdown(self) {
        let tasks_persistence_path = get_spinova_home().await.join("tasks");
        let data = postcard::to_allocvec(&self).unwrap();
        tokio::fs::write(tasks_persistence_path, data)
            .await
            .unwrap();
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

impl Display for Tasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.tasks.is_empty() {
            return Ok(());
        }

        if self.working_task.is_none() {
            writeln!(
                f,
                "当前没有选中的任务。如果要执行任务，必须先选择一个任务再执行。"
            )?;
        }

        for (id, task) in self.tasks.iter() {
            if Some(id) == self.working_task.as_ref() {
                writeln!(f, "--- 选中的任务 ---")?;
                writeln!(f, "{id}. {}", task.description)?;
                writeln!(f, "--- 选中的任务 ---")?;
            } else {
                writeln!(f, "{id}. {}", task.description)?;
            }
        }
        Ok(())
    }
}
