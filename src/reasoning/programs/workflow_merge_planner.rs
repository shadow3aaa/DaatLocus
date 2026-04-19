use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::reasoning::{ir::PromptIR, program::Program, signature::Signature};

const WORKFLOW_MERGE_PLANNER_SYSTEM_PROMPT: &str = r#"你现在负责判断两个 workflow 是否应该 merge。
你的任务是基于两份 workflow spec、各自的 reflection 与 run evidence，输出：
1. should_merge
2. merge rationale
3. confidence
4. accepted / selected

要求：
- 只有当两个 workflow 实际上描述的是同类可复用流程时，才 should_merge=true。
- 不要只看词面相似，要看任务边界、失败模式和流程骨架是否兼容。
- 如果 merge 风险高，应明确拒绝。"#;

pub struct WorkflowMergePlannerProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowMergePlannerOutput {
    pub should_merge: bool,
    pub rationale: String,
    pub confidence: f64,
    pub accepted: bool,
    pub selected: bool,
}

impl Program for WorkflowMergePlannerProgram {
    type Output = WorkflowMergePlannerOutput;

    fn name(&self) -> &'static str {
        "workflow_merge_planner"
    }

    fn description(&self) -> &'static str {
        "基于两个 workflow 的 spec、reflection 与 evidence 判断是否应该 merge。"
    }

    fn signature(&self) -> Signature {
        Signature::new("判断两个 workflow 是否应该 merge。")
            .input("target workflow id", "候选 target workflow id。")
            .input("target workflow spec", "target workflow spec。")
            .input("target workflow reflection", "target workflow 的反思结果。")
            .input("target run evidence", "target workflow 的运行证据。")
            .input("source workflow id", "候选 source workflow id。")
            .input("source workflow spec", "source workflow spec。")
            .input("source workflow reflection", "source workflow 的反思结果。")
            .input("source run evidence", "source workflow 的运行证据。")
            .output("should_merge", "这两个 workflow 是否应该 merge。")
            .output("rationale", "merge 或拒绝 merge 的依据。")
            .output("confidence", "0 到 1 之间的置信度。")
            .output("accepted", "是否接受这个 merge 候选。")
            .output("selected", "是否把这个 merge 候选选为本轮执行对象。")
            .rule("如果 should_merge=false，则 accepted 和 selected 也必须为 false。")
            .rule("只有在任务边界与流程骨架都兼容时才 should_merge=true。")
    }
}

impl WorkflowMergePlannerProgram {
    #[allow(clippy::too_many_arguments)]
    pub fn dataset_ir(
        &self,
        target_workflow_id: String,
        target_workflow_spec: String,
        target_workflow_reflection: String,
        target_run_evidence: String,
        source_workflow_id: String,
        source_workflow_spec: String,
        source_workflow_reflection: String,
        source_run_evidence: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(WORKFLOW_MERGE_PLANNER_SYSTEM_PROMPT);
        ir.push_instruction("不要因为措辞相似就 merge；必须确认两者在任务边界和流程结构上可收敛。");
        ir.push_instruction("如果只共享某些恢复策略或局部步骤，但整体用途不同，应拒绝 merge。");
        ir.push_section("target workflow id", target_workflow_id);
        ir.push_section("target workflow spec", target_workflow_spec);
        ir.push_section("target workflow reflection", target_workflow_reflection);
        ir.push_section("target run evidence", target_run_evidence);
        ir.push_section("source workflow id", source_workflow_id);
        ir.push_section("source workflow spec", source_workflow_spec);
        ir.push_section("source workflow reflection", source_workflow_reflection);
        ir.push_section("source run evidence", source_run_evidence);
        ir
    }
}
