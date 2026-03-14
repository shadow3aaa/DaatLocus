mod config;
mod context;
mod core;
mod dashboard;
mod device;
mod embeding;
mod emotion;
mod memory;
mod obligations;
mod projects;
mod providers;
mod pty;
mod reasoning;
mod snapshot;
mod strategy;
mod system_info;
mod tasks;
mod telegram_acl;
mod telegram_device;
mod telegram_transport;
mod terminal_device;

use std::{env, path::PathBuf, time::Duration};

use chrono::{Local, TimeZone};
use miette::miette;
use uuid::Uuid;

use crate::{
    config::load_config,
    context::Context,
    core::{Action, TelegramResolution},
    dashboard::{DashboardState, DashboardTaskEntry, run_tui_dashboard},
    device::{DeviceId, DeviceManager},
    emotion::Emotion,
    memory::Memory,
    obligations::{ObligationSource, ObligationStatus, Obligations},
    projects::{ProjectOrigin, Projects},
    providers::OpenAIClient,
    reasoning::{
        compiled::CompiledPromptStore,
        eval::run_reasoning_eval,
        optimize::ensure_reasoning_compiled,
        optimize::run_reasoning_optimize,
        programs::action_phase::{ActionPhase, ActionPhaseProgram},
        programs::resolve_telegram::{
            ResolveTelegramChatProgram, ResolveTelegramProgramAction, ResolveTelegramProgramOutput,
        },
        render::openai_tools::OpenAIToolRenderer,
        runtime::execute_program,
    },
    snapshot::Snapshot,
    strategy::Strategy,
    tasks::Tasks,
    telegram_acl::TelegramAclHandle,
    telegram_device::TelegramDevice,
    telegram_transport::TelegramTransport,
    terminal_device::TerminalDevice,
};

#[tokio::main]
async fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let config = match load_config().await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    if is_reasoning_eval_command(&args) {
        let context = build_eval_context(config).await;
        match run_reasoning_eval(&context).await {
            Ok(results) => {
                print_reasoning_eval_results(&results);
                context.shutdown().await;
                return;
            }
            Err(err) => {
                eprintln!("{err:?}");
                context.shutdown().await;
                std::process::exit(1);
            }
        }
    }

    if is_reasoning_optimize_command(&args) {
        let context = build_eval_context(config).await;
        match run_reasoning_optimize(&context).await {
            Ok(results) => {
                print_reasoning_optimization_results(&results);
                context.shutdown().await;
                return;
            }
            Err(err) => {
                eprintln!("{err:?}");
                context.shutdown().await;
                std::process::exit(1);
            }
        }
    }

    let compiled_prompts = match prepare_compiled_prompts(&config).await {
        Ok(store) => store,
        Err(err) => {
            eprintln!("{err:?}");
            std::process::exit(1);
        }
    };

    let memory = Memory::new().await;
    let obligations = Obligations::new().await;
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
                telegram_handle.clone(),
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
        obligations,
        projects,
        tasks,
        emotion,
        devices,
        telegram: telegram_handle,
        compiled_prompts,
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
        latest_trail: context.memory.trail().last().cloned(),
        last_cycle_elapsed_ms: None,
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

fn is_reasoning_eval_command(args: &[String]) -> bool {
    matches!(args, [command, target] if command == "eval" && target == "reasoning")
        || matches!(args, [command] if command == "eval-reasoning")
}

fn is_reasoning_optimize_command(args: &[String]) -> bool {
    matches!(args, [command, target] if command == "optimize" && target == "reasoning")
        || matches!(args, [command] if command == "optimize-reasoning")
}

async fn build_eval_context(config: crate::config::Config) -> Context {
    let memory = Memory::new().await;
    let obligations = Obligations::new().await;
    let projects = Projects::new().await;
    let tasks = Tasks::new().await;
    let emotion = Emotion::new().await;
    let terminal = TerminalDevice::new();
    let telegram = TelegramDevice::new();
    let telegram_handle = telegram.handle();
    let devices = DeviceManager::new(
        Some(DeviceId::Terminal),
        vec![Box::new(terminal), Box::new(telegram)],
    )
    .await
    .unwrap();
    let client = OpenAIClient::new(&config);

    Context {
        llm: Box::new(client),
        config,
        memory,
        obligations,
        projects,
        tasks,
        emotion,
        devices,
        telegram: telegram_handle,
        compiled_prompts: CompiledPromptStore::empty(),
    }
}

