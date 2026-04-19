# Next TODO: Workflow Task-Level Runner

状态：Phase 1–4 均已完成。主线已关闭。

## 已完成

### Prompt 线

- Prompt Improvement Pipeline 与 Workflow Improvement Pipeline 已彻底分离
- prompt 线只消费 `RuntimeTrace`
- prompt frontier 已持久化
- prompt candidate 已进入 executable rollout
- prompt replay 复用真实 turn demo 执行与 judge 聚合

### Workflow 线

- workflow 线只消费 `WorkflowRunRecord`
- workflow frontier 已持久化
- workflow patch / merge 已从启发式迁移到：
  - `reflection -> candidate -> evaluation -> selection`
- workflow candidate 会进入隔离 `WorkflowStore`
- candidate patch / merge 会被真实应用到隔离 workflow store
- rollout case 已不再只是复制旧 record

### Workflow Task-Level Runner（本次完成）

#### Phase 1: WorkflowTaskCase

- 新增 `WorkflowTaskCase` 结构体，作为执行输入侧的抽象
- 字段：`task_summary`、`origin`、`baseline_outcome`、`baseline_turns`、`baseline_tool_actions`、
  `manual_fix_detected`、`rollback_detected`、`failure_types`、`started_at_ms`、`ended_at_ms`、`baseline_run_id`
- `WorkflowRunRecord` 退回为结果证据，不再是主要执行输入

#### Phase 2: WorkflowTaskRunner

- `WorkflowTaskRolloutRunnerState` 所有方法已切换为接受 `&WorkflowTaskCase`：
  - `begin_bound_workflow_session(workflow, task)`
  - `accumulate_task(workflow, task)`
- `simulated_executed_workflow_steps(workflow, task)` 从 task 直接计算 step 执行
- `workflow_rollout_boundary_events(task)` 从 task 直接计算边界事件
- `workflow_rollout_outputs_from_task(workflow, task, steps)` 从 task 驱动 step outputs
- 顶层入口 `run_workflow_task_rollout(workflow, task)` 替代旧的 `simulate_workflow_task_rollout_case`

#### Phase 3: Evaluator 消费 runner 输出

- `replay_workflow_frontier_entry` 已从 records 派生 `WorkflowTaskCase`
- 使用 `select_workflow_task_cases` 选取 task cases
- 使用 `run_workflow_task_rollout` 生成 rollout case
- evaluator 消费 runner 输出（包含 step 执行证据），不再消费旧 run record 的重建版本

#### Phase 4: Task case 替换 replay 采样输入

- `replay_workflow_frontier_entry` 中 target/source evidence 均经由 `workflow_task_case_from_record` 转换
- `select_workflow_rollout_cases`（旧的 record 直接选取）已删除
- sampling 入口从 record 切换为 task case

### 完成标准核对

- workflow rollout 不再主要依赖既有 `WorkflowRunRecord` 反推 step execution：已完成
- workflow rollout runner 能在隔离上下文中推进 task-level execution：已完成
- runner 直接生成 rollout case：已完成
- evaluator 消费 runner 输出：已完成
- `cargo test` 全通过：已完成（62 passed）

## 后续可做（不阻塞主线）

### 更强的 frontier ranking

- 更明确的多目标排序
- 更强的代际保留 / 淘汰
- 更细的 acceptance policy

### lineage / ancestry 增强

- ancestry graph
- lineage diff 解释
- 更强的 dashboard / visualization

### WorkflowTaskCase 独立持久化

- 当前 task case 从 run records 按需派生，不单独持久化
- 后续可考虑持久化 task cases，支持合成 / 手动注入 task cases
