use crate::{
    context::Context,
    core::Output,
    reasoning::{
        datasets::action_phase as dataset,
        examples::ProgramExample,
        ir::PromptIR,
        program::Program,
        prompts::{SYSTEM_PROMPT, build_device_context_prompt},
        signature::Signature,
    },
    snapshot::Snapshot,
};

#[derive(Clone, Copy)]
pub enum ActionPhase {
    AttendNotifications,
    ExecuteTask,
    PlanFromProject,
    ExploreNewTasks,
}

pub struct ActionPhaseProgram {
    phase: ActionPhase,
}

impl ActionPhaseProgram {
    pub fn new(phase: ActionPhase) -> Self {
        Self { phase }
    }

    pub fn phase(&self) -> ActionPhase {
        self.phase
    }

    fn title(&self) -> &'static str {
        match self.phase {
            ActionPhase::AttendNotifications => "处理提醒",
            ActionPhase::ExecuteTask => "执行下一步动作",
            ActionPhase::PlanFromProject => "为项目规划下一步",
            ActionPhase::ExploreNewTasks => "探索与规划新任务",
        }
    }

    fn instructions(&self) -> Vec<&'static str> {
        match self.phase {
            ActionPhase::AttendNotifications => vec![
                "先区分两类需要处理的东西：义务列表中的 `Pending` 义务；以及 Telegram 会话中显示“待判断：是”或“待回复：是”的消息。",
                "Telegram 原始消息不自动等于义务。对原始来信做语义判断时，应优先使用 `ResolveTelegramChat`，而不是先创建义务。",
                "如果某个 Telegram 会话只剩“待回复：是”而“待判断：否”，说明语义已经判断完了，此时优先保持 Telegram 在前景并发送/补发消息，不要再重新做语义判定。",
                "义务列表中的内容通常是结构化待处理责任，例如项目完成后的回报。处理这类义务时，可以使用设备动作回复，并在妥善完成后用 `ObligationSatisfy` 关单。",
                "只有当你明确接受某项结构化义务并承诺后续会持续推进时，才使用 `CommitToProject` 将它升级为项目。",
                "在相关提醒处理完成之前，不要切回 Terminal，也不要恢复探索性终端操作。",
                "如果你刚发出消息，正在等待 transport 结果，或正在等待某个明确外部状态变化，可以输出 `Wait`。",
                "如果只是空闲等待对方继续发言、新消息或新输入，不要写普通 `Wait`；应输出 `SilentWait`。",
            ],
            ActionPhase::ExecuteTask => vec![
                "检查下一步动作列表：如果你还没有选中任何动作，请优先使用 `TaskSelect`。",
                "先读懂当前选中动作需要哪个设备；如果目标设备不在前景，请先 `FocusDevice`。",
                "如果当前动作属于某个项目，请确保它确实在推进那个项目，而不是偏离目标。",
                "如果当前动作是回复 Telegram 消息，优先保持 Telegram 在前景并回复，不要因为旧习惯切回 Terminal。",
                "只有当所需设备已经在前景时，才输出相应的 `DeviceAction`。",
                "如果终端当前停在交互式认证、登录向导、密码提示或需要人工授权的提问界面，不要继续回答这些问题；应优先输出 `DeviceAction` -> `TerminalInput` 发送 Ctrl+C（`\\u0003`）中断，再改用非交互替代方案。",
                "不要主动启动 `gh auth login`、`docker login`、`npm login` 这类需要外部账号或浏览器授权的命令。",
                "如果你发现还缺少别的下一步动作，而且它明确属于某个项目，请用 `TaskAdd` 并填入 `project_id`。",
                "当你判断某个项目的成功标准已经满足时，应优先输出 `ProjectComplete`。",
                "如果当前选中的动作已经彻底完成，但所属项目还未完成，请输出 `TaskDelete`。",
                "如果某条义务已经被你妥善处理完，例如刚完成最终回报、且不再需要继续跟进，请输出 `ObligationSatisfy`。",
                "如果刚执行了耗时命令，或刚发送了 Telegram 消息正在等待 transport 结果，可以输出 `Wait`。",
                "如果只是空闲地等待用户回复，不要写普通 `Wait`；应输出 `SilentWait`。",
            ],
            ActionPhase::PlanFromProject => vec![
                "查看项目列表，找出最值得优先推进的 `Active` 项目。",
                "为这个项目生成一条具体、可执行、足够小的下一步动作，并用 `TaskAdd` 添加到下一步动作列表。",
                "这条新动作若明确属于某个项目，必须在 `TaskAdd.project_id` 中填写对应项目 id。",
                "下一步动作应尽量直接可执行，避免空泛表述。",
                "如果某个项目当前处于外部等待状态，且确实还不适合生成新的动作，可以输出 `Wait`，但不要因此转去探索无关新任务。",
                "如果某个项目其实已经达到成功标准，不要继续规划新动作，应直接输出 `ProjectComplete`。",
            ],
            ActionPhase::ExploreNewTasks => vec![
                "只有在没有待处理义务、没有可执行的下一步动作、也没有需要先规划的活跃项目时，才进入探索。",
                "如果你需要探索 Terminal，但它当前不在前景，请先输出 `FocusDevice` 将 `Terminal` 切到前景。",
                "探索环境时，可在 Terminal 处于前景时输出 `DeviceAction` 来执行探索性命令。",
                "一旦构思好新目标，请立即输出 `TaskAdd` 将计划添加到任务列表中。",
                "你的首要职责是寻找并创建新任务。",
                "如果当前完全空闲，只是在等新的 Telegram 消息或外部输入，请输出 `SilentWait`，不要把这种空转等待写进记忆。",
            ],
        }
    }

    pub fn dataset_ir(&self, device_context: String, snapshot_text: String) -> PromptIR {
        let mut ir = PromptIR::with_system(SYSTEM_PROMPT);
        for instruction in self.instructions() {
            ir.push_instruction(instruction);
        }
        ir.push_section("当前阶段", self.title());
        ir.push_section("设备上下文", device_context);
        ir.push_section("完整快照", snapshot_text);
        ir
    }

    pub fn eval_suite_name(&self) -> &'static str {
        match self.phase {
            ActionPhase::AttendNotifications => "action_phase.attend_notifications",
            ActionPhase::ExecuteTask => "action_phase.execute_task",
            ActionPhase::PlanFromProject => "action_phase.plan_from_project",
            ActionPhase::ExploreNewTasks => "action_phase.explore_new_tasks",
        }
    }
}

