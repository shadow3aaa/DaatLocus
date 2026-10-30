# License and attribution

Daat Locus is licensed under [Apache-2.0](../LICENSE). Preserve [NOTICE](../NOTICE) when redistributing the adapted OpenAI Codex Seatbelt policy resources.

## Vendored code

| Code | Source and license files |
| --- | --- |
| Wry native WebView fork | Based on Wry 0.57.0; [upstream README](../vendor/wry/README.md), [Apache license](../vendor/wry/LICENSE-APACHE), [MIT license](../vendor/wry/LICENSE-MIT) |
| Embedded ripgrep CLI | Based on ripgrep 15.2.0; [source and adaptation notes](../crates/daat-locus-uutils/vendor/ripgrep/README.md), with COPYING, LICENSE-MIT and UNLICENSE in that directory |
| Windows driver dependencies | Preserve `LICENSE.microsoft` with the driver package built by xtask |

The Wry fork includes Daat Locus's Windows `native-composition` support. Keep its upstream license files with the fork when updating or distributing it.

## Other dependencies and assets

Rust dependencies are recorded in [Cargo.toml](../Cargo.toml) and [Cargo.lock](../Cargo.lock). Frontend packages and their versions are recorded in [package.json](../webui/package.json) and [bun.lock](../webui/bun.lock). The native client and relay have their own manifests and lockfiles.

The UI bundles Noto Sans, Noto Sans SC, Noto Emoji and Maple Mono through Fontsource packages. Preserve the font packages' SIL Open Font License texts when distributing those assets.

Tauri, the authentication WASM, embedded shell utilities and native platform libraries can add dependencies to a package. Check the licenses of the files actually shipped, including downloaded driver and WebView installer components. This page is an attribution index, not a complete license inventory for every build.
