//! 本模块定义任务列表

use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::get_spinova_home;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Tasks {
    working_task: Option<usize>,
    tasks: Vec<Task>,
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

    pub fn select_working_task(&mut self, index: usize) -> Option<&Task> {
        if index > 0 && index <= self.tasks.len() {
            self.working_task = Some(index - 1);
            self.tasks.get(index - 1)
        } else {
            None
        }
    }

    pub fn add_task(&mut self, description: String) {
        self.tasks.push(Task { description });
    }

    pub fn delete_task(&mut self, index: usize) -> Option<Task> {
        if index > 0 && index <= self.tasks.len() {
            Some(self.tasks.remove(index - 1))
        } else {
            None
        }
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
        if self.tasks.is_empty() {
            return Ok(());
        }

        if self.working_task.is_none() {
            writeln!(
                f,
                "当前没有选中的任务。如果要执行任务，必须先选择一个任务再执行。"
            )?;
        }

        for (i, task) in self.tasks.iter().enumerate() {
            if Some(i) == self.working_task {
                writeln!(f, "--- 选中的任务 ---")?;
                writeln!(f, "{}. {}", i + 1, task.description)?;
                writeln!(f, "--- 选中的任务 ---")?;
            } else {
                writeln!(f, "{}. {}", i + 1, task.description)?;
            }
        }
        Ok(())
    }
}
