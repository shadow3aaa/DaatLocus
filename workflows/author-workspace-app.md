---
id: author-workspace-app
---

## When To Use
- 需要在 `~/daat-locus-workspace/apps/<app-name>/` 下新建一个第三方 app
- 需要为某个 app 补齐最小可运行包结构、prompt 说明和 Lua 运行入口
- 任务目标是把 app 写出来并让 runtime 能识别、加载和渲染，而不是只修一个很小的局部 bug

## Preconditions
- 已明确 app id、目标能力和主要交互表面
- 已确认 app 属于 workspace app，而不是内置 Rust app
- 可以写入 runtime workspace 下的 app 源码目录
- 已了解最小包结构至少包含 `app.toml`、`runtime/app.lua`、`prompt/usage.md`、`prompt/how_to_use.md`

## Workflow
1. 明确 app 的目标、边界、输入输出和是否真的需要建模为 `App`
2. 检查 `~/daat-locus-workspace/apps/<app-name>/` 是否已存在，并决定是新建还是在现有目录上继续补齐
3. 搭建最小包结构，先保证 `app.toml` 指向 `runtime/app.lua`
4. 实现 `runtime/app.lua` 的最小可运行闭环，至少覆盖 state/render、tool 调用或 notice/poll 中任务真正需要的部分
5. 编写 `prompt/usage.md`，说明这个 app 是什么、什么时候值得 focus
6. 编写 `prompt/how_to_use.md`，说明 focus 之后如何正确操作，避免把 workflow 内容混进 app prompt
7. 尝试使用 `focus_app` 工具验证 app 是否能被加载、识别和正确渲染；必要时根据加载或校验错误修正包结构和 schema
8. 在可行时做一次最小复验，确认 render、tool 输入输出和 reload 行为没有明显断裂

## Done Criteria
- app 目录结构完整，最小必需文件都已存在
- runtime 能识别这个 workspace app，并能产出合理的 app state 和 prompt 信息
- app 的核心能力至少有一条可运行路径，不是只有静态占位文件
- `usage.md` 与 `how_to_use.md` 已明确 app 语义和操作方式，没有把 workflow 规范混写进去

## Recovery
- 如果 app 边界不清，先收缩为最小可运行能力，再补其他特性
- 如果 Lua 运行入口过大或不稳定，先做最小 state/render 闭环，再逐步补 tool 和 notice
- 如果 schema 或加载校验失败，先修包结构和主入口路径，不要同时改动过多逻辑
- 如果发现任务其实只是修已有 app 的局部问题，应切换到更窄的修复型 workflow，而不是继续按全量 author 流程推进
