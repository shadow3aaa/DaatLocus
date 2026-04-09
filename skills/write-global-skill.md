---
name: 编写全局 Skill
when_to_use:
  - 需要新增一个始终可见的 global skill 时。
  - 需要修改某个现有 global skill 的触发条件、正文或结构时。
  - 需要把跨 app 的通用方法沉淀成稳定 skill，而不是做成某个 app 的局部 skill 时。
---

# 编写全局 Skill

## Purpose

把跨 app、始终可见的操作规范写成 global skill，放在：

- 内置：仓库根目录 `skills/*.md`
- 拓展：`~/daat-locus-workspace/skills/*.md`

global skill 不挂在任何 app 上，不需要 `focus_app`。

## File Contract

一个 global skill 对应一个 markdown 文件，文件名就是 skill id。

当前 frontmatter 约束：

- 必须有 `name`
- `when_to_use` 必须是非空字符串数组
- 不允许未知字段
- 文件名只允许 ASCII 字母、数字、`_`、`-`

正文必须非空。

## Workflow

1. 先确认这是不是 global skill，而不是 app skill 或 app 本体
2. 选一个简短、稳定、可读的 skill id
3. 写 frontmatter，先把触发语义讲清楚
4. 再写正文，只保留 agent 真需要的操作规范
5. 跑测试或至少验证能被 runtime 加载

## Global Or App Skill

优先做 global skill 的条件：

- 这套方法跨多个 app 都成立
- 它主要是工作方法，不依赖某个 app 的局部状态
- 读取它不应要求先 focus 某个 app

如果技能只在某个 app 内成立，放进那个 app 的 `skills/*.md`。

## Writing Rules

- `when_to_use` 负责触发，要写成真正可匹配任务的句子
- 不要把关键触发条件只写在正文里
- 正文重点写 workflow、约束、校验和失败恢复
- 正文默认保持短；只写另一个 agent 真的不知道、但执行时必须知道的东西
- 不要把系统已知事实、代码细节清单或冗长背景复制进去
- 不要额外创建 README、变更记录或旁支说明文件；当前 global skill 包只有这一个 `*.md`

## Recommended Body Shape

优先保持这几个块：

- `Purpose`
- `Workflow`
- `Rules`
- `Validation`

只在确实需要时再加更多章节。

## Validation

提交前至少确认：

- skill 文件名合法
- frontmatter 能通过当前 runtime 校验
- 正文非空
- `when_to_use` 真能帮助模型在正确时机触发这个 skill

## Design Rule

global skill 的目标不是塞更多背景知识，而是给 agent 一套更稳的执行规范。能短就短，但必须能直接改变 agent 的行动方式。
