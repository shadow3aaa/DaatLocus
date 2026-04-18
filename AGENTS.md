# Daat Locus Agent Guidelines

本文档定义 Daat Locus 当前代码实现对应的 agent-facing 边界。

目标不是写一份抽象口号，而是给未来修改 `app`、`events`、`runtime_tools`、`snapshot`、`telegram_transport`、`workflow`、`memory`、`sleep` 等模块时提供一套与代码一致的设计约束。

## Project Reality

- Daat Locus 是一个长期运行的、工具驱动的 agent。
- 它的主循环不是“用户发一句话，模型回一句话”的 chat app，而是“世界状态进入快照，模型判断，再通过工具改世界”的 runtime。
- 外部输入主要通过 `Event`、`PendingWork`、后台 app notice 和自动召回记忆进入当前 turn。
- 普通 assistant 文本默认只是运行时中的解释和中间记录，不会自动发送给 Telegram 或其他外部系统。
- 世界发生真实变化，必须依赖显式 tool call。

## Non-Negotiables

- Telegram 不是 `App`，而是 transport 和 event source。
- 常规事件收尾不能只输出文本，必须显式调用 `finish_and_send`。
- Browser / Terminal 是 `App`，因为它们代表需要聚焦和继续操作的交互表面。
- `App` 和 `Event` 是并列概念，不要互相偷换。
- 让模型做语义判断，不要让模型做代码已经能完成的机械枚举、定位、去重和 freshness 校验。

## Runtime Model

一次 runtime turn 通常包含这些层：

1. 系统 prompt contract
2. 记忆与历史消息
3. 当前世界快照 `world_snapshot`
4. 模型输出文本或工具调用
5. 工具执行结果回写到历史
6. 必要时进入下一轮 tool cycle

当前快照至少覆盖：

- sensory：时间和机器状态
- plan：当前分步计划
- workflows：当前绑定 workflow 与候选 workflow 摘要
- events：待处理事件
- apps：当前前景 app 与 app 结构状态
- memories：自动召回的长期记忆

因此，新增 agent-facing 接口时，要先判断它属于哪个层，而不是直接堆进某个 app 状态里。

## Core Objects

### App

`App` 是“需要被聚焦后才合理操作的交互表面”。

当前实现只有两个：

- `Browser`
- `Terminal`

一个东西只有同时满足下列条件，才应该建模为 `App`：

- 模型必须先把注意力切到它，后续操作才成立
- 可见信息天然是局部的，需要逐步探索
- 操作具有时间语义，例如等待加载、继续交互、处理中会话、稳定后再读取

每个 `App` 必须向模型暴露三个分离层：

- `state`：当前结构化可见事实
- `usage`：这个 app 是干什么的，什么时候值得 focus
- `how_to_use`：focus 之后如何正确操作

这三层不要混写。

- `state` 不是操作手册
- `usage` 不是完整教程
- `how_to_use` 不是世界状态

当前代码里这套分层主要体现在 `App::render_state`、`usage()`、`how_to_use()`。

不要再把可自优化的任务执行流程塞进 app 的补充说明层。跨任务可复用的方法资产应单独建模为 `Workflow`，而不是 app 的局部附属文本。

### Event

`Event` 是“系统已经收到了一个结构化外部事实，现在需要模型做语义判断”的对象。

当前实现里真正进入 `EventStore` 的 payload 只有：

- `TelegramIncoming`

事件回答的问题是：

- 刚刚发生了什么
- 是否需要响应
- 应该以什么 disposition 结束

事件不是会话游标，不是 app 内部选中项，也不是导航过程。

### PendingWork

`PendingWork` 是驱动主循环的调度单元，不等于 `Event`。

当前实现有两类：

- `PendingWork::Event { event_id }`
- `PendingWork::AppNotice { app, reason }`

规则：

- event 优先级高于 app notice
- queue 负责调度，不负责语义判断
- queue 可以 claim / release / consume / requeue_front

不要把 queue 当作另一个业务状态机；它只是驱动下一轮 turn 的执行入口。

### Plan

`Plan` 是当前任务的最新分步执行计划，不是 backlog 数据库。

当前实现要求：

