# Daat Locus

Daat Locus 是一个长期运行 agent。目标是让 agent 拥有长期经验，并拥有自我改进能力。

## 特性

- 基于 `Hindsight` 的长期记忆与经验积累
- 睡眠驱动的自我改进
- 基于多设备理念的工具管理
- 自动适配模型的 prompt compile

## Hindsight

Daat Locus 依赖 Hindsight 作为长期记忆后端。

推荐使用 Docker 启动：

```powershell
docker run --rm -it -p 8888:8888 -p 9999:9999 `
  -e HINDSIGHT_API_LLM_PROVIDER=lmstudio `
  -e HINDSIGHT_API_LLM_MODEL=gpt-4.1 `
  -e HINDSIGHT_API_LLM_API_KEY=dummy `
  -e HINDSIGHT_API_LLM_BASE_URL=http://host.docker.internal:3030/v1 `
  -v ${HOME}\.hindsight-docker:/home/hindsight/.pg0 `
  ghcr.io/vectorize-io/hindsight:latest
```

`~/.daat-locus/config.toml` 需要至少这样配置：

```toml
[hindsight]
base_url = "http://localhost:8888"
api_key = "" # 本地无鉴权服务（默认如此）可留空
namespace = "default"
bank_id = "daat-locus"
request_timeout_secs = 120 # retain 可能较慢，30s 容易误超时
default_recall_budget = "mid"
default_reflect_budget = "low" # 显式 `deep_recall` 可按需拉高

# 可选：覆盖内置的 bank contract
retain_extraction_mode = "verbose"
retain_custom_instructions = ""
enable_observations = true
disposition_skepticism = 4
disposition_literalism = 4
disposition_empathy = 3
entities_allow_free_form = true

# reflect_mission = "..."
# retain_mission = "..."
# observations_mission = "..."
```

如果不显式配置，Daat Locus 会自动写入一套默认 bank contract，包括：

- `reflect_mission`：要求 Hindsight 以 Daat Locus runtime maintainer 的视角做可审查推理。
- `retain_mission`：优先保留 runtime 边界、项目事实、失败模式、用户偏好和可复用策略。
- `observations_mission`：把重复证据沉淀成 durable knowledge，而不是保留一次性状态。
- `directives`：默认同步 3 条高优先级规则，例如基于证据下结论、保持 runtime 边界、避免把瞬时状态固化成长期事实。
- `mental_models`：默认维护 `Project State`、`Runtime Boundaries`、`User Preferences`、`Runtime Strategy` 四类模型。

如果你想自定义 directives 或 mental models，可以直接在配置里声明：

```toml
[[hindsight.directives]]
id = "ground-claims-in-evidence"
name = "Ground Claims In Evidence"
content = "Prefer conclusions that can be tied back to retrieved memories, observations, or mental models."
priority = 100
is_active = true
tags = ["runtime", "reasoning"]

[[hindsight.mental_models]]
id = "project-state"
name = "Project State"
source_query = "What is the current project state of Daat Locus, including active workstreams, unresolved technical threads, and recently stabilized decisions?"
max_tokens = 1600
tags = ["mental-model", "scope:project", "scope:runtime"]
refresh_after_consolidation = true
```

运行时会在连接 Hindsight 后自动 bootstrap bank config 和 directives。mental models 会在 `sleep` 后刷新，也可以手动触发。

## 启动

先启动 [Hindsight](#hindsight)，再启动 Daat Locus：

```bash
cargo run
```

## 记忆治理

当前提供的 Hindsight 管理命令：

```bash
cargo run -- hindsight config
cargo run -- hindsight directives
cargo run -- hindsight mental-models
cargo run -- hindsight clear-observations
cargo run -- hindsight refresh-mental-models
```

常用重置命令：

```bash
cargo run -- reset memory
cargo run -- reset all
```

`reset memory` 会清空：

- 本地 `runtime_conversation`
- 本地 `hindsight_queue`
- `reasoning_traces.jsonl` 与 `runtime_reviews.jsonl`
- Hindsight bank 中的 memories、observations、directives、mental models
- 当前 runtime plan

`reset memory` 不会清空：

- `config/`
- `state/`
- `artifacts/`
- `logs/`