async fn prepare_compiled_prompts(
    config: &crate::config::Config,
) -> miette::Result<CompiledPromptStore> {
    let context = build_eval_context(config.clone()).await;
    let compiled = ensure_reasoning_compiled(&context).await;
    context.shutdown().await;
    compiled.map(CompiledPromptStore::from_entries)
}

fn print_reasoning_eval_results(results: &[crate::reasoning::eval::EvalCaseResult]) {
    let passed = results.iter().filter(|result| result.passed).count();
    let failed = results.len().saturating_sub(passed);
    println!(
        "reasoning eval: total={} passed={} failed={}",
        results.len(),
        passed,
        failed
    );
    for result in results {
        let status = if result.passed { "PASS" } else { "FAIL" };
        println!(
            "[{}] {} / {} - {}",
            status, result.suite, result.case_name, result.detail
        );
    }
}

fn print_reasoning_optimization_results(
    results: &[crate::reasoning::optimizer::OptimizationResult],
) {
    println!("reasoning optimize:");
    for result in results {
        println!(
            "- suite={} best_candidate={} score={}/{}",
            result.suite, result.best_candidate, result.score, result.total_cases
        );
    }
}

async fn spinova_loop(context: &mut Context, tx: &tokio::sync::watch::Sender<DashboardState>) {
    let cycle_started_at = std::time::Instant::now();
    context
        .devices
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    let snapshot = Snapshot::new(context).await;
    let renderer = OpenAIToolRenderer;
    let strategy = Strategy::route(context);
    let output = match strategy {
        Strategy::AttendNotifications => {
            if context.telegram.has_pending_resolution() {
                let program = ResolveTelegramChatProgram;
                match execute_program(
                    context.llm.as_ref(),
                    context,
                    &snapshot,
                    &renderer,
                    &program,
                )
                .await
                {
                    Ok(program_output) => translate_resolve_telegram_output(program_output),
                    Err(err) => {
                        eprintln!("{err:?}");
                        crate::core::Output {
                            observation: format!("ResolveTelegramChatProgram 执行失败：{err}"),
                            description: "结构化 Telegram 消息处理失败，当前保守等待。".to_string(),
                            current_doing: "等待 Telegram 消息处理程序恢复".to_string(),
                            action: Action::Wait,
                        }
                    }
                }
            } else {
                let program = ActionPhaseProgram::new(ActionPhase::AttendNotifications);
                execute_program(
                    context.llm.as_ref(),
                    context,
                    &snapshot,
                    &renderer,
                    &program,
                )
                .await
                .unwrap_or_else(|err| crate::core::Output {
                    observation: format!("AttendNotifications program 执行失败：{err}"),
                    description: "处理提醒阶段的结构化决策失败，当前保守等待。".to_string(),
                    current_doing: "等待提醒处理程序恢复".to_string(),
                    action: Action::Wait,
                })
            }
        }
        Strategy::ExecuteTask => {
            let program = ActionPhaseProgram::new(ActionPhase::ExecuteTask);
            execute_program(
                context.llm.as_ref(),
                context,
                &snapshot,
                &renderer,
                &program,
            )
            .await
            .unwrap_or_else(|err| crate::core::Output {
                observation: format!("ExecuteTask program 执行失败：{err}"),
                description: "下一步动作执行阶段的结构化决策失败，当前保守等待。".to_string(),
                current_doing: "等待动作执行程序恢复".to_string(),
                action: Action::Wait,
            })
        }
        Strategy::PlanFromProject => {
            let program = ActionPhaseProgram::new(ActionPhase::PlanFromProject);
            execute_program(
                context.llm.as_ref(),
                context,
                &snapshot,
                &renderer,
                &program,
            )
            .await
            .unwrap_or_else(|err| crate::core::Output {
                observation: format!("PlanFromProject program 执行失败：{err}"),
                description: "项目规划阶段的结构化决策失败，当前保守等待。".to_string(),
                current_doing: "等待项目规划程序恢复".to_string(),
                action: Action::Wait,
            })
        }
        Strategy::ExploreNewTasks => {
            let program = ActionPhaseProgram::new(ActionPhase::ExploreNewTasks);
            execute_program(
                context.llm.as_ref(),
                context,
                &snapshot,
                &renderer,
                &program,
            )
            .await
            .unwrap_or_else(|err| crate::core::Output {
                observation: format!("ExploreNewTasks program 执行失败：{err}"),
                description: "探索阶段的结构化决策失败，当前保守等待。".to_string(),
                current_doing: "等待探索程序恢复".to_string(),
                action: Action::Wait,
            })
        }
    };
    context
        .memory
        .record(output.current_doing, output.observation, output.description)
        .await;
    execute_action(context, output.action).await;
    if matches!(strategy, Strategy::ExecuteTask) {
        context.tasks.touch_working_task();
    }
    sync_dashboard_state(context, tx, Some(cycle_started_at.elapsed().as_millis()));
}

