<div align="center">

# Daat Locus

<img src="assets/logo.svg" alt="Logo" style="width:220px; height:auto;" />

[![English][readme-en-badge]][readme-en-url]
[![Crates.io][crates-badge]][crates-url]
[![CI][ci-badge]][ci-url]
[![License][license-badge]][license-url]

一个真正拥有经验的 agent runtime。

</div>

[readme-en-badge]: https://img.shields.io/badge/README-English-blue.svg?style=for-the-badge
[readme-en-url]: README.md
[crates-badge]: https://img.shields.io/crates/v/daat-locus?style=for-the-badge
[crates-url]: https://crates.io/crates/daat-locus
[ci-badge]: https://img.shields.io/github/actions/workflow/status/shadow3aaa/DaatLocus/ci.yml?style=for-the-badge&label=CI
[ci-url]: https://github.com/shadow3aaa/DaatLocus/actions/workflows/ci.yml
[license-badge]: https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=for-the-badge
[license-url]: LICENSE

## 这是什么

Daat Locus 是一个常驻本地的自治理 Agent Runtime。

它适合那些会从历史中变得更好的工作：长期维护同一个项目、反复处理同一类任务，Daat Locus 会记住你的偏好和实践经验，沉淀以优化后续行为。

## 核心理念

## 面向 Agent 的 App

人类使用电脑时，很少是从“所有可能操作的全局列表”里挑一个动作。我们会打开终端，看当前输出，输入命令，等待结果；或者打开浏览器，看当前页面，点击、跳转，再根据新页面继续操作。

Daat Locus 给 agent 提供类似的交互模型。

App 为 runtime 提供有状态的操作表面。每个 App 会渲染当前 agent 能看到的状态，说明什么时候应该使用它，说明应该怎样操作它，并在被聚焦时暴露一组局部工具。

相比平铺工具列表，这给模型带来三件事：

1. **局部性**：agent 只看到当前操作表面相关的工具。
2. **状态 grounding**：行动基于 App 当前展示的状态，而不是凭空选择工具。
3. **时间连续性**：Terminal、Browser 这类长运行表面可以被安全地继续使用。

App 是 Daat Locus 把“工具”提升成“软件操作表面”的方式。

因此 Daat Locus 不需要 `SKILLS.md` 来说明某组工具如何使用，App 本身就是自说明的。

### Workflow 自我改进

Daat Locus 会以 workflow 为蓝图执行任务，并在独立的睡眠阶段将执行经验反哺于 Workflow。

在清醒阶段，Daat Locus 执行任务并记录实践经验；在 sleep 阶段，它会整理这些经验，修正重复出现的问题，并改进后续任务所依赖的 workflow。

睡眠优化亦会尝试合并相近的 Workflow，避免无限膨胀。

## 快速开始

推荐使用 `cargo-binstall` 安装。它会下载与你的平台匹配的 GitHub
Release 预编译二进制。第一次启动时，Daat Locus 会从项目的 sidecar release
下载匹配平台的自包含 Hindsight sidecar，并缓存到本地。正常安装不需要
Python、`uv` 或 PyInstaller。

```bash
cargo install cargo-binstall
cargo binstall daat-locus
```

也可以直接从 [GitHub Releases][releases-url] 下载对应平台的压缩包，解压后把
`daat-locus` 放进 `PATH`。

第一次启动时，Daat Locus 会进入交互式引导流程。

### 源码构建

`cargo install daat-locus` 会从 crates.io 源码编译。和预编译发布二进制一样，
源码构建在本地没有缓存时也会在第一次启动时下载匹配的 Hindsight sidecar。

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
cargo run --locked
```

[releases-url]: https://github.com/shadow3aaa/DaatLocus/releases

## 文档

- [English README](README.md)
- [架构](docs/architecture_zh-CN.md)
- [内置 workflows](workflows/README.md)

## 许可证

Daat Locus 使用 [Apache License 2.0](LICENSE)。