- 非空 plan 在“尚未全部完成”时，必须恰好有一个 `in_progress`
- 全部完成时，plan 应直接清空，而不是保留一组已完成步骤
- 每次 `update_plan` 都提交完整 plan，而不是增量 patch

不要把 plan 用成：

- 长期知识库
- 事件列表镜像
- 隐式游标

### Workflow

`Workflow` 是“针对某类任务的可复用执行规范”，不是模型内在能力，也不是某个 app 的局部附属说明。

它回答的问题是：

- 这类任务什么时候适合复用一套稳定流程
- 这套流程通常按什么顺序推进
- 做到什么程度算完成
- 失败或阻塞时应该如何稳定恢复

`Workflow` 必须拆成三层：

- `WorkflowSpec`：workflow 本体，是 agent-facing 规范资产
- `WorkflowBinding`：当前任务是否绑定某个 workflow，只属于 runtime 状态
- `WorkflowRunRecord`：白天执行后自动沉淀的运行证据，供 sleep 使用；当前实现要求在 work 完成边界直接写入，而不是由 sleep 事后回放生成

规则：

- `WorkflowSpec` 不承载运行期选中态，不承载“active”这类瞬时状态
- `WorkflowBinding` 只表示当前任务正在采用哪个 workflow，不写回 workflow 本体
- `WorkflowRunRecord` 由代码自动记录，不要求模型手工写 daytime outcome log
- workflow 的主要演化动作是 `patch` 和 `merge`
- v1 不引入 `deprecate`
- v1 不要求语义搜索；直接在快照展示候选 workflow 即可
- workflow evolution 必须只依赖 workflow-bound execution evidence，不依赖 error demo、failure pattern 或 prompt evaluation artifacts

不要把 workflow 用成：

- plan 的长期化镜像
- 运行时隐式状态槽
- 自动生成的默认模板集合
- 需要模型自己记账的绩效表

### Memory

`Memory` 由两部分组成：

- runtime conversation：当前线程上下文
- hindsight queue：待 retain/已 retain 的长期记忆队列

它服务于线程延续和长期经验积累，不服务于机械状态同步。

### Sleep / Self-Improvement

Daat Locus 有显式的自我改进闭环：

- runtime trace
- sleep
- turn compile
- compiled prompt additions

这意味着运行时设计不是一次性的。任何 agent-facing 接口设计如果会系统性诱导错误行为，最终都会污染 trace 与 workflow run evidence，并影响后续 compile。

所以接口设计要偏稳定、显式、可评审，不要依赖模糊约定。

sleep 内部必须明确分成两条独立 pipeline：

- `Prompt Improvement Pipeline`
- `Workflow Improvement Pipeline`

二者可以在同一次 sleep 中并行运行，但不能互相作为输入依赖。

`Prompt Improvement Pipeline` 负责：

- 只基于 runtime trace 修正 system prompt 与行为约束
- 直接产出 prompt patch、compile artifacts
- failure pattern、bootstrap demo、stress case 等如果存在，也只允许作为 trace 内部分析产物，不得成为独立中间证据层

`Workflow Improvement Pipeline` 负责：

- 只基于 `WorkflowRunRecord` 修正 workflow spec
- 产出 workflow patch、workflow merge

明确禁止：

- 用 runtime review、error demo 或 failure pattern 直接驱动 workflow patch
- 用 workflow merge/patch 结果反向充当 prompt compile 的证据

要区分清楚两类对象：

- prompt compile 修的是“模型该怎么想、怎么决策”
- workflow evolution 修的是“这类任务通常该按什么流程做”

## Current App Semantics

### Terminal

`Terminal` 是本地命令执行和持续进程交互界面。

它之所以是 `App`，不是因为“命令行很重要”，而是因为：

- 会话会持续存在
- 需要等待输出
- 需要继续写 stdin
- 存在前景/后台注意力差异

操作约束：

- 只能通过 `terminal_exec` / `terminal_write_stdin` / `terminal_terminate`
- 禁止把交互式全屏程序当常规路径
- 禁止把交互式登录/认证流程交给模型接管
- session 是显式地址，不允许隐藏选中态

### Browser

`Browser` 是网页查看与交互界面。

它之所以是 `App`，因为：

