# 贡献指南

Daat Locus 是一个 runtime 项目，不是 prompt 集合。贡献应当保留那些让状态明确、行动可审计、经验可复用的边界。

如果你要修改 runtime 对象、工具、App、workflow、memory、sleep、daemon 行为或持久化，请先阅读 [架构说明](docs/architecture_zh-CN.md)。面向 coding agent 的约束见 [AGENTS.md](AGENTS.md)。

## 设计规则

- 普通 assistant 文本不能产生外部副作用。
- 优先使用显式 id，而不是隐藏的当前选中游标。
- 保持 `Event`、`PendingWork`、`App`、`Plan`、`Workflow`、`Memory` 和 `Sleep` 的概念分离。
- 让代码负责查找、去重、freshness 检查、持久化、delivery bookkeeping 和 evidence 记录等机械工作。
- 对有状态操作表面使用 App-scoped tools，不要扩张成一个全局平铺工具列表。
- 保持 runtime error correction 与 workflow improvement 分离。
- Workflow 变更应当视为对可复用执行资产的变更，而不是 prompt 编辑或聊天记忆。

## 质量门禁

当前 CI 会运行：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo deny --locked check bans sources licenses
```

提交修改前，请在本地运行相关检查。对于高风险 runtime 变更，应增加聚焦测试，或写清楚人工验证结果。

## TUI 性能命令

`tui-perf-cmd` 会启用一个隐藏的开发者命令，用于确定性地检查 dashboard
渲染性能。它是非默认 feature，不属于普通 CLI 功能面。

运行默认 mixed 场景：

```bash
cargo run --features tui-perf-cmd -- dev tui-perf \
  --scenario mixed --frames 120 --warmup 10 --width 120 --height 40
```

为脚本输出 JSON：

```bash
cargo run --features tui-perf-cmd -- dev tui-perf \
  --scenario long-history --frames 240 --warmup 20 --width 140 --height 48 --json
```

可用场景：

- `mixed`：committed activity、live cells、skills toggle panel、markdown、diff、
  browser、terminal 和 reply cells。
- `long-history`：大量 committed activity cells，并设置显式滚动位置。
- `live-activity`：活跃 runtime status 和 live activity cells。
- `command-panels`：skills list panel 和 command bar 渲染。

该命令使用真实 TUI 的 dashboard frame 渲染函数，但通过
`ratatui::backend::TestBackend` 运行，不进入真实 terminal alternate screen。
报告包含 frame、prep、draw、activity、command timing，以及 activity render
cache 的 hit/miss 计数。

当 TUI 改动可能影响渲染成本或 frame scheduling 时使用这个命令。绝对毫秒数只适合作为本机数据；更可靠的方式是在相邻 revision 之间比较同一场景的指标，而不是设置很紧的跨机器阈值。

## Commit Message

Commit message 必须使用英文。标题应当说明真实改变的行为或边界，例如：

- `Fix Telegram event completion retry`
- `Add workspace app notice polling tests`
- `Document workflow sleep evidence model`

避免使用 `fix`、`update`、`misc`、`wip`、`cleanup` 这类含糊标题。

## 添加或修改工具

添加或修改工具时：

- 说明它读取或改变什么 world state；
- 在可能存在 stale state 的地方要求显式标识符；
- 在声明和执行阶段都校验 schema；
- 判断该工具是否会产生 turn boundary；
- 测试非法参数和工具可见性；
- 只有当误用可以被代码检测时，才加入 runtime error evidence。

当可以使用具体 id 或 freshness guard 时，不要依赖隐藏 UI 状态。

## 添加或修改 App

App 是可聚焦、有状态的操作表面。

修改 App 时：

- 保持 `state`、`usage` 和 `how_to_use` 分离；
- 在相关时定义 focus / blur 行为；
- 保持 dynamic tools 归属于该 App；
- 保持 app notice 显式且可 resolution；
- 不要把可复用任务流程写进 App prompt docs。

Telegram 这类 transport 默认不是 App。如果代码已经收到了结构化外部事实，应把它建模成 event source，而不是要求模型去导航的界面。

## 修改 Workflow 或 Sleep

修改 workflow 或 sleep 行为时：

- 声明消费的 evidence 类型；
- 声明产生的持久化 artifact；
- 保持 runtime protocol correction 与 workflow process improvement 分离；
- 不要把完整原始对话流喂给 runtime error correction；
- 不要用 runtime protocol error 直接 patch workflow；
- 不要让 sleep 修改 builtin workflow。

Runtime error correction 改的是全局工具和协议约束。Workflow improvement 改的是某类任务的可复用执行流程。

## 高风险区域

以下区域视为高风险：

- daemon auth 与生命周期；
- runtime turn scheduling、context compaction 和 pending work；
- event completion 与 Telegram delivery；
- terminal process management；
- browser reference freshness；
- filesystem sandbox；
- workspace app worker；
- provider credentials 与 OAuth storage；
- Hindsight retain / recall 集成；
- sleep-time contract 与 workflow evolution。

高风险修改应当尽量小、可审查，并在可行时加入针对性测试。
