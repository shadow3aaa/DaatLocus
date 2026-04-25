<div align="center">

# Daat Locus

<img src="../assets/logo.svg" alt="Logo" style="width:250px; height:auto;" />

[English](../README.md)
[![License][license-badge]][license-url]

一个长期运行、具备自我治理能力的 agent runtime。

</div>

[license-badge]: https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=for-the-badge
[license-url]: ../LICENSE

## 特性

- 基于 `Hindsight` 的长期记忆与经验积累
- 睡眠驱动的自我改进
- 基于 App 理念的工具管理，而非平铺工具调用
- 自动适配模型能力的 prompt compile
- 前台 TUI + 后台 daemon 双模式运行

## 理念

### App For Agent

随着 Agent 工具的发展，简单平铺 tool calling 的方式已经无法满足复杂场景的需求。成百上千的工具调用会让 agent 的注意力彻底分散，导致它最终只专注于用终端之类的几个基本工具完成一切。

正如我们将邮件列表、发件功能、联系人等能力整合到一个邮件 App 中，将好友列表、收藏、动态等功能整合到一个即时通讯 App 中一样，Daat Locus 认为 Agent 也需要原生的 App 生态。现在的 MCP、Workflow 等概念与此接近，但并不等价。

一个真正的 Agent App 应该满足以下条件：

- 标准化：App 必须符合清晰、固定、标准的格式，便于 Agent 管理，而不是零散的说明和脚本。
- 状态化：App 应该有自己的状态和生命周期，而不是一堆工具调用和说明。
- 交互式：App 被聚焦时应主动向 Agent 渲染结构化状态，而不是让 Agent 通过 `list_xxx` 机械探索。
- 前后台管理：App 应该有能力在后台存在影响，如发送通知等。
- 自说明：App 本身应该说明自己的用途与使用方法，而不是把理解成本转嫁给 workflow 或 prompt 拼贴。

因此，Daat Locus 将工具管理提升到 App 级别，提供了一个原生支持 Agent App 的运行时环境。当前内置系统 App 包括 `Terminal` 与 `Browser`，同时也支持第三方 workspace App。

### 睡眠驱动的异步自优化

通过异步的睡眠机制，Daat Locus 在空闲时间对 Agent 行为模式进行自优化。这参考了 [Hermes Agent](https://github.com/NousResearch/hermes-agent) 和 [EvoMap](https://github.com/EvoMap/evolver) 的设计理念。

但 Daat Locus 不把“自我改进”放在前台运行里强行完成，而是把它设计成独立的睡眠阶段。

“清醒”时，Agent 会为多步任务绑定合适的 workflow，或者在没有可复用项时新建 workflow。运行过程中积累的 traces 与 workflow run records 会留给睡眠阶段分析。

睡眠阶段当前分成两条独立 pipeline：

- Prompt Improvement Pipeline：基于 runtime trace 修正 system prompt 和行为约束
- Workflow Improvement Pipeline：基于 workflow run records 修正 workspace workflow

## 快速开始

目前 Daat Locus 仍以源码方式运行：

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
cargo run
```

第一次运行如果不存在 `~/.daat-locus/config.toml`，会自动进入交互式配置向导。

## 许可证

Daat Locus 使用 [Apache License 2.0](../LICENSE) 授权。

版权所有 2026 shadow3 <shadow3aaaa@gmail.com>。

## 运行模型

Daat Locus 现在默认采用 daemon 模型，而不是单次前台直接跑完就退出。

- `cargo run`
  会优先连接已有 daemon；如果 daemon 不存在，则自动启动后台 daemon 并 attach 到 TUI。
- `cargo run -- attach`
  只连接已运行的 daemon。
- `cargo run -- daemon serve`
  以前台方式直接运行 daemon，主要供内部和调试使用。

后台 daemon 持有运行时状态、HTTP 控制接口、TUI 同步状态和 Telegram transport。当前 daemon 默认监听固定端口 `127.0.0.1:53825`。

## 配置

主配置文件位于：

- `~/.daat-locus/config.toml`

人格配置文件位于：

- `~/.daat-locus/persona.md`

推荐优先使用交互式命令维护配置：

```bash
cargo run -- config
cargo run -- config show
cargo run -- config add-provider
cargo run -- config add-model
cargo run -- config set-main-model
cargo run -- config set-hindsight-model
```

当前配置结构的核心是：

- `[providers]`：provider 凭据注册表
- `[models]`：模型定义注册表
- `locale`：用户界面本地化语言
- `main_model`：主模型引用
- `[daemon]`：daemon 端口
- `[judge]`：judge / pairwise 评估配置
- `[hindsight]`：由 Daat Locus 托管的 hindsight-embed 配置
- `[telegram]`：Telegram transport 配置

一个最小可运行示例：

```toml
locale = "zh-CN"
main_model = "default"

[providers.openai]
type = "openai"
api_key = "your-api-key"

[models.default]
provider = "openai"
model_id = "gpt-4.1"
temperature = 1.0
request_timeout_secs = 300
stream_idle_timeout_secs = 45
context_window_tokens = 128000
effective_context_window_percent = 95
max_completion_tokens = 4000
tool_output_max_tokens = 2000

[daemon]
port = 53825

[hindsight]
namespace = "default"
bank_id = "daat-locus"
request_timeout_secs = 180
embed_version = ""
profile = "daat-locus"
port = 8888

[telegram]
enabled = true
bot_token = "your-telegram-bot-token"
poll_timeout_secs = 30
```

说明：

- `hindsight` 现在由 Daat Locus 自动托管，不需要你先手动起 Docker 或单独跑服务。
- `telegram.enabled = true` 但 `bot_token` 仍是占位符时，不会真正启用 Telegram transport。
- `hindsight.model = "xxx"` 可选；为空时回退到 `main_model`。
- `judge.model = "xxx"` 可选；为空时同样回退到 `main_model`。