- 页面内容天然是局部和时序性的
- 需要等待加载
- 需要读取语义快照后拿到 `element_ref`
- 后续交互依赖 page session 持续存在

操作约束：

- 只通过 browser tools 操作
- 交互必须显式提供 `page_id + element_ref`
- 页面变化导致 ref 失效时，应重新读取页面，不应盲重试旧 ref
- 搜索结果页通常只是线索定位，不是最终事实来源

## Third-Party App Package

未来的第三方 `App` 扩展按 source-first 方式设计，不复制 Codex 的 plugin / connector 结构。

### Directory Placement

- 第三方 App 源码目录固定在 runtime workspace 下：`~/daat-locus-workspace/apps/<app-name>/`
- 当前 runtime workspace 默认由 `resolve_runtime_workspace_dir()` 决定，即 `~/daat-locus-workspace`
- `app_id` 直接等于文件夹名 `<app-name>`
- `~/.daat-locus` 是受保护 runtime 目录，不存放第三方 App 源码
- 之所以这样设计，是因为 `~/.daat-locus` 在 sandbox 中被视为 protected runtime path，而 workspace 是默认的工作区

### Package Layout

最小目录结构：

```text
~/daat-locus-workspace/apps/<app-name>/
  app.toml
  runtime/
    app.lua
  prompt/
    usage.md
    how_to_use.md
```

规则：

- `runtime/app.lua` 是唯一的 Lua 主入口
- `prompt/usage.md` 是 app 的 pre-focus 说明
- `prompt/how_to_use.md` 是 app 的 post-focus 说明
- 不在第三方 app 包内承载可自优化 workflow 资产

### `app.toml`

`app.toml` 在 v1 里极简化，只承担一个职责：指定 Lua 主入口相对路径。

规则：

- 不承载 `id`
- 不承载权限
- 不承载 usage/how_to_use/workflow 元数据
- 默认情况下，主入口约定为 `runtime/app.lua`

最小示例：

```toml
entry = "runtime/app.lua"
```

身份来自目录名，配置才来自 `app.toml`。

### Lua Runtime

第三方 App 的运行时技术栈固定为：

- Rust 侧使用 `mlua`
- Lua 方言使用标准 `Lua 5.4`
- 不使用旧的 `rlua`
- 不使用 `JS/TS` 作为 v1 App 运行时
- 不使用 `Wasm` 作为 v1 App 运行时

这样做的原因是：

- agent 需要能够直接编写和修改 app
- source-first 的 Lua + Markdown 比 ABI-first 的 Wasm 更适合作为 v1 authoring format
- `mlua` 在 Rust 中对 Lua 5.4 的支持足够成熟，适合做宿主嵌入

### Unified Lua Interface

不要把一个 app 设计成多个彼此独立的 Lua 入口脚本。

正确模型是：

- 一个第三方 `App` = 一个统一的 Lua 模块实例
- 宿主只加载 `runtime/app.lua`
- `render_state`、tool 调用、notice 轮询共享同一份 app 实例状态

不要引入额外 IPC 来同步 tool 结果和 render state。

这意味着第三方 app 的行为本体是一个对象模型，不是脚本集合。

### Workflow Assets

可自优化 workflow 不属于 app package，而属于 runtime 级资产。

规则：

- workflow 默认不挂在任何 app 上
- 内置 workflow 位于仓库根目录 `workflows/*.md`
- workspace 拓展 workflow 位于 `~/daat-locus-workspace/workflows/*.md`
- 每个 workflow 一个 markdown 文件，文件名就是 workflow id
- workflow 使用 frontmatter + markdown 正文 schema
- workflow 是跨任务可复用的执行规范，不是 app 的局部说明
- `prompt/*.md` 负责 app 说明；`workflows/*.md` 负责可自优化执行流程；两者不要混写

### Reload Strategy

第三方 app 不应每轮 turn 全量重解析。

推荐策略：

- 启动时全量扫描一次 `~/daat-locus-workspace/apps`
- workflow 目录 `~/daat-locus-workspace/workflows` 单独扫描与监听
- 运行时使用 `notify` 监听受支持目录变化
- 根据文件事件定位到受影响的 `<app-name>`
- 只把该 app 标记为 dirty 并按 app 增量重载
- workflow 文件变化时，只把受影响 workflow 标记为 dirty 并增量重载
- watcher 异常或目录状态不可信时，再回退到一次全量 rescan

