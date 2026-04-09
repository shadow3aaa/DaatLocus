---
name: 编写第三方 App
when_to_use:
  - 需要编写新的 app 来拓展你的能力时。
---

# 编写第三方 App

## Purpose

交付一个能被当前 runtime 直接发现、加载、focus 和调用的第三方 app。

不要猜接口。优先从下面的精确模板开始，再按需求改小处。

## Start From This Exact Shape

```text
~/daat-locus-workspace/apps/<app-name>/
  app.toml
  runtime/
    app.lua
  prompt/
    usage.md
    how_to_use.md
  skills/
```

规则：

- `app_id` 直接等于文件夹名 `<app-name>`
- `app.toml` 可以省略；如果保留，内容必须是顶层 `entry = "runtime/app.lua"`
- `runtime/app.lua` 必须返回一个 Lua table
- `prompt/usage.md` 和 `prompt/how_to_use.md` 必须存在
- `skills/` 目录可以为空；只有真的需要 app-scoped skill 时再加 `*.md`

`app.toml` 的正确写法只有这一种：

```toml
entry = "runtime/app.lua"
```

不要写成：

- `[entry]`
- `lua = "..."`
- 任何别的嵌套表

## Minimal Starter

优先从这个最小骨架开始：

```lua
local app = {}

function app.init(ctx)
  return { last_result = nil }
end

function app.render_state(ctx, state)
  return {
    title = "Calculator",
    lines = {
      "kind=workspace_app",
      "app_id=" .. ctx.app_id,
      "last_result=" .. tostring(state.last_result),
    },
  }
end

function app.list_tools(ctx, state)
  return {
    {
      name = "calculate",
      description = "Run a basic arithmetic operation",
      input_schema = {
        type = "object",
        properties = {
          a = { type = "number" },
          b = { type = "number" },
          op = { type = "string", enum = { "+", "-", "*", "/" } },
        },
        required = { "a", "b", "op" },
      },
      output_schema = {
        type = "object",
        properties = {
          result = { type = "number" },
        },
        required = { "result" },
      },
    },
  }
end

function app.call_tool(ctx, state, name, args)
  if name ~= "calculate" then
    error("unknown tool: " .. tostring(name))
  end

  local result
  if args.op == "+" then
    result = args.a + args.b
  elseif args.op == "-" then
    result = args.a - args.b
  elseif args.op == "*" then
    result = args.a * args.b
  elseif args.op == "/" then
    result = args.a / args.b
  else
    error("unsupported operator: " .. tostring(args.op))
  end

  local next_state = {
    last_result = result,
  }

  return {
    summary = "calculation complete",
    payload = {
      result = result,
    },
    state = next_state,
  }
end

return app
```

先让这个骨架能加载和调用，再继续加复杂逻辑。

## Runtime Contract

`runtime/app.lua` 是唯一入口。宿主只会读取这个模块返回的 table。

当前可实现的字段：

- `init(ctx)`
- `render_state(ctx, state)`
- `list_tools(ctx, state)`
- `call_tool(ctx, state, name, args)`
- `on_focus(ctx, state)`
- `on_blur(ctx, state)`
- `poll_notices(ctx, state)`

可以缺省；只有实现了才会被调用。

`ctx` 当前可读字段：

- `ctx.app_id`
- `ctx.app_dir`
- `ctx.state_dir`

## Exact Return Shapes

### `init(ctx)`

返回初始 state。通常是一个 table。

```lua
function app.init(ctx)
  return { count = 0 }
end
```

### `render_state(ctx, state)`

返回：

```lua
{
  title = "My App",
  lines = { "key=value", "other=value" },
  state = optional_next_state,
}
```

说明：

- `title` 可省略；默认会退回 app id
- `lines` 应该是字符串数组
- 如果要顺手修正 state，可以返回 `state = ...`

### `list_tools(ctx, state)`

返回 tool 描述数组。每个 tool 至少包含：

- `name`
- `description`
- `input_schema`
- 可选 `output_schema`

`input_schema` 和 `output_schema` 必须是 JSON-schema 子集，至少按这种形状写：

```lua
input_schema = {
  type = "object",
  properties = {
    amount = { type = "integer" },
  },
  required = { "amount" },
}
```

不要写成：

```lua
input_schema = { amount = "integer" }
```

