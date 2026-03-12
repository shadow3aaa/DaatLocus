//! 本模块定义任务列表

use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::get_spinova_home;

#[derive(Serialize, Deserialize, Default)]
pub struct Tasks {
    tasks: Vec<Task>,
}

#[derive(Serialize, Deserialize)]
struct Task {
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

    pub fn add_task(&mut self, description: String) {
        self.tasks.push(Task { description });
    }

    pub fn delete_task(&mut self, index: usize) {
        self.tasks.remove(index);
    }

    pub async fn shutdown(self) {
        let tasks_persistence_path = get_spinova_home().await.join("tasks");
        let data = postcard::to_allocvec(&self).unwrap();
        tokio::fs::write(tasks_persistence_path, data)
            .await
            .unwrap();
    }
}

impl Display for Tasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, task) in self.tasks.iter().enumerate() {
            writeln!(f, "{}. {}", i + 1, task.description)?;
        }
        Ok(())
    }
}