不要把全量解析作为常规路径。

### State and Cache

v1 不设计专门的第三方 App cache 目录。

当前结论：

- 只定义 source 目录：`~/daat-locus-workspace/apps`
- workflow source 目录单独定义为 `~/daat-locus-workspace/workflows`
- 不定义 `cache/apps`
- 不定义 `cache/workflows`
- 如果未来确实需要宿主持久化某个 app 的运行状态，再使用受保护的 runtime state 体系，例如 `~/.daat-locus/state/apps/<app-name>/`
- 如果未来需要宿主持久化 workflow telemetry，再使用受保护的 runtime state 体系，例如 `~/.daat-locus/state/workflows/`

第三方 app 与 workflow spec 都是 agent 可编辑资产，但不是 agent 直接拥有的运行时状态。

## Current Event Semantics

### Telegram

Telegram 在当前代码中是：

- 输入侧：`TelegramTransport` 轮询 Bot API，注册 incoming event
- 状态侧：`TelegramTransportState` 维护已知 chat 和 outbox
- 发送侧：完成事件时将消息入 outbox，由 transport 异步投递

Telegram 不是 `App`。

原因：

- 新消息到达时，代码已经知道足够多的结构化事实
- 常规处理任务是“判定并回复”，不是“先导航到某个聊天界面再探索”
- 标准动作应该绑定 `event_id` 和显式 `chat_id`，而不是隐藏 cursor

当前 approved Telegram message 的标准路径：

1. transport 收到消息
2. 生成 `TelegramIncomingEvent`
3. 注册到 `EventStore`
4. enqueue `PendingWork::Event`
5. runtime claim event
6. 模型做判断与工具调用
7. 使用 `finish_and_send` 终结事件
8. transport 从 outbox 投递并更新事件状态

unknown Telegram chat 不进入普通事件处理路径，而是进入 ACL pending 流程。

## Resolution Rules

所有 resolution 都应绑定具体事件，而不是容器。

当前最低要求：

- 通过 `event_id` 操作事件
- disposition 明确为 `resolved` / `dismissed` / `failed`
- `resolved` 或 `failed` 时必须提供非空 `reply_message`

事件状态流当前包括：

- `Pending`
- `Claimed`
- `AwaitingDelivery`
- `Resolved`
- `Dismissed`
- `Failed`

设计新事件类型时，遵守这些原则：

- 如果世界上存在“新旧事件冲突”风险，动作必须绑定具体版本或等价 freshness guard
- 不要只按 `chat_id`、`thread_id`、`page_id` 这类容器 id 做终结
- 失败状态应允许重试或重新验证，而不是静默吞掉

## Tool Design Rules

### General

- 工具应该显式改世界。
- 普通文本不应隐式触发副作用。
- tool 参数必须尽量使用显式标识，而不是依赖隐藏前序选择。
- 一个正常操作应尽量在一次明确调用里完成。

### App-Scoped Tools

app-scoped tool 可以要求先 `focus_app`。

这不是多余仪式，而是为了保留注意力纪律：

- 当前前景 app 决定可用 tool scope
- `focus_app` / `put_away_app` 会触发 turn boundary，要求重新渲染世界状态

因此，不要设计成“明明属于 Browser/Terminal 的操作，却能在任意上下文悄悄执行”。

### Event Tools

事件终结工具必须：

- 显式接收 `event_id`
- 对需要向用户提交最终答复的收尾显式接收 `reply_message`
- `dismissed` 只用于静默结束；`failed` 仍然应向用户发送失败说明

不要把“最终回复”设计成 assistant 文本本身。

### Plan Tools

`update_plan` 只维护当前完整 plan。

不要新增“append_plan_step”或“select_plan_step”这类会引入隐藏游标和增量同步复杂度的工具，除非有强证据说明当前 contract 不够用。

### Workflow Tools

workflow 的职责是提供跨任务可复用的执行规范，不是承载动态世界状态。

当前规则：

