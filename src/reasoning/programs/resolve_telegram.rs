use crate::{
    context::Context,
    core::TelegramResolution,
    device::DeviceId,
    reasoning::{
        datasets::resolve_telegram as dataset,
        examples::ProgramExample,
        ir::PromptIR,
        program::Program,
        prompts::{SYSTEM_PROMPT, TELEGRAM_PROMPT},
        signature::Signature,
    },
    snapshot::Snapshot,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub struct ResolveTelegramChatProgram;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ResolveTelegramProgramOutput {
    pub observation: String,
    pub description: String,
    pub current_doing: String,
    pub action_kind: String,
    pub action_summary: String,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub resolution: Option<TelegramResolution>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResolveTelegramProgramOutputWire {
    observation: String,
    description: String,
    current_doing: String,
    #[serde(default)]
    action_kind: Option<String>,
    #[serde(default)]
    action_summary: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    resolution: Option<TelegramResolution>,
    #[serde(default)]
    text: Option<String>,
}

impl<'de> Deserialize<'de> for ResolveTelegramProgramOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ResolveTelegramProgramOutputWire::deserialize(deserializer)?;
        let action_kind = wire.action_kind.unwrap_or_else(|| "wait".to_string());
        let action_summary = wire
            .action_summary
            .unwrap_or_else(|| "implicit wait".to_string());
        Ok(Self {
            observation: wire.observation,
            description: wire.description,
            current_doing: wire.current_doing,
            action_kind,
            action_summary,
            chat_id: wire.chat_id,
            resolution: wire.resolution,
            text: wire.text,
        })
    }
}

impl Program for ResolveTelegramChatProgram {
    type Output = ResolveTelegramProgramOutput;

    fn name(&self) -> &'static str {
        "resolve_telegram_chat"
    }

    fn description(&self) -> &'static str {
        "判断一条 Telegram 原始来信应如何处理，并输出与 Telegram 处理相关的动作语义。"
    }

    fn signature(&self) -> Signature {
        Signature::new("判断 Telegram 原始来信的处理方式，并输出 Telegram 相关动作。")
            .input("待判断会话", "仍显示“待判断：是”的 Telegram 会话列表。")
            .input("当前前景设备", "当前是否已经聚焦 Telegram。")
            .input("Telegram 设备约束", "Telegram 设备的可操作规则。")
            .input("当前状态", "用于理解消息上下文、当前项目和当前工作状态。")
            .output("observation", "从消息和当前状态中提炼出的关键信息。")
            .output("description", "为何选择该动作。")
            .output("current_doing", "正在处理哪类 Telegram 会话问题。")
            .output(
                "action_kind",
                "只能是 focus_device、telegram_select_chat、resolve_telegram_chat、telegram_send_message 或 wait。",
            )
            .output("action_summary", "对动作参数的紧凑摘要。")
            .output("chat_id", "若动作针对某个 Telegram 会话，则填写对应 chat_id。")
            .output(
                "resolution",
                "仅当 action_kind=resolve_telegram_chat 时填写 TelegramResolution。",
            )
            .output(
                "text",
                "仅当 action_kind=telegram_send_message 时填写要发送的消息文本。",
            )
            .rule("不要在这个 program 里直接做项目 bookkeeping 或终端探索。")
            .rule("只有明确接受未来持续工作时，才使用 AcceptAsProject。")
            .rule("如果只差补发消息，不要重新做语义判定。")
    }

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        dataset::examples()
    }

    fn build_ir(&self, context: &Context, snapshot: &Snapshot) -> PromptIR {
        let pending_resolution = context.telegram.pending_resolution_refs();
        let pending_text = if pending_resolution.is_empty() {
            "当前没有待判断的 Telegram 会话。".to_string()
        } else {
            pending_resolution
                .into_iter()
                .map(|(chat_id, title)| format!("- {title} ({chat_id})"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let focus = match context.devices.focused() {
            Some(DeviceId::Telegram) => "Telegram",
            Some(DeviceId::Terminal) => "Terminal",
            None => "None",
        };
        self.dataset_ir(pending_text, focus.to_string(), snapshot.to_string())
    }
}

impl ResolveTelegramChatProgram {
    pub fn dataset_ir(
        &self,
        pending_text: String,
        focus: String,
        snapshot_text: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(SYSTEM_PROMPT);
        ir.push_instruction(
            "你现在只负责处理 Telegram 原始来信，不要规划终端探索，也不要处理无关任务。",
        );
        ir.push_instruction(
            "如果 Telegram 还没到前景，输出 `focus_device` 并将 device 设为 Telegram。",
        );
        ir.push_instruction(
            "如果 Telegram 已到前景，但相关会话还没打开，输出 `telegram_select_chat` 并填写 chat_id。",
        );
        ir.push_instruction(
            "当相关会话已经打开且仍显示“待判断：是”时，输出 `resolve_telegram_chat` 并给出一个 `TelegramResolution`。",
        );
        ir.push_instruction(
            "如果会话已经“待判断：否”但仍“待回复：是”，说明语义已判断，只差发送/补发消息，此时输出 `telegram_send_message` 或 `wait`。",
        );
        ir.push_instruction(
            "只有当你明确接受未来需要持续推进的工作时，才在 `resolution` 中使用 `AcceptAsProject`。",
        );
        ir.push_instruction(
            "不要输出项目 bookkeeping；系统会根据你的 `TelegramResolution` 自动创建项目并设置第一条当前工作目标。",
        );
        ir.push_section("待判断会话", pending_text);
        ir.push_section("当前前景设备", focus);
        ir.push_section("Telegram 设备约束", TELEGRAM_PROMPT);
        ir.push_section("当前状态", snapshot_text);
        ir
    }

    pub fn train_eval_cases(
        &self,
    ) -> Vec<crate::reasoning::eval::EvalCase<ResolveTelegramProgramOutput>> {
        dataset::train_eval_cases(self)
    }

    pub fn dev_eval_cases(
        &self,
    ) -> Vec<crate::reasoning::eval::EvalCase<ResolveTelegramProgramOutput>> {
        dataset::dev_eval_cases(self)
    }

    pub fn acceptance_eval_cases(
        &self,
    ) -> Vec<crate::reasoning::eval::EvalCase<ResolveTelegramProgramOutput>> {
        dataset::acceptance_eval_cases(self)
    }

    pub fn stress_eval_cases(
        &self,
    ) -> Vec<crate::reasoning::eval::EvalCase<ResolveTelegramProgramOutput>> {
        dataset::stress_eval_cases(self)
    }
}
