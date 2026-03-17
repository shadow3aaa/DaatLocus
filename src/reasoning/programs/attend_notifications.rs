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
pub struct AttendNotificationsProgram;

impl Program for AttendNotificationsProgram {
    type Output = Output;

    fn name(&self) -> &'static str {
        "attend_notifications"
    }

    fn description(&self) -> &'static str {
        "根据完整快照选择处理提醒阶段的下一步执行效果。"
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

impl ActionPhaseProgramSpec for AttendNotificationsProgram {
    fn suite_name(&self) -> &'static str {
        "action_phase.attend_notifications"
    }

    fn phase_title(&self) -> &'static str {
        "处理提醒"
    }

    fn phase_instructions(&self) -> Vec<&'static str> {
        vec![
            "先区分两类需要处理的东西：义务列表中的 `Pending` 义务；以及 Telegram 会话中显示“待判断：是”或“待回复：是”的消息。",
            "Telegram 原始消息不自动等于义务。对原始来信做语义判断时，应优先使用 `ResolveTelegramChat`，而不是先创建义务。",
            "如果某个 Telegram 会话只剩“待回复：是”而“待判断：否”，说明语义已经判断完了，此时优先保持 Telegram 在前景并发送/补发消息，不要再重新做语义判定。",
            "义务列表中的内容通常是结构化待处理责任，例如项目完成后的回报。处理这类义务时，可以使用设备动作回复，并在妥善完成后用 `ObligationSatisfy` 关单。",
            "只有当你明确接受某项结构化义务并承诺后续会持续推进时，才使用 `CommitToProject` 将它升级为项目。",
            "在相关提醒处理完成之前，不要切回 Terminal，也不要恢复探索性终端操作。",
            "如果你刚发出消息，正在等待 transport 结果，或正在等待某个明确外部状态变化，可以输出 `Wait`。",
            "如果只是空闲等待对方继续发言、新消息或新输入，不要写普通 `Wait`；应输出 `SilentWait`。",
        ]
    }
}
