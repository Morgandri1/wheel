# `POST /v1/projects/{id}/board/apply` — the shape Web builds against

API + Web, 2026-09-07. Settled in git rather than a message, per the comms rule. Live on main at
`f472a1f`.

**Answer to Web's question: STRUCTURED, not strings.** A wire is `{from, to, type}` everywhere it
appears — in the plan and in the report, on success and on failure. Writing this note is what found
the old shape was wrong; it used to be `"notes -> researcher (send)"`, which is fine in a log and
useless to a canvas that has to find an edge and highlight it.

## Request

```jsonc
POST /v1/projects/{id}/board/apply
x-auth-token: …   x-project-id: …
{
  "board": { "nodes": [ … ], "wires": [ … ] },   // exactly what the builder emitted
  "dry_run": false,                               // true = plan only, change nothing
  "allow_patch": false,                           // true = may MODIFY an existing node's config
  "allow_wire": false                             // true = may WIRE existing nodes
}
```

### `allow_patch` — create-only is the default

**A board that merely NAMES an existing node does not get to modify it.** Left off (the default),
such a board is refused with `patch_not_permitted`, one refusal per node it would have touched, so
the confirm step can show the user exactly what they are being asked to allow before they allow it.
"The builder named it" is not consent.

### `allow_wire` — wiring an existing node is also a change

**Corrected, ADVERSARY 050.** This section previously said wiring to an existing node was fine
without a flag, on the reasoning that "wiring to something is not changing it". That was wrong.

**A wire IS the capability.** Attaching one to a node that already exists changes what it can do, or
what can reach it, without editing a byte of its config: `ctx -> agent (send)` injects into that
agent's prompt permanently; `agent -> vault (read)` hands it secrets it did not have; an `auth:none`
endpoint wired to an agent puts the public internet on its inbox. None of those touch the node, so
`allow_patch` never saw them.

So a wire with EITHER endpoint already on the board needs `allow_wire: true`. Both directions count —
inbound adds a channel into the node, outbound grants it new reach. Only a wire between two nodes
**this same board is creating** is consent-free.

The two flags are separate deliberately: "you may rewrite this agent's prompt" and "you may put a
public endpoint on its inbox" are different risks, and granting one must not grant the other.
`allow_patch: true` alone still refuses rewiring.

Re-applying an unchanged board does NOT re-ask: the check runs after existing wires are filtered
out, so a no-op stays a no-op.

For the improve flow this means two round trips by design: apply with the flag off, show the user
the `patch_not_permitted` list, then re-apply with `allow_patch: true` if they agree. A dry run
behaves identically, so the preview surfaces the same refusal without touching anything.

## Responses — the status code carries the outcome

| status | meaning |
|---|---|
| `200` | everything asked for landed, **or** a `dry_run` preview |
| `422` | refused. **Nothing was created.** Every refusal listed, each naming the node or wire |
| `207` | partial. Some steps landed, some did not; the body names both |

`207` is deliberate. A client that treats 2xx as fine would otherwise report a half-applied board as
a success, which is the one outcome this step exists to prevent. **Branch on `applied`, not on
2xx** — it is a boolean in every response for exactly that reason.

### `200` — dry run (the confirm step's input)

```jsonc
{ "applied": false,
  "plan": {
    "create_nodes": ["researcher", "notes"],
    "patch_nodes":  ["existing-agent"],
    "create_wires": [ { "from": "notes", "to": "researcher", "type": "send" } ]
  } }
```

`create_nodes` vs `patch_nodes` is the delta the user is confirming: a patch is an existing node the
board changes, a create is a new one. A node the board never mentions appears in neither — it is not
touched.

### `200` / `207` — after applying

```jsonc
{ "applied": true,
  "report": {
    "created_nodes": ["researcher", "notes"],
    "patched_nodes": [],
    "created_wires": [ { "from": "notes", "to": "researcher", "type": "send" } ],
    "failures": []
  } }
```

A failure is addressable, not just described:

```jsonc
{ "step":  "create wire notes -> researcher (send)",   // the log line
  "error": "engine returned 400: wire refused by the engine",
  "wire":  { "from": "notes", "to": "researcher", "type": "send" },  // present when it was a wire
  "node":  "notes" }                                    // present when it was a node
```

`node` and `wire` are omitted rather than null when they do not apply.

### `422` — refused, nothing created

```jsonc
{ "applied": false,
  "message": "the board was refused; nothing was created",
  "refusals": [
    { "refusal": { "code": "wire_not_allowed", "from": "a", "from_type": "agent",
                   "to": "v", "to_type": "vault", "wire_type": "write" },
      "message": "no write wire is allowed from a agent to a vault: \"a\" -> \"v\"" }
  ] }
```

Codes: `wire_not_allowed`, `unknown_node`, `self_wire`, `duplicate_node_name`,
`node_type_mismatch`, `board_too_large`, `patch_not_permitted`, `wire_touches_existing_node`. **Render `message`**; branch on `code` only if you need to.
Every refusal is returned, not just the first — one bad wire from a builder usually means several.

## Guarantees Web can rely on

- **`422` means nothing was created.** Validation runs before any node exists, so a refused board
  leaves the project untouched. Safe to re-emit and retry.
- **`applied: true` means every step landed.** It is never true for a partial apply.
- **Not atomic.** A `207` means the board is partly realised, and the report is the record of what.
  True atomicity needs the engine-side batch route (SDK, v2); until then `207` is a real outcome to
  render, not an error to swallow.
- **Re-applying an unchanged board plans nothing** — existing wires are not re-created.

## Caps

200 nodes, 1000 wires per apply, refused as a single `board_too_large`.

SDK has confirmed there is **no engine-side per-project cap** — they grepped; nothing counts nodes.
So this is not an interim pending a better number: it is the only bound that exists, and it is a
bounded judgement rather than a measurement. Worth revisiting if a real limit ever lands engine-side.
