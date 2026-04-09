# Daat Locus Agent Guidelines

本文档定义 Daat Locus 当前代码实现对应的 agent-facing 边界。

目标不是写一份抽象口号，而是给未来修改 `app`、`events`、`runtime_tools`、`snapshot`、`telegram_transport`、`memory`、`sleep` 等模块时提供一套与代码一致的设计约束。

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

`skills()` 是可选的补充层，用于在已经聚焦某个 app 之后，提供更细的执行规范；它不应替代 `usage` 或 `how_to_use`。

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

### Memory

`Memory` 由两部分组成：

- runtime conversation：当前线程上下文
- hindsight queue：待 retain/已 retain 的长期记忆队列

它服务于线程延续和长期经验积累，不服务于机械状态同步。

### Sleep / Self-Improvement

Daat Locus 有显式的自我改进闭环：

- runtime trace
- runtime review
- sleep
- turn compile
- compiled prompt additions

这意味着运行时设计不是一次性的。任何 agent-facing 接口设计如果会系统性诱导错误行为，最终都会污染 trace/review，并影响后续 compile。

所以接口设计要偏稳定、显式、可评审，不要依赖模糊约定。

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
  skills/
    <skill-name>.md
```

规则：

- `runtime/app.lua` 是唯一的 Lua 主入口
- `prompt/usage.md` 是 app 的 pre-focus 说明
- `prompt/how_to_use.md` 是 app 的 post-focus 说明
- `skills/*.md` 是附着在该 app 上的结构化 skill 文档

### `app.toml`

`app.toml` 在 v1 里极简化，只承担一个职责：指定 Lua 主入口相对路径。

规则：

- 不承载 `id`
- 不承载权限
- 不承载 usage/how_to_use/skills 元数据
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

### Skills

第三方 app 的 skill 文件位于 `skills/*.md`。

规则：

- 每个 skill 一个 markdown 文件
- skill 使用 frontmatter schema
- skill 是附着在 app 上的结构化操作规范
- skill 不是独立能力单元
- skill 不定义宿主生命周期、权限或运行时边界

`prompt/*.md` 和 `skills/*.md` 都是 agent-facing 文本资产；`runtime/app.lua` 是宿主执行入口。

## Global Skills

除了 app-scoped skills，runtime 还支持不挂在任何 app 上的 global skills。

规则：

- global skills 始终可见，不需要 `focus_app`
- 内置 global skills 位于仓库根目录的 `skills/*.md`
- workspace 拓展 global skills 位于 `~/daat-locus-workspace/skills/*.md`
- global skills 与 app skills 使用同一套 frontmatter schema
- workspace `skills/` 走单独的 `notify + digest + safe-point reload`，不与 `apps/` 共用一套目录

这层的作用是承载跨 app 的稳定操作规范，例如未来“编写 app”“编写 skill”这类能力。

### Reload Strategy

第三方 app 不应每轮 turn 全量重解析。

推荐策略：

- 启动时全量扫描一次 `~/daat-locus-workspace/apps`
- 运行时使用 `notify` 监听该目录变化
- 根据文件事件定位到受影响的 `<app-name>`
- 只把该 app 标记为 dirty 并按 app 增量重载
- watcher 异常或目录状态不可信时，再回退到一次全量 rescan

不要把全量解析作为常规路径。

### State and Cache

v1 不设计专门的第三方 App cache 目录。

当前结论：

- 只定义 source 目录：`~/daat-locus-workspace/apps`
- 不定义 `cache/apps`
- 如果未来确实需要宿主持久化某个 app 的运行状态，再使用受保护的 runtime state 体系，例如 `~/.daat-locus/state/apps/<app-name>/`

第三方 app 是 agent 可编辑资产，但不是 agent 直接拥有的运行时状态。

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
- `resolved` 时必须提供非空 `reply_message`

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
- 对成功收尾显式接收 `reply_message`
- 允许 `dismissed` / `failed`

不要把“最终回复”设计成 assistant 文本本身。

### Plan Tools

`update_plan` 只维护当前完整 plan。

不要新增“append_plan_step”或“select_plan_step”这类会引入隐藏游标和增量同步复杂度的工具，除非有强证据说明当前 contract 不够用。

### Skill Tools

skill 的职责是补充具体操作规范，不是承载动态世界状态。

当前规则：

- global skill 列表会始终出现在快照里
- focused app 的 skill 列表只会随当前前景 app 一起暴露
- `read_skill` 会先尝试读取当前前景 app 的同名 skill；若当前 app 没有该 skill，再回退到 global skill
- 因此，是否 `focus_app` 应先由任务需要、app usage 或 app notice 驱动；只有在需要 app-scoped skill 时才需要先 focus

## What Code Should Do

代码负责：

- 轮询和接收 Telegram 更新
- 去重事件
- 持久化状态
- claim / release / requeue pending work
- 维护 outbox
- 加载快照
- 控制 tool scope
- 记录 trace / review
- 执行 sleep 与 compile

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
6. 它是否与 trace/review/sleep 的评估闭环兼容？

如果答案偏“探索和聚焦”，优先建模为 `App`。

如果答案偏“到达事实和 resolution”，优先建模为 `Event`。

如果答案偏“驱动下一轮处理”，通常属于 `PendingWork`，而不是 `Event` 或 `App`。

## In Short

- `App` 解决“要把注意力放到哪里，聚焦后怎么操作”
- `Event` 解决“发生了什么，要不要响应，如何终结”
- `PendingWork` 解决“下一轮该驱动什么”
- `Plan` 解决“当前任务如何持续推进”
- `Memory` 解决“线程延续与长期经验”
- `Sleep` 解决“如何从 runtime 错误中持续改进”

修改这些边界时，以当前代码的真实运行方式为准，不要为了表面统一性把不同概念硬揉在一起。
