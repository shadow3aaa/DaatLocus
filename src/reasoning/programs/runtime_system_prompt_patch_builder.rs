use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const RUNTIME_SYSTEM_PROMPT_PATCH_BUILDER_SYSTEM_PROMPT: &str = r#"你现在负责为 runtime system prompt 生成下一版最小 patch 候选。
你的任务不是重写整段 prompt，而是在已有 prompt 基础上提出尽量少、但足够解决 failed demos 的增量规则。

要求：
- 优先输出最小 patch 列表，每条尽量独立、可读、可直接追加到 evolvable system layer。
- 只能依据 failed demos、judge feedback 和 sleep hypotheses 提建议。
- 不要重复已有 prompt 中已经明确表达的规则。
- 如果当前材料不足以提出可靠 patch，就输出空列表。 "#;

pub struct RuntimeSystemPromptPatchBuilderProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeSystemPromptPatchBuilderOutput {
    pub title: String,
    pub rationale: String,
    pub prompt_patches: Vec<String>,
}

impl Program for RuntimeSystemPromptPatchBuilderProgram {
    type Output = RuntimeSystemPromptPatchBuilderOutput;

    fn name(&self) -> &'static str {
        "runtime_system_prompt_patch_builder"
    }

    fn description(&self) -> &'static str {
        "根据 failed demos、judge 建议和 sleep hypotheses 生成 runtime system prompt 的最小 patch 候选。"
    }

    fn signature(&self) -> Signature {
        Signature::new("为 runtime system prompt 生成下一版 patch 候选。")
            .input("current system prompt", "当前 runtime system prompt。")
            .input("failed demos", "未通过 demos 的摘要。")
            .input("judge feedback", "judge 给出的 needed_changes 和原因。")
            .input("sleep hypotheses", "sleep 产生的 instruction hypotheses。")
            .output("title", "候选 patch 标题。")
            .output("rationale", "为什么这些 patch 能修复当前 failed demos。")
            .output(
                "prompt_patches",
                "建议追加到 evolvable layer 的最小 patch 列表。",
            )
            .rule("尽量输出 1 到 5 条 patch。")
            .rule("每条 patch 应是稳定的系统规则，而不是 case 特化描述。")
            .rule("不要重写整个 prompt。")
    }

    fn build_ir(&self, _: &Context, _: &Snapshot) -> PromptIR {
        self.dataset_ir(String::new(), String::new(), String::new(), String::new())
    }
}

impl RuntimeSystemPromptPatchBuilderProgram {
    pub fn dataset_ir(
        &self,
        current_system_prompt: String,
        failed_demos: String,
        judge_feedback: String,
        sleep_hypotheses: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(RUNTIME_SYSTEM_PROMPT_PATCH_BUILDER_SYSTEM_PROMPT);
        ir.push_instruction("优先把 judge 提到的 needed_changes 抽象成更稳定的 runtime 规则。");
        ir.push_instruction(
            "如果 sleep hypotheses 与 judge feedback 冲突，以 judge 针对 failed demos 的反馈为准。",
        );
        ir.push_section("current system prompt", current_system_prompt);
        ir.push_section("failed demos", failed_demos);
        ir.push_section("judge feedback", judge_feedback);
        ir.push_section("sleep hypotheses", sleep_hypotheses);
        ir
    }
}
