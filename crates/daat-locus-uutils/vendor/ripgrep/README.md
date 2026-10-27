# ripgrep CLI source

Vendored from the published `ripgrep` 15.2.0 crate, `crates/core/`:
https://github.com/BurntSushi/ripgrep/tree/15.2.0/crates/core

The CLI is compiled as private modules of `daat-locus-uutils`; its supporting
libraries remain Cargo dependencies. The host calls it only at process entry.
Local changes remove the binary's module/allocator declarations, expose its entry
function, and retain the upstream version in its version output. The embedding
service owns its allocator and process termination. Preserve these changes when
updating the pinned upstream source.

Upstream copyright and license terms are in COPYING, LICENSE-MIT and UNLICENSE.