这是错的。

### `call_tool(ctx, state, name, args)`

必须返回一个 object，正确字段是：

- `summary`
- `payload`
- 可选 `model_content`
- 可选 `ui_lines`
- 可选 `state`
- 可选 `turn_boundary`

最小合法形状：

```lua
return {
  summary = "done",
  payload = {},
  state = next_state,
}
```

不要写成：

```lua
return {
  result = 42,
  summary = "done",
}
```

这是错的。结果必须放进 `payload`，不是顶层随便起字段。

### `poll_notices(ctx, state)`

返回：

```lua
{
  notices = { "notice text" },
  state = optional_next_state,
}
```

如果没有 notice，返回空数组或 `nil` 都可以。

## Lua Features You Can Use

当前 runtime 支持：

- app 包内纯 Lua `require(...)`
- 基本 `io`
- 基本 `os`
- `package.path` 指向 app 包内模块

这意味着你可以：

- 读取 `ctx.app_dir` 下的静态文件
- 读写 `ctx.state_dir` 下自己的运行文件
- 把复杂逻辑拆到 app 包内的纯 Lua 模块
- 必要时调用外部二进制

但不要把协议拆成多个宿主入口文件。宿主入口仍然只有 `runtime/app.lua`。

## Prompt And Skills

`prompt/usage.md` 只回答两件事：

- 这个 app 是干什么的
- 什么时候值得 focus

`prompt/how_to_use.md` 只回答：

- focus 之后怎么正确操作

`skills/*.md` 只在真的需要更细的 app-scoped 规范时再加。不要把核心接口约定藏进 app skill。

## Common Failure Modes

优先排查这些错误：

1. `app.toml` 写成嵌套表，导致 app 根本无法加载
2. `runtime/app.lua` 没有 `return app`
3. `list_tools` 里的 schema 不是 JSON object schema
4. `call_tool` 把结果写到 `result` 之类的顶层字段，而不是 `payload`
5. tool 更新了 Lua 局部变量，却没把 `state = next_state` 返回给宿主
6. 把“何时使用这个 app”的说明塞进 skill，而不是 `usage.md`

如果 app 连 focus 都做不到，先检查 1 和 2，不要先怀疑 tool 逻辑。

## Common Errors

优先按现象排查，不要一上来就怀疑整个 runtime。

### 无法 focus app

先检查这些点：

- app 是否放在 `~/daat-locus-workspace/apps/<app-name>/`
- `app.toml` 是否写成了顶层 `entry = "runtime/app.lua"`，而不是 `[entry]` 或别的嵌套表
- `runtime/app.lua` 是否真的存在
- `runtime/app.lua` 是否 `return` 了一个 Lua table

### app 能加载，但没有任何 tool

先检查这些点：

- 是否实现了 `list_tools(ctx, state)`
- `list_tools` 是否返回数组，而不是单个 object
- 每个 tool 是否都带 `name`、`description`、`input_schema`
- `input_schema` 是否写成了 JSON object schema，而不是 `{ amount = "integer" }` 这种简写

### tool 调用时报 schema 错误

先检查这些点：

- `input_schema` / `output_schema` 是否带 `type`
- object schema 是否把字段放在 `properties` 里
- 必填项是否放在 `required = { ... }`
- `enum` 是否写成数组

### tool 调用成功，但结果丢了

先检查这些点：

- `call_tool` 是否返回了 `summary`
- 结果是否放进 `payload`
- 如果 tool 改了状态，是否返回了 `state = next_state`

不要把结果写成：

```lua
return {
  result = 42,
  summary = "done",
}
```

应该写成：

```lua
return {
  summary = "done",
  payload = { result = 42 },
  state = next_state,
}
```

### tool 调用了，但前景状态没变化

先检查这些点：

- 是否实现了 `render_state(ctx, state)`
- `render_state` 是否真的把关键 state 渲染到 `lines`
- tool 更新后是否把新 state 返回给宿主

### notice 不出现或不消失

先检查这些点：

- 是否实现了 `poll_notices(ctx, state)`
- 返回值是否是 `{ notices = {...}, state = ... }`
- notice 被处理后，下一次 `poll_notices` 是否会返回空 notices

不要在第一版里堆太多 tools、skills 或 notices。先交付一个最小可加载、可 focus、可调用的 app。
