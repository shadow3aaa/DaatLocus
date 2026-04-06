use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    context::Context,
    reasoning::{ir::PromptIR, program::Program, signature::Signature},
    snapshot::Snapshot,
};

const RUNTIME_TURN_DEMO_GENERATOR_SYSTEM_PROMPT: &str = r#"你现在负责根据人格配置与渠道契约生成 turn rollout demos。
这些 demos 会被用于冷启动 prompt 编译，检验 agent 在 event-driven turn 中是否会：
- 在需要时继续调用工具
- 在停止时给出可直接交付给外部用户的最终答复
- 避免把阶段性计划、承诺或 handoff 文本误当成最终回复

要求：
- 输出多条高价值 demos，覆盖符合 persona 与渠道契约的多个关键风险面向。
- 优先把 `terminal_answer_rules` 视为 demo 设计主轴：尽量让每个高价值 demo 主要检验其中一条终局规则，再叠加相关的过程约束。
- 若 `terminal_answer_rules` 中存在多条规则，必须按规则分组输出：每条规则对应一个 `rule_demo_group`，每个 group 至少包含一个 demo；如果某条规则过于复杂，可以在该 group 下拆成多个 demo 分别检验其不同失败模式。
- 每个 demo 都必须是一个具体的外部事件场景，而不是抽象规则。
- demos 应整体覆盖多个不同的风险轴，例如：是否需要工具查证、是否容易过早终止、是否要求自主决策、是否依赖当前世界状态、是否需要保持语言与风格一致。
- 不要生成重复场景，不要只改写同一句话。
- 不要把 persona 规则原样改写成 demo；demo 必须像真实用户会发来的消息。
- 优先生成贴近 Spinova 真实高风险场景的 demo，例如：代码库理解、本地状态查询、待办判断、事件判断、设备/系统状态判断。
- 避免生成过于宽泛又缺少可验证终局的场景；每个 demo 都必须能明确判断 agent 这一轮做得对还是错。
- `expected_behavior` 与 `judge_focus` 要强调“停止就意味着交付”的契约。
- 为每个 demo 选择一个主要的 `terminal_answer_rules` 检验目标，并让 `behavior_rules`、`tool_use_rules`、`anti_patterns` 成为支撑约束，而不是各自孤立成题。
- demo 的 `title`、`scenario_summary`、`incoming_text`、`expected_behavior`、`judge_focus` 默认应使用 persona 指定语言；除非 persona 明确要求双语或英文，否则不要自行切换语言。
- 每个 demo 都必须显式给出 `coverage_axes`，说明它主要覆盖哪些风险轴。
- 每个 demo 都必须显式给出 `persona_anchors`，指出它对应 persona spec 中哪些方向，例如 `language`、`channel_contract`、`behavior_rules`、`terminal_answer_rules`、`tool_use_rules`、`anti_patterns`。
- 对于依赖当前世界状态的场景，`requires_fresh_world_state` 必须为 true。
- `must_use_tools` 只在答案不能直接从当前 runtime snapshot 读取、必须额外探索或执行时才设为 true。
- TodoBoard 摘要、事件列表、设备结构状态本来就会出现在 runtime snapshot 里；如果 demo 只是读取这些当前可见摘要并直接作答，可以 `requires_fresh_world_state=true` 且 `must_use_tools=false`。
- 代码库文件、目录、内容，以及任何 runtime snapshot 里没有直接给出的事实，必须设为 `must_use_tools=true`。
- 只有像问候、无需查询当前世界状态的极短直接问答，`requires_fresh_world_state` 才可以为 false。
- `must_not_final_answer_patterns` 只写高风险的终局反模式短语。 "#;

pub struct RuntimeTurnDemoGeneratorProgram;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeTurnDemoGeneratorOutput {
    pub rationale: String,
    pub rule_demo_groups: Vec<GeneratedTurnDemoGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GeneratedTurnDemoGroup {
    pub terminal_answer_rule: String,
    #[serde(default)]
    pub demos: Vec<GeneratedTurnDemo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GeneratedTurnDemo {
    pub title: String,
    pub scenario_summary: String,
    pub incoming_text: String,
    pub expected_behavior: String,
    #[serde(default)]
    pub judge_focus: Vec<String>,
    #[serde(default)]
    pub coverage_axes: Vec<String>,
    #[serde(default)]
    pub persona_anchors: Vec<String>,
    #[serde(default)]
    pub requires_fresh_world_state: bool,
    #[serde(default)]
    pub must_use_tools: bool,
    #[serde(default)]
    pub must_not_final_answer_patterns: Vec<String>,
    #[serde(default = "default_true")]
    pub must_end_with_terminal_answer: bool,
    pub confidence: f64,
}

fn default_true() -> bool {
    true
}

impl Program for RuntimeTurnDemoGeneratorProgram {
    type Output = RuntimeTurnDemoGeneratorOutput;

    fn name(&self) -> &'static str {
        "runtime_turn_demo_generator"
    }

    fn description(&self) -> &'static str {
        "根据 persona 契约生成用于冷启动 turn-rollout 编译的 demos。"
    }

