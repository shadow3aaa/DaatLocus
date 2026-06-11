# SCOPE Usage

SCOPE is the semantic code search, read, edit, and propagation engine behind the
Coding app. The model should not write SCOPE positioning syntax. SCOPE produces
stable read handles from search results; the model copies those handles into
`read_code` and copies read line anchors into `edit_code`.

## `search_code`

`search_code` is the normal entry point for locating code. It accepts a content
query plus optional narrowing fields such as `path`, `include`, and `limit`.

Search returns compact read targets:

```text
1268#k7Qp|src/dashboard/mod.rs::fn run_tui_dashboard #L1268-L1320
286#b91Z|src/dashboard/mod.rs::trait DashboardHistoryLoader #L286-L302
1#a0F2|src/dashboard/mod.rs#L1-L24
```

The left side is a stable read handle. Its format is `start_line#hash4`.

Rules:

- The handle is a read capability for the canonical target label, not a content
  fingerprint.
- The four-character hash is derived only from the canonical target label.
- The handle must not include target body text, search query, session salt, file
  mtime, read timestamp, line hashes, or freshness data.
- Search results inside an AST symbol point at that symbol's canonical target
  label.
- Search results outside an AST symbol point at a small canonical line range.
- Multiple matches inside the same target are deduplicated.

## `read_code`

The normal read path uses a search handle:

```json
{ "ref": "1268#k7Qp" }
```

`read_code` also supports explicit path ranges for imports, top-level code,
search misses, and user-specified locations:

```json
{ "path": "src/dashboard/mod.rs", "start_line": 1, "line_count": 24 }
```

Read output is source text with per-line edit anchors:

```text
1268#7a|fn run_tui_dashboard(...) {
1269#c1|    ...
```

Do not repeat the search handle, canonical target label, or path in model-facing
read output when the model already obtained them from search. The structured
response may still carry the path for UI and scoped-instruction plumbing.

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

Line anchors use `line#hash2`, where `hash2` is a two-character hex prefix of
the current line content. Line hashes are stale-edit guards, not target
identity. SCOPE verifies anchors against the current file before writing,
rejects mismatches, applies edits transactionally per call, reparses modified
source, and returns propagation results.

Use raw file tools only for non-source files or cases outside SCOPE engine
responsibility.
