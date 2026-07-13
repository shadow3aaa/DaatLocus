# Lua 工作流

Lua 工作流是用于编排隔离 agent worker 的一等、可执行程序。每个工作流都是一个 Lua 5.4
源文件：它自行声明类型化输入和输出、worker 指令、显式授予的能力、工作流局部工具以及普通的
控制流。

Workflow 与运行时的其他对象刻意保持不同边界：

- **Workflow**：可执行且允许副作用的 Lua 编排程序；它协调隔离 worker turn 并返回类型化结果。
- **App**：Browser、Terminal、Coding 等长期存在且有状态的能力域。只有工作流明确授予某个
  App 工具时，worker 才能使用该精确工具。
- **Event**：Session 主 agent 需要判断并完成的外部事实。worker 不会获得已 claim 的 event，也没有
  `finish_and_send`。
- **Skill**：面向 agent 的可复用 Markdown 指南。Skill 可以说明如何编写或使用工作流，但不执行、
  调度或拥有一次工作流运行。

## 发现与重新加载

Session 启动时，运行时会扫描下面的目录：

```text
<execution_cwd>/workflows/*.lua
```

目录不存在时会自动创建。`execution_cwd` 是当前 Session 的工作区；请在希望运行工作流的
Session 工作区中创建文件，而不要默认它一定是当前代码仓库。

文件名去掉 `.lua` 后就是工作流 id，且必须是 lower snake case。例如
`research_brief.lua` 的 id 为 `research_brief`。工作流不使用 manifest、sidecar metadata 或预先
声明的任务目录。

某个文件无效不会阻止其他有效工作流加载；错误会显示在工作流选择器中。新增或修改文件后，使用
Dashboard 的 `/skills reload` 动作重新加载；该动作会同时重新加载 skills 和 workflows。

## 启动工作流

交互用户可在 TUI 或 WebUI 中打开 `/workflows`，选择已加载的文件，并填写自动生成的类型化表单。

Session 主 agent 会为每个已加载的工作流获得一个类型化工具：

```text
workflow__research_brief(...)
```

该工具使用工作流声明的输入 schema，并返回其声明的结果。系统刻意不提供
`workflow__start({ workflow_id, arbitrary_json })` 这种通用启动工具。主 agent 始终位于工作流图之外，
并在正常 turn 中消费结果。

每次调用会以以下一种状态结束：

- `completed`：Lua 脚本返回了符合输出 schema 的结果。
- `failed`：加载、Lua 执行、worker、工具调用或输出校验失败。
- `interrupted`：运行时被协作式中断。中断后的运行不会自动重放。

工作流运行也会作为类型化 Workflow activity row 出现在 Dashboard 中。

## 最小工作流

可将 [`examples/workflows/research_brief.lua`](../examples/workflows/research_brief.lua)
复制到 `<execution_cwd>/workflows/research_brief.lua`：

```lua
local Input = {
  type = "object",
  properties = {
    topic = { type = "string" },
    sources = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "topic", "sources" },
  additionalProperties = false,
}

local ResearchOutput = {
  type = "object",
  properties = {
    summary = { type = "string" },
  },
  required = { "summary" },
  additionalProperties = false,
}

local Output = {
  type = "object",
  properties = {
    summary = { type = "string" },
    source_count = { type = "integer" },
  },
  required = { "summary", "source_count" },
  additionalProperties = false,
}

workflow.tool({
  name = "count_sources",
  input = {
    type = "object",
    properties = {
      sources = { type = "array", items = { type = "string" } },
    },
    required = { "sources" },
    additionalProperties = false,
  },
  output = {
    type = "object",
    properties = { count = { type = "integer" } },
    required = { "count" },
    additionalProperties = false,
  },
  run = function(input)
    return { count = #input.sources }
  end,
})

local researcher = workflow.agent({
  model = "efficient",
  input = Input,
  output = ResearchOutput,
  instruction = [[
根据提供的主题和来源 URL 写一份简短研究摘要。
需要时使用 count_sources 工具。只返回所需 JSON 对象，不要附加 Markdown。
]],
  extra_tools = { "count_sources" },
})

workflow.define({
  input = Input,
  output = Output,
  run = function(input, ctx)
    local research = workflow.await(researcher:run(input))
    return {
      summary = research.summary,
      source_count = #input.sources,
    }
  end,
})
```

调用 worker factory 时使用 Lua 的冒号形式 `worker:run(input)`。冒号会把 factory 作为第一个参数传入。
`ctx` 是 host 所有的预留占位符；它不会暴露 Session 主 agent、会话历史或稳定的公开 API。

## Lua 接口与控制流

每个工作流文件必须且只能调用一次 `workflow.define(...)`，并提供 `run` 函数。脚本中可使用普通的
Lua 函数、局部状态、条件、循环、递归、由结果驱动的重试和分支。执行图会随着脚本创建和等待 worker
handle 动态形成，而不是静态 DAG。

