# SCOPE Usage

SCOPE is the semantic code positioning and propagation engine behind the Coding app. Its selector language is a SCOPE-owned positioning DSL: selectors answer "where is the target?" and "what range should this operation use?". Selectors do not encode edit behavior, refactor intent, propagation policy, or other operation semantics.

## Selector forms

- `src/foo.rs::Bar::new` — existing symbol-level selector.
- `src/foo.rs#L120-L180` — explicit file line range.
- `src/foo.rs#around:L150±40` — context around a line.
- `src/foo.rs#match:/ProjectInstructions/` — regex match point.
- `src/foo.rs#match:/ProjectInstructions/#around:40` — context around a unique regex match.
- `src/foo.rs#enclosing:L150` — innermost symbol containing a line.
- `src/foo.rs#outline` — file structure / symbol outline.

## Operation support

Read operations may accept broad selectors. `read_code` may support symbols, files, ranges, around contexts, matches, outlines, and enclosing symbols.

Edit operations use `edit_code` with a structured `edits` array. Each edit specifies the file path, operation kind, line-hash anchors from `read_code`, and optional content.

## `edit_code` format

`edit_code` takes a single `edits` argument containing an array of structured edit objects:

```json
{
  "edits": [
    {
      "path": "src/foo.rs",
      "op": "replace",
      "start": "11#VK",
      "end": "33#MB",
      "content": "pub fn new_func() {\n    println!(\"hi\");\n}\n"
    },
    {
      "path": "src/foo.rs",
      "op": "append",
      "start": "33#MB",
      "content": "\npub fn extra() {\n    // added\n}\n"
    },
    {
      "path": "src/foo.rs",
      "op": "prepend",
      "start": "11#VK",
      "content": "// header comment\n\n"
    },
    {
      "path": "src/foo.rs",
      "op": "replace",
      "start": "11#VK",
      "end": "33#MB",
      "content": null
    }
  ]
}
```

### Operations

- **`replace`** — Replace the range from `start` to `end` (inclusive) with `content`. Requires both `start` and `end`. Set `content` to `null` to delete the range.
- **`append`** — Insert `content` after the line identified by `start`. Only `start` is required; `end` is ignored.
- **`prepend`** — Insert `content` before the line identified by `start`. Only `start` is required; `end` is ignored.

### Line-hash anchors

`start` and `end` use the format `line#hash` where:

- `line` is a 1-based line number
- `hash` is a 2-char hex prefix (first byte of SHA-256) of that line's content

These anchors come directly from `read_code` output. The `read_code` response returns content with per-line hash prefixes:

```json
{
  "path": "src/foo.rs",
  "content": "11#VK|pub fn old_func() {\n22#XJ|    println!(\"hello\");\n33#MB|}\n"
}
```

The model copies `11#VK` and `33#MB` from the read response into the edit `start`/`end` fields. The system verifies that line hashes match before applying edits, providing freshness validation without requiring a separate guard field. If a hash does not match, the edit is rejected and the model should re-read the file.

### Content

`content` accepts three forms:
- A **string** with newline-delimited lines
- An **array of strings** (one entry per line)
- **`null`** — only valid with `replace`, meaning delete the range

### Execution behavior

SCOPE applies edits by parsing line-hash anchors, verifying every anchor against the current file state, applying edits transactionally per call, reparsing modified files, and returning propagation results. The preferred failure mode is all-or-nothing: hash mismatches, overlapping edits, or tree-sitter parse errors reject the edit before writes complete.

Use `edit_code` when the target is source code and line-level anchoring with freshness validation helps keep edits safe. Use raw `apply_patch` only for non-source files or cases outside SCOPE engine responsibility. SCOPE exposes `is_responsible_source` so callers can ask whether a path is source code owned by SCOPE before allowing raw file edits.

## Grep bridge

Text search returns matches with file, line, text, and selector, enabling direct navigation from search to reading or editing:

```json
{
  "file": "src/coding_app.rs",
  "line": 150,
  "text": "fn how_to_use() {",
  "selector": "src/coding_app.rs::fn how_to_use #L140-L170"
}
```

Use the returned selectors to call `read_code`, which produces line-hash anchors for subsequent `edit_code` calls.

## read_code

`read_code` resolves a selector and returns the file path and annotated content:

```json
{
  "path": "src/foo.rs",
  "content": "11#VK|pub fn target() {\n22#XJ|    let x = 1;\n33#MB|}\n"
}
```

Each line is prefixed with `line#hash|`. The model uses these anchors directly in `edit_code` `start`/`end` fields.
