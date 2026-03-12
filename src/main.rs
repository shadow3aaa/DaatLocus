mod config;
mod context;
mod core;
mod embeding;
mod emotion;
mod memory;
mod pty;
mod snapshot;
mod system_info;
mod tasks;

use std::{env, path::PathBuf};

use crate::{
    config::load_config, context::Context, emotion::Emotion, memory::Memory, pty::Pty,
    snapshot::Snapshot, tasks::Tasks,
};

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
    let tasks = Tasks::new().await;
    let emotion = Emotion::new().await;
    let pty = Pty::new();
    let mut context = Context {
        config,
        memory,
        tasks,
        emotion,
        pty,
    };

    loop {
        tokio::select! {
            _ = spinova_loop(&mut context) => {},
            _ = tokio::signal::ctrl_c() => {
                context.shutdown().await;
                break;
            }
        }
    }
}

async fn spinova_loop(context: &mut Context) {
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let snapshot = Snapshot::new(context).await;
    println!("{snapshot}");
}

pub async fn get_spinova_home() -> PathBuf {
    let path = env::home_dir().unwrap().join(".spinova");
    if !path.exists() {
        tokio::fs::create_dir_all(&path).await.unwrap();
    }
    path
}
