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
- frontier 在选择前会经过 sampled rollout replay judge 复评，而不是单轮直接执行
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
- `sampled rollout replay judge`
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
- `sampled rollout replay judge`
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

### Replay

当前 replay 不是“单批次最近样本”，而是 sampled rollout style：

- prompt replay：最近 trace 按 batch 切片，多批次 judge 后聚合
- workflow replay：最近 run evidence 按 batch 切片，多批次 judge 后聚合

这一步已经完成了从：

- `single minibatch judge`

到：

- `sampled rollout replay + aggregation`

的迁移。

## 完成标准核对

- 双管线保持独立：已完成
- 没有新增 runtime 主账本：已完成
- artifacts 只承担审计和可视化：已完成
- builtin workflows 不可被优化管线修改：已完成
- `cargo test` 全通过：已完成

## 后续仅剩研究性增强

这些不是当前迁移阻塞项，也不再属于本 TODO 的“未完成”部分：

- 把 replay 从 sampled rollout judge 提升到真实 executable rollout
- 引入更强的多目标 frontier ranking，而不只是当前非支配筛选 + replay score
- 给 lineage 增加 ancestry graph 可视化，而不只是统计摘要
- 决定是否引入跨 sleep 的更显式代际保留/淘汰策略
