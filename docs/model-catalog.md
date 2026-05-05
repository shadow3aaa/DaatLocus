# Model Catalog

Daat Locus uses `src/model_catalog.rs` as a static fallback catalog for model
context windows and maximum completion tokens.

The catalog is not the source of truth for live provider capabilities. It is a
local fallback used when provider model discovery does not return capacity
metadata. Values returned by a provider API during setup take precedence over
this static catalog.

## Source

The current catalog is based on LiteLLM model metadata, with manual review for
entries that need Daat Locus-specific conservative defaults.

Documented snapshot date: 2026-04-25.

Primary fields:
 
- `model_id`: normalized, lower-case provider model identifier.
- `context_window_tokens`: total context window used for budgeting.
- `max_completion_tokens`: maximum output budget used for conservative setup.
- `supports_vision`: whether the model accepts image/vision input (from litellm
  `supports_vision` field when available, falling back to name heuristic).

## Refresh Process

Use this manual process until a checked-in refresh script exists:

1. Fetch the latest LiteLLM model metadata.
2. Extract model id, context window, and maximum output tokens.
3. Normalize model ids by trimming whitespace and lowercasing.
4. Use conservative defaults when a field is missing or ambiguous.
5. Sort entries strictly by model id.
6. Deduplicate exact model ids.
7. Keep the data table in `src/model_catalog.rs`.
8. Keep provider-specific discovery and fallback logic in `src/config_wizard.rs`.
9. Update the snapshot date in this document.
10. Run:

```sh
cargo test --no-default-features model_catalog config_wizard::tests::model_capacity
```

## Boundary

`src/model_catalog.rs` should remain a data table plus minimal lookup helpers.
Do not add provider API calls, fuzzy matching, setup wizard logic, or runtime
selection state to it.

Provider discovery belongs in `src/config_wizard.rs`. Runtime provider clients
belong in `src/providers.rs`.

Unknown models must fall back to conservative capacity defaults rather than
substring matching similar known model names.
