# SCOPE Usage

SCOPE is the semantic code search, read, edit, and propagation engine behind the
Coding app. Search returns matched source lines with line-hash anchors. The
model reads follow-up context by passing `path + line#hash` to `read_code`, then
copies read line anchors into `edit_code`.

## `search_code`

`search_code` is the normal entry point for locating code. It accepts a content
query plus optional narrowing fields such as `path`, `include`, and `limit`.
The query defaults to literal matching with smart case, so code fragments such
as `matching_commands(` do not need escaping. Use `mode: "regex"` only when a
regular expression is intended.

Common options mirror the useful parts of `rg`:

```json
{
  "query": "matching_commands(",
  "mode": "literal",
  "path": "src/dashboard",
  "include": ["*.rs"],
  "exclude": ["target/**"],
  "types": ["rust"],
  "case": "smart",
  "word": false,
  "line": false,
  "hidden": false,
  "respect_ignore": true,
  "follow": false,
  "limit": 20
}
```

- `mode: "literal" | "regex"` corresponds to `rg -F` versus regex search.
- `case: "sensitive" | "insensitive" | "smart"` corresponds to `rg -s`, `rg -i`, and `rg -S`.
- `word` and `line` correspond to `rg -w` and `rg -x`; `line` overrides `word`.
- `path` restricts the searched subtree, like passing a path to `rg`.
- `include` and `exclude` are glob arrays; exclusions are separate instead of `!glob`.
- `types` and `type_not` filter by SCOPE language type or known extension.
- `hidden`, `respect_ignore`, and `follow` correspond to `rg --hidden`, default ignore behavior, and `rg -L`.

Search returns compact matched-line hits:

```text
src/dashboard/mod.rs|1268#7a|fn run_tui_dashboard(...) {
src/dashboard/mod.rs|1291#c1|    render_dashboard(...);
```

Rules:

- Each result is the actual matched line, not an enclosing declaration.
- The path plus `line#hash` anchor is the follow-up read target.
- `line#hash` is file-local. Do not use it without the path.
- Search does not return separate target identities, canonical target labels, or
  enclosing metadata.

## `read_code`

The normal read path uses a path plus line-hash anchor:

```json
{ "path": "src/dashboard/mod.rs", "anchor": "1268#7a", "mode": "full" }
```

Explicit path ranges for imports, top-level code, search misses, and
user-specified locations belong to the runtime `read_file` tool, not to
SCOPE `read_code`.

`mode` has exactly two values:

- `around` returns a fixed local window around the anchor.
- `full` automatically returns the enclosing AST symbol when one is recognized,
  otherwise it falls back to `around`.

Read output is source text with per-line edit anchors:

```text
1268#7a|fn run_tui_dashboard(...) {
1269#c1|    ...
```

The structured read response returns only `content`. It does not repeat `path`,
`mode`, resolved range, or enclosing metadata.

## `edit_code`

`edit_code` keeps the explicit path plus line-anchor API:

```json
{
  "edits": [
    {
      "path": "src/dashboard/mod.rs",
      "op": "replace",
      "start": "1268#7a",
      "end": "1320#d4",
      "content": "fn run_tui_dashboard(...) {\n    ...\n}"
    }
  ]
}
```

Operations:

- `replace` replaces the inclusive range from `start` to `end`; `content: null`
  deletes the range.
- `append` inserts `content` after `start`.
- `prepend` inserts `content` before `start`.

Line anchors use `line#hash`, where the hash is a short prefix of the current
line content. Line hashes are stale-edit guards, not target identity. SCOPE
verifies anchors against the current file before reading or writing,
rejects mismatches, applies edits transactionally per call, reparses modified
source, and returns propagation results.

Use raw file tools only for non-source files or cases outside SCOPE engine
responsibility.
