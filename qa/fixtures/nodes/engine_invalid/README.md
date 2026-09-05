# Engine-validated invalid fixtures

These are rejected by the **engine**, not by JSON Schema, because the rule cannot be expressed in
a schema:

- `tool_dup_op_id` — uniqueness of `operations[].id` within a node (`TOOL-op-slug-unique`). Draft
  2020-12 has no "unique by property" keyword.
- `tool_base_url_*` — SSRF policy (`TOOL-ssrf-base-url`). A pattern could catch the literal
  `127.0.0.1`, but not a public hostname that *resolves* to a private address, so encoding this in
  the schema would give false confidence while missing the actual attack.

Putting them in `invalid/` would force the schema to encode policy it has no business encoding.
They are asserted against a running engine instead (`qa/integration/`).
