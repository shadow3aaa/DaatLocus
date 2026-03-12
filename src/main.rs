mod config;
mod core;
mod embeding;
mod memory;
mod snapshot;
mod system_info;
mod tasks;

use std::{env, path::PathBuf};

use crate::{config::load_config, memory::Memory};

#[tokio::main]
async fn main() {
    let config = match load_config().await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let memory = Memory::new().await;
    let tasks = tasks::Tasks::new().await;

    loop {
        tokio::select! {
            _ = spinova_loop() => {},
            _ = tokio::signal::ctrl_c() => {
                memory.shutdown().await;
                tasks.shutdown().await;
                break;
            }
        }
    }
}

async fn spinova_loop() {
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
}

pub async fn get_spinova_home() -> PathBuf {
    let path = env::home_dir().unwrap().join(".spinova");
    if !path.exists() {
        tokio::fs::create_dir_all(&path).await.unwrap();
    }
    path
}
