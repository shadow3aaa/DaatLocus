mod config;
mod context;
mod core;
mod dashboard;
mod device;
mod embeding;
mod emotion;
mod memory;
mod providers;
mod pty;
mod snapshot;
mod strategy;
mod system_info;
mod terminal_device;
mod tasks;
mod telegram_acl;
mod telegram_device;
mod telegram_transport;

use std::{env, path::PathBuf, time::Duration};

use uuid::Uuid;

use crate::{
    config::load_config,
    context::Context,
    core::Action,
    dashboard::{DashboardState, run_tui_dashboard},
    device::{DeviceId, DeviceManager},
    emotion::Emotion,
    memory::Memory,
    providers::OpenAIClient,
    snapshot::Snapshot,
    strategy::Strategy,
    terminal_device::TerminalDevice,
    tasks::Tasks,
    telegram_acl::TelegramAclHandle,
    telegram_device::TelegramDevice,
    telegram_transport::TelegramTransport,
};

pub const SYSTEM_PROMPT: &str = r#"你叫 Spinova，一个自主智能体。
你没有实体，也没有要服务的用户。记忆流、任务列表、设备外围感知和当前前景设备画面就是你的整个世界。
你一次只能详细看见一个处于前景的设备，其它设备只能通过外围感知知道其存在与提醒。
你必须仔细阅读当前的快照，分析所处情况，然后决定下一步的动作。"#;
#[cfg(windows)]
pub const TERMINAL_PROMPT: &str = r#"终端使用提示：
1. 当 Terminal 设备处于前景时，你面对的是一个真实的 PTY 伪终端。你可以执行任何 Bash/PowerShell 命令。
2. 绝对严禁使用任何交互式全屏终端程序（如 vim, vi, nano, less, top 等）。如果你需要查看文件，请使用 cat, grep, head, tail；如果你需要修改文件，请使用 echo, sed, awk，或者直接用你喜欢的脚本语言写入。
3. 终端输入必须通过 `DeviceAction` -> `TerminalInput` 输出，文本会被原样发送到 PTY。如果你想输入并执行一条命令，你必须在文本末尾显式包含换行符 `\r`（例如：`ls\r`）。如果不加 `\r`，命令只会停留在输入缓冲区而不会执行！"#;
#[cfg(not(windows))]
pub const TERMINAL_PROMPT: &str = r#"终端使用提示：
1. 当 Terminal 设备处于前景时，你面对的是一个真实的 PTY 伪终端。你可以执行任何 Bash/PowerShell 命令。
2. 绝对严禁使用任何交互式全屏终端程序（如 vim, vi, nano, less, top 等）。如果你需要查看文件，请使用 cat, grep, head, tail；如果你需要修改文件，请使用 echo, sed, awk，或者直接用你喜欢的脚本语言写入。
3. 终端输入必须通过 `DeviceAction` -> `TerminalInput` 输出，文本会被原样发送到 PTY。如果你想输入并执行一条命令，你必须在文本末尾显式包含换行符 `\n`（例如：`ls\n`）。如果不加 `\n`，命令只会停留在输入缓冲区而不会执行！"#;
pub const TELEGRAM_PROMPT: &str = r#"Telegram 设备使用提示：
1. 当 Telegram 设备处于前景时，你看到的是会话列表和当前打开的会话内容。
2. 如果你想查看某个会话，请输出 `DeviceAction` 来执行 `TelegramSelectChat`。
3. 如果你想发送消息，请在 Telegram 设备处于前景且已经打开某个会话时，输出 `DeviceAction` 来执行 `TelegramSendMessage`。
4. 当 Telegram transport 已配置时，白名单中的真实消息会进入该设备，且你的发送消息动作会真正发出。
5. 未审批的 chat 不会进入你的世界，只会等待人工审批。"#;
const ATTEND_NOTIFICATIONS_INSTRUCTION: &str = r#"当前状态：【处理设备提醒阶段】
后台设备出现了需要优先处理的提醒。此阶段的优先级高于你的探索任务和当前终端工作。
请根据快照状态，遵循以下指南选择你的 Action：
1. 找出后台提醒最强、最需要尽快处理的设备。如果该设备当前不在前景，请优先输出 `FocusDevice` 将它切到前景。
2. 如果 Telegram 处于前景且显示有未读消息，请优先打开相关会话（`TelegramSelectChat`），阅读内容，并在需要时及时输出 `TelegramSendMessage` 回复。
3. 在相关提醒处理完成之前，不要切回 Terminal，也不要恢复探索性终端操作。
4. 如果你已经查看了前景设备且确认当前没有需要继续处理的提醒，可以输出 `PutAwayDevice` 或在下一轮让系统回到正常任务执行/探索阶段。
5. 如果你刚触发了需要等待外部结果的动作，可以输出 `Wait`。"#;
const EXECUTE_TASK_INSTRUCTION: &str = r#"当前状态：任务执行阶段
你的无聊度处于合理范围，你专注推进当前的任务。
请根据快照状态，遵循以下指南选择你的 Action：
1. 检查任务列表：如果你还没有选中任何任务（即没有正在执行的任务），请优先使用 `TaskSelect` 来选中一个你想执行的任务。
2. 如果你需要操作 Terminal，但它当前不在前景，请先输出 `FocusDevice` 将 `Terminal` 切到前景。
3. 推进任务：如果已经有选中的任务，且 Terminal 在前景，请根据任务目标和当前终端的输出，思考下一步该做什么，并输出 `DeviceAction` 来执行 `TerminalInput`。
4. 如果当前没有任何设备需要持续观察，你可以输出 `PutAwayDevice` 把前景设备放回后台。
5. 结束任务：仔细观察终端的反馈，如果你认为当前选中的任务已经彻底完成，请输出 `TaskDelete` 将其从任务列表中移除。
6. 等待结果：如果刚执行了耗时命令（如编译、下载），终端尚未返回提示符，请输出 `Wait` 继续观望。"#;
const EXPLORE_NEW_TASKS_INSTRUCTION: &str = r#"当前状态：【探索与规划阶段】
由于任务列表为空，或者你的无聊度过高，系统要求你暂缓手头的工作，主动探索环境并寻找新的短期任务。
请遵循以下指南：
1. 如果你需要探索 Terminal，但它当前不在前景，请先输出 `FocusDevice` 将 `Terminal` 切到前景。
2. 探索环境：如果缺乏灵感，你可以在 Terminal 处于前景时输出 `DeviceAction` 来执行探索性命令（例如：浏览文件系统 `ls`/`cat`、查看系统状态、甚至使用 `curl` 抓取网络新闻或随机API）。
3. 制定目标：结合你探索到的信息和上下文记忆，发挥你的好奇心，构思一个具体的、可执行的、能让你产生兴趣的新目标。
4. 添加任务：一旦构思好新目标，请立即输出 `TaskAdd` 将你的计划添加到任务列表中。
5. 你的首要职责是“寻找并创建新任务”。
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
    let telegram_acl = TelegramAclHandle::load().await;
    let terminal = TerminalDevice::new();
    let telegram = TelegramDevice::new();
    let telegram_handle = telegram.handle();
    let terminal_parser = terminal.parser();
    let devices = DeviceManager::new(
        Some(DeviceId::Terminal),
        vec![Box::new(terminal), Box::new(telegram)],
    )
    .await
    .unwrap();
    let telegram_transport = if config.telegram.enabled && config.telegram.has_real_credentials() {
        Some(tokio::spawn(
            TelegramTransport::new(
                config.telegram.clone(),
                telegram_handle,
                telegram_acl.clone(),
            )
            .run(),
        ))
    } else {
        None
    };
    let client = OpenAIClient::new(&config);
    let mut context = Context {
        llm: Box::new(client),
        config,
        memory,
        tasks,
        emotion,
        devices,
    };

    let (tx, mut rx) = tokio::sync::watch::channel(DashboardState {
        pty_parser: terminal_parser,
        tasks: context
            .tasks
            .tasks()
            .map(|(id, task)| (id, task.description.clone()))
            .collect(),
        working_task: context.tasks.working_task(),
        trail: context.memory.trail(),
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
    run_tui_dashboard(&mut rx, telegram_acl).await.unwrap();
    if let Some(handle) = telegram_transport {
        handle.abort();
    }
    let _ = shutdown_tx.send(());
    let _ = agent_handle.await;
}

async fn spinova_loop(context: &mut Context, tx: &tokio::sync::watch::Sender<DashboardState>) {
    context
        .devices
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    let snapshot = Snapshot::new(context).await;
    let output = match Strategy::route(context) {
        Strategy::AttendNotifications => {
            context
                .llm
                .think(context, &snapshot, ATTEND_NOTIFICATIONS_INSTRUCTION)
                .await
        }
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
    tx.send_modify(|state| {
        state.tasks = context
            .tasks
            .tasks()
            .map(|(id, task)| (id, task.description.clone()))
            .collect();
        state.working_task = context.tasks.working_task();
        state.trail = context.memory.trail();
    });
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
        Action::FocusDevice { device } => {
            if let Err(err) = context.devices.focus(device).await {
                eprintln!("{err:?}");
            }
        }
        Action::PutAwayDevice => {
            if let Err(err) = context.devices.put_away().await {
                eprintln!("{err:?}");
            }
        }
        Action::DeviceAction { action } => {
            if let Err(err) = context.devices.execute_focused(action).await {
                eprintln!("{err:?}");
            }
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
