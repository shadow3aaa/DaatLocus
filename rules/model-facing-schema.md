# Model-Facing Schema Rules

## Model-Facing JSON Schema Design

Model-facing schemas are an engineering contract, not a provider-specific
cleanup target. This section applies to runtime tool inputs, app tool inputs,
structured model outputs, and fallback schemas for freeform tools.

Daat uses a conservative portable schema dialect. The
`daat-locus-macros::model_schema` attribute macro is the normal compile-time
entry point for Rust model-facing input and output types. The dialect is the
source of truth, and the macro must implement it exactly. Runtime normalization
must not be relied on for correctness. Provider boundaries should receive
already-valid schemas. Dynamic schemas from workspace apps or other external
sources cannot be compile-time checked, so they must be validated at load time
instead of silently rewritten.

Rules:

- Root schemas must be JSON objects.
- Every object schema must contain `properties`, `required`, and
  `additionalProperties: false`.
- Every property listed in `properties` must also be listed in `required`.
- Optional model-visible values are represented as nullable required fields,
  such as `type: ["string", "null"]`. Do not represent optionality by omitting a
  required entry.
- Rust deserializers may accept missing fields for backward compatibility, but
  the model-facing schema must not advertise missing fields as valid input.
- Do not emit JSON Schema `default`. Defaults are local deserialization or
  implementation behavior, not part of the model contract.
- Do not use `skip_serializing_if`, serde defaults, or provider normalization to
  define model-visible optionality.
- Use simple scalar types only: string, integer, number, boolean, object,
  homogeneous array, and null unions.
- Use string enums for finite choices. Avoid tagged, untagged, adjacently
  tagged, or payload-carrying enums in model-facing schemas.
- Avoid maps, dictionaries, dynamic object keys, tuple arrays, `prefixItems`,
  and schema-valued `additionalProperties`.
- Avoid composition and conditional keywords: `oneOf`, `allOf`, `anyOf`, `not`,
  `if`, `then`, `else`, `dependentRequired`, and `dependentSchemas`.
- Avoid validation keywords that are not consistently supported across strict
  structured-output implementations: string `minLength`, `maxLength`,
  `pattern`, `format`; numeric `minimum`, `maximum`, `multipleOf`; array
  `minItems`, `maxItems`, `uniqueItems`, `contains`.
- Prefer inlined schemas over `$defs`/`$ref` for model-facing tool schemas. If a
  provider-specific path later allows references, a `$ref` schema must not have
  sibling keywords such as `default` or `description` next to the `$ref`.
- Keep schema names, property names, enum values, and descriptions concise.
  Large schemas cost context and can hit provider schema limits.
- Schema generation tests must inspect the final model-facing JSON, not only
  Rust type definitions.

When adding a model-facing Rust input/output type:

- Use `#[model_schema]` on the Rust type and call
  `model_schema_for::<Type>()`.
- Do not expose raw `schemars` output directly unless the test proves it already
  conforms to this section.
- Do not add provider-specific normalization to make a bad schema pass. Fix the
  type shape or the schema macro instead.
- If the type cannot express the model contract directly, add a narrow
  schema-only mirror type with `#[model_schema]`; do not hand-build provider
  JSON in runtime or provider code.