impl Program for ActionPhaseProgram {
    type Output = Output;

    fn name(&self) -> &'static str {
        "decide_next_action"
    }

    fn description(&self) -> &'static str {
        "根据当前阶段和完整快照，输出下一步全局动作。"
    }

    fn tuning_key(&self) -> String {
        self.eval_suite_name().to_string()
    }

    fn signature(&self) -> Signature {
        Signature::new("基于当前阶段、设备上下文和完整快照，选择一条最合适的全局动作。")
            .input(
                "阶段",
                "当前推理阶段，例如处理提醒、执行下一步动作、为项目规划下一步、探索新任务。",
            )
            .input("设备上下文", "当前前景设备、可操作约束和后台提醒摘要。")
            .input(
                "完整快照",
                "世界状态的完整文本视图，包含记忆、义务、项目、下一步动作和设备画面。",
            )
            .output(
                "observation",
                "本轮从快照中提炼出的具体事实、结论或新信息。",
            )
            .output(
                "description",
                "为何选择该动作，以及它与当前阶段目标的关系。",
            )
            .output("current_doing", "正在持续推进的高层行为。")
            .output("action", "一条合法的全局动作，必须能直接交给执行层处理。")
            .rule("输出必须与当前阶段目标一致，不要越阶段做无关决策。")
            .rule("优先使用快照中出现的 UUID 作为 task_id、obligation_id、project_id。")
            .rule("不要把动作 bookkeeping 解释成 observation；observation 必须包含环境事实。")
    }

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        dataset::examples(self.phase)
    }

    fn build_ir(&self, context: &Context, snapshot: &Snapshot) -> PromptIR {
        self.dataset_ir(build_device_context_prompt(context), snapshot.to_string())
    }
}

impl ActionPhaseProgram {
    pub fn eval_cases(&self) -> Vec<crate::reasoning::eval::EvalCase<Output>> {
        dataset::eval_cases(self)
    }
}
