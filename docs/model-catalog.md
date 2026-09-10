# Model Catalog

Daat Locus uses the [models.dev](https://models.dev) catalog as a local fallback
for model context windows, maximum completion tokens, vision/tool-call
capability, and reasoning options.

The catalog is not the source of truth for live provider capabilities. It is a
fallback used when provider model discovery does not return capacity metadata.
Values returned by a provider API during setup take precedence over this
catalog.

## Source

- `build.rs` downloads `https://models.dev/api.json` into `OUT_DIR` and embeds
  it into the binary through `include_str!` in `src/model_catalog.rs`.
- At runtime, `~/.daat-locus/cache/models-dev-api.json` takes precedence over
  the embedded snapshot.
- The Manager refreshes that cache in the background on startup through
  `model_catalog::refresh_models_dev_cache()`.

Fields used:

- `limit.context`: total context window used for budgeting.
- `limit.output`: maximum output budget used for conservative setup.
- `modalities.input`: whether the model accepts image/vision input.
- `tool_call`: whether the model supports tool calls.
- `reasoning_options`: reasoning controls offered by the model.
- provider `api` URL: maps configured provider base URLs to catalog sections.

Unknown models fall back to conservative capacity defaults. Do not add substring
matching for similar known model names.

## Refresh Process

Catalog refresh is automatic:

1. Building the crate re-runs `build.rs`, which downloads a fresh snapshot when
   the build script re-runs.
2. Manager startup refreshes `~/.daat-locus/cache/models-dev-api.json` in the
   background.

To force a refresh manually:

1. Re-run the build script by touching `build.rs` and rebuilding, or delete the
   build script output under `target/`.
2. Optionally refresh the runtime cache by replacing
   `~/.daat-locus/cache/models-dev-api.json` with a fresh
   `https://models.dev/api.json` response.

Validate with:

```sh
cargo test --bin daat-locus model_catalog
cargo test --bin daat-locus config_wizard
```

## Boundary

`src/model_catalog.rs` should remain a catalog loader plus minimal lookup
helpers. Do not add provider API calls, fuzzy matching, setup wizard logic, or
runtime selection state to it.

Provider discovery belongs in `src/config_wizard.rs`. Runtime provider clients
belong in `src/providers.rs`.