fn translate_resolve_telegram_output(
    program_output: ResolveTelegramProgramOutput,
) -> crate::core::Output {
    crate::core::Output {
        observation: program_output.observation,
        description: program_output.description,
        current_doing: program_output.current_doing,
        action: match program_output.action {
            ResolveTelegramProgramAction::FocusTelegram => Action::FocusDevice {
                device: DeviceId::Telegram,
            },
            ResolveTelegramProgramAction::OpenChat { chat_id } => Action::DeviceAction {
                action: crate::device::DeviceAction::TelegramSelectChat { chat_id },
            },
            ResolveTelegramProgramAction::ResolveChat {
                chat_id,
                resolution,
            } => Action::ResolveTelegramChat {
                chat_id,
                resolution,
            },
            ResolveTelegramProgramAction::ReplyInCurrentChat { text } => Action::DeviceAction {
                action: crate::device::DeviceAction::TelegramSendMessage { text },
            },
            ResolveTelegramProgramAction::Wait => Action::Wait,
        },
    }
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
        Action::ResolveTelegramChat {
            chat_id,
            resolution,
        } => {
            if let Err(err) = execute_resolve_telegram_chat(context, &chat_id, resolution).await {
                eprintln!("{err:?}");
            }
        }
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

fn sync_dashboard_state(
    context: &Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    last_cycle_elapsed_ms: Option<u128>,
) {
    tx.send_modify(|state| {
        state.obligations = render_obligations_for_dashboard(context);
        state.projects = render_projects_for_dashboard(context);
        state.tasks = context
            .tasks
            .tasks()
            .map(|(id, task)| (id, render_task_for_dashboard(task, context)))
            .collect();
        state.working_task = context.tasks.working_task();
        state.latest_trail = context.memory.trail().last().cloned();
        state.last_cycle_elapsed_ms = last_cycle_elapsed_ms;
    });
}

fn render_obligations_for_dashboard(context: &Context) -> Vec<String> {
    let mut obligations = context.obligations.active_obligations().collect::<Vec<_>>();
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

fn render_task_for_dashboard(task: &crate::tasks::Task, context: &Context) -> DashboardTaskEntry {
    let description_tail = truncate_from_left(&task.description, 42);
    let last_touched = format_last_touched(task.last_touched_at_ms);

    let display = match task.project_id {
        Some(project_id) => {
            let project_title = context
                .projects
                .projects()
                .find(|(id, _)| *id == project_id)
                .map(|(_, project)| project.title.clone())
                .unwrap_or_else(|| project_id.to_string());
            format!(
                "{description_tail}\n  上次处理: {last_touched} | 项目: {}",
                truncate_from_left(&project_title, 18)
            )
        }
        None => format!("{description_tail}\n  上次处理: {last_touched}"),
    };

    DashboardTaskEntry {
        display,
        last_touched_at_ms: task.last_touched_at_ms,
    }
}

fn format_last_touched(last_touched_at_ms: Option<i64>) -> String {
    let Some(timestamp_ms) = last_touched_at_ms else {
        return "未处理".to_string();
    };

    let Some(datetime) = Local.timestamp_millis_opt(timestamp_ms).single() else {
        return "时间无效".to_string();
    };

    let now = Local::now();
    if now.date_naive() == datetime.date_naive() {
        datetime.format("%H:%M:%S").to_string()
    } else {
        datetime.format("%m-%d %H:%M").to_string()
    }
}

fn truncate_from_left(text: &str, max_chars: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return text.to_string();
    }

    let tail = chars[chars.len().saturating_sub(max_chars - 1)..]
        .iter()
        .collect::<String>();
    format!("…{tail}")
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

fn resolve_string_reference(
    kind: &str,
    reference: &str,
    candidates: Vec<(String, String)>,
) -> miette::Result<String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(miette!("{kind} reference is empty"));
    }

    if let Some((id, _)) = candidates
        .iter()
        .find(|(candidate_id, _)| candidate_id == reference)
    {
        return Ok(id.clone());
    }

    let normalized_reference = normalize_reference(reference);
    let exact_matches = candidates
        .iter()
        .filter_map(|(id, label)| {
            (normalize_reference(label) == normalized_reference).then_some(id.clone())
        })
        .collect::<Vec<_>>();
    if exact_matches.len() == 1 {
        return Ok(exact_matches[0].clone());
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
            .then_some(id.clone())
        })
        .collect::<Vec<_>>();
    if fuzzy_matches.len() == 1 {
        return Ok(fuzzy_matches[0].clone());
    }
    if fuzzy_matches.len() > 1 {
        return Err(miette!(
            "ambiguous {kind} reference `{reference}`: matched {} items fuzzily",
            fuzzy_matches.len()
        ));
    }

    Err(miette!(
        "invalid {kind} reference `{reference}`: expected a chat id from the device view, or a unique matching title"
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

fn resolve_telegram_chat_reference(context: &Context, reference: &str) -> miette::Result<String> {
    resolve_string_reference("telegram chat", reference, context.telegram.chat_refs())
}

fn trim_optional_field(value: Option<String>) -> Option<String> {
    value.and_then(trim_required_field)
}

fn trim_required_field(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn require_field(value: String, field_name: &str) -> miette::Result<String> {
    trim_required_field(value).ok_or_else(|| miette!("missing required field: {field_name}"))
}

async fn send_telegram_message(
    context: &mut Context,
    chat_id: &str,
    text: String,
) -> miette::Result<()> {
    context.devices.focus(DeviceId::Telegram).await?;
    context
        .devices
        .execute_focused(crate::device::DeviceAction::TelegramSelectChat {
            chat_id: chat_id.to_string(),
        })
        .await?;
    context
        .devices
        .execute_focused(crate::device::DeviceAction::TelegramSendMessage { text })
        .await?;
    Ok(())
}

async fn execute_resolve_telegram_chat(
    context: &mut Context,
    chat_reference: &str,
    resolution: TelegramResolution,
) -> miette::Result<()> {
    let chat_id = resolve_telegram_chat_reference(context, chat_reference)?;

    match resolution {
        TelegramResolution::ReplyOnly { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::AskClarification { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::Decline { reply } => {
            let reply = require_field(reply, "reply")?;
            send_telegram_message(context, &chat_id, reply).await?;
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::NoReplyNeeded => {
            context.telegram.resolve_chat(&chat_id, Some(false))?;
        }
        TelegramResolution::AcceptAsProject {
            reply,
            project_title,
            success_criteria,
            first_next_action,
        } => {
            let project_title = require_field(project_title, "project_title")?;
            let success_criteria = require_field(success_criteria, "success_criteria")?;
            let project_id = context.projects.add(
                project_title,
                ProjectOrigin::Telegram,
                success_criteria,
                Some(crate::projects::ReportTarget {
                    device: DeviceId::Telegram,
                    target: chat_id.clone(),
                }),
            );

            if let Some(next_action) = trim_optional_field(first_next_action) {
                let task_id = context
                    .tasks
                    .add_task_with_project(next_action, Some(project_id));
                context.tasks.select_working_task(task_id);
            }

            if let Some(reply) = trim_optional_field(reply) {
                send_telegram_message(context, &chat_id, reply).await?;
                context.telegram.resolve_chat(&chat_id, Some(false))?;
            } else {
                context.telegram.resolve_chat(&chat_id, None)?;
            }
        }
    }

    Ok(())
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
