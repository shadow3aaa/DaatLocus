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

Edit operations use `edit_code` with a single `diff` argument containing a complete SCOPE Diff document. Do not pass separate selector or patch fields to `edit_code`.

Selector support for SCOPE Diff actions:

- Symbol selector: allow semantic edits and normal propagation analysis.
- File range selector: allow patch actions, then run affected-symbol analysis.
- Match selector: edit only when the match is unique; if multiple matches exist, return candidates instead of guessing.
- Enclosing selector: resolve to the enclosing symbol first, then use symbol edit semantics.
- Outline selector: read-only; never edit an outline.

## SCOPE Diff format for `edit_code`

`edit_code` takes exactly one argument:

```json
{ "diff": "*** Begin Patch\n*** Update: src/foo.rs::fn old()\n@@\n-old()\n+new()\n*** End Patch\n" }
```

The `diff` string is a selector-based patch wrapped in an explicit envelope:

```text
*** Begin Patch
*** Add: <selector>
+new content

*** Delete: <selector>
-old content guard

*** Update: <selector>
@@
 context
-old content
+new content
 context
*** End Patch
```

Each action header is one of:

```text
*** Add: <selector>
*** Delete: <selector>
*** Update: <selector>
```

### Add

`Add` inserts new text at the selector-designated insertion point.

```text
*** Begin Patch
*** Add: src/user.rs#L1-L1
+use crate::display::DisplayName;
*** End Patch
```

Rules:

- Body lines must start with `+`.
- The payload is the body with the leading `+` prefixes removed.
- The selector must resolve to exactly one insertion point or creation target.
- `Add` must not overwrite existing text. Use `Update` for replacement.

### Delete

`Delete` removes the selector-designated range.

```text
*** Begin Patch
*** Delete: src/user.rs::fn legacy_name()
-pub fn legacy_name(&self) -> &str {
-    &self.name
-}
*** End Patch
```

Rules:

- Body lines, when present, must start with `-`.
- The old-side body is a guard and must match the selected range after removing leading `-` prefixes.
- If the body is omitted, SCOPE deletes the full resolved selector range only for stable selectors such as unique symbols or exact line ranges.
- If the selector is stale, ambiguous, or the guard does not match, the edit fails without modifying files.

### Update

`Update` applies one or more guarded hunks inside the selector-designated range.

```text
*** Begin Patch
*** Update: src/user.rs::fn display_name()
@@
 pub fn display_name(&self) -> &str {
-    &self.name
+    &self.display_name
 }
*** End Patch
```

Rules:

- Hunks use selector-scoped unified-diff body lines:
  - space-prefixed lines are context
  - `-` lines are old text
  - `+` lines are new text
- A bare `@@` hunk header is allowed when the selector already narrows the target enough for unambiguous matching.
- Numbered hunk headers are supported. Line numbers are relative to the resolved selector range.
- Old-side text plus context must match exactly one location inside the resolved selector range.
- `Update` can express replacement by deleting all old-side lines in the selected range and adding the new body.

### Execution behavior

SCOPE applies a diff by parsing the envelope, resolving every selector against the current project state, validating guards and hunk contexts, applying edits transactionally per call, reparsing modified files, and returning propagation results. The preferred failure mode is all-or-nothing: stale selectors, ambiguous matches, invalid syntax, read-only selectors, or guard mismatches reject the edit before writes complete.

Use SCOPE Diff when the target is code and selector semantics help keep the edit anchored to symbols, unique matches, enclosing symbols, or explicit ranges. Use raw `apply_patch` only for non-source files or cases outside SCOPE engine responsibility. SCOPE exposes `is_responsible_source` so callers can ask whether a path is source code owned by SCOPE before allowing raw file edits.

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
