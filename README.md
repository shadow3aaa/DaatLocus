# Spinova

自用通用agent。

## 运行

长期记忆依赖 Hindsight 服务。所以需要先启动 Hindsight 服务，再启动 Spinova。

### 1. 启动 Hindsight 服务

推荐使用 Docker 启动服务：

```powershell
docker run --rm -it -p 8888:8888 -p 9999:9999 `
  -e HINDSIGHT_API_LLM_PROVIDER=lmstudio `
  -e HINDSIGHT_API_LLM_MODEL=gpt-4.1 `
  -e HINDSIGHT_API_LLM_API_KEY=dummy `
  -e HINDSIGHT_API_LLM_BASE_URL=http://host.docker.internal:3030/v1 `
  -v ${HOME}\.hindsight-docker:/home/hindsight/.pg0 `
  ghcr.io/vectorize-io/hindsight:latest
```

`~/.spinova/config.toml` 内需要这样配置：

```toml
[hindsight]
base_url = "http://localhost:8888"
api_key = "" # 本地默认留空，如果使用了 API 密钥，请在此处填写
namespace = "default"
bank_id = "spinova"
request_timeout_secs = 30
default_recall_budget = "mid"
default_reflect_budget = "low"
```

### 2. 启动 Spinova 主程序

```bash
# ubuntu
sudo apt install protobuf-compiler
# macos
brew install protobuf
cargo run
```
