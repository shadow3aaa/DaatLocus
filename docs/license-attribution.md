# License And Attribution Notes

The project license is [Apache-2.0](../LICENSE). This note describes attribution
sources in the retained repository; it is not a completed distribution audit.

The [model schema macros](../crates/daat-locus-macros/Cargo.toml) declare Apache-2.0.
The shared [account authentication crate](../crates/daat-locus-auth/Cargo.toml)
also declares Apache-2.0. Its browser WASM contains `opaque-ke 4.0.1` (MIT OR
Apache-2.0), with the same protocol dependencies recorded in Cargo.lock.
Device execution coordination uses `openraft 0.9.21` (MIT OR Apache-2.0).
Its native storage/network integration lives in `src/stage/coordination`; it is
not included in CF Worker, which has no execution-coordination state machine.
The desktop client uses Tauri and its native window/tray dependencies (MIT OR
Apache-2.0), plus the single-instance and native-dialog plugins. Build-time icon
generation rasterizes the existing project Logo using `resvg`. Exact versions
and dependency/license information are recorded in Cargo.lock.

Rust dependency manifests and lockfiles, WebUI's package manifest and lockfile,
and any browser/runtime assets actually included in a future distribution are
the inputs to the next attribution review. Do not assume that deleting an old
packaging path constitutes that review.

Windows/macOS archives and installers include the service, Tauri client and embedded WebUI.
Audit the actual bundled dependencies and preserve their required license/notice
text before distributing them. Create or update NOTICE only when required;
do not create an empty notice file.
