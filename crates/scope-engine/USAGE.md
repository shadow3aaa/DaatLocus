# SCOPE Usage

SCOPE is the semantic code positioning and propagation engine behind the Coding app. Its selector language is a SCOPE-owned positioning DSL: selectors answer “where is the target?” and “what range should this operation use?”. Selectors do not encode edit behavior, refactor intent, propagation policy, or other operation semantics.

## Selector forms

- `src/foo.rs::Bar::new` — existing symbol-level selector.
- `src/foo.rs#L120-L180` — explicit file line range.
- `src/foo.rs#around:L150±40` — context around a line.
- `src/foo.rs#match:/ProjectInstructions/` — regex match point.
- `src/foo.rs#match:/ProjectInstructions/#around:40` — context around a unique regex match.
- `src/foo.rs#enclosing:L150` — innermost symbol containing a line.
- `src/foo.rs#outline` — file structure / symbol outline.

## Operation support

Read operations may accept broad selectors. `read_code` and future `read_selection`-style operations may support symbols, files, ranges, around contexts, matches, outlines, and enclosing symbols.

Edit operations must be conservative:

- Symbol selector: allow semantic edit and normal propagation analysis.
- File range selector: allow patch, then run affected-symbol analysis.
- Match selector: edit only when the match is unique; if multiple matches exist, return candidates instead of guessing.
- Enclosing selector: resolve to the enclosing symbol first, then use symbol edit semantics.
- Outline selector: read-only; never edit an outline.

## Grep bridge

Text search should bridge into selectors. A grep/search match should include structured fields such as `file`, `line`, `match_id`, `selector`, `enclosing_selector`, and structured selector metadata so callers can move directly from search to reading or editing:

```json
{
  "file": "src/coding_app.rs",
  "line": 150,
  "match_id": "src/coding_app.rs:150:1",
  "enclosing_selector": "src/coding_app.rs::fn how_to_use #L140-L170"
}
```

Useful follow-up selectors include:

- `src/x.rs#around:L42±30`
- `src/x.rs#enclosing:L42`
- `src/x.rs#match:/needle/`

## Structured selector data

Human-writable selector strings are convenient, but tool results should also return structured selector data. Include line location information whenever possible to reduce ambiguity:

```json
{
  "file": "src/coding_app.rs",
  "range": { "start_line": 120, "end_line": 180 },
  "kind": "enclosing_symbol",
  "symbol_selector": "src/coding_app.rs::CodingApp::open_project",
  "symbol_start_line": 120,
  "symbol_end_line": 180,
  "definition_line": 120
}
```

Prefer passing structured selector data between tools over reconstructing selector strings by hand.
