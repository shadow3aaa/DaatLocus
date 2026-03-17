use crate::{
    context::Context,
    core::Output,
    reasoning::{
        datasets::action_phase as dataset,
        examples::ProgramExample,
        ir::PromptIR,
        program::Program,
        programs::action_phase_common::{ActionPhaseProgramSpec, base_signature},
        prompts::build_device_context_prompt,
    },
    snapshot::Snapshot,
};

#[derive(Clone, Copy)]
pub struct ExecuteTaskProgram;

impl Program for ExecuteTaskProgram {
    type Output = Output;

    fn name(&self) -> &'static str {
        "execute_task"
    }

    fn description(&self) -> &'static str {
        "根据完整快照选择执行下一步动作阶段的执行效果。"
    }

    fn tuning_key(&self) -> String {
        self.suite_name().to_string()
    }

    fn signature(&self) -> crate::reasoning::signature::Signature {
        base_signature(self.phase_title())
    }

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        dataset::examples(self)
    }

    fn build_ir(&self, context: &Context, snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir(build_device_context_prompt(context), snapshot.to_string())
    }
}

impl ActionPhaseProgramSpec for ExecuteTaskProgram {
    fn suite_name(&self) -> &'static str {
        "action_phase.execute_task"
    }

    fn phase_title(&self) -> &'static str {
        "执行下一步动作"
    }

    fn phase_instructions(&self) -> Vec<&'static str> {
        vec![
            "检查下一步动作列表：如果你还没有选中任何动作，请优先使用 `TaskSelect`。",
            "先读懂当前选中动作需要哪个设备；如果目标设备不在前景，请先 `FocusDevice`。",
            "如果当前动作属于某个项目，请确保它确实在推进那个项目，而不是偏离目标。",
            "如果当前动作是回复 Telegram 消息，优先保持 Telegram 在前景并回复，不要因为旧习惯切回 Terminal。",
            "只有当所需设备已经在前景时，才输出相应的 `DeviceAction`。",
            "如果终端底部已经回到 shell prompt，说明上一条命令已经结束；不要因为输出上方被窗口截断，就误判成命令仍在运行。",
            "如果终端只是持续输出普通命令结果，且没有出现需要输入的提示，应优先 `Wait`，不要抢着发送输入。",
            "如果终端当前停在交互式认证、登录向导、密码提示或需要人工授权的提问界面，不要继续回答这些问题；应优先输出 `DeviceAction` -> `TerminalInput` 发送 Ctrl+C（`\\u0003`）中断，再改用非交互替代方案。",
            "如果终端进入 `less`、`man` 等分页器，而当前目标只是退出它回到 shell，可发送安全、短小、确定的输入，例如 `q`。",
            "不要主动启动 `gh auth login`、`docker login`、`npm login` 这类需要外部账号或浏览器授权的命令。",
            "如果你发现还缺少别的下一步动作，而且它明确属于某个项目，请用 `TaskAdd` 并填入 `project_id`。",
            "当你判断某个项目的成功标准已经满足时，应优先输出 `ProjectComplete`。",
            "如果当前选中的动作已经彻底完成，但所属项目还未完成，请输出 `TaskDelete`。",
            "如果某条义务已经被你妥善处理完，例如刚完成最终回报、且不再需要继续跟进，请输出 `ObligationSatisfy`。",
            "如果刚执行了耗时命令，或刚发送了 Telegram 消息正在等待 transport 结果，可以输出 `Wait`。",
            "如果只是空闲地等待用户回复，不要写普通 `Wait`；应输出 `SilentWait`。",
        ]
    }
}