- workflow 列表会以摘要形式直接出现在快照里
- 当前绑定的 workflow 会以更完整的形式暴露给模型
- v1 只需要 `create_workflow` 和 `activate_workflow`（或等价的 bind tool）
- 不要引入 `select_workflow` 语义搜索；候选 workflow 应直接由快照展示
- 不要引入显式 `log_workflow_outcome`；白天证据应由代码自动写入 `WorkflowRunRecord`
- 是否绑定 workflow 由任务复杂度和可复用性驱动，而不是由 `focus_app` 决定

## What Code Should Do

代码负责：

- 轮询和接收 Telegram 更新
- 去重事件
- 持久化状态
- 加载与持久化 workflow specs
- 在 work 完成边界直接写入 `WorkflowRunRecord`
- claim / release / requeue pending work
- 维护 outbox
- 加载快照
- 控制 tool scope
- 记录 trace
- 分别执行 prompt compile 和 workflow evolution

不要把这些工作转嫁给模型。

尤其不要让模型反复做：

- list
- select
- open
- read latest state
- dedupe
- freshness check
- delivery bookkeeping

## What The Model Should Do

模型负责：

- 理解事件语义
- 判断是否需要回复
- 选择是否 focus 某个 app
- 判断是否需要创建或绑定 workflow
- 规划步骤
- 选择工具
- 在需要时调用 `deep_recall`
- 给出最终 reply_message

如果一个新接口主要让模型做机械定位，它大概率设计错了。

## Snapshot Rules

快照应提供“足够做判断”的信息，而不是逼模型做机械探索。

允许进入快照的内容：

- 当前前景 app
- app 结构化状态
- 当前绑定 workflow
- workflow 摘要
- 事件摘要
- plan
- 记忆摘录
- 机器状态

不应进入快照的内容：

- 隐藏多步 choreographies
- 长期选中游标
- 本该由工具参数显式提供的定位信息
- 过长、未压缩的低价值原始日志

## Anti-Patterns

避免以下设计：

- 把 Telegram、邮件、通知中心这类 transport 默认建模成 `App`
- 强迫模型先“打开某个聊天”才能处理已知新消息
- 在 app state 中保存 `selected_chat`、`selected_thread`、`opened_message`
- 让 send / resolve 依赖隐藏 viewport state
- 对事件只绑定容器 id，不绑定事件 id
- 把 workflow 设计成某个 app 的局部附属说明或模型内在能力
- 强迫模型先做 workflow 语义搜索才能继续执行
- 自动生成通用默认 workflow 模板交给模型盲用
- 把当前绑定 workflow 写回 workflow spec 本体
- 让模型手工提交 workflow 结果日志
- 把长期记忆当即时状态缓存
- 把 plan 当 backlog 仓库
- 让模型通过普通文本隐式提交最终发送动作

## Design Checklist

在新增 agent-facing 接口之前，先问：

1. 这是交互表面，还是已经到达的结构化事实？
2. 人类会描述成“去操作那个界面”，还是“某件事发生了，决定怎么处理”？
3. 代码是否已经掌握了模型需要的事实？
4. 动作是否绑定了具体对象和 freshness guard？
5. 这个接口会不会诱导模型做机械枚举？
6. 它是否与 trace / workflow-run-record / sleep 的评估闭环兼容？

如果答案偏“探索和聚焦”，优先建模为 `App`。

如果答案偏“到达事实和 resolution”，优先建模为 `Event`。

如果答案偏“驱动下一轮处理”，通常属于 `PendingWork`，而不是 `Event` 或 `App`。

## In Short

- `App` 解决“要把注意力放到哪里，聚焦后怎么操作”
- `Event` 解决“发生了什么，要不要响应，如何终结”
- `PendingWork` 解决“下一轮该驱动什么”
- `Workflow` 解决“这类任务用什么可复用流程推进，以及如何在 sleep 中持续修正”
- `Plan` 解决“当前任务如何持续推进”
- `Memory` 解决“线程延续与长期经验”
- `Sleep` 解决“如何从 runtime 错误中持续改进”

修改这些边界时，以当前代码的真实运行方式为准，不要为了表面统一性把不同概念硬揉在一起。
