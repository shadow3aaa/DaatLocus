use crate::{
    context::Context,
    core::Output,
    device::DeviceId,
    reasoning::{
        examples::{ExampleField, ProgramExample},
        ir::PromptIR,
        program::Program,
        prompts::{SYSTEM_PROMPT, build_device_context_prompt},
        signature::Signature,
    },
    snapshot::Snapshot,
};

pub struct TerminalNextStepProgram;

impl TerminalNextStepProgram {
    fn current_task_summary(context: &Context) -> String {
        let Some(working_task_id) = context.tasks.working_task() else {
            return "当前没有选中的下一步动作。".to_string();
        };
        context
            .tasks
            .tasks()
            .find(|(id, _)| *id == working_task_id)
            .map(|(id, task)| {
                format!(
                    "{id}. {}{}",
                    task.description,
                    task.project_id
                        .map(|project_id| format!(" [project={project_id}]"))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "当前 working_task={}，但在列表里未找到对应描述。",
                    working_task_id
                )
            })
    }

    fn terminal_view(context: &Context) -> String {
        match context.devices.focused_render() {
            Some(render) if context.devices.focused() == Some(DeviceId::Terminal) => render.content,
            Some(render) => format!(
                "当前前景不是 Terminal，而是 {}。\n{}",
                render.title, render.content
            ),
            None => "当前没有前景设备。".to_string(),
        }
    }

    fn dataset_ir(
        &self,
        current_task: String,
        terminal_view: String,
        device_context: String,
        snapshot_text: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(SYSTEM_PROMPT);
        ir.push_instruction("你现在只负责判断前景 Terminal 的下一步动作。目标不是泛化规划，而是正确使用 PTY 终端推进当前任务。");
        ir.push_instruction("如果底部已经回到 shell prompt，说明上一条命令已结束；不要因为上方输出被窗口截断，就重复发送同一条命令。");
        ir.push_instruction("如果当前只是输出很多静态结果，需要换查看策略，应优先选择更合适的下一步，例如 grep/head/less，而不是机械重跑原命令。");
        ir.push_instruction(
            "如果终端只是持续输出普通命令结果、尚未回到 prompt、也没有输入提示，应优先 Wait。",
        );
        ir.push_instruction("如果终端进入交互式认证、登录向导、REPL 或不适合自动推进的交互界面，应优先发送 Ctrl+C 中断，并改走非交互方案。");
        ir.push_instruction("如果终端进入 less/man 等分页器，而当前目标只是退出它回到 shell，可发送安全、短小、确定的输入，例如 q。");
        ir.push_instruction("不要把同一条命令的重复执行当作默认答案；只有在你明确判断需要重试同一命令时，才再次发送它。");
        ir.push_section("当前选中动作", current_task);
        ir.push_section("前景 Terminal 画面", terminal_view);
        ir.push_section("设备上下文", device_context);
        ir.push_section("完整快照", snapshot_text);
        ir
    }
}

impl Program for TerminalNextStepProgram {
    type Output = Output;

