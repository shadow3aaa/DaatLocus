---
name: 编写第三方 App
when_to_use:
  - 需要创建一个新的第三方 app 包时。
  - 需要修改已有 workspace app 的 Lua 逻辑、prompt 或 app skill 时。
  - 需要让某类能力以 app 形式进入 Daat Locus，而不是做成 global skill 或内置 Rust app 时。
---

# 编写第三方 App

## Purpose

把能力做成 source-first 的第三方 App，放在 `~/daat-locus-workspace/apps/<app-name>/` 下，由 runtime 自动发现、加载和重载。

## Directory Contract

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

- `app_id` 直接等于文件夹名 `<app-name>`
- `app.toml` v1 只负责指定 Lua 入口；默认是 `runtime/app.lua`
- `prompt/usage.md` 负责 pre-focus 说明
- `prompt/how_to_use.md` 负责 post-focus 操作说明
- `skills/*.md` 是这个 app 自己的 app-scoped skills

## Workflow

1. 先判断这件事是否真的该建模成 `App`
2. 建立 app 目录并补齐最小文件集
3. 在 `runtime/app.lua` 实现统一导出接口
4. 用 `prompt/*.md` 写清用途和操作方式
5. 只在需要时再添加 `skills/*.md`
6. 跑测试；至少覆盖加载、工具调用和重载相关路径

## Keep It Minimal

- 先交付最小可加载 app，不要一开始就铺太多 skill、tool 或 notice
- 只创建 runtime 真会读取的文件；不要额外堆 README、设计说明或占位目录
- 如果某段 Lua 会反复复用，优先拆成 app 包内可 `require` 的纯 Lua 模块，而不是把 `app.lua` 写成一大块

## App Or Not

优先做成 `App` 的条件：

- 需要先 `focus_app` 才合理操作
- 可见信息天然局部，需要逐步探索
- 操作带时间语义，例如等待、继续交互、会话持续存在

如果只是跨任务通用方法，优先做 global skill，不要做 app。

## Lua Contract

`runtime/app.lua` 是唯一入口。不要把宿主协议拆成多个独立脚本入口。

当前可实现的入口包括：

- `init(ctx)`
- `render_state(ctx, state)`
- `list_tools(ctx, state)`
- `call_tool(ctx, state, name, args)`
- `on_focus(ctx, state)`
- `on_blur(ctx, state)`
- `poll_notices(ctx, state)`

工具规则：

- `list_tools` 返回的 `input_schema` / `output_schema` 会被 runtime 校验
- `call_tool` 返回的 `summary` 不能为空
- tool 改状态后，要通过返回 `state` 回写，而不是依赖隐藏宿主状态

## Prompt And Skills

`usage.md` 回答：

- 这个 app 是干什么的
- 什么时候值得 focus

`how_to_use.md` 回答：

- focus 之后怎么正确操作

`skills/*.md` 只写更细的高阶规范，不要重复 `usage` 或 `how_to_use`。

如果某个说明只是为了让 agent 更容易决定“这是不是该用这个 app”，优先放进 `usage.md`，不要埋进 app skill 正文里。

## Validation

提交前至少确认：

- app 能被 runtime 发现并加载
- `app.toml`、prompt、skills frontmatter 都合法
- `runtime/app.lua` 可以通过加载
- 如果声明了 tool，schema 合法且调用路径可运行
- 如果用了 `poll_notices`，notice 能出现也能消失
