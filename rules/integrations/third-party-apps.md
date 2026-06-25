# Third-Party App Rules

## Third-Party App Package

Future third-party `App` extensions use a source-first design. Do not copy another product's plugin or connector structure.

### Directory Placement

- Third-party app source directories are fixed under the runtime workspace: `~/daat-locus-workspace/apps/<app_id_snake_case>/`.
- The current runtime workspace is resolved by `resolve_runtime_workspace_dir()`, which defaults to `~/daat-locus-workspace`.
- `app_id` is exactly the folder name `<app_id_snake_case>`.
- `~/.daat-locus` is a protected runtime directory and must not store third-party app source code.
- This design exists because `~/.daat-locus` is treated as a protected runtime path inside the sandbox, while the workspace is the default editable area.

### Package Layout

Minimal directory structure:

```text
~/daat-locus-workspace/apps/<app_id_snake_case>/
  app.toml
  runtime/
    app.lua
  prompt/
    usage.md
    how_to_use.md
```

Rules:

- `runtime/app.lua` is the only Lua entry point.
- `prompt/usage.md` is the capability-domain description.
- `prompt/how_to_use.md` is the operation manual for the app's tools.
- Third-party app packages do not carry self-optimizable workflow assets.

### `app.toml`

In v1, `app.toml` is intentionally minimal. It has one responsibility: specify the relative path to the Lua entry point.

Rules:

- It does not carry `id`.
- It does not carry permissions.
- It does not carry usage, how-to-use, or workflow metadata.
- By default, the entry point is `runtime/app.lua`.

Minimal example:

```toml
entry = "runtime/app.lua"
```

Identity comes from the directory name. Configuration comes from `app.toml`.

### Lua Runtime

The third-party app runtime stack is fixed as:

- Rust side uses `mlua`.
- Lua dialect is standard `Lua 5.4`.
- Do not use legacy `rlua`.
- Do not use `JS/TS` as the v1 app runtime.
- Do not use `Wasm` as the v1 app runtime.

Rationale:

- The agent needs to be able to directly write and modify apps.
- Source-first Lua plus Markdown is a better v1 authoring format than ABI-first Wasm.
- `mlua` has mature enough Lua 5.4 support in Rust for host embedding.

### Unified Lua Interface

Do not design an app as multiple independent Lua entry scripts.

The correct model is:

- One third-party `App` equals one unified Lua module instance.
- The host loads only `runtime/app.lua`.
- `render_state`, tool calls, and notice polling share the same app instance state.

Do not introduce additional IPC to synchronize tool results and render state.

This means the behavioral body of a third-party app is an object model, not a collection of scripts.

### Workflow Assets

Self-optimizable workflows do not belong to the app package. They are runtime-level SOP primitive assets.

Rules:

- Workflows are not attached to any app by default.
- Builtin primitive specs live in repository root `workflows/*.md` and are compiled into the program by `build.rs`.
- Evolvable workspace primitive specs live in `~/daat-locus-workspace/workflows/*.md`.
- Each primitive spec is one Markdown file, and the filename is the primitive id.
- Primitive filenames may contain only lowercase `a-z` and `-`; identity comes only from the file stem.
- Primitive Markdown content is unrestricted by the primitive id; any legacy frontmatter is ignored for identity, and runtime writes specs as Markdown bodies without frontmatter.
- A primitive spec file should describe one reusable SOP primitive, not a composite task class.
- Composite task execution is a temporary runtime graph assembled from primitives; it is not a workflow asset to save by default.
- `prompt/*.md` is for app descriptions; `workflows/*.md` is for self-optimizable execution processes. Do not mix them.
- Builtin primitive specs do not fall into a writable runtime directory and are not touched by optimization pipelines.

### Reload Strategy

Third-party apps should not be fully reparsed on every turn.

Recommended strategy:

- Perform one full scan of `~/daat-locus-workspace/apps` at startup.
- Scan and watch the primitive spec directory `~/daat-locus-workspace/workflows` separately.
- Use `notify` at runtime to watch supported directory changes.
- Map file events to the affected `<app_id_snake_case>`.
- Mark only that app as dirty and reload it incrementally.
- When primitive spec files change, mark only the affected primitive as dirty and reload it incrementally.
- If the watcher fails or directory state becomes untrusted, fall back to one full rescan.

Do not make full parsing the normal path.

### State and Cache

v1 does not define a dedicated third-party app cache directory.

Current conclusions:

- Define only the source directory: `~/daat-locus-workspace/apps`.
- Define workflow source separately as `~/daat-locus-workspace/workflows`.
- Do not define `cache/apps`.
- Do not define `cache/workflows`.
- If the host later truly needs to persist app runtime state, use the protected runtime state system, for example `~/.daat-locus/state/apps/<app_id_snake_case>/`.
- If workflow telemetry later needs host persistence, use the protected runtime state system, for example `~/.daat-locus/state/workflows/`.

Third-party apps and workspace primitive specs are agent-editable assets, but they are not runtime state owned directly by the agent.
