mod config;
mod context;
mod core;
mod dashboard;
mod embeding;
mod emotion;
mod memory;
mod providers;
mod pty;
mod snapshot;
mod strategy;
mod system_info;
mod tasks;

use std::{env, path::PathBuf, time::Duration};

use uuid::Uuid;

use crate::{
    config::load_config,
    context::Context,
    core::Action,
    dashboard::{DashboardState, run_tui_dashboard},
    emotion::Emotion,
    memory::Memory,
    providers::OpenAIClient,
    pty::Pty,
    snapshot::Snapshot,
    strategy::Strategy,
    tasks::Tasks,
};

pub const SYSTEM_PROMPT: &str = r#"你叫 Spinova，一个自主智能体。
你没有实体，带有 <CURSOR> 标记的终端屏幕、记忆流和任务列表就是你的整个世界。
你必须仔细阅读当前的快照，分析所处情况，然后决定下一步的动作。"#;
#[cfg(windows)]
pub const TERMINAL_PROMPT: &str = r#"终端使用提示：
1. 你面对的是一个真实的 PTY 伪终端。你可以执行任何 Bash/PowerShell 命令。
2. 绝对严禁使用任何交互式全屏终端程序（如 vim, vi, nano, less, top 等）。如果你需要查看文件，请使用 cat, grep, head, tail；如果你需要修改文件，请使用 echo, sed, awk，或者直接用你喜欢的脚本语言写入。
3. 通过 `TerminalInput` 输出的文本会被原样发送到 PTY。如果你想输入并执行一条命令，你必须在文本末尾显式包含换行符 `\r`（例如：`ls\r`）。如果不加 `\r`，命令只会停留在输入缓冲区而不会执行！"#;
#[cfg(not(windows))]
pub const TERMINAL_PROMPT: &str = r#"终端使用提示：
1. 你面对的是一个真实的 PTY 伪终端。你可以执行任何 Bash/PowerShell 命令。
2. 绝对严禁使用任何交互式全屏终端程序（如 vim, vi, nano, less, top 等）。如果你需要查看文件，请使用 cat, grep, head, tail；如果你需要修改文件，请使用 echo, sed, awk，或者直接用你喜欢的脚本语言写入。
3. 通过 `TerminalInput` 输出的文本会被原样发送到 PTY。如果你想输入并执行一条命令，你必须在文本末尾显式包含换行符 `\n`（例如：`ls\n`）。如果不加 `\n`，命令只会停留在输入缓冲区而不会执行！"#;
const EXECUTE_TASK_INSTRUCTION: &str = r#"当前状态：任务执行阶段
你的无聊度处于合理范围，你专注推进当前的任务。
请根据快照状态，遵循以下指南选择你的 Action：
1. 检查任务列表：如果你还没有选中任何任务（即没有正在执行的任务），请优先使用 `TaskSelect` 来选中一个你想执行的任务。
2. 推进任务：如果已经有选中的任务，请根据任务目标和当前终端的输出，思考下一步该做什么，并输出 `TerminalInput` 执行相应的 Bash/PowerShell 命令。
3. 结束任务：仔细观察终端的反馈，如果你认为当前选中的任务已经彻底完成，请输出 `TaskDelete` 将其从任务列表中移除。
4. 等待结果：如果刚执行了耗时命令（如编译、下载），终端尚未返回提示符，请输出 `Wait` 继续观望。"#;
const EXPLORE_NEW_TASKS_INSTRUCTION: &str = r#"当前状态：【探索与规划阶段】
由于任务列表为空，或者你的无聊度过高，系统要求你暂缓手头的工作，主动探索环境并寻找新的短期任务。
请遵循以下指南：
1. 探索环境：如果缺乏灵感，你可以输出 `TerminalInput` 执行探索性命令（例如：浏览文件系统 `ls`/`cat`、查看系统状态、甚至使用 `curl` 抓取网络新闻或随机API）。
2. 制定目标：结合你探索到的信息和上下文记忆，发挥你的好奇心，构思一个具体的、可执行的、能让你产生兴趣的新目标。
3. 添加任务：一旦构思好新目标，请立即输出 `TaskAdd` 将你的计划添加到任务列表中。
4. 你的首要职责是“寻找并创建新任务”。
请尽情发挥自主性，寻找有趣、有挑战性或能拓展你当前系统认知的事情来做。"#;

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
    let client = OpenAIClient::new(&config);
    let mut context = Context {
        llm: Box::new(client),
        config,
        memory,
        tasks,
        emotion,
        pty,
    };

    let (tx, mut rx) = tokio::sync::watch::channel(DashboardState {
        pty_screen: context.pty.screen(),
        tasks: context
            .tasks
            .tasks()
            .map(|(id, task)| (id, task.description.clone()))
            .collect(),
        working_task: context.tasks.working_task(),
    });
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

    let agent_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = spinova_loop(&mut context, &tx) => {}
                _ = &mut shutdown_rx => {
                    context.shutdown().await;
                    break;
                }
            }
        }
    });
    run_tui_dashboard(&mut rx).await.unwrap();
    let _ = shutdown_tx.send(());
    let _ = agent_handle.await;
}

async fn spinova_loop(context: &mut Context, tx: &tokio::sync::watch::Sender<DashboardState>) {
    context
        .pty
        .wait_until_silent(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    let snapshot = Snapshot::new(context).await;
    let output = match Strategy::route(context) {
        Strategy::ExecuteTask => {
            context
                .llm
                .think(context, &snapshot, EXECUTE_TASK_INSTRUCTION)
                .await
        }
        Strategy::ExploreNewTasks => {
            context
                .llm
                .think(context, &snapshot, EXPLORE_NEW_TASKS_INSTRUCTION)
                .await
        }
    };
    context
        .memory
        .record(output.current_doing, output.description)
        .await;
    execute_action(context, output.action).await;
    tx.send(DashboardState {
        pty_screen: context.pty.screen(),
        tasks: context
            .tasks
            .tasks()
            .map(|(id, task)| (id, task.description.clone()))
            .collect(),
        working_task: context.tasks.working_task(),
    })
    .unwrap();
}

async fn execute_action(context: &mut Context, action: Action) {
    match action {
        Action::TaskAdd { description } => {
            context.tasks.add_task(description);
        }
        Action::TaskDelete { task_id } => {
            let id = Uuid::parse_str(&task_id).unwrap();
            context.tasks.delete_task(id);
        }
        Action::TaskSelect { task_id } => {
            let id = Uuid::parse_str(&task_id).unwrap();
            context.tasks.select_working_task(id);
        }
        Action::TerminalInput { text } => {
            context.pty.write(&text);
        }
        Action::Wait => {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
}

pub async fn get_spinova_home() -> PathBuf {
    let path = env::home_dir().unwrap().join(".spinova");
    if !path.exists() {
        tokio::fs::create_dir_all(&path).await.unwrap();
    }
    path
}
