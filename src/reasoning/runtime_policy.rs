use crate::{
    context::Context,
    core::{Effect, Output},
    device::DeviceId,
    reasoning::{
        programs::{
            attend_notifications::AttendNotificationsProgram,
            execute_task::ExecuteTaskProgram,
            explore_new_tasks::ExploreNewTasksProgram,
            plan_from_project::PlanFromProjectProgram,
            resolve_telegram::{
                ResolveTelegramChatProgram, ResolveTelegramProgramAction,
                ResolveTelegramProgramOutput,
            },
            terminal_next_step::TerminalNextStepProgram,
        },
        render::Renderer,
        runtime::execute_program,
    },
    snapshot::Snapshot,
};

pub struct RuntimePolicyProgram;

pub struct RuntimePolicyOutcome {
    pub output: Output,
    pub touched_working_task: bool,
}

impl RuntimePolicyProgram {
    const BOREDOM_THRESHOLD: f32 = 0.8;

    pub async fn run_once<R: Renderer>(
        &self,
        context: &Context,
        snapshot: &Snapshot,
        renderer: &R,
        work_phase: &str,
    ) -> RuntimePolicyOutcome {
        if context.devices.requires_attention() || context.obligations.has_pending() {
            return RuntimePolicyOutcome {
                output: self.run_attend_notifications(context, snapshot, renderer).await,
                touched_working_task: false,
            };
        }

        if !context.tasks.is_empty() && !self.should_explore_instead_of_execute(context) {
            return RuntimePolicyOutcome {
                output: self
                    .run_execute_task(context, snapshot, renderer, work_phase)
                    .await,
                touched_working_task: true,
            };
        }

        if context.projects.has_active() && context.tasks.is_empty() {
            return RuntimePolicyOutcome {
                output: self
                    .run_program(context, snapshot, renderer, &PlanFromProjectProgram)
                    .await
                    .unwrap_or_else(|err| Output {
                        observation: format!("PlanFromProject program 执行失败：{err}"),
                        description: "项目规划阶段的结构化决策失败，当前保守等待。".to_string(),
                        current_doing: "等待项目规划程序恢复".to_string(),
                        effect: Effect::Wait,
                    }),
                touched_working_task: false,
            };
        }

        RuntimePolicyOutcome {
            output: self
                .run_program(context, snapshot, renderer, &ExploreNewTasksProgram)
                .await
                .unwrap_or_else(|err| Output {
                    observation: format!("ExploreNewTasks program 执行失败：{err}"),
                    description: "探索阶段的结构化决策失败，当前保守等待。".to_string(),
                    current_doing: "等待探索程序恢复".to_string(),
                    effect: Effect::Wait,
                }),
            touched_working_task: false,
        }
    }

    fn should_explore_instead_of_execute(&self, context: &Context) -> bool {
        !context.tasks.is_empty()
            && context.emotion.boredom > Self::BOREDOM_THRESHOLD
            && !context.projects.has_active()
    }

    async fn run_attend_notifications<R: Renderer>(
        &self,
        context: &Context,
        snapshot: &Snapshot,
        renderer: &R,
    ) -> Output {
        if context.telegram.has_pending_resolution() {
            let program = ResolveTelegramChatProgram;
            match execute_program(context.llm.as_ref(), context, snapshot, renderer, &program).await
            {
                Ok(program_output) => translate_resolve_telegram_output(program_output),
                Err(err) => Output {
                    observation: format!("ResolveTelegramChatProgram 执行失败：{err}"),
                    description: "结构化 Telegram 消息处理失败，当前保守等待。".to_string(),
                    current_doing: "等待 Telegram 消息处理程序恢复".to_string(),
                    effect: Effect::Wait,
                },
            }
        } else {
            self.run_program(context, snapshot, renderer, &AttendNotificationsProgram)
            .await
            .unwrap_or_else(|err| Output {
                observation: format!("AttendNotifications program 执行失败：{err}"),
                description: "处理提醒阶段的结构化决策失败，当前保守等待。".to_string(),
                current_doing: "等待提醒处理程序恢复".to_string(),
                effect: Effect::Wait,
            })
        }
    }

    async fn run_execute_task<R: Renderer>(
        &self,
        context: &Context,
        snapshot: &Snapshot,
        renderer: &R,
        work_phase: &str,
    ) -> Output {
        if context.devices.focused() == Some(DeviceId::Terminal) {
            let program = TerminalNextStepProgram {
                work_phase: work_phase.to_string(),
            };
            execute_program(context.llm.as_ref(), context, snapshot, renderer, &program)
                .await
                .unwrap_or_else(|err| Output {
                    observation: format!("TerminalNextStep program 执行失败：{err}"),
                    description: "终端下一步决策失败，当前保守等待。".to_string(),
                    current_doing: "等待终端决策程序恢复".to_string(),
                    effect: Effect::Wait,
                })
        } else {
            self.run_program(context, snapshot, renderer, &ExecuteTaskProgram)
                .await
                .unwrap_or_else(|err| Output {
                    observation: format!("ExecuteTask program 执行失败：{err}"),
                    description: "下一步动作执行阶段的结构化决策失败，当前保守等待。".to_string(),
                    current_doing: "等待动作执行程序恢复".to_string(),
                    effect: Effect::Wait,
                })
        }
    }

    async fn run_program<R: Renderer, P: crate::reasoning::program::Program<Output = Output>>(
        &self,
        context: &Context,
        snapshot: &Snapshot,
        renderer: &R,
        program: &P,
    ) -> miette::Result<Output> {
        execute_program(context.llm.as_ref(), context, snapshot, renderer, program).await
    }
}

fn translate_resolve_telegram_output(program_output: ResolveTelegramProgramOutput) -> Output {
    Output {
        observation: program_output.observation,
        description: program_output.description,
        current_doing: program_output.current_doing,
        effect: match program_output.action {
            ResolveTelegramProgramAction::FocusTelegram => Effect::FocusDevice {
                device: DeviceId::Telegram,
            },
            ResolveTelegramProgramAction::OpenChat { chat_id } => Effect::DeviceAction {
                action: crate::device::DeviceAction::TelegramSelectChat { chat_id },
            },
            ResolveTelegramProgramAction::ResolveChat {
                chat_id,
                resolution,
            } => Effect::ResolveTelegramChat {
                chat_id,
                resolution,
            },
            ResolveTelegramProgramAction::ReplyInCurrentChat { text } => Effect::DeviceAction {
                action: crate::device::DeviceAction::TelegramSendMessage { text },
            },
            ResolveTelegramProgramAction::Wait => Effect::Wait,
        },
    }
}
