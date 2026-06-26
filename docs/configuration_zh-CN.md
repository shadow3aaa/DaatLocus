# 配置

Daat Locus 的运行时配置保存在 `~/.daat-locus` 下。除非正在调试或审查某个具体配置变更，否则优先使用交互式 setup 和 config 命令。

## 文件

- `~/.daat-locus/config.toml`：provider、model、daemon、sandbox 和 Telegram 配置。
- `~/.daat-locus/persona.md`：运行时使用的本地人格文本。

## 交互式命令

```bash
cargo run -- config
cargo run -- config show
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
- `efficient_model`：judge、compaction 等非主循环工作使用的 model key。
- `[daemon]`：daemon 端口。daemon 会监听所有 IPv4 接口（`0.0.0.0`）以支持 LAN 访问；远程 Dashboard/API 访问需要用 daemon token 保护。
- `[judge]`：judge / pairwise 评估配置。
- `[sandbox]`：运行时沙箱控制。
- `[telegram]`：Telegram transport 配置。

## 最小示例

```toml
locale = "zh-CN"
main_model = "default"
efficient_model = "default"

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

Daat Locus daemon 绑定到 `0.0.0.0:<daemon.port>`，不再绑定回环地址，因此同一 LAN 内的设备可以通过 `http://<本机-LAN-IP>:<daemon.port>/` 打开内嵌 WebUI。Dashboard API、命令 API 和 WebSocket stream 仍需要 daemon token；可用 `daat-locus token create <name>` 创建，然后在 WebUI 登录页粘贴。

## Provider 说明

支持的 provider type 可通过交互式 `config` 菜单配置：

- `openai`：OpenAI API key provider。
- `openai-compatible`：OpenAI-compatible HTTP API provider。
- `github-copilot`：GitHub Copilot provider。
- `openai-codex-oauth`：OpenAI Codex provider。

OpenAI Codex 使用 ChatGPT Codex Responses backend，而不是公开 OpenAI API key 路径。默认登录方式是浏览器本地回调，device-code 登录保留为 fallback。轮换 OAuth 凭据保存在私有 auth JSON 文件里，`config.toml` 只保存 auth 文件路径。

模型的 `thinking_budget` 是 provider 无关的可选枚举：`none`、`minimal`、`low`、`medium`、`high` 或 `max`。Daat Locus 会把它降级/映射到各 provider 支持的请求形态。对 OpenAI Codex，`max` 会作为 Codex 的 `xhigh` reasoning effort 发送。若 provider 拒绝 thinking 控制，运行时会不带该参数重试。


## Judge

`judge.model = "model-key"` 是可选项。为空时，judge 使用 `efficient_model`。

## OpenSkills

Daat Locus 会在 session 启动时扫描 OpenSkills，并把轻量 `<skills>` 索引注入运行时系统 prompt。Skill body 不会预加载；当某个 skill 适用时，模型会先按索引里的路径读取该 skill 的 `SKILL.md`，再使用其中的指令。如果已 claim 的用户输入用 `$skill-name` 显式点名了唯一的 skill，Daat Locus 也会把该 `SKILL.md` body 注入当前 turn 的 afterclaim context。

默认扫描路径：

- 从检测到的项目根目录到运行时工作目录之间的 `.agents/skills`
- `~/.daat-locus/skills`
- `~/.agents/skills`

OpenSkills 没有全局配置开关，也没有可配置扫描目录。如果固定目录里没有 skill，就不会注入 `<skills>` 块。

每个 skill 是一个包含 `SKILL.md` 的目录，`SKILL.md` 需要 YAML frontmatter：

```markdown
---
name: demo
description: Use this skill for demo workflows.
---

# Demo
```

`name` 可选，默认使用 skill 目录名。`description` 必填，因为它是 prompt 可见的路由摘要。Skill 也可以包含 `agents/openai.yaml`；设置 `policy.allow_implicit_invocation = false` 后，它不会进入自动 prompt 索引。

使用 dashboard slash command `/skills` 查看和管理已加载 skills：

- `/skills` 或 `/skills list`
- `/skills show <skill>`
- `/skills disable <skill>`
- `/skills enable <skill>`
- `/skills reload`

`/skills disable <skill>` 只会把该 skill 移出自动 prompt 索引；它仍然可以通过显式 `$skill-name` 使用。用户 override 存在 `config/openskills.toml`，记录被禁用自动使用的 `SKILL.md` 路径；没有 override 时这个文件会被移除。

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
