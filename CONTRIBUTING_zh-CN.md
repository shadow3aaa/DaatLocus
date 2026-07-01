# 贡献指南

Daat Locus 是 runtime 项目，不是 prompt 集合。贡献应当保留那些让状态明确、行动可审计、经验可复用的边界。

如果你要修改 runtime 对象、工具、App、workflow、memory、sleep、daemon 行为、sessions、dashboard clients 或持久化，请先阅读 [架构说明](docs/architecture_zh-CN.md)。面向 coding agent 的约束见 [AGENTS.md](AGENTS.md)。

## 设计规则

- 普通 assistant 文本不能产生外部副作用。
- 优先使用显式 id 和 freshness guard，而不是隐藏的当前选中游标。
- 保持 `Event`、`PendingWork`、`App`、`Plan`、`Workflow`、`Memory`、`Sleep`、`Manager` 和 `Session` 的概念分离。
- App 工具直接通过 namespace 调用。不要重新引入 app focus 或 activation 作为工具暴露 gate。
- Telegram 是 transport 和 event source，不是 App。
- 静态文件工具是 `read_file` 和 `edit_file`；它们是 runtime tools，不属于某个 App namespace。
- 让代码负责查找、去重、freshness 检查、持久化、delivery bookkeeping、schema validation 和 evidence recording 等机械工作。
- 保持 AfterClaim Context、PreTurn Context、capability docs、App state 和 memory 分层。
- 保持 runtime error correction 与 workflow improvement 分离。
- Workflow 变更应当视为对可复用 SOP skill assets 的变更，而不是 prompt 编辑或聊天记忆。

## 质量门禁

当前 CI 会运行：

```bash
cargo fmt --all -- --check
cd webui && bun install --frozen-lockfile && bun run test
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo deny --locked check bans sources licenses
```

提交修改前，请在本地运行相关检查。对于高风险 runtime 变更，应增加聚焦测试，或写清楚人工验证结果。

## TUI 性能命令

`tui-perf-cmd` 会启用一个隐藏的开发者命令，用于确定性地检查 dashboard 渲染性能。它是非默认 feature，不属于普通 CLI 功能面。

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

- `mixed`：committed activity、live cells、skills toggle panel、markdown、diff、browser、terminal 和 reply cells。
- `long-history`：大量 committed activity cells，并设置显式滚动位置。
- `scrolling`：在大型 activity list 上执行确定性滚动。
- `live-activity`：活跃 runtime status 和 live activity cells。
- `command-panels`：skills list panel 和 command bar 渲染。

该命令使用真实 TUI 的 dashboard frame 渲染函数，但通过 `ratatui::backend::TestBackend` 运行，不进入真实 terminal alternate screen。报告包含 frame、prep、draw、activity、command timing，以及 activity render cache 的 hit/miss 计数。

当 TUI 改动可能影响渲染成本或 frame scheduling 时使用这个命令。绝对毫秒数只适合作为本机数据；更可靠的方式是在相邻 revision 之间比较同一场景的指标，而不是设置很紧的跨机器阈值。

## Commit Message

Commit message 必须使用英文。标题应当说明真实改变的行为或边界，例如：

- `Fix Telegram event completion retry`
- `Add workspace app notice polling tests`
- `Document workflow sleep evidence model`

避免使用 `fix`、`update`、`misc`、`wip`、`cleanup` 这类含糊标题。

## 添加或修改工具

添加或修改 model-facing tool 时：

- 说明它读取或改变什么 world state；
- 在可能存在 stale state 的地方要求显式标识符；
- 在声明和执行阶段都校验 schema；
- 使用保守 model-facing JSON Schema dialect；
- 不要引入 `Finished`、`Error`、`Compacted`、`Interrupt` 之外的 turn stop reason；
- 测试非法参数和工具可见性；
- 只有当误用可以被代码检测时，才加入 runtime error evidence。

当可以使用具体 id 或 freshness guard 时，不要依赖隐藏 UI 状态。

## 添加或修改 App

App 是有状态能力域，不是 focus gate。

修改 App 时：

