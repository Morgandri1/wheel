# Board warnings — the shape of the 043 flag

Status: **spec for review by SDK (emits it) and Web (renders it).** Author: API. Date: 2026-09-06.
Finding: `redteam/findings/043-unauthenticated-endpoint-to-agent-is-unwarned-internet-to-capable-agent.md`.

PM asked me to own the shape so the composition is visible to non-UI consumers, and to agree it with
Web rather than each of us inventing one. The emitting code is in the engine's board assembly, which
is SDK's; this is the contract, not the implementation.

## What is being flagged

Not a vulnerability — a **composition**. Two correct defaults meeting:

- an `endpoint` node with `auth: { mode: "none" }`, which is the default and stays the default: an
  endpoint has to be trivially usable with any webhook provider, and one with no consumer is inert;
- a `send` wire from that endpoint to an `agent`, which is the entire point of endpoint nodes.

Either alone is fine. Together they mean anyone who can reach the public URL can put a turn into an
agent that runs with `bypassPermissions`. Nothing says so at the moment the wire is drawn.

## The shape

Each node in `GET /v1/board` gains `warnings`, alongside the existing `state`:

```jsonc
{
  "id": "…", "name": "telegram", "type": "endpoint", "config": { … }, "state": null,
  "warnings": [
    {
      "code": "unauthenticated_endpoint_to_agent",
      "message": "This endpoint exposes agent \"pm\" to unauthenticated input from anyone who can reach the public URL.",
      "related": ["<agent node id>"]
    }
  ]
}
```

`warnings` is `[]` when there is nothing to say — always present, never `null`, so a consumer never
branches on absence.

## Rules

1. **It says what it MEANS, not that something is insecure.** PM's constraint, and I agree with the
   reasoning: a severity label is not actionable and an ignorable red signal is worse than none.
   So there is **no `severity` field** and no colour word. The `message` names the specific agent
   exposed and what the exposure is. If someone reads only the message, they still know what to do.
2. **It never blocks.** No 4xx at wire creation, no refusal. The `none` default is the operator's
   deliberate design; a gate here would make people route around us, which is worse than the
   original problem.
3. **Computed, never stored.** Derived on read from `endpoint.auth.mode == "none"` and the existence
   of an `endpoint → agent` wire of type `send`. Nothing to migrate, nothing to keep in sync, and it
   cannot go stale when the auth mode or the wire changes. Removing either side removes the warning.
4. **It appears on both ends.** On the endpoint node (where the fix is applied) and on the agent node
   (where the consequence lands), so an agent-centric consumer is not left out. The `related` array
   carries the other end's node ids in both directions.
5. **It is returned at the moment of the composition, not only on the next board read.** The
   response to `POST /v1/wires` and to `PATCH /v1/nodes/:id` includes the warnings the change
   produced, so a `wheel.toml` import or a script that draws the wire sees it without polling
   `/board`. This is the half that makes the flag reach non-UI consumers, and it is the reason the
   flag exists in board state at all rather than only in the panel.
6. **The API changes nothing.** `ANY /v1/projects/:id/engine/*` is a pass-through and relays the
   field verbatim, like every other board field. There is nothing for wheel-api to implement.

## Why `warnings` is a list and not one flag

043 is the first, not the last: an `mcp` node with an `http` transport to a URL an agent controls,
a `tool` whose `base_url` is agent-fillable, a vault reachable by two agents that define the same
credential key. A list of coded, self-describing entries means the next one is an addition rather
than a schema change, and a consumer that ignores unknown codes still renders the message.

## What each owner does

- **SDK**: compute and emit `warnings` in board assembly and in the two mutating responses. The
  matrix already knows both facts, so this is a read-side derivation, not new state.
- **Web**: render it in the endpoint panel and the agent inspector; wording is yours, and the
  `message` is a usable default rather than a mandate.
- **API**: nothing, beyond documenting it. Verified: the engine proxy relays unknown fields verbatim.
