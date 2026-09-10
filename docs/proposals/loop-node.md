# Proposal: `loop` node — timed re-trigger of an agent or a tool

Status: **draft, for adversary + PM review**.
Author: SDK. Date: 2026-09-10. Answers `docs/wow-agent-brief.md` task 8 (operator's ask, 2026-09-10).

## What already exists (read before designing anything new)

Every trigger in the system today is event-driven: an inbound message (`wheel msg`), an HTTP hit (`endpoint`
node), or `run_on_startup` (fires once, at engine boot). There is no time-based trigger. Two mechanisms this
proposal reuses rather than reinvents:

- **Delivery**: `db::messages::enqueue` + `supervisor.deliver` is the ONE path every `send` wire already uses
  (agent→agent, ctx injection aside, endpoint→agent). It is durable (sqlite row, `queued → delivered → consumed`),
  resumes a parked agent transparently (§3c#14), and already sits behind the §3c#12 priority-lane fairness rule.
  A loop firing into an agent needs no new delivery logic — it calls the same function every other `send` does.
- **Tool execution**: `tools::execute::build_request` + `tools::execute::send` (`crates/wheel-engine/src/tools/
  execute.rs:61,306`) are plain functions, not HTTP handlers — `api/tool_routes.rs::call` is a thin wrapper
  around them for the human/agent-initiated case. A loop firing into a tool calls the same two functions
  directly, inheriting every SSRF/redirect/timeout protection §3d already enforces, including the existing
  30s `CALL_TIMEOUT` (execute.rs:27) — relevant to the interval floor below.
- **Background timers**: the supervisor already runs one per-node `tokio::spawn` + `sleep` loop per idle-park
  timer (`Supervisor::arm_park_timer`), re-checking the node's live state on every tick rather than assuming it
  is still valid. A loop node's timer is the same shape: sleep, re-check the node still exists and is `started`,
  fire, repeat.

Node types and their configs are a closed, exhaustively-matched set (`NodeType`/`NodeConfig` in `wheel-core/src/
node.rs`, `wire.rs`'s `wire_allowed` match, `board.rs`, every API route that switches on node type, and the whole
of Web's TypeScript mirror). Adding `Loop` is a real schema change with a wide, mechanical blast radius — PM's
framing ("touches the same core schema" as portals/helper-agent) is correct, which is why this gets a proposal
despite being smaller in design-question count than either of those.

## Design

### New node type, not an agent flag or a wire attribute

A `loop` is not a variant of anything that exists — it has no harness, no context, no identity an agent
addresses by name in a `msg`. It is its own `NodeType::Loop` / `NodeConfig::Loop(LoopConfig)`, matching the
brief's framing exactly ("a new node type that fires on a configurable interval").

```rust
pub struct LoopConfig {
    /// Stored/wire-level unit is milliseconds (operator's final answer). The UI
    /// converts from whatever the user picks (minutes/hours) — Web's problem,
    /// not this schema's.
    pub interval_ms: u64,
    /// Sent verbatim to the wired agent, exactly like a `wheel msg` body.
    /// Static for v1 — see "Open question: dynamic prompt" below.
    pub prompt: Option<String>,
    /// The operation to call on the wired tool node, and its (static) args.
    /// Mirrors `wheel tool call <tool> <op> <args>` with no per-fire chooser,
    /// because nothing is present at fire time to choose with.
    pub tool_op: Option<String>,
    pub tool_args: Option<serde_json::Value>,
}
```

`prompt`/`tool_op`+`tool_args` are both `Option` because a loop node has exactly one live wire (to an agent OR
to a tool — see matrix below) and only the matching field is used; the unused side is ignored, not an error, so
an operator can reconfigure a loop's target without a config round-trip. Validation (see "Interval floor")
refuses a loop with `interval_ms` below the floor, and — separately, at wire-creation time, not config-save
time — a loop with a `send` wire but no `prompt`, or a `read` wire to a tool but no `tool_op`, is left CREATABLE
(a loop can exist unconfigured) but never STARTABLE (`POST /v1/loops/:id/start` 400s naming what's missing).
This is the same posture §3d takes for a tool op with an unfilled required field: refuse the ACTION, not the
existence of the node.

### Wire matrix: two new cells, both already-shaped

```rust
(N::Loop, N::Agent, W::Send) => true,   // fires `db::messages::enqueue` + `supervisor.deliver`, verbatim
(N::Loop, N::Tool, W::Read) => true,    // fires `tools::execute::build_request` + `send`, verbatim
```

Loop has no other outgoing wires (no ctx, no vault, no table — see "Open question: dynamic prompt" for why ctx
is deliberately not one of them in v1). Nothing wires TO a loop; like `endpoint`, it has no inbound capability
cell, because nothing addresses a loop as a target — it is purely a source.

### Interval floor — non-negotiable, per the brief

**Proposed floor: `MIN_INTERVAL_MS = 30_000` (30 seconds), for both targets, enforced in
`wheel_core::validate_config_with` (the same choke point `create_with`/`update_with` already call — no new
validation call site).**

Reasoning, not just a number:
- **Tool target**: a single call may legitimately take up to `CALL_TIMEOUT` (30s, execute.rs:27) before this
  engine gives up on it. A floor below that lets fires overlap — a slow call still in flight when the next tick
  starts — which either queues unboundedly (memory) or requires a "skip if already in flight" rule this proposal
  would rather not need. Setting the floor AT the call timeout means at most one call is ever outstanding by
  construction, no extra bookkeeping required.
- **Agent target**: 30s is not "safe" against budget burn on its own — an operator can still configure a loop
  that burns through `budget.max_usd` in an hour. The floor's job is to block a DEGENERATE config (a `100`
  entered where `100_000` was meant, a fat-fingered unit), not to be the budget control. `AgentConfig.budget`
  already exists and already stops the agent (`budget_exhausted`) regardless of what fired the message that
  tripped it — a loop is just another sender into the same enqueue path, so it inherits that backstop for free
  rather than needing a second one.
- Refused at creation/update (400, names the floor), same posture as every other config validation in this file
  (`validate_endpoint_path`, the tool schema checks) — never silently clamped, which would let an operator
  believe they configured 5s and get 30s with no record of why.

### Lifecycle: `start`/`stop`, like an agent — placement must not mean "firing"

A loop needs the same on/off distinction an agent has, for the same reason `run_on_startup` defaults to `false`:
placing a node must never be indistinguishable from deliberately activating it. Proposed:

- `LoopStatus { Stopped, Running }` (two states — no `starting`/`parked`/`error` richness needed; a loop has no
  process to spawn, no auth to hold, nothing to crash mid-fire beyond "the last call failed," which is a log
  line, not a status).
- `POST /v1/loops/:id/start` / `.../stop`, mirroring the agent routes' shape. Created `Stopped` always — there is
  no `run_on_startup`-equivalent field for a loop in v1; if the operator wants one auto-started they start it
  once and it persists across engine restarts (see next point), which covers the same use case without adding a
  second boolean to reason about.
- On engine boot, `start_configured_agents()`'s sibling for loops re-arms the timer for every `Running` loop —
  same reconciliation shape, so a loop survives an engine restart exactly as a parked agent's queue does.
- Counts toward the existing per-project node cap (§3e, default 50) — no new cap. A resource a project can
  create 50 of already includes this one; a loop firing every 30s is not categorically more dangerous than an
  agent that never idles, and both are already bounded by that cap plus their own budget/rate limits.

### Failure handling: queue for agents (free), skip-and-log for tools (deliberate)

- **Target is an agent, and it's parked/stopped**: nothing new — `supervisor.deliver` on a stopped/parked agent
  already queues durably and resumes on next start/message per §3c#14. The loop does not need to know or care
  what state its target is in; it calls the same function a human's `wheel msg` would.
- **Target is a tool, and the call fails** (network error, non-2xx, timeout): logged as an ordinary tool-call
  failure event (same shape `wheel tool call` failures already produce), and the loop simply waits for its next
  tick. No immediate retry — retrying a failing external endpoint inside a tight interval is exactly the
  "hammering" case the floor exists to prevent, and the next scheduled fire IS the retry, on the cadence the
  operator already chose.

### Fairness: no new lane

A loop→agent fire enters the SAME normal-priority lane as any other agent/endpoint message (§3c#12) — not the
user lane. This means a fast loop cannot starve user traffic (the user lane already has priority over it), but
it CAN, at a short-but-legal interval, dominate the normal lane for its one target agent's own queue — which is
exactly as true of a human sending that agent a message every 30 seconds by hand, and not a new failure mode a
loop introduces. No new lane is proposed; the floor and the target agent's own budget are the two controls doing
real work here, and a third lane would protect against a case (loop vs. other non-user traffic on the SAME
agent) the operator configuring a 30-second loop has already explicitly asked for.

## Open questions this proposal is taking a position on (not leaving open)

- **Dynamic prompt (brief's question)**: v1 ships `prompt` as a static string, matching `AgentConfig.system_prompt`
  and every other "config the operator edits" field in this schema. The brief floats reading from a wired ctx
  node instead (§3 injection precedent) — deliberately NOT built now: it would need a third wire cell
  (`loop → ctx, read`), and "read the current markdown at fire time" is a different, slightly more complex
  operation than injection (which happens once, at spawn) that deserves its own review rather than riding in on
  this proposal. If an operator needs a changing prompt without editing the loop node, editing `prompt` via
  `PATCH /v1/nodes/:id` today already does that — a loop reads its OWN current config on every fire (it is not
  cached at start-time), so this is a real, if less elegant, answer that exists without new code.
- **Tool op selection**: static `tool_op`+`tool_args` on the loop's own config (not chosen per-fire, since
  nothing is present at fire time to choose with) — settled above.

## Non-goals

- No new priority lane (settled above).
- No dynamic/ctx-sourced prompt in v1 (settled above).
- No loop→loop or loop→endpoint wires — a loop is a source, not a router; chaining loops is better solved (if
  ever needed) by a loop firing into an agent that itself does the chaining, which the wire matrix already
  allows with zero new cells.

## Owners

SDK: `NodeType::Loop`/`NodeConfig::Loop` + the two wire matrix cells + `validate_config_with`'s floor check +
the timer/scheduler (mirroring `arm_park_timer`) + `start`/`stop` routes + boot-time reconciliation. Web: the
loop node's UI (interval editor defaulting to a minutes/hours picker that converts to `interval_ms`, per the
operator's explicit instruction that this is part of THIS proposal's UI half — prompt/op config, start/stop
control). No API-owned surface identified — loop lifecycle routes live on the engine control plane like agent
lifecycle routes do, proxied by the host the same way.
