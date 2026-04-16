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

`main_model.request_timeout_secs` 是单次 runtime turn 请求的总超时；`main_model.stream_idle_timeout_secs` 是流式响应连续无新 chunk 的空闲超时。两者都会触发日志和重试，避免 UI 长时间停在“runtime turn 正在运行”。

`main_model.thinking_budget` 是可选的思考预算等级，例如 `low` / `medium` / `high`。Daat Locus 会在请求体里按常见兼容格式注入；如果当前 provider 明确拒绝该参数，会自动回退为不带思考预算继续请求，不会把整条链路打断。

`main_model.rpm` 是本地 requests-per-minute 限制。启用后，Daat Locus 会在每次发 HTTP 请求前先做节流，避免对按次数限流的 provider 反复打出必然失败的请求。

Hindsight 的 bank contract 现在完全内置在代码里，会自动 bootstrap，包括：

- `reflect_mission`：要求 Hindsight 以 Daat Locus runtime maintainer 的视角做可审查推理。
- `retain_mission`：优先保留 runtime 边界、项目事实、失败模式、用户偏好和可复用策略。
- `observations_mission`：把重复证据沉淀成 durable knowledge，而不是保留一次性状态。
- `directives`：默认同步 3 条高优先级规则，例如基于证据下结论、保持 runtime 边界、避免把瞬时状态固化成长期事实。
- `mental_models`：默认维护 `Project State`、`Runtime Boundaries`、`User Preferences`、`Runtime Strategy` 四类模型。

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
