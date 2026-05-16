# 配置

Daat Locus 的运行时配置保存在 `~/.daat-locus` 下。除非正在调试或审查某个具体配置变更，否则优先使用交互式 setup 和 config 命令。

## 文件

- `~/.daat-locus/config.toml`：provider、model、daemon、sandbox 和 Telegram 配置。
- `~/.daat-locus/persona.md`：运行时使用的本地人格文本。

## 交互式命令

```bash
cargo run -- config
cargo run -- config show
cargo run -- config add-provider
cargo run -- config add-model
cargo run -- config set-main-model
cargo run -- config set-telegram
cargo run -- config-schema
```

`config show` 会脱敏 secret。Provider 凭据也可以用 `$NAME`、`${NAME}` 或 `env:NAME` 引用环境变量。

`config.toml` 的 JSON Schema 已提交在 `schemas/config.schema.json`。编辑器可以通过 GitHub raw 引用，例如：

```toml
# yaml-language-server: $schema=https://raw.githubusercontent.com/shadow3aaa/DaatLocus/main/schemas/config.schema.json
```

## 核心结构

- `[providers]`：provider 凭据注册表。
- `[models]`：模型定义注册表。
- `locale`：用户界面本地化语言。
- `main_model`：主运行时使用的 model key。
- `[daemon]`：daemon 端口。daemon 会监听所有 IPv4 接口（`0.0.0.0`）以支持 LAN 访问；远程 Dashboard/API 访问需要用 daemon token 保护。
- `[judge]`：judge / pairwise 评估配置。
- `[sandbox]`：运行时沙箱控制。
- `[telegram]`：Telegram transport 配置。

## 最小示例

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
thinking_budget = "medium"
request_timeout_secs = 300
stream_idle_timeout_secs = 45
context_window_tokens = 128000
effective_context_window_percent = 95
max_completion_tokens = 4000
tool_output_max_tokens = 2000

[daemon]
# Daat Locus 会监听 0.0.0.0:<port>，LAN 客户端可打开
# http://<本机-LAN-IP>:<port>/ 并用 daemon token 认证。
port = 53825

[sandbox]
enabled = true
strong_filesystem = "off"


[telegram]
enabled = true
bot_token = "your-telegram-bot-token"
poll_timeout_secs = 30
```

## Daemon LAN 访问

Daat Locus daemon 绑定到 `0.0.0.0:<daemon.port>`，不再绑定回环地址，因此同一 LAN 内的设备可以通过 `http://<本机-LAN-IP>:<daemon.port>/` 打开内嵌 WebUI。Dashboard API、命令 API 和 WebSocket stream 仍需要 daemon token；可用 `daat-locus daemon token create <name>` 创建，然后在 WebUI 登录页粘贴。

## Provider 说明

支持的 provider type 通过 `config add-provider` 配置：

- `openai`：OpenAI API key provider。
- `openai-compatible`：OpenAI-compatible HTTP API provider。
- `github-copilot`：GitHub Copilot provider。
- `openai-codex-oauth`：ChatGPT Codex OAuth provider。

OpenAI Codex OAuth 使用 ChatGPT Codex Responses backend，而不是公开 OpenAI API key 路径。默认登录方式是浏览器本地回调，device-code 登录保留为 fallback。轮换 OAuth 凭据保存在私有 auth JSON 文件里，`config.toml` 只保存 auth 文件路径。

模型的 `thinking_budget` 是 provider 无关的可选枚举：`none`、`minimal`、`low`、`medium`、`high` 或 `max`。Daat Locus 会把它降级/映射到各 provider 支持的请求形态。对 OpenAI Codex OAuth，`max` 会作为 Codex 的 `xhigh` reasoning effort 发送。若 provider 拒绝 thinking 控制，运行时会不带该参数重试。


## Judge

`judge.model = "model-key"` 是可选项。为空时，judge 同样回退到 `main_model`。

## Telegram

如果 `telegram.enabled = true`，但 `bot_token` 为空或仍是占位符，Telegram transport 不会真正启用。

Telegram 是 transport 和 event source。它在这里配置，但不是 agent app。

## Sandbox

`sandbox.enabled = false` 会完全关闭运行时沙箱。这会移除轻量路径保护，
不再从子进程环境中剥离受保护的环境变量，并忽略 `sandbox.strong_filesystem`。

当 `sandbox.enabled = true` 时，`sandbox.strong_filesystem` 控制可选强文件系统沙箱：

- `"off"`：默认，仅使用轻量保护。
- `"auto"`：可用时使用支持的强沙箱 backend。
- `"required"`：无法使用强沙箱 backend 时，子进程启动失败。

平台 backend 设计见 [Sandbox backend 选择](sandbox-backend-selection.md)。
