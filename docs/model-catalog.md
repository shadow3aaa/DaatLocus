# Model Catalog Infrastructure

The retained [catalog loader](../src/model_catalog.rs) supplies fallback model
capacity and capability metadata. Live provider discovery remains in
[model_discovery.rs](../src/model_discovery.rs); provider clients remain in
[providers.rs](../src/providers.rs).

- A valid local models.dev cache takes precedence over compiled catalog data.
- Missing catalog entries use conservative capacity defaults.
- Catalog lookups use model/provider identifiers and configured API URLs; do not
  infer capacities by fuzzy matching similar model names.
- Context/output limits, input modalities, tool-call support, and reasoning
  options are metadata, not guarantees that a live provider will accept a request.

The provider settings page opens secondary provider/model editors within the
settings dialog. Entering model selection automatically queries the selected
provider. Discovery refreshes models.dev at most daily, retries failed refreshes
after five minutes, and publishes a validated snapshot immediately after an
atomic cache write. Failed or malformed updates retain the previous snapshot;
missing or invalid caches fall back to the compiled catalog.

Live model IDs receive catalog capacity and reasoning metadata. If the provider
query is unavailable or empty, the API identifies its suggestions as catalog
results, not live availability. Model selection supplies capacity/vision defaults;
reasoning and capacity controls accept directory options or custom values.
Token-budget reasoning metadata preserves its range rather than discarding it.
Provider/model edits retain explicit Save; other preferences auto-save. The old
Manager lifecycle is not retained.

For an offline library check, set `DAAT_LOCUS_MODEL_CATALOG_PATH` to an existing,
valid models.dev JSON snapshot, then run
`cargo check --locked --offline --lib`. The catalog path supplies the compiled
snapshot without downloading it. Backend-only checks do not build/install the
frontend when `DAAT_LOCUS_BUILD_WEBUI=0` is explicitly set. Normal builds embed it. Cargo's `--offline`
alone does not prevent a build script from making
network requests. Without the catalog path, the build script fetches the catalog.

Keep catalog loading and lookup independent of Actor state, setup UI, and
provider network protocols. Record catalog changes and validate the relevant
loader/discovery tests when those modules change.
