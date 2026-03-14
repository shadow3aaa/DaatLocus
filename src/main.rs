mod config;
mod context;
mod core;
mod dashboard;
mod device;
mod embeding;
mod emotion;
mod memory;
mod obligation_queue;
mod obligations;
mod projects;
mod providers;
mod pty;
mod snapshot;
mod strategy;
mod system_info;
mod tasks;
mod telegram_acl;
mod telegram_device;
mod telegram_transport;
mod terminal_device;

use std::{env, path::PathBuf, time::Duration};

use miette::miette;
use uuid::Uuid;

use crate::{
    config::load_config,
    context::Context,
    core::Action,
    dashboard::{DashboardState, run_tui_dashboard},
    device::{DeviceId, DeviceManager},
    emotion::Emotion,
    memory::Memory,
    obligation_queue::ObligationQueue,
    obligations::{ObligationSource, ObligationStatus, Obligations},
    projects::{ProjectOrigin, Projects},
    providers::OpenAIClient,
    snapshot::Snapshot,
    strategy::Strategy,
    tasks::Tasks,
    telegram_acl::TelegramAclHandle,
    telegram_device::TelegramDevice,
    telegram_transport::TelegramTransport,
    terminal_device::TerminalDevice,
};

pub const SYSTEM_PROMPT: &str = r#"你叫 Spinova，一个自主智能体。
你没有实体，也没有要服务的用户。记忆流、义务列表、项目列表、下一步动作列表、设备外围感知和当前前景设备画面就是你的整个世界。
义务不等于任务；项目是跨多个步骤的承诺；下一步动作是当前可立即执行的一步。
你一次只能详细看见一个处于前景的设备，其它设备只能通过外围感知知道其存在与提醒。
在每次输出中，你必须把“观察到/学到的关键信息”与“决定采取的动作”区分开来。
`observation` 必须总结具体事实、报错、文件内容、消息内容或分析结论，而不是只写自己执行了什么操作。
凡是动作参数中的 `task_id`、`obligation_id`、`project_id`，都应优先填写快照列表中显示的 UUID；不要把中文描述、标题或摘要直接塞进这些字段。
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
5. 未审批的 chat 不会进入你的世界，只会等待人工审批。
6. 如果某个会话显示“待回复：是”，那意味着这条对话还没有完成处理。除非你已经发出合适的回复，否则不要把它当作已结束。"#;
const ATTEND_NOTIFICATIONS_INSTRUCTION: &str = r#"当前状态：【处理设备提醒阶段】
后台设备出现了需要优先处理的提醒。此阶段的优先级高于你的探索任务和当前终端工作。
请根据快照状态，遵循以下指南选择你的 Action：
1. 先查看义务列表，找出当前最需要处理的 `Pending` 义务，尤其是 `需回复=是` 的义务。
2. 后台提醒并不自动等于项目或下一步动作。先判断这条消息只是需要短答，还是会引出一个需要持续推进的工作。
3. 如果只是礼貌回复、状态说明或短答，不要创建项目或下一步动作；直接去处理设备本身即可。如果目标设备当前不在前景，先输出 `FocusDevice` 将它切到前景。
4. 如果 Telegram 处于前景且某个会话显示“待回复：是”，请优先打开相关会话（`TelegramSelectChat`），阅读内容，并及时输出 `TelegramSendMessage` 回复。对方若直接询问你在做什么，应先正面回答。
5. 如果你只是礼貌回复、状态说明或短答，不要升级成项目；在你确认已经妥善回复、且没有后续持续工作时，应使用 `ObligationSatisfy` 将这条义务关单。
6. 只有当你明确接受某项义务并承诺后续会持续推进时，才使用 `CommitToProject` 将它升级为项目。这个动作会原子地创建项目、可选创建第一条下一步动作，并可选发送确认消息。
7. 如果你在对外回复中表达了“我会调查 / 我会去做 / 稍后给你结果 / 我来处理”等未来承诺，就必须使用 `CommitToProject`，不能只发送一条确认消息。
8. 如果你只是接受了工作但还没给出下一步动作，可以在 `CommitToProject` 的 `initial_next_action` 中填写第一步；如果只是短答，则不要升级成项目。
9. 当你后来追加新的下一步动作，而且它明显属于某个项目时，请在 `TaskAdd.project_id` 中填写对应项目 id，不要制造悬空动作。
10. 在相关提醒处理完成之前，不要切回 Terminal，也不要恢复探索性终端操作。
11. 只有当消息已经得到合适回复，或你明确判断当前无需回复时，才可以输出 `PutAwayDevice` 或在下一轮回到正常任务执行/探索阶段。
12. 如果你刚发出消息，正在等待 transport 结果，或正在等待对方继续发言，可以输出 `Wait`。"#;
const EXECUTE_TASK_INSTRUCTION: &str = r#"当前状态：下一步动作执行阶段
你的无聊度处于合理范围，你专注推进当前已存在的下一步动作。
请根据快照状态，遵循以下指南选择你的 Action：
1. 检查下一步动作列表：如果你还没有选中任何动作（即没有正在执行的动作），请优先使用 `TaskSelect` 来选中一个你想执行的动作。
2. 下一步动作并不一定属于 Terminal。先读懂当前选中动作的内容，判断它需要哪个设备；如果需要操作某个设备而它当前不在前景，请先输出 `FocusDevice` 切过去。
3. 如果当前动作属于某个项目，请确保它确实在推进那个项目，而不是偏离目标。
4. 如果当前动作是回复 Telegram 消息，优先保持 Telegram 在前景，打开对应会话并回复；不要因为旧习惯切回 Terminal。
5. 推进动作：只有当所需设备已经在前景时，才输出相应的 `DeviceAction` 来继续推进。
6. 如果你发现还缺少别的下一步动作，而且它明确属于某个项目，请用 `TaskAdd` 并填入 `project_id`。
7. 如果当前没有任何设备需要持续观察，你可以输出 `PutAwayDevice` 把前景设备放回后台。
8. 当你判断某个项目的成功标准已经满足时，应优先输出 `ProjectComplete`，而不是只删除一条动作。项目完成会自动收尾相关动作，并在需要时生成回报义务。
9. 如果你认为当前选中的动作已经彻底完成，但所属项目还未完成，请输出 `TaskDelete` 将其从下一步动作列表中移除。
10. 如果某条义务其实已经被你妥善处理完，例如你刚完成最终回报、且不再需要继续跟进，请在合适的一轮输出 `ObligationSatisfy` 关闭它。
11. 等待结果：如果刚执行了耗时命令，或你刚发送了 Telegram 消息正在等待结果/回复，可以输出 `Wait` 继续观望。"#;
const PLAN_FROM_PROJECT_INSTRUCTION: &str = r#"当前状态：【项目规划阶段】
当前没有可执行的下一步动作，但仍然存在活跃项目。你现在的职责不是探索新事物，而是先为已有项目规划出下一步。
请根据快照状态，遵循以下指南选择你的 Action：
1. 查看项目列表，找出最值得优先推进的 `Active` 项目。
2. 为这个项目生成一条具体、可执行、足够小的下一步动作，并用 `TaskAdd` 添加到下一步动作列表。
3. 这条新动作若明确属于某个项目，必须在 `TaskAdd.project_id` 中填写对应项目 id。
4. 下一步动作应尽量直接可执行，例如“切到 Telegram 回复已接受该请求”或“在 Terminal 查看某目录结构”，避免空泛表述。
5. 如果某个项目当前处于外部等待状态，且确实还不适合生成新的动作，可以输出 `Wait`，但不要因此转去探索无关新任务。
6. 如果某个项目其实已经达到成功标准，不要继续规划新动作，应直接输出 `ProjectComplete`。
7. 只有当你确认当前没有任何值得推进的活跃项目时，下一轮系统才应该考虑回到探索。"#;
const EXPLORE_NEW_TASKS_INSTRUCTION: &str = r#"当前状态：【探索与规划阶段】
当前没有待处理义务、没有可执行的下一步动作、也没有需要先规划的活跃项目，或者你的无聊度过高。系统要求你主动探索环境并寻找新的短期任务。
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
    let obligations = Obligations::new().await;
    let obligation_queue = ObligationQueue::new();
    let projects = Projects::new().await;
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
                obligation_queue.clone(),
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
        obligations,
        obligation_queue,
        projects,
        tasks,
        emotion,
        devices,
    };

    let (tx, mut rx) = tokio::sync::watch::channel(DashboardState {
        pty_parser: terminal_parser,
        obligations: render_obligations_for_dashboard(&context),
        projects: render_projects_for_dashboard(&context),
        tasks: context
            .tasks
            .tasks()
            .map(|(id, task)| (id, render_task_for_dashboard(task, &context)))
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
    if context.obligation_queue.apply_to(&mut context.obligations) {
        sync_dashboard_state(context, tx);
    }
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
        Strategy::PlanFromProject => {
            context
                .llm
                .think(context, &snapshot, PLAN_FROM_PROJECT_INSTRUCTION)
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
        .record(output.current_doing, output.observation, output.description)
        .await;
    execute_action(context, output.action).await;
    sync_dashboard_state(context, tx);
}

async fn execute_action(context: &mut Context, action: Action) {
    match action {
        Action::TaskAdd {
            description,
            project_id,
        } => {
            let project_id = project_id
                .as_deref()
                .map(|project_id| resolve_project_reference(context, project_id))
                .transpose();
            match project_id {
                Ok(project_id) => {
                    context.tasks.add_task_with_project(description, project_id);
                }
                Err(err) => eprintln!("{err:?}"),
            }
        }
        Action::TaskDelete { task_id } => match resolve_task_reference(context, &task_id) {
            Ok(id) => {
                context.tasks.delete_task(id);
            }
            Err(err) => eprintln!("{err:?}"),
        },
        Action::TaskSelect { task_id } => match resolve_task_reference(context, &task_id) {
            Ok(id) => {
                context.tasks.select_working_task(id);
            }
            Err(err) => eprintln!("{err:?}"),
        },
        Action::ObligationSatisfy { obligation_id } => {
            match resolve_obligation_reference(context, &obligation_id) {
                Ok(id) => {
                    context
                        .obligations
                        .set_status(id, ObligationStatus::Satisfied);
                }
                Err(err) => eprintln!("{err:?}"),
            }
        }
        Action::CommitToProject {
            obligation_id,
            title,
            success_criteria,
            initial_next_action,
            acknowledgment,
        } => {
            if let Err(err) = execute_commit_to_project(
                context,
                &obligation_id,
                title,
                success_criteria,
                initial_next_action,
                acknowledgment,
            )
            .await
            {
                eprintln!("{err:?}");
            }
        }
        Action::ProjectComplete {
            project_id,
            summary,
        } => {
            if let Err(err) = execute_project_complete(context, &project_id, summary) {
                eprintln!("{err:?}");
            }
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

fn sync_dashboard_state(context: &Context, tx: &tokio::sync::watch::Sender<DashboardState>) {
    tx.send_modify(|state| {
        state.obligations = render_obligations_for_dashboard(context);
        state.projects = render_projects_for_dashboard(context);
        state.tasks = context
            .tasks
            .tasks()
            .map(|(id, task)| (id, render_task_for_dashboard(task, context)))
            .collect();
        state.working_task = context.tasks.working_task();
        state.trail = context.memory.trail();
    });
}

fn render_obligations_for_dashboard(context: &Context) -> Vec<String> {
    let mut obligations = context.obligations.obligations().collect::<Vec<_>>();
    obligations.sort_by_key(|(id, _)| id.to_string());
    obligations
        .into_iter()
        .map(|(_, obligation)| {
            format!(
                "[{} / {} / reply={}] {}",
                obligation.status,
                obligation.urgency,
                if obligation.requires_reply {
                    "yes"
                } else {
                    "no"
                },
                obligation.summary
            )
        })
        .collect()
}

fn render_projects_for_dashboard(context: &Context) -> Vec<String> {
    let mut projects = context.projects.projects().collect::<Vec<_>>();
    projects.sort_by_key(|(id, _)| id.to_string());
    projects
        .into_iter()
        .map(|(_, project)| {
            format!(
                "[{} / {}] {}",
                project.status, project.origin, project.title
            )
        })
        .collect()
}

fn render_task_for_dashboard(task: &crate::tasks::Task, context: &Context) -> String {
    let Some(project_id) = task.project_id else {
        return task.description.clone();
    };
    let project_title = context
        .projects
        .projects()
        .find(|(id, _)| *id == project_id)
        .map(|(_, project)| project.title.clone())
        .unwrap_or_else(|| project_id.to_string());
    format!("{} [project: {}]", task.description, project_title)
}

fn normalize_reference(reference: &str) -> String {
    reference.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve_reference(
    kind: &str,
    reference: &str,
    candidates: Vec<(Uuid, String)>,
) -> miette::Result<Uuid> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(miette!("{kind} reference is empty"));
    }

    if let Ok(id) = Uuid::parse_str(reference) {
        if candidates
            .iter()
            .any(|(candidate_id, _)| *candidate_id == id)
        {
            return Ok(id);
        }
        return Err(miette!("unknown {kind} id: {reference}"));
    }

    let normalized_reference = normalize_reference(reference);
    let exact_matches = candidates
        .iter()
        .filter_map(|(id, label)| {
            (normalize_reference(label) == normalized_reference).then_some(*id)
        })
        .collect::<Vec<_>>();
    if exact_matches.len() == 1 {
        return Ok(exact_matches[0]);
    }
    if exact_matches.len() > 1 {
        return Err(miette!(
            "ambiguous {kind} reference `{reference}`: matched {} items by exact description/title",
            exact_matches.len()
        ));
    }

    let fuzzy_matches = candidates
        .iter()
        .filter_map(|(id, label)| {
            let normalized_label = normalize_reference(label);
            (normalized_label.contains(&normalized_reference)
                || normalized_reference.contains(&normalized_label))
            .then_some(*id)
        })
        .collect::<Vec<_>>();
    if fuzzy_matches.len() == 1 {
        return Ok(fuzzy_matches[0]);
    }
    if fuzzy_matches.len() > 1 {
        return Err(miette!(
            "ambiguous {kind} reference `{reference}`: matched {} items fuzzily",
            fuzzy_matches.len()
        ));
    }

    Err(miette!(
        "invalid {kind} reference `{reference}`: expected a UUID from the snapshot, or a unique matching title/summary"
    ))
}

fn resolve_task_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_reference(
        "task",
        reference,
        context
            .tasks
            .tasks()
            .map(|(id, task)| (id, task.description.clone()))
            .collect(),
    )
}

fn resolve_obligation_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_reference(
        "obligation",
        reference,
        context
            .obligations
            .obligations()
            .map(|(id, obligation)| (id, obligation.summary.clone()))
            .collect(),
    )
}

fn resolve_project_reference(context: &Context, reference: &str) -> miette::Result<Uuid> {
    resolve_reference(
        "project",
        reference,
        context
            .projects
            .projects()
            .map(|(id, project)| (id, project.title.clone()))
            .collect(),
    )
}

async fn execute_commit_to_project(
    context: &mut Context,
    obligation_id: &str,
    title: String,
    success_criteria: String,
    initial_next_action: Option<String>,
    acknowledgment: Option<String>,
) -> miette::Result<()> {
    let obligation_id = resolve_obligation_reference(context, obligation_id)?;
    let Some(obligation) = context.obligations.get(obligation_id).cloned() else {
        return Err(miette!("unknown obligation: {obligation_id}"));
    };

    let project_id = context.projects.add(
        title,
        project_origin_from(obligation.source),
        success_criteria,
        obligation.reply_target.clone(),
    );
    context.obligations.link_project(obligation_id, project_id);

    if let Some(next_action) = initial_next_action.map(|s| s.trim().to_string()) {
        if !next_action.is_empty() {
            let task_id = context
                .tasks
                .add_task_with_project(next_action, Some(project_id));
            context.tasks.select_working_task(task_id);
        }
    }

    if let Some(ack) = acknowledgment.map(|s| s.trim().to_string()) {
        if !ack.is_empty() {
            enqueue_obligation_acknowledgment(context, obligation_id, &obligation, ack).await?;
            return Ok(());
        }
    }

    if obligation.requires_reply {
        context
            .obligations
            .set_status(obligation_id, ObligationStatus::Seen);
    } else {
        context
            .obligations
            .set_status(obligation_id, ObligationStatus::Satisfied);
    }
    Ok(())
}

async fn enqueue_obligation_acknowledgment(
    context: &mut Context,
    obligation_id: Uuid,
    obligation: &crate::obligations::Obligation,
    acknowledgment: String,
) -> miette::Result<()> {
    let Some(target) = obligation.reply_target.clone() else {
        return Err(miette!("obligation {obligation_id} has no reply target"));
    };

    match target.device {
        DeviceId::Telegram => {
            context.devices.focus(DeviceId::Telegram).await?;
            context
                .devices
                .execute_focused(crate::device::DeviceAction::TelegramSelectChat {
                    chat_id: target.target,
                })
                .await?;
            context
                .devices
                .execute_focused(crate::device::DeviceAction::TelegramSendMessage {
                    text: acknowledgment,
                })
                .await?;
            context
                .obligations
                .set_status(obligation_id, ObligationStatus::Seen);
            Ok(())
        }
        DeviceId::Terminal => Err(miette!(
            "terminal obligations do not support external acknowledgment"
        )),
    }
}

fn project_origin_from(source: ObligationSource) -> ProjectOrigin {
    match source {
        ObligationSource::Telegram => ProjectOrigin::Telegram,
        ObligationSource::Terminal => ProjectOrigin::Terminal,
        ObligationSource::System => ProjectOrigin::System,
    }
}

fn execute_project_complete(
    context: &mut Context,
    project_id: &str,
    summary: String,
) -> miette::Result<()> {
    let project_id = resolve_project_reference(context, project_id)?;
    let Some(project) = context.projects.get(project_id).cloned() else {
        return Err(miette!("unknown project: {project_id}"));
    };

    context
        .projects
        .set_status(project_id, crate::projects::ProjectStatus::Completed);
    context.tasks.delete_tasks_for_project(project_id);

    if let Some(target) = project.report_back_to {
        context.obligations.add(
            match target.device {
                DeviceId::Telegram => ObligationSource::Telegram,
                DeviceId::Terminal => ObligationSource::Terminal,
            },
            format!(
                "把项目《{}》的结果回复给对方：{}",
                project.title,
                summary.trim()
            ),
            true,
            crate::obligations::Urgency::High,
            Some(project_id),
            Some(target),
        );
    }

    Ok(())
}

pub async fn get_spinova_home() -> PathBuf {
    let path = env::home_dir().unwrap().join(".spinova");
    if !path.exists() {
        tokio::fs::create_dir_all(&path).await.unwrap();
    }
    path
}
