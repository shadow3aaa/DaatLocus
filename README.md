# Spinova

自用通用agent。

## 运行

```bash
# ubuntu
sudo apt install protobuf-compiler
# macos
brew install protobuf
cargo run
```

## Hindsight

长期记忆依赖 Hindsight。

## 启动

如果没有现成的 Hindsight 服务，最简单是用 Docker：

```powershell
docker run --rm -it -p 8888:8888 -p 9999:9999 `
  -e HINDSIGHT_API_LLM_PROVIDER=lmstudio `
  -e HINDSIGHT_API_LLM_MODEL=gpt-4.1 `
  -e HINDSIGHT_API_LLM_API_KEY=dummy `
  -e HINDSIGHT_API_LLM_BASE_URL=http://host.docker.internal:3030/v1 `
  -v ${HOME}\.hindsight-docker:/home/hindsight/.pg0 `
  ghcr.io/vectorize-io/hindsight:latest
```

然后在 `~/.spinova/config.toml` 里配置：

```toml
[hindsight]
enabled = true
base_url = "http://localhost:8888"
api_key = ""
namespace = "default"
bank_id = "spinova"
request_timeout_secs = 30
default_recall_budget = "mid"
default_reflect_budget = "low"
retain_async = true
```
