//! 本模块定义下一步动作列表。

use std::{collections::HashMap, fmt::Display};

use chrono::Utc;
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
    pub description: String,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub last_touched_at_ms: Option<i64>,
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
            self.touch_task(id);
            self.tasks.get(&id)
        } else {
            None
        }
    }

    pub fn add_task(&mut self, description: String) -> Uuid {
        self.add_task_with_project(description, None)
    }

    pub fn add_task_with_project(&mut self, description: String, project_id: Option<Uuid>) -> Uuid {
        let id = Uuid::new_v4();
        self.tasks.insert(
            id,
            Task {
                description,
                project_id,
                last_touched_at_ms: None,
            },
        );
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

    pub fn tasks(&self) -> impl Iterator<Item = (Uuid, &Task)> {
        self.tasks.iter().map(|(id, task)| (*id, task))
    }

    pub fn delete_tasks_for_project(&mut self, project_id: Uuid) -> usize {
        let task_ids = self
            .tasks
            .iter()
            .filter_map(|(id, task)| (task.project_id == Some(project_id)).then_some(*id))
            .collect::<Vec<_>>();
        let deleted = task_ids.len();
        for task_id in task_ids {
            self.delete_task(task_id);
        }
        deleted
    }

    pub fn working_task(&self) -> Option<Uuid> {
        self.working_task
    }

    pub fn touch_working_task(&mut self) {
        if let Some(id) = self.working_task {
            self.touch_task(id);
        }
    }

    fn touch_task(&mut self, id: Uuid) {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.last_touched_at_ms = Some(Utc::now().timestamp_millis());
        }
    }
}

impl Display for Tasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.tasks.is_empty() {
            return write!(f, "当前没有下一步动作。");
        }

        if self.working_task.is_none() {
            writeln!(
                f,
                "当前没有选中的下一步动作。如果要执行动作，必须先选择一个动作再执行。"
            )?;
        }

        for (id, task) in self.tasks.iter() {
            let project_suffix = task
                .project_id
                .map(|project_id| format!(" [project={project_id}]"))
                .unwrap_or_default();
            if Some(id) == self.working_task.as_ref() {
                writeln!(f, "--- 选中的下一步动作 ---")?;
                writeln!(f, "{id}. {}{}", task.description, project_suffix)?;
                writeln!(f, "--- 选中的下一步动作 ---")?;
            } else {
                writeln!(f, "{id}. {}{}", task.description, project_suffix)?;
            }
        }
        Ok(())
    }
}
