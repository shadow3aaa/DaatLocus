# Workflows

这个目录存放 Daat Locus 的内置 workflow 规范资产。

规则：

- 每个 workflow 一个 markdown 文件
- 文件名即 workflow id
- frontmatter 只保留 `id`
- 这些 workflow 通过 `build.rs` 编译进程序，属于 builtin 基本能力
- builtin workflow 只读，不会被 `create_workflow`、sleep patch 或 sleep merge 写回
- runtime 可演化 workflow 只存在于 `~/daat-locus-workspace/workflows`

如果未来需要新增内置 workflow，请直接在这个目录下添加对应的 `*.md` 文件。
