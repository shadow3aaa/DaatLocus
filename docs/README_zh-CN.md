<div align="center">

# Daat Locus

<img src="../assets/logo.svg" alt="Logo" style="width:220px; height:auto;" />

[English](../README.md)
[![License][license-badge]][license-url]

一个长期运行、具备记忆、App 工具边界、Telegram 事件处理和睡眠自改进能力的本地 agent runtime。

</div>

[license-badge]: https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=for-the-badge
[license-url]: ../LICENSE

## 它是什么

Daat Locus 是一个 daemon-first 的本地 agent runtime。它不是单轮聊天包装器，也不是把每段 assistant 文本都自动发到外部世界的聊天 UI。

外部输入会以结构化 event、app notice、after-claim context、pre-turn context 和自动召回的记忆进入运行时。模型负责判断语义和选择工具；真正影响外部世界的动作必须通过明确工具完成，例如终端、浏览器、workflow 绑定、记忆召回或 Telegram event 完结。

## 核心理念

### Agent App

当 agent 拥有越来越多能力时，简单平铺 tool list 很快会失控。Daat Locus 把需要持续交互的能力收拢为 App：App 拥有自己的状态、生命周期、用途说明、操作说明和前后台语义。

当前内置系统 App 是 `Terminal` 与 `Browser`。Daat Locus 同时支持 source-first 的 Lua 第三方 workspace App。

### 睡眠期自我改进

Daat Locus 不把“自我改进”强塞进前台任务执行，而是在空闲时进入独立的 sleep 阶段。

清醒时，运行时会记录代码识别出的 runtime error case，以及绑定 workflow 的执行证据。睡眠时，独立管线可以分别修正全局 runtime contract 和 workspace workflow spec。

## 特性

- 前台 TUI + 后台 daemon 双模式运行。
- 托管 `Hindsight`，用于长期记忆、经验积累和自动召回。
- Telegram 是 transport 和 event source，而不是需要打开浏览的 App。
- Workflow 绑定与睡眠期 workflow 演化，用于复用复杂任务流程。
- App-scoped tools，而不是全局平铺的工具列表。
- 交互式配置 provider、model、Telegram 和运行时参数。

## 快速开始

目前 Daat Locus 仍以源码方式运行：

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
cargo run
```

第一次运行如果不存在 `~/.daat-locus/config.toml`，会自动进入交互式配置向导。

## 常用命令

```bash
cargo run                       # 启动或连接 daemon-backed TUI
cargo run -- attach             # 只连接已运行的 daemon
cargo run -- daemon status      # 查看 daemon 状态
cargo run -- daemon restart     # 重启后台 daemon
cargo run -- config             # 打开交互式配置菜单
cargo run -- config show        # 查看已脱敏的配置摘要
```

## 配置

主配置文件位于 `~/.daat-locus/config.toml`。人格配置文件位于 `~/.daat-locus/persona.md`。

日常配置优先使用交互式命令：

```bash
cargo run -- config add-provider
cargo run -- config add-model
cargo run -- config set-main-model
cargo run -- config set-hindsight-model
cargo run -- config set-telegram
```

配置结构、provider 说明和最小 TOML 示例见 [配置文档](configuration_zh-CN.md)。

## 文档

- [English README](../README.md)
- [配置文档](configuration_zh-CN.md)
- [模型目录](model-catalog.md)
- [Sandbox backend 选择](sandbox-backend-selection.md)
- [内置 workflows](../workflows/README.md)

## 许可证

Daat Locus 使用 [Apache License 2.0](../LICENSE) 授权。
