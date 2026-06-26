# Configuration

Daat Locus stores runtime configuration under `~/.daat-locus`. Prefer the
interactive setup and config commands unless you are debugging or reviewing a
specific config change.

## Files

- `~/.daat-locus/config.toml`: provider, model, daemon, sandbox, and Telegram
  configuration.
- `~/.daat-locus/persona.md`: local persona text used by the runtime.

## Interactive Commands

```bash
cargo run -- config
cargo run -- config show
```

`config show` masks secrets. Provider credentials may also reference environment
variables with `$NAME`, `${NAME}`, or `env:NAME`.

The JSON Schema for `config.toml` is committed at `schemas/config.schema.json`.
Editors can reference it through GitHub raw, for example:

```toml
# yaml-language-server: $schema=https://raw.githubusercontent.com/shadow3aaa/DaatLocus/main/schemas/config.schema.json
```

## Core Shape

- `[providers]`: provider credential registry.
- `[models]`: model definition registry.
- `locale`: UI localization language.
- `main_model`: model key used by the main runtime.
- `efficient_model`: model key used by non-main-loop work such as judge and compaction.
- `[daemon]`: daemon port. The daemon listens on all IPv4 interfaces (`0.0.0.0`) for LAN access; protect remote dashboard/API access with daemon tokens.
- `[judge]`: judge / pairwise evaluation config.
- `[sandbox]`: runtime sandbox controls.
- `[telegram]`: Telegram transport config.

## Minimal Example

```toml
locale = "en-US"
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
# Daat Locus listens on 0.0.0.0:<port>, so LAN clients can open
# http://<this-machine-lan-ip>:<port>/ and authenticate with a daemon token.
port = 53825

[sandbox]
enabled = true
strong_filesystem = "off"

[telegram]
enabled = true
bot_token = "your-telegram-bot-token"
poll_timeout_secs = 30
```

## Daemon LAN Access

The Daat Locus daemon binds to `0.0.0.0:<daemon.port>` instead of a loopback
address, so machines on the same LAN can open the embedded WebUI at
`http://<this-machine-lan-ip>:<daemon.port>/`. Dashboard APIs, command APIs,
and the WebSocket stream require a daemon token; create one with
`daat-locus token create <name>` and paste it into the WebUI login page.

## Provider Notes

Supported provider types can be configured through the interactive `config` menu:

- `openai`: OpenAI API key provider.
- `openai-compatible`: OpenAI-compatible HTTP API provider.
- `github-copilot`: GitHub Copilot provider.
- `openai-codex-oauth`: OpenAI Codex provider.

OpenAI Codex uses the ChatGPT Codex Responses backend rather than a
public OpenAI API key. Browser callback login is the default flow; device-code
login remains available as a fallback. Rotating OAuth credentials are stored in
a private auth JSON file, while `config.toml` keeps only the auth-file path.

Model `thinking_budget` is a provider-agnostic optional enum: `none`,
`minimal`, `low`, `medium`, `high`, or `max`. Daat Locus lowers it to each
provider's supported request shape. For OpenAI Codex, `max` is sent as
Codex's `xhigh` reasoning effort. Providers that reject thinking controls are
retried without them.


## Judge

`judge.model = "model-key"` is optional. If unset, the judge uses
`efficient_model`.

## OpenSkills

Daat Locus scans OpenSkills at session startup and injects a lightweight
`<skills>` index into the runtime system prompt. Skill bodies are not preloaded;
when a skill applies, the model reads that skill's `SKILL.md` from the listed
path before using it. If claimed user input explicitly names a unique skill as
`$skill-name`, Daat Locus also injects that `SKILL.md` body into the current
turn's afterclaim context.

Default roots:

- project `.agents/skills` directories from the detected project root to the
  runtime working directory
- `~/.daat-locus/skills`
- `~/.agents/skills`

There is no global config switch for OpenSkills and there are no configurable
scan roots. If no skill exists in the fixed roots, no `<skills>` block is
injected.

Each skill is a directory containing `SKILL.md` with YAML frontmatter:

```markdown
---
name: demo
description: Use this skill for demo workflows.
---

# Demo
```

`name` is optional and defaults to the skill directory name. `description` is
required because it is the prompt-visible routing summary. A skill may also
include `agents/openai.yaml`; `policy.allow_implicit_invocation = false` keeps
it out of the automatic prompt index.

Use the dashboard slash command `/skills` to inspect loaded skills. Supported
forms are:

- `/skills` or `/skills list`
- `/skills show <skill>`
- `/skills disable <skill>`
- `/skills enable <skill>`
- `/skills reload`

`/skills disable <skill>` keeps the skill available for explicit `$skill-name`
use, but removes it from the automatic prompt index. User overrides are stored
in `config/openskills.toml` as disabled `SKILL.md` paths. The file is removed
again when no local override remains.

## Telegram

If `telegram.enabled = true` but `bot_token` is empty or still the placeholder
value, the Telegram transport is not enabled.

Telegram is treated as a transport and event source. It is configured here, but
it is not modeled as an agent app.

## Sandbox

`sandbox.enabled = false` disables the runtime sandbox entirely. This removes
the lightweight path guard, stops stripping protected environment variables from
child processes, and ignores `sandbox.strong_filesystem`.

When `sandbox.enabled = true`, `sandbox.strong_filesystem` controls the optional
strong filesystem sandbox:

- `"off"`: default lightweight guard only.
- `"auto"`: use a supported strong backend when available.
- `"required"`: fail child process launch if a strong backend cannot be used.

See [Sandbox backend selection](sandbox-backend-selection.md) for the platform
backend design.
