# Config Readiness Rules

### Manager Boot And Config Readiness

Manager startup must not depend on complete agent/runtime configuration.

The Manager is the product shell and public control plane. It must be able to
start, serve WebUI, authenticate requests, expose logs/status, and guide
configuration even when agent configuration is missing, incomplete, or damaged.
Configuration readiness controls whether Session workers and agent operations
are allowed; it does not decide whether the Manager can bind its port.

The only config value needed for Manager boot is the daemon port. Read it
through a lightweight boot reader, not full agent config validation:

```text
read_manager_boot_config()
  -> read daemon.port from config.toml if possible
  -> if current config is damaged, try config.toml.bak
  -> if both are unavailable or damaged, use default port 53825
```

Do not call full `load_config()` to decide whether the Manager can start. The
port belongs to Manager boot config; provider/model configuration belongs to
agent runtime readiness.

Config readiness distinguishes these cases:

```text
damaged       config.toml cannot be parsed or deserialized reliably
unconfigured no real agent config exists, including only-port config
incomplete   partial agent config exists but cannot run an agent
complete     config parses and validates as runnable agent config
```

Only `unconfigured`, `incomplete`, and `complete` are durable public readiness
states. `damaged` is a recovery path, not a long-term operating mode. Public
readiness responses should surface it as a recovery note and then report the
post-recovery state. On startup or readiness refresh:

1. Move damaged `config.toml` to a timestamped `.corrupt-*` file.
2. Try restoring `config.toml.bak`.
3. If the restored backup parses, classify readiness from it.
4. If the backup is missing or damaged, move the bad backup aside and write a
   setup-safe default config.

The setup-safe default config must not be `Config::default()` if that contains
fake providers, fake API keys, or fake model entries. It should contain only
Manager boot-safe values such as the default daemon port. Fake provider/model
placeholders must never make readiness look `complete`.

`unconfigured` means no real agent config exists. This includes no
`config.toml`, an only-port config, or the setup-safe default written during
recovery. WebUI and TUI route to initialization. Agent/session creation,
`/send`, runtime dashboard commands, and Session worker startup are disabled
with a clear config-not-ready error.

`incomplete` means the config parses and contains some agent configuration
intent, but cannot run the agent. Examples include provider without valid model
roles, model references to missing providers, missing `main_model` or
`efficient_model`, or empty required credential/base URL fields. WebUI routes
to settings/configuration completion, TUI routes to interactive config repair,
and agent operations remain disabled.

`complete` means provider/model/main/efficient references are valid and the
runtime can construct model providers. Only this state enables agent operations.

`config.toml.bak` is the latest successfully parsed config:

- every successful config parse updates `.bak` atomically
- every successful config write updates `.bak` atomically
- recovery preserves damaged files as `.corrupt-*`
- if both current config and backup are damaged, write setup-safe defaults and
  classify readiness as `unconfigured`

The Manager may serve these regardless of readiness:

- embedded WebUI
- auth/token endpoints
- `/health` and `/status`
- config readiness and setup endpoints
- logs
- settings/setup pages

These require `complete` readiness:

- creating sessions
- Session worker startup
- `/send`
- `/commands/run`
- runtime dashboard actions
- any operation that needs provider/model config

If config is deleted, damaged, or changed after Manager startup, readiness must
be recomputed. New agent operations must be rejected until readiness returns to
`complete`. A stricter implementation may stop or pause existing Session
workers when readiness degrades.

WebUI startup reads readiness before normal routing:

```text
damaged/recovered -> show recovery note, then route by final state
unconfigured      -> setup wizard
incomplete        -> settings/configuration completion
complete          -> normal app
```

TUI startup follows the same state:

```text
unconfigured -> initialization wizard
incomplete   -> interactive config repair
complete     -> normal session selector
```

WebUI must not duplicate TUI setup/probing logic. Extract shared Rust setup
logic, for example a `config_setup` module, to own setup-safe defaults,
provider/model input normalization, probing, readiness classification,
validation, atomic writes, and backup updates. TUI and WebUI are frontends over
that shared layer only.
