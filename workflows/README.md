# Workflows

这个目录存放 Daat Locus 的内置 workflow 规范资产。

规则：

- 每个 workflow 一个 markdown 文件
- 文件名即 workflow id
- frontmatter 只保留 `id`
- workflow 是跨任务可复用的执行规范
- runtime 绑定状态不写回 workflow spec
- sleep 可以基于运行证据对 workflow 做 patch 或 merge

如果未来需要新增内置 workflow，请直接在这个目录下添加对应的 `*.md` 文件。