    fn signature(&self) -> Signature {
        Signature::new("根据 persona 配置生成高价值 turn demos。")
            .input("base runtime contract", "当前固定的 runtime kernel 与基础工具契约。")
            .input("persona spec", "人格描述文件内容。")
            .input("workspace facts", "当前仓库与系统可见事实，只能基于这些事实构造需要新鲜世界状态的 demo。")
            .output("rationale", "为什么这组 demos 能覆盖最关键的行为风险。")
            .output("rule_demo_groups", "按 terminal_answer_rules 分组的 turn demos 列表；每条规则一个 group。")
            .rule("输出多条 demos，覆盖多个关键风险面向，而不是只围绕单一类型反复改写。")
            .rule("应覆盖多个风险轴，而不是机械凑固定题型。")
            .rule("不得输出重复或仅同义改写的 demos。")
            .rule("应优先按 terminal_answer_rules 组织 demo 主轴，再叠加相关的 behavior_rules、tool_use_rules 与 anti_patterns。")
            .rule("如果 persona spec 中存在多条 terminal_answer_rules，必须让每条规则各自对应一个 rule_demo_group；每个 group 至少有一个 demo，复杂规则可以在该 group 下拆成多个 demo。")
            .rule("最终 assistant 必须可直接交付给用户，这个约束要体现在 expected_behavior 与 judge_focus 中。")
            .rule("每个 demo 都应给出 coverage_axes 和 persona_anchors，说明它为何存在以及锚定了 persona 的哪些方向。")
            .rule("每个 rule_demo_group 都必须给出 terminal_answer_rule，并逐条引用 persona spec 中对应的 terminal_answer_rules 原文。")
            .rule("凡是依赖当前世界状态的 demo，都必须把 requires_fresh_world_state 设为 true。")
            .rule("只有当答案不能直接从当前 runtime snapshot 读取、必须额外探索或执行时，才把 must_use_tools 设为 true。")
            .rule("TodoBoard 摘要、事件列表、设备结构状态这些 runtime snapshot 已直接可见的只读状态，可以 requires_fresh_world_state=true 但 must_use_tools=false。")
    }

    fn build_ir(&self, _: &Context, _: &Snapshot) -> PromptIR {
        self.dataset_ir(String::new(), String::new(), String::new())
    }
}

impl RuntimeTurnDemoGeneratorProgram {
    pub fn dataset_ir(
        &self,
        base_runtime_contract: String,
        persona_spec: String,
        workspace_facts: String,
    ) -> PromptIR {
        let mut ir = PromptIR::with_system(RUNTIME_TURN_DEMO_GENERATOR_SYSTEM_PROMPT);
        ir.push_instruction("优先生成能暴露终局性判断错误的 demos，而不是覆盖面泛泛的普通案例。");
        ir.push_instruction("不要机械凑固定数量，也不要为了凑类别而生成低价值 demo；重点是多方面覆盖真实高风险场景。");
        ir.push_instruction("优先从 persona spec 的 terminal_answer_rules 中抽取终局约束；尽量让每个 demo 围绕其中一条主要终局规则展开。");
        ir.push_instruction("如果 terminal_answer_rules 有多条，必须为每条规则输出一个 rule_demo_group；不要把多条规则合并进同一个 group。");
        ir.push_instruction("每个 rule_demo_group 的 terminal_answer_rule 必须逐字引用 persona spec 中对应的 terminal_answer_rules 原文。");
        ir.push_instruction("每个 rule_demo_group 至少包含一个 demo；如果某条规则过于复杂，允许在该 group 下输出多个 demos。");
        ir.push_instruction("demo 的 title、scenario_summary、incoming_text、expected_behavior、judge_focus 默认应跟随 persona spec 中的 language。");
        ir.push_instruction("先从 persona spec 中抽取真正需要检验的行为方向，再把这些方向落成具体 demo。不要让内置题型主导生成。");
        ir.push_instruction("不要把 behavior_rules、tool_use_rules、anti_patterns 各自孤立成题；应把它们作为对 terminal_answer_rules 的交叉约束，组合进同一个 demo。");
        ir.push_instruction("coverage_axes 应描述抽象风险轴，例如 tool_verification、terminal_answer_risk、decision_ownership、world_state_dependency、language_consistency、persona_boundary。");
        ir.push_instruction("persona_anchors 应引用 persona spec 中实际存在的方向名称，例如 language、channel_contract、behavior_rules、terminal_answer_rules、tool_use_rules、anti_patterns。");
        ir.push_instruction("如果 persona 明确要求 agent 自主决策，就至少生成一个会测试‘不要把决策推回给用户’的 demo。");
        ir.push_instruction(
            "如果 persona 明确要求先做事再给结论，就至少生成一个需要工具后再终结的 demo。",
        );
        ir.push_instruction(
            "如果 demo 的答案依赖当前世界状态，请显式把 requires_fresh_world_state 设为 true。",
        );
        ir.push_instruction(
            "只有当答案不能直接从当前 runtime snapshot 读取、必须额外探索或执行时，才把 must_use_tools 设为 true。",
        );
        ir.push_instruction(
            "像 TodoBoard 摘要、事件列表、设备结构状态这类已直接出现在 runtime snapshot 的只读状态，不要机械地要求工具；除非 demo 明确需要额外探索、刷新或执行动作。",
        );
        ir.push_instruction(
            "每个 demo 都应包含清晰、可验证的正确收尾条件；避免那种即使 agent 做得很浅也难以判错的宽泛请求。",
        );
        ir.push_instruction(
            "tool_backed_fact_answer 和 long_request_terminal 应尽量锚定 Spinova 当前真实可见的世界状态，如本地代码库、待办、事件、设备或系统状态。",
        );
        ir.push_instruction(
            "对于依赖仓库事实的 demo，只能引用 workspace facts 中明确出现的文件、目录、模块或系统对象；不要编造 main.py、虚构待办、假设备或未给出的系统状态。",
        );
        ir.push_instruction(
            "short_direct_reply 必须是无需查询当前世界状态即可直接作答的极短消息；其正确结果应当明显不同于阶段性计划文本。",
        );
        ir.push_section("base runtime contract", base_runtime_contract);
        ir.push_section("persona spec", persona_spec);
        ir.push_section("workspace facts", workspace_facts);
        ir
    }
}
