# GEPA-Style Sleep Migration Status

状态：主迁移已完成。

目标已经落地：

- Prompt Improvement Pipeline 与 Workflow Improvement Pipeline 已彻底分离
- prompt 线只消费 `RuntimeTrace`
- workflow 线只消费 `WorkflowRunRecord`
- 两条主链都不再依赖代码内启发式反思、启发式评分或 token overlap merge 决策
- sleep 已从“直接 patch”迁移到：
  - `reflection -> candidate -> evaluation -> selection`
- frontier / pool 已持久化并进入生产主链
- frontier 已有 lineage：
  - `parent_keys`
  - `generation`
- frontier 在选择前会经过逐 case rollout evaluator 复评，而不是批量摘要 judge
- artifacts / dashboard / summary 都已跟上新模型

## 当前完成态

### Prompt 管线

当前主链：

- `RuntimeTrace`
- `failure_patterns` 仅作为 prompt-side audit artifact
- `prompt_reflections`
- `prompt_candidates`
- `prompt_candidate_evaluations`
- `prompt_frontier`
- `case-level rollout evaluator`
- `frontier selection`
- `compiled runtime prompt update`

已满足：

- prompt additions 不再直接由 failure patterns 生成
- 所有 prompt 更新都经过：
  - reflection
  - candidate generation
  - candidate evaluation
  - replay reevaluation
  - selection

### Workflow 管线

当前主链：

- `WorkflowRunRecord`
- `workflow_reflections`
- `workflow patch / merge candidates`
- `workflow candidate evaluations`
- `workflow_frontier`
- `case-level rollout evaluator`
- `frontier selection`
- `workspace workflow patch / merge apply`

已满足：

- sleep 不再根据计数阈值直接构造 patch
- merge 不再由 token overlap 直接决定
- 所有 workflow 更新都经过：
  - reflection
  - candidate generation
  - candidate evaluation
  - replay reevaluation
  - selection

### Frontier / Lineage

当前能力：

- prompt frontier：`~/.daat-locus/state/sleep_frontiers/prompt_frontier.json`
- workflow frontier：`~/.daat-locus/state/sleep_frontiers/workflow_frontier.json`
- 非支配筛选已接入生产主链
- lineage 字段：
  - `parent_keys`
  - `generation`
- lineage stats 已进入 summary / dashboard：
  - `root_entries`
  - `branched_entries`
  - `max_generation`

### Rollout Evaluation

当前 replay 已升级为 executable rollout：

- prompt replay：candidate prompt additions 会进入隔离 `CompiledPromptStore`，再执行真实 turn demos 并聚合为 frontier evaluation
- workflow replay：candidate workflow 会进入隔离 `WorkflowStore`，真实应用 patch/merge，并显式模拟
  - workflow binding
  - session accumulate
  - work flush boundary
  - outcome collection

这一步已经完成了从：

- `batched evidence judge`

到：

- `executable rollout + frontier aggregation`

的迁移。

## 完成标准核对

- 双管线保持独立：已完成
- 没有新增 runtime 主账本：已完成
- artifacts 只承担审计和可视化：已完成
- builtin workflows 不可被优化管线修改：已完成
- `cargo test` 全通过：已完成

## 下一阶段：Executable Rollout

主迁移已经完成，下一阶段的主目标是把当前 `case-level rollout evaluator` 提升为真正的 `executable rollout`。

实施顺序固定为：

1. 先做 Prompt executable rollout
2. 再做 Workflow executable rollout
3. 最后再考虑更强的 frontier ranking

原因：

- prompt 线已经有现成的可执行基础设施：`TurnRolloutRunner` / `run_cold_start_turn_demo`
- workflow 线虽然证据边界更干净，但缺少现成的“给定 candidate spec 后重放任务”的执行器

### 1. Prompt executable rollout

状态：已完成。

优先复用的现有代码：

- `src/reasoning/turn_compile.rs`
  - `TurnRolloutRunner`
  - `run_cold_start_turn_demo`
  - `RuntimeTurnTraceJudgeProgram`
  - `IsolatedEvalContext`

当前实现：

- prompt frontier 复评已不再走 case-level LLM evaluator
- 现在会把 candidate prompt additions 临时应用到隔离 `CompiledPromptStore`
- 复用 `TurnCompileEngine::evaluate_cold_start` 执行 turn demos
- 再由现有 turn trace judge 聚合成 frontier evaluation

当前数据来源：

- sleep 中由 `runtime_demos -> turn_demos` 自动投影

完成标准：已满足

### 2. Workflow executable rollout

状态：v1 已完成，并已补上显式 bind/session/flush 边界模拟。

当前实现：

- workflow frontier 复评不再直接拿原始 candidate 文本 judging
- 现在会在隔离 `DAAT_LOCUS_HOME` 下构建真实 `WorkflowStore`
- 把 target/source workflows 写入临时 store
- 真实应用 patch/merge candidate
- 再把“rollout 后的 target workflow spec + rollout result summary + 显式模拟出的 WorkflowRunRecord rollout case”
  喂给 workflow rollout evaluator 聚合
- workflow rollout case 不再是简单复制原 record，而是通过最小 runner-state 明确模拟：
  - bind workflow
  - 按 `workflow_steps` 逐步生成 rollout outputs
  - 逐步 accumulate session evidence
  - queue flush
  - collect flushed run record
- rollout case 现在还会附带 step-level execution evidence：
  - executed steps
  - boundary events
  - 哪一步在 blocked / no_progress / abandoned 边界停下

也就是说，workflow candidate 已经真实进入隔离执行上下文，而不是只作为静态文本被评估。

初版不要求完全重建真实外部世界，但要求：

- candidate workflow spec 真正进入执行上下文
- runner 明确模拟：
  - workflow binding
  - work execution boundary
  - outcome collection

注意：

- builtin workflows 仍不可修改
- rollout 一律在隔离上下文中执行，不污染主 runtime state

完成标准：

- workflow frontier 复评主链从当前 `WorkflowCandidateRolloutEvaluatorProgram`
  切到“真实 workflow rollout + outcome judge 聚合”

### 3. Frontier ranking 之后再增强

只有在 executable rollout 稳定后，再继续：

- 更强的多目标 frontier ranking
- ancestry graph 可视化
- 更显式的代际保留 / 淘汰策略
