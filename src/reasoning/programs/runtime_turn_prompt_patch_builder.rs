use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const RUNTIME_TURN_PROMPT_PATCH_BUILDER_SYSTEM_PROMPT: &str = r#"你现在负责为 runtime system prompt 生成下一版最小 patch 候选。
目标不是重写整段 prompt，而是在已有 prompt 基础上提出尽量少、但足够修复 turn-level failed demos 的增量规则。

要求：
- 优先输出最小 patch 列表，每条尽量独立、可读、可直接追加到 evolvable system layer。
- 只能依据 failed turn demos、turn judge feedback 和 sleep hypotheses 提建议。
- 不要重复已有 prompt 中已经明确表达的规则。
- 如果当前材料不足以提出可靠 patch，就输出空列表。 "#;

pub struct RuntimeTurnPromptPatchBuilderProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeTurnPromptPatchBuilderOutput {
    pub title: String,
    pub rationale: String,
    pub prompt_patches: Vec<String>,
}

impl Program for RuntimeTurnPromptPatchBuilderProgram {
    type Output = RuntimeTurnPromptPatchBuilderOutput;

    fn name(&self) -> &'static str {
        "runtime_turn_prompt_patch_builder"
    }

    fn description(&self) -> &'static str {
        "根据 failed turn demos、turn judge 建议和 sleep hypotheses 生成 runtime system prompt 的最小 patch 候选。"
    }

    fn signature(&self) -> Signature {
        Signature::new("为 runtime system prompt 生成面向 turn rollout 的 patch 候选。")
            .input("current system prompt", "当前 runtime system prompt。")
            .input(
                "failed turn demos",
                "未通过 turn demos 的完整失败包，包括 demo 结构与对应 trace 摘要。",
            )
            .input(
                "turn judge feedback",
                "turn judge 给出的 needed_changes 和原因。",
            )
            .input("sleep hypotheses", "sleep 产生的 instruction hypotheses。")
            .output("title", "候选 patch 标题。")
            .output(
                "rationale",
                "为什么这些 patch 能修复当前 failed turn demos。",
            )
            .output(
                "prompt_patches",
                "建议追加到 evolvable layer 的最小 patch 列表。",
            )
            .rule("尽量输出 1 到 5 条 patch。")
            .rule("每条 patch 应是稳定的运行时行为规则，而不是 case 特化描述。")
            .rule("优先修复过早终止、阶段性话术误结案、遗漏必要工具推进这三类问题。")
            .rule("不要重写整个 prompt。")
    }
}

impl RuntimeTurnPromptPatchBuilderProgram {
    pub fn dataset_ir(
        &self,
        current_system_prompt: String,
        failed_turn_demos: String,
        turn_judge_feedback: String,
        sleep_hypotheses: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(RUNTIME_TURN_PROMPT_PATCH_BUILDER_SYSTEM_PROMPT);
        ir.push_instruction("优先把 turn judge 提到的 needed_changes 抽象成更稳定的运行时规则。");
        ir.push_instruction(
            "如果 sleep hypotheses 与 turn judge feedback 冲突，以 turn judge 针对 failed demos 的反馈为准。",
        );
        ir.push_instruction(
            "认真阅读 failed turn demos 中的 incoming_text、expected_behavior、must_use_tools、must_not_final_answer_patterns，以及 trace_rendered / final_assistant_message / final_reply_message / actions_rendered。",
        );
        ir.push_instruction(
            "如果当前失败模式并不是缺少‘先查再答’这个字面规则，而是模型仍然会以泛泛问候、空泛服务承诺或错误终局文本结束，就应输出更强、更直接的终局/工具使用约束，而不是重复同义规则。",
        );
        ir.push_instruction(
            "不要重复 current system prompt 中已经存在的同义规则；若已有规则未生效，应提出更可执行、更难被误解的新约束。",
        );
        ir.push_section("current system prompt", current_system_prompt);
        ir.push_section("failed turn demos", failed_turn_demos);
        ir.push_section("turn judge feedback", turn_judge_feedback);
        ir.push_section("sleep hypotheses", sleep_hypotheses);
        ir
    }
}
