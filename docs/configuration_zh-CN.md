# 配置

Daat Locus 的运行时配置保存在 `~/.daat-locus` 下。除非正在调试或审查某个具体配置变更，否则优先使用交互式 setup 和 config 命令。

## 文件

- `~/.daat-locus/config.toml`：provider、model、daemon、sandbox、Hindsight 和 Telegram 配置。
- `~/.daat-locus/persona.md`：运行时使用的本地人格文本。

## 交互式命令

```bash
cargo run -- config
cargo run -- config show
cargo run -- config add-provider
cargo run -- config add-model
cargo run -- config set-main-model
cargo run -- config set-hindsight-model
cargo run -- config set-telegram
```

`config show` 会脱敏 secret。Provider 凭据也可以用 `$NAME`、`${NAME}` 或 `env:NAME` 引用环境变量。

## 核心结构

- `[providers]`：provider 凭据注册表。
- `[models]`：模型定义注册表。
- `locale`：用户界面本地化语言。
- `main_model`：主运行时使用的 model key。
- `[daemon]`：daemon 端口。
- `[judge]`：judge / pairwise 评估配置。
- `[sandbox]`：可选强文件系统沙箱模式。
- `[hindsight]`：由 Daat Locus 托管的 `hindsight-embed` 配置。
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
request_timeout_secs = 300
stream_idle_timeout_secs = 45
context_window_tokens = 128000
effective_context_window_percent = 95
max_completion_tokens = 4000
tool_output_max_tokens = 2000

[daemon]
port = 53825

[sandbox]
strong_filesystem = "off"

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

## Provider 说明

支持的 provider type 通过 `config add-provider` 配置：

- `openai`：OpenAI API key provider。
- `openai-compatible`：OpenAI-compatible HTTP API provider。
- `github-copilot`：GitHub Copilot provider。
- `openai-codex-oauth`：ChatGPT Codex OAuth provider。

OpenAI Codex OAuth 使用 ChatGPT Codex Responses backend，而不是公开 OpenAI API key 路径。默认登录方式是浏览器本地回调，device-code 登录保留为 fallback。轮换 OAuth 凭据保存在私有 auth JSON 文件里，`config.toml` 只保存 auth 文件路径。

`hindsight-embed` 目前还不支持 ChatGPT Codex Responses backend。如果主模型使用 OpenAI Codex OAuth，请把 `hindsight.model` 设为另一个 provider 支持的模型。

## Hindsight

Daat Locus 会自动托管 `hindsight-embed`。不需要在启动前手动起 Docker 或单独跑服务。

`hindsight.model = "model-key"` 是可选项。为空时，Hindsight 回退到 `main_model`。

## Judge

`judge.model = "model-key"` 是可选项。为空时，judge 同样回退到 `main_model`。

## Telegram

如果 `telegram.enabled = true`，但 `bot_token` 为空或仍是占位符，Telegram transport 不会真正启用。

Telegram 是 transport 和 event source。它在这里配置，但不是 agent app。

## Sandbox

`sandbox.strong_filesystem` 控制可选强文件系统沙箱：

- `"off"`：默认，仅使用轻量保护。
- `"auto"`：可用时使用支持的强沙箱 backend。
- `"required"`：无法使用强沙箱 backend 时，子进程启动失败。

平台 backend 设计见 [Sandbox backend 选择](sandbox-backend-selection.md)。
