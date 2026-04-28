# Configuration

Daat Locus stores runtime configuration under `~/.daat-locus`. Prefer the
interactive setup and config commands unless you are debugging or reviewing a
specific config change.

## Files

- `~/.daat-locus/config.toml`: provider, model, daemon, sandbox, Hindsight, and
  Telegram configuration.
- `~/.daat-locus/persona.md`: local persona text used by the runtime.

## Interactive Commands

```bash
cargo run -- config
cargo run -- config show
cargo run -- config add-provider
cargo run -- config add-model
cargo run -- config set-main-model
cargo run -- config set-hindsight-model
cargo run -- config set-telegram
```

`config show` masks secrets. Provider credentials may also reference environment
variables with `$NAME`, `${NAME}`, or `env:NAME`.

## Core Shape

- `[providers]`: provider credential registry.
- `[models]`: model definition registry.
- `locale`: UI localization language.
- `main_model`: model key used by the main runtime.
- `[daemon]`: daemon port.
- `[judge]`: judge / pairwise evaluation config.
- `[sandbox]`: optional strong filesystem sandbox mode.
- `[hindsight]`: Daat Locus-managed `hindsight-embed` config.
- `[telegram]`: Telegram transport config.

## Minimal Example

```toml
locale = "en-US"
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
profile = "daat-locus"
port = 8888

[telegram]
enabled = true
bot_token = "your-telegram-bot-token"
poll_timeout_secs = 30
```

## Provider Notes

Supported provider types are configured through `config add-provider`:

- `openai`: OpenAI API key provider.
- `openai-compatible`: OpenAI-compatible HTTP API provider.
- `github-copilot`: GitHub Copilot provider.
- `openai-codex-oauth`: ChatGPT Codex OAuth provider.

OpenAI Codex OAuth uses the ChatGPT Codex Responses backend rather than a
public OpenAI API key. Browser callback login is the default flow; device-code
login remains available as a fallback. Rotating OAuth credentials are stored in
a private auth JSON file, while `config.toml` keeps only the auth-file path.

`hindsight-embed` currently does not support the ChatGPT Codex Responses
backend. If the main model uses OpenAI Codex OAuth, set `hindsight.model` to a
model backed by another provider.

## Hindsight

Daat Locus manages `hindsight-embed` automatically from an embedded sidecar
bundled into the binary. Runtime startup does not use `uvx`, pip, or package
downloads.

`hindsight.model = "model-key"` is optional. If unset, Hindsight falls back to
`main_model`.

## Judge

`judge.model = "model-key"` is optional. If unset, the judge also falls back to
`main_model`.

## Telegram

If `telegram.enabled = true` but `bot_token` is empty or still the placeholder
value, the Telegram transport is not enabled.

Telegram is treated as a transport and event source. It is configured here, but
it is not modeled as an agent app.

## Sandbox

`sandbox.strong_filesystem` controls the optional strong filesystem sandbox:

- `"off"`: default lightweight guard only.
- `"auto"`: use a supported strong backend when available.
- `"required"`: fail child process launch if a strong backend cannot be used.

See [Sandbox backend selection](sandbox-backend-selection.md) for the platform
backend design.
