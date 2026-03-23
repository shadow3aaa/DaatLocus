# Spinova

Spinova 是一个长期运行 agent。目标是让 agent 拥有长期经验，并拥有自我改进能力。

## 特性

- 基于 `Hindsight` 的长期记忆与经验积累
- 睡眠驱动的自我改进
- 基于多设备理念的工具管理
- 自动适配模型的 prompt compile

## Hindsight

Spinova 依赖 Hindsight 作为长期记忆后端。没有 Hindsight，进程会直接启动失败。

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

`~/.spinova/config.toml` 需要这样配置：

```toml
[hindsight]
base_url = "http://localhost:8888"
api_key = "" # 本地无鉴权服务（默认如此）可留空
namespace = "default"
bank_id = "spinova"
request_timeout_secs = 30
default_recall_budget = "mid"
default_reflect_budget = "low"
```

## 启动

先启动 [Hindsight](#hindsight)，再启动 Spinova：

```bash
cargo run
```