    fn name(&self) -> &'static str {
        "terminal_next_step"
    }

    fn description(&self) -> &'static str {
        "在 Terminal 处于前景时，根据 PTY 画面和当前任务选择最合理的下一步终端动作。"
    }

    fn tuning_key(&self) -> String {
        "terminal_next_step".to_string()
    }

    fn signature(&self) -> Signature {
        Signature::new("根据当前前景 Terminal 画面和任务目标，选择一条最合理的下一步终端动作。")
            .input("当前选中动作", "当前正在执行的任务描述。")
            .input("前景 Terminal 画面", "当前 PTY 终端画面。")
            .input(
                "完整快照",
                "完整世界状态，必要时可用于判断是否应切设备或改走别的路径。",
            )
            .output(
                "observation",
                "从终端画面中提炼出的具体事实、状态判断或结果结论。",
            )
            .output(
                "description",
                "为什么当前应采取这个终端动作，而不是重复命令或误判状态。",
            )
            .output("current_doing", "当前持续推进的终端分析主线。")
            .output("action", "一条可直接交给执行层处理的全局动作。")
            .rule("不要因为输出上方被窗口截断就重复执行同一条命令。")
            .rule("如果命令仍在自然运行且没有输入提示，应优先 Wait。")
            .rule("如果已经回到 shell prompt，应把上一条命令视为结束，再决定下一步查看或分析策略。")
            .rule("如果进入认证向导、REPL 或不适合自动推进的交互式界面，应优先中断或退出。")
    }

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        vec![
            ProgramExample {
                title: "终端停在 gh auth login 时应中断".to_string(),
                inputs: vec![
                    ExampleField {
                        name: "当前选中动作".to_string(),
                        value: "在 Terminal 检索 GitHub 开源项目资料。".to_string(),
                    },
                    ExampleField {
                        name: "前景 Terminal 画面".to_string(),
                        value: "ubuntu@spinova-lab:~$ gh auth login\n? What account do you want to log into? GitHub.com\n? What is your preferred protocol for Git operations on this host? HTTPS\n? Authenticate Git with your GitHub credentials? (Y/n)".to_string(),
                    },
                    ExampleField {
                        name: "完整快照".to_string(),
                        value: "当前主线：在 Terminal 检索 GitHub 项目资料。".to_string(),
                    },
                ],
                output: Output {
                    observation: "终端已进入 gh auth login 的交互式认证向导，需要人工账号授权，不适合继续自动推进。".to_string(),
                    description: "当前应先中断错误进入的认证向导，再改用非交互替代方案，而不是继续回答登录问题。".to_string(),
                    current_doing: "中断交互式认证流程以恢复可自动执行状态".to_string(),
                    action: crate::core::Action::DeviceAction {
                        action: crate::device::DeviceAction::TerminalInput {
                            text: "\u{3}".to_string(),
                        },
                    },
                },
            },
            ProgramExample {
                title: "分页器里用 q 退出".to_string(),
                inputs: vec![
                    ExampleField {
                        name: "当前选中动作".to_string(),
                        value: "查看命令输出后回到 shell 继续分析。".to_string(),
                    },
                    ExampleField {
                        name: "前景 Terminal 画面".to_string(),
                        value: "libfoo.so\nlibbar.so\n(END)".to_string(),
                    },
                    ExampleField {
                        name: "完整快照".to_string(),
                        value: "当前主线：通过 Terminal 查看输出并继续推进任务。".to_string(),
                    },
                ],
                output: Output {
                    observation: "终端当前停在分页器的 (END) 界面，这不是 shell prompt，也不是需要人工授权的交互式认证。".to_string(),
                    description: "当前目标只是退出分页器并回到 shell，发送 q 是短小、安全且确定的下一步。".to_string(),
                    current_doing: "退出分页器回到 shell 继续终端分析".to_string(),
                    action: crate::core::Action::DeviceAction {
                        action: crate::device::DeviceAction::TerminalInput {
                            text: "q".to_string(),
                        },
                    },
                },
            },
            ProgramExample {
                title: "持续输出时等待".to_string(),
                inputs: vec![
                    ExampleField {
                        name: "当前选中动作".to_string(),
                        value: "等待测试命令产出结果。".to_string(),
                    },
                    ExampleField {
                        name: "前景 Terminal 画面".to_string(),
                        value: "running 42 tests\ntest foo ... ok\ntest bar ...".to_string(),
                    },
                    ExampleField {
                        name: "完整快照".to_string(),
                        value: "当前主线：等待测试命令完成。".to_string(),
                    },
                ],
                output: Output {
                    observation: "终端当前只是持续输出普通测试进度，还没有回到 shell prompt，也没有出现需要输入的提示。".to_string(),
                    description: "此时最合理的动作是等待命令自然完成，而不是发送多余输入或误判为命令已结束。".to_string(),
                    current_doing: "等待非交互命令继续运行并产出结果".to_string(),
                    action: crate::core::Action::Wait,
                },
            },
        ]
    }

    fn build_ir(&self, context: &Context, snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir(
            Self::current_task_summary(context),
            Self::terminal_view(context),
            build_device_context_prompt(context),
            snapshot.to_string(),
        )
    }
}
