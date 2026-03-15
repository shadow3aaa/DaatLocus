use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

pub struct ResolveTelegramChatProgram;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ResolveTelegramProgramAction {
    FocusTelegram,
    OpenChat {
        chat_id: String,
    },
    ResolveChat {
        chat_id: String,
        resolution: TelegramResolution,
    },
    ReplyInCurrentChat {
        #[serde(alias = "reply")]
        text: String,
    },
    Wait,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ResolveTelegramProgramOutput {
    pub observation: String,
    pub description: String,
    pub current_doing: String,
    pub action: ResolveTelegramProgramAction,
}

impl Program for ResolveTelegramChatProgram {
    type Output = ResolveTelegramProgramOutput;

    fn name(&self) -> &'static str {
        "resolve_telegram_chat"
    }

    fn description(&self) -> &'static str {
        "判断一条 Telegram 原始来信应如何处理，只允许输出与 Telegram 消息处理相关的局部动作。"
    }

    fn signature(&self) -> Signature {
        Signature::new("判断 Telegram 原始来信的处理方式，并只输出 Telegram 局部处理动作。")
            .input("待判断会话", "仍显示“待判断：是”的 Telegram 会话列表。")
            .input("当前前景设备", "当前是否已经聚焦 Telegram。")
            .input("Telegram 设备约束", "Telegram 设备的可操作规则。")
            .input("完整快照", "用于理解消息上下文、当前项目和任务状态。")
            .output("observation", "从消息和快照中提炼出的关键信息。")
            .output("description", "为何选择该 Telegram 局部动作。")
            .output("current_doing", "正在处理哪类 Telegram 会话问题。")
            .output(
                "action",
                "只能是 FocusTelegram、OpenChat、ResolveChat、ReplyInCurrentChat 或 Wait。",
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
        ir.push_instruction("如果 Telegram 还没到前景，先输出 `FocusTelegram`。");
        ir.push_instruction("如果 Telegram 已到前景，但相关会话还没打开，先输出 `OpenChat`。");
        ir.push_instruction(
            "当相关会话已经打开且仍显示“待判断：是”时，再输出 `ResolveChat` 并给出一个 `TelegramResolution`。",
        );
        ir.push_instruction(
            "如果会话已经“待判断：否”但仍“待回复：是”，说明语义已判断，只差发送/补发消息，此时输出 `ReplyInCurrentChat` 或 `Wait`。",
        );
        ir.push_instruction(
            "只有当你明确接受未来需要持续推进的工作时，才在 `ResolveChat` 中使用 `AcceptAsProject`。",
        );
        ir.push_instruction(
            "不要输出项目 bookkeeping；系统会根据你的 `TelegramResolution` 自动创建项目和第一条下一步动作。",
        );
        ir.push_section("待判断会话", pending_text);
        ir.push_section("当前前景设备", focus);
        ir.push_section("Telegram 设备约束", TELEGRAM_PROMPT);
        ir.push_section("完整快照", snapshot_text);
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
}