支持的工作流 API 保持很小：

```lua
workflow.define({ input = InputSchema, output = OutputSchema,
  run = function(input, ctx) ... end })
workflow.agent({ model, input, output, instruction, capabilities, extra_tools })
workflow.await(handle)
workflow.await_all(handles)
workflow.tool({ name, input, output, run })
```

`workflow.agent(...)` 返回 worker factory。`factory:run(value)` 创建 handle，
`workflow.await(handle)` 返回 worker 的类型化输出，`workflow.await_all({ handle_a, handle_b })`
按 handle 顺序返回输出。需要协调一组 worker 时，请先创建 handles 再等待。一个 handle 只能被等待一次。

## 可移植 Schema

`workflow.define`、`workflow.agent` 和 `workflow.tool` 所声明的 schema 会在加载时校验。传给工作流、
worker、局部工具的输入，以及最终输出，会在运行时再次校验。

请使用运行时的可移植、面向模型的 schema 方言：

- 根 schema 必须是对象。
- 每个对象必须有 `properties`、`required` 和 `additionalProperties = false`。
- `required` 必须列出 `properties` 中的每个键。模型可见的可选值应是必填的 nullable 字段，例如
  `type = { "string", "null" }`。
- 只使用 `string`、`integer`、`number`、`boolean`、`object`、同构 `array` 和 nullable union；有限集合
  可使用字符串 enum。
- 不要使用 default、动态键 map、tuple、schema 形式的 `additionalProperties`、`$ref`、组合关键字
  （`oneOf`、`allOf`、`anyOf`）、条件关键字或不被支持的校验约束。

请把 schema 收窄到真正需要的范围。它既是交互表单，也是主 agent 工具的契约。

## Worker、工具与能力

每个 worker 都是隔离的 agent turn。它只会得到：

- `instruction` 和类型化输入；
- 声明的 `model`（`"main"` 或 `"efficient"`）；
- `capabilities` 中列出的 App 工具；以及
- `extra_tools` 中列出的局部工具。

它**不会**继承 Session 主 agent 的 Context、已 claim 的 event id、完成 event 的权限、会话历史或不受
限制的工具列表。worker 必须通过返回恰好一个符合输出 schema 的 JSON 对象来完成，不能附加 Markdown。
当前 host 最多允许一个 worker 进行 16 个 model/tool round。

能力必须使用已安装 App 工具的精确名称，例如：

```lua
capabilities = {
  "browser__browser_open_page",
  "browser__browser_snapshot",
}
```

请按最小权限授予。`get_state`、`next_review` 等 state/review 工具不能作为 worker capability；
`read_file`、`edit_file`、`finish_and_send` 等静态运行时工具和任意主 agent 工具也不会暴露给 worker。

`workflow.tool(...)` 创建具名的局部工具。它的 `run` 会收到经过校验、可 JSON 表示的值，且必须返回满足
声明输出 schema 的值。只有把名称加入某个 worker 的 `extra_tools` 后，该 worker 才能调用此工具。
局部工具适合确定性转换、工作流状态访问，以及工作流有意建模为工具的 host 控制效果。

## 文件、Shell 与中断

工作流设计上允许副作用。它可使用提供的 Lua `io.open`、`io.popen` 和 `os.execute`。读写和子进程仍然
受 Session 的 `RuntimeSandboxPolicy`、工作区、可写根目录和进程沙箱限制。Lua 标准库会保持精简；请勿
假定存在不受限制的 `os`、包加载能力或 Session Context 访问。

脚本可能写入文件或执行命令，因此运行时绝不会在中断后自动恢复或重放一次调用。尽可能让外部效果
幂等；需要时显式保存 checkpoint，并在 Lua 代码中编写明确的恢复分支。

## 编写检查表

1. 在目标 Session 的 `workflows` 目录下选择一个 lower-snake-case 文件名。
2. 声明可移植的输入/输出 schema，并且只调用一次 `workflow.define`。
3. 为每个 worker 提供聚焦的 instruction、类型化输入/输出、明确模型，以及它真正需要的能力和局部工具。
4. 使用 `worker:run(...)`、`await` 和 `await_all` 把属于 Lua 的控制流保留在 Lua 中；Session event 完成
   留在工作流外。
5. 将文件和 shell 操作视为沙箱下的真实副作用；自行实现幂等性或 checkpoint。
6. 通过 `/skills reload` 重新加载，在 `/workflows` 中解决加载错误，并先用生成的表单测试，再依赖动态
   `workflow__<id>` 主 agent 工具。

面向 agent 的编写指南请参见内置
[`author-lua-workflow` skill](../skills/author-lua-workflow/SKILL.md)。
