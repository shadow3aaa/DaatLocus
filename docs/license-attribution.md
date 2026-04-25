# License And Attribution Notes

Daat Locus is distributed under the project license in `LICENSE`.

This document tracks the release-time license and attribution checks for
third-party dependencies. It is a maintenance note, not legal advice.

## Release Audit

Before a release:

1. Review direct and transitive Rust dependencies from `Cargo.lock`.
2. Check dependency license expressions against the intended release channel.
3. Review bundled or downloaded runtime components separately from Rust crates.
4. Record any attribution-sensitive dependency in release notes or a project
   `NOTICE` file when required.

Useful commands:

```sh
cargo tree
cargo metadata --format-version 1
```

If a dedicated license checker is added later, document the exact command in
`docs/release-checklist.md`.

## Watch List

Treat these license families as requiring explicit review before release:

- GPL
- LGPL
- AGPL
- MPL
- EPL
- CDDL
- SSPL
- custom source-available licenses
- dependencies that require attribution notices beyond preserving license text

Permissive licenses such as MIT, Apache-2.0, BSD, and ISC still require license
text preservation when redistributing covered code or binaries.

## Runtime Downloads

Managed downloads are not the same as Rust crate dependencies. Review these
separately:

- managed `uv` downloads
- browser runtime downloads
- Hindsight-managed Python packages and their transitive dependencies

The supply-chain pinning TODOs cover version and integrity verification. This
document covers only license and attribution tracking.

## NOTICE Threshold

Add a project-level `NOTICE` file when any shipped dependency or bundled runtime
requires attribution text that is not already satisfied by preserving its
license file.

Also add or update `NOTICE` when:

- a dependency explicitly requires prominent attribution
- a binary/runtime artifact is redistributed with its own notice file
- release packaging copies third-party source, assets, models, or data files
- a license checker flags an attribution obligation that is not covered
  elsewhere

If no dependency requires a separate notice, keep this document as the audit
record and do not create an empty `NOTICE` file.