- 保持 `state` 和 `docs` 分离；
- 通过 App namespace 暴露 model-facing tools；
- 保持生成的 `appid__get_state` surface 准确且便宜；
- 保持 app notice 显式且可 resolution；
- 让操作绑定 page id、terminal session id、path 或 app-specific object id 等显式标识符；
- 不要把可复用任务流程写进 App prompt docs；
- 除非明确重设计 runtime contract，否则不要加入 focus/blur 要求。

Telegram 这类 transport 默认不是 App。如果代码已经收到了结构化外部事实，应把它建模成 event source，而不是要求模型去导航的界面。

Workspace Apps 当前从 `runtime/app.lua` 加载一个 Lua 5.4 module。支持的 hook surface 是 `config`、`init`、`render_state`、`list_tools`、`call_tool` 和 `poll_notices`；不要记录或依赖已经移除的 `on_focus` / `on_blur` hooks。

## 修改 Coding 或文件工具

Coding source operations 使用 `path + line#hash` anchors：

- `coding__search_code` 返回 matched source lines。
- `coding__read_code` 接受 path plus anchor，以及 `around` 或 `full` mode。
- `coding__edit_code` 通过 SCOPE validation 和 review 应用 structured edits。
- `read_file` 处理显式 file/range reads。
- `edit_file` 处理非 SCOPE 的 ordinary file edits。

当 Coding project 已打开时，`edit_file` 不应编辑 SCOPE-owned source files。请使用 `coding__edit_code`，以便保留 propagation review。

## 修改 Manager、Sessions 或 Transports

Manager 是唯一 public server。Session processes 是私有 runtime workers，只能通过 Manager-owned IPC 访问。

修改这一区域时：

- 保持 public clients 连接 Manager，而不是直连 Session endpoints；
- 保持 runtime `Context`、`EventStore`、`PendingWorkQueue`、`Plan`、memory、apps 和 model loop 位于一个 Session 内；
- 保持 Manager 负责 public auth、session registry、lifecycle、routing 和 Telegram default-session mapping；
- 不要把 IPC names、tokens、process ids 或 lifecycle internals 作为普通 WebUI/TUI/API state 暴露；
- 保持 `daat-locus code <project-dir>` 是 multi-session project selector，而不是 project-to-single-session mapping。

## 修改 Dashboard Clients

`DashboardState` 是共享 session/runtime state。`TuiViewState` 是单个 TUI client 的本地状态。不要通过修改共享 dashboard state 来解决本地 TUI 行为。

TUI rendering 应保持由 `FrameRequester` 调度的纯 full-frame projection。Input handling 应把输入规约成本地 view-state 变化或显式 `DashboardAction` effect；它不应直接 render。

WebUI session rendering 应使用结构化 dashboard 和 activity-cell 数据。当应该存在 typed contract 时，不要把渲染后的 TUI 字符串、prose 或 command output 解析成 web structure。

## 修改 Workflow 或 Sleep

修改 workflow 或 sleep 行为时：

- 声明消费的 evidence 类型；
- 声明产生的持久化 artifact；
- 保持 runtime protocol correction 与 workflow process improvement 分离；
- 不要把完整原始对话流喂给 runtime error correction；
- 不要用 runtime protocol error 直接 patch workflow；
- 不要让 sleep 修改 builtin workflow；
- 默认不要把临时 skill compositions 持久化为新的 skill specs。

Runtime error correction 改的是全局工具和协议约束。Workflow improvement 改的是某类任务的可复用 SOP skill specs。

## 高风险区域

以下区域视为高风险：

- daemon auth 与生命周期；
- Manager/Session IPC 与 session registry；
- runtime turn scheduling、context compaction 和 pending work；
- event completion 与 Telegram delivery；
- terminal process management；
- browser reference freshness；
- filesystem sandbox；
- Coding/SCOPE edit 与 propagation review 边界；
- workspace app worker lifecycle 与 schema validation；
- provider credentials 与 OAuth storage；
- Hindsight retain / recall 集成；
- sleep-time contract 与 workflow evolution。

高风险修改应当尽量小、可审查，并在可行时加入针对性测试。
