# File and Coding Tool Rules

### Static Runtime File Tools

Plain file reading and plain file editing are static runtime tools, not an
`App` and not a `global::*` namespace. They do not represent an app-owned
interactive surface. They are exposed as ordinary runtime tools:

```text
read_file
edit_file
```

`read_file` is the model-visible primitive for explicit file/range reads:

```text
read_file({
  path: "src/dashboard/mod.rs",
  start_line: 1268,
  line_count: 53
})
-> 1268#7a|fn run_tui_dashboard(...) {
   1269#c1|    ...
```

Rules:

- `read_file` accepts a path plus optional `start_line` and `line_count`.
- Paths may be workspace-relative or absolute paths allowed by the sandbox.
- `read_file` output lines use the same `line#hash|source line` format used by
  Coding reads.
- Line hashes are stale-edit guards. They are not long-term identities and
  should stay short.
- `read_file` is the fallback for imports, top-level code, search misses,
  user-specified locations, and non-source/config/document files.
- Do not put arbitrary path/range read compatibility into `read_code`; that
  belongs here. `read_code` accepts only a path plus a line-hash anchor and an
  `around`/`full` mode.

`edit_file` is the model-visible primitive for plain file edits:

```text
edit_file({
  edits: [{
    path: "AGENTS.md",
    op: "replace",
    start: "708#4b",
    end: "724#d1",
    content: "..."
  }]
})
```

Rules:

- `edit_file` uses the same structured edit schema and `line#hash` anchors as
  `edit_code`.
- `edit_file` verifies line hashes before writing.
- The model-visible edit schema should be flat and must not use JSON Schema
  `oneOf`/`anyOf`. Expose edit `content` as a string field; implementation may
  accept legacy array content, but the schema should not advertise it.
- `edit_file` handles ordinary non-SCOPE files such as Markdown, TOML, YAML,
  JSON, shell scripts, and unsupported file types.
- `edit_file` does not run SCOPE propagation analysis and does not produce
  propagation review events.
- When a project scope is open and the target is a SCOPE-owned source file,
  `edit_file` must be rejected with an instruction to use `coding__edit_code`.

`apply_patch` must not be a normal model-facing editing API. Patch-envelope or
unified-diff parsing may remain as an internal implementation detail or
migration aid, but the agent-facing path is `read_file` plus `edit_file`, or
`search_code` plus `read_code` plus `edit_code` for SCOPE-owned source files.

### Coding Search / Read / Edit Protocol

The Coding tool protocol uses one source-location vocabulary:
`path + line#hash`. A `line#hash` anchor is meaningful only inside one file.
Do not introduce a second model-facing target identity or session-local target
registry for search/read flows.

`search_code` is the model-visible search primitive. It replaces separate
model-visible `grep` and `glob` tools while staying aligned with `rg`
semantics. Inputs should cover the useful `rg` shape: `query`, `path`, `mode`,
`case`, `word`, `line`, `include`, `exclude`, `types`, `type_not`, `hidden`,
`respect_ignore`, `follow`, and `limit`.

Search output is an array of hits:

```json
{
  "matches": [
    {
      "path": "src/foo.rs",
      "hit": "42#ab|    call_target();"
    }
  ]
}
```

Rules:

- Return one match object per matched line.
- `hit` must be exactly one `line#hash|source line`.
- Return the actual matched line. If a match is inside a function, method, type,
  or other AST symbol, the search hit is still the matched line, not the
  enclosing declaration line.
- Do not split or repeat `line_number`, `hash`, `text`, `label`, `enclosing`,
  or other metadata already encoded by the line anchor and source line.
- `path + line#hash` is the target identity for follow-up reads. `line#hash`
  alone is file-local and not globally unique.
- Search may use ASTs internally for ranking, filtering, or presentation, but
  it must not replace the visible hit with an enclosing symbol target.

`read_code` reads a path-scoped line anchor:

```json
{
  "path": "src/foo.rs",
  "anchor": "42#ab",
  "mode": "full"
}
```

Rules:

- `read_code` accepts any syntactically valid `line#hash` anchor with a path. It
  must not require the anchor to have been produced by a prior `search_code`
  call.
- Before reading, verify that the current file line still matches the supplied
  hash. On mismatch, return a stale-anchor error and tell the model to search or
  read again.
- `mode` has exactly two values: `around` and `full`.
- `around` returns a fixed local window around the anchor, roughly a dozen lines
  above and below. It does not perform AST expansion and has no tunable context
  parameters.
- `full` automatically returns the enclosing AST symbol when the anchor is
  inside a recognizable symbol. If no enclosing symbol is recognizable, it
  falls back to `around`.
- Do not add manual `enclosing`, `selector`, `context_before`,
  `context_after`, path/range, or other compatibility fields to `read_code`.
- `read_code` output should be minimal: `{ "content": "..." }`.
- `content` lines use the existing `line#hash|source line` format. Do not
  repeat `path`, `mode`, resolved range, enclosing symbol metadata, or other
  values the caller already supplied or that are implementation detail.
- Line hashes stay short. They are stale-edit guards, not long-term identities.

`edit_code` uses the same structured edit schema as `edit_file`, but with SCOPE
propagation analysis and review:

```text
code::edit({
  edits: [{
    path: "src/dashboard/mod.rs",
    op: "replace",
    start: "1268#7a",
    end: "1320#d4",
    content: "..."
  }]
})
```

The model copies `path` from the search hit and copies `start`/`end` line
anchors from `read_code.content`. Existing replace/append/prepend semantics,
line hash verification, parse validation, and propagation analysis remain
unchanged. `edit_code` must not accept opaque target handles or search result
objects as edit targets.
Like `edit_file`, `edit_code` must expose a flat structured-edit schema without
JSON Schema `oneOf`/`anyOf`.

### SCOPE Current Boundary and Static File Tool Boundary

SCOPE (scope-engine) provides semantic code reading, searching, hash-anchored
editing, and propagation review. Do not document unimplemented refactoring
features as expected model-facing capabilities.

| Capability | SCOPE Status | Boundary |
|---|---|---|
| Target discovery | ✅ `search_code` | Content search returns path-scoped matched-line hits in `line#hash|source line` form. |
| Read code | ✅ `read_code` | Reads a path plus line-hash anchor in `around` or `full` mode and returns hash-anchored source lines. Explicit path/range reads belong to `read_file`. |
| Edit code | ✅ `edit_code` | Applies the same structured hash-anchored edits as `edit_file`, plus SCOPE parse validation and propagation review. |
| Propagation review | ✅ review tools | Edit impact is surfaced through propagation results and review events. |
| New source files | ⚠️ explicit supported creation paths | Use supported creation/edit paths; SCOPE has no template system. |
| Non-source/config files | Outside SCOPE | Use `read_file` and `edit_file` for `.toml`, `.yaml`, `.md`, `.json`, `.sh`, and other non-source files. |

**Static file edit boundary:**

When a project scope is open and `edit_file` targets a source-code file that
SCOPE owns (for example `.rs`, `.py`, `.go`, `.ts`, `.js`, `.java`, `.c`,
`.cpp`, `.rb`, `.php`), the runtime rejects the call and requires
`coding__edit_code` instead.

For non-source-code files (`.toml`, `.yaml`, `.md`, `.json`, `.sh`, etc.) or
unsupported cases outside SCOPE responsibility, `edit_file` is allowed.
Propagation review is then limited to what Coding can observe through its own
semantic operations and explicit review events; do not assume plain file edits
silently receive the same propagation analysis as `edit_code`.
