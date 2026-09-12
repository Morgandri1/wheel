# 056 — A bare `wheel inbox` over MCP still shows raw, unescaped message bodies in the list view

- **Severity:** Low (narrow, and the single-message path this same PR fixed already closes the
  higher-value case)
- **Type:** Residual left open by defect #2's implementation (PR #88, ADVERSARY review), not a new
  design gap — `docs/proposals/tool-mcp-output-escaping.md` did not anticipate this asymmetry between
  the single-item and list shapes of one endpoint's response.
- **Owner:** SDK/Engine
- **Status:** OPEN
- **Boundary:** the same channel finding 001 and defect #2 both cover — content reaching a model's
  context — narrowed to one specific render path.

## Claim

`GET .../v1/cli/inbox` (`crates/wheel-engine/src/api/cli_routes.rs::inbox`) answers two shapes:

- **One message** (`?id=`): `{"message": {..., "body": "<raw>"}, "value": "<wrapped>"}` — a top-level
  `value` sibling field, deliberately left next to the untouched `body` so `message.body`/`sha256`/
  `bytes` stay byte-identical to what was sent (§3c#3's tested contract). `wheel-cli`'s
  `inbox_single_text` reads `value`, not `body` (`crates/wheel-cli/src/main.rs`).
- **The list** (no `id`): `{"messages": [{..., "body": "<raw>", "value": "<wrapped>"}, ...]}` — every
  item ALSO gets a per-item `value` field (`cli_routes.rs:653`), and `wheel-cli`'s own
  `inbox_preview_line` correctly reads and escapes it for the CLI's own rendering.

Both are fine for `wheel inbox` the CLI binary. The gap is the **third** consumer: `wheel mcp-serve`'s
generic `render()` (`crates/wheel-cli/src/mcp.rs:197-206`), which every board-memory tool (`read`,
`write`, `ls`, `query`, `secret_get`, `run`, `inbox` itself) answers through. Its rule is "a string as
given, an object with a top-level `value` as that value, anything else as `v.to_string()`." The
single-message shape has a top-level `value` and is handled correctly. **The list shape has no
top-level `value` — the per-item ones are nested inside `messages[]`** — so it falls to the `_ =>
v.to_string()` arm, which serializes the WHOLE structure. That includes every message's raw `body`
field, unescaped, sitting right next to its own already-wrapped `value` field, both reaching the model
in the same tool result.

Concretely: an agent whose harness calls the `inbox` MCP tool with no `id` argument receives text
containing both `"body":"<the actual forged tag, live>"` and `"value":"<wheel:tool-output>...escaped
version...</wheel:tool-output>"` for the same message — the fix is present and inert at once, because
the raw field never stopped being serialized alongside it.

## Why this is scoped Low, not Medium

- The single-message read (`inbox <id>`), which is how §3c#2 ("re-read a message once delivered") is
  normally exercised and is the shape PR #88's own end-to-end test (`tool_output_escaping.rs`) proves
  against a real engine and a real `wheel` binary, is unaffected — the top-level `value` there IS what
  `render()` picks up.
- Reaching this path requires an agent to call `inbox` with no id, which lists PREVIEWS truncated to 60
  chars in the ordinary CLI path (`inbox_preview_line`) — but the MCP path bypasses that entirely by
  serializing the full `messages[]` structure, so the 60-char preview discipline does not actually bound
  this exposure the way it looks like it should from reading `wheel-cli`'s own code.
- A message body is capped at 256 KiB (§3c#6), so this is bounded, unlike an unbounded external
  response.

## Impact

Any board where a `send`-wired peer (or the operator, `from=user`) can put attacker-shaped text into a
message body — which is exactly finding 001's original threat model — gets a second, still-open path to
the same live tag, through a tool call every agent already has (`inbox`), rather than needing a
crafted single-message read.

## Recommendation

Give the list shape the same top-level convention the single-message shape already has, rather than
teaching `render()` a `messages[]`-specific case (which would make MCP's generic renderer less generic
for one caller's benefit). Concretely: `cli_routes.rs::inbox`'s list branch should build each list item
so ITS `body` field is never included unescaped in what a generic `.to_string()` fallback could ever
serialize — either drop `body` from the list response entirely (nothing in `wheel-cli`'s own
`inbox_preview_line` reads it as anything but a preview source, and previews already go through
`value`-equivalent escaping) and rename `value` to be the whole per-item text field the list exposes, or
restructure the list response so the top-level shape itself is `{"value": "<pre-rendered, escaped
multi-line list text>"}` the way a `ctx`/`table` read already answers `render()`. Either fix should ship
with an MCP-level test (mirroring `tool_output_escaping.rs`'s real-binary rigor) that calls
`inbox`/no-id over the real `mcp-serve` stdio path and asserts the forged tag is not the literal text
anywhere in the JSON-RPC result — not just that a `value` field exists somewhere inside it, which is the
assertion gap that let this residual through PR #88 undetected until this review.

## What would change my mind

If `wheel-cli`'s `render_inbox` (the plain-CLI renderer) is the ONLY thing `cli_routes.rs::inbox`'s list
shape is meant to serve, and no MCP tool schema ever exposes a no-argument `inbox` call to a harness,
this narrows to a residual worth a code comment rather than a finding. That is not the case today: the
built-in MCP server's tool list (§3c#1) includes `inbox` with no required arguments, so a harness can
and will call it that way.
