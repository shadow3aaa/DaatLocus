<div align="center">

# Daat Locus

<img src="assets/logo.svg" alt="Logo" style="width:250px; height:auto;" />

一个长期运行、具备自我治理能力的 agent runtime。

</div>

## 特性

- 基于 `Hindsight` 的长期记忆与经验积累
- 睡眠驱动的自我改进
- 基于APP理念的工具管理，而非平铺工具调用
- 自动适配模型的 prompt compile

## 理念

### App For Agent

随着Agent工具的发展，简单平铺 toolcalling 的方式已经无法满足复杂场景的需求。成百上千的工具调用会让agent的注意力彻底分散，导致它最终只专注于用终端之类的几个基本工具完成一切。

正如我们将邮件列表，发件功能，联系人等功能整合到一个邮件APP中，将好友列表，收藏，动态等功能整合到一个即时通讯APP中一样，Daat Locus 认为 Agent 也需要原生的 App 生态。现在的 MCP、Workflow 等概念很接近，但并不完全符合此理念。

一个真正的 Agent App 应该满足以下条件：

- 标准化：App 必须符合清晰、固定、标准的格式、以便 Agent 方便的管理它们。而不是零散的说明和脚本。
- 状态化：App 应该有自己的状态和生命周期，而不是一堆工具调用和说明。
- 交互式：不应该让 Agent 使用 App 时负责主动了解 App 的结构化状态。App应有能力在被聚焦时渲染内容到 Agent 的上下文中。典型的反模式是通过 `list_xxx` 让 Agent 自己查看 App 状态。
- 前后台管理：App 应该有能力在后台存在影响，如发送通知等。
- 自说明：App 自身应该说明自己的使用方法，不应让 Agent 猜测或者必须读取额外 workflow 资产才能理解。

因此，Daat Locus 将工具管理提升到 App 级别，提供了一个原生支持 Agent App 的运行时环境。同时 Daat Locus 分类了系统 App (直接在Agent Runtime内编码实现) 和 第三方 App (可额外安装或者由 Agent 自己编写)，兼具不可或缺的基本能力和可拓展性。

### 睡眠驱动的异步自优化

通过异步的睡眠机制，Daat Locus实现了在空闲时间对Agent行为模式的自优化能力。这参考了 [Hermes Agent](https://github.com/NousResearch/hermes-agent) 和 [EvoMap](https://github.com/EvoMap/evolver) 的设计理念。

但 Daat Locus 不把“自我改进”放在前台运行里强行完成，而是把它设计成一个独立的睡眠阶段。

“清醒”时，Agent 对于多步任务会绑定合适的 workflow，或者在没有可复用项时新建 workflow 来确认自己的工作流。此时实际工作会积累大量的实践经验，留待睡眠阶段进行分析。

睡眠阶段会消费运行过程中积累的 traces 和 runtime reviews，进行模式挖掘，并从中提炼出更高层的治理信号，修正、优化、合并 system prompt 和 workflows，反哺“清醒”时的行为模式，从而更加从容客观地改进 Agent 的行为。

## 启动

目前 Daat Locus 处于开发阶段，仅支持源码运行，所以首先需要克隆代码：

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
```

Daat Locus 依赖 Hindsight 作为长期记忆后端。

推荐使用 Docker 启动 Hindsight：

```powershell
docker run --rm -it -p 8888:8888 -p 9999:9999 `
  -e HINDSIGHT_API_LLM_PROVIDER=lmstudio `
  -e HINDSIGHT_API_LLM_MODEL=gpt-4.1 `
  -e HINDSIGHT_API_LLM_API_KEY=dummy `
  -e HINDSIGHT_API_LLM_BASE_URL=http://host.docker.internal:3030/v1 `
  -v ${HOME}\.hindsight-docker:/home/hindsight/.pg0 `
  ghcr.io/vectorize-io/hindsight:latest
```

`~/.daat-locus/config.toml` 需要这样配置：

```toml
[main_model]
request_timeout_secs = 300
stream_idle_timeout_secs = 45
thinking_budget = "medium" # 可选；仅对支持“思考预算/effort 等级”的模型生效
rpm = 30 # 可选；本地每分钟请求上限，避免按次数限流的 provider 上多打无效请求

[hindsight]
base_url = "http://localhost:8888"
api_key = "" # 本地无鉴权服务（默认如此）可留空
namespace = "default"
bank_id = "daat-locus"
request_timeout_secs = 180 # retain 可能较慢，建议至少 3 分钟
```

如果你想启用 Telegram 支持，请继续添加：

```toml
[telegram]
enabled = true
bot_token = "your-telegram-bot-token"
poll_timeout_secs = 30
```

只有在 `enabled = true` 且 `bot_token` 非默认占位符时，程序才会启动 Telegram 模块。

Telegram 主要用于接收私人消息事件、准备消息草稿及同步 Telegram 相关状态。

然后运行 Daat Locus：

```bash
cargo run
```
