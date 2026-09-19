# Proposal: `send {mode:"steer"}` — redirect a running turn without killing it

Status: **proposal, nothing built.** Author: SDK. Date: 2026-09-19. Answers PM's request for the
hosted-client-parity item in `docs/proposals/agent-grid-engine.md` §2 item 7 (the `steer` half; `interrupt`
shipped). Adversary reviews before any code (it changes a delivery invariant and a stdin write path).

Anchors are `origin/dev` at `8a82fa1`. Line numbers in `supervisor/mod.rs` are where I read them today and will
drift.

## 1. Why, and what exists today

AgentGrid's client already exposes steer. Against a current engine it cannot work: `SendBody`
(`api/agent_routes.rs`) has no `mode` field and is not `deny_unknown_fields`, so an unaware engine would
**silently ignore** `mode:"steer"` and queue an ordinary message. Any client must therefore gate on the
`steer` feature id (below) and never infer support from a 2xx.

The only redirect available now is `interrupt` (`Supervisor::interrupt`, `mod.rs:1497`): it kills the child,
consumes the in-flight message as `INTERRUPTED_BY_USER`, parks the agent keeping its session, then `deliver`s
the queued follow-up on a resumed child (test `interrupt-then-steer`, `mod.rs:~6407`). That loses the turn's
in-progress work and restarts the process. Steer is the non-destructive variant.

## 2. The invariant this touches

`docs/ARCHITECTURE.md` §3c #12: *"Delivery is strictly serial: one message per turn, the next written only
after the harness's `result`. User messages … are never injected mid-turn. … Explicit interrupt is a separate,
deliberate action."* In code that is `Running.in_flight: Option<Uuid>` (`mod.rs:146`); `pump_queue`
(`mod.rs:1611`) returns early while it is set, and a `result` does `in_flight.take()` (`mod.rs:~1791`) and
settles that one message (`settle_turn`, `mod.rs:2638`).

**Steer is a deliberate amendment to #12, not a loophole in it**: a second, explicit, operator-initiated action,
like interrupt, that writes mid-turn. It needs a PM ruling and an edit to §3c #12 in the same change as the code.
What must **not** change: exactly one writer to a child's stdin (the supervisor's delivery path, under the slot
lock), and nothing but an explicit user action ever writes mid-turn.

## 3. What I do not know, and why it gates everything

Whether steer can be built as designed depends on how the real `claude --input-format stream-json` child treats
a user message written while a turn is running. This repo cannot answer that: `qa/harness/fake-claude` and the
shell shims model *our* assumptions, and the driver only ever writes when idle (`pump_queue`). I have **not**
run the real CLI for this proposal. Candidate behaviours:

- **Fold** — the CLI incorporates the message into the running turn; one `result` covers both.
- **Queue** — the CLI buffers it and runs it as its own turn after the current one; two `result`s, in order.
- **Drop / reject / corrupt** — ignored, an error event, or interleaved output. (Cannot be ruled out.)

**Gate 0 (before any code): a spike against the real CLI**, recorded in this file. It must answer: which of the
three; whether a `result` appears per message or per turn; whether `--replay-user-messages` echoes the mid-turn
message and with what identifier; what happens if the message arrives during a tool call vs while the model is
streaming; whether stdin reads stall while a turn runs (§5.3); and the same for a large body. Codex is out of
scope (no driver, `has_driver` refuses it).

The design below is parameterised on this so the spike selects a constant rather than forcing a redesign.

## 4. Design

### 4.1 API

`POST /v1/agents/:id/send {body, mode?: "steer", reply_to?, await_secs?}`. Same route, same **Prompter** tier
(`auth/policy.rs`), control plane only. Response is today's receipt plus `"steered": bool`.

- Turn in flight → the message is written now: `steered:true`, state `delivered`.
- No turn in flight (idle/parked/stopped/starting) → behaves exactly as a normal send, `steered:false`. Not an
  error: the turn can end between the client's decision and our check, and failing that race helps nobody.
- Engine or harness lacks steer → `409 steer_unsupported` (the client should have gated on the id).

### 4.2 State

Add to `Running`: `steers: VecDeque<Uuid>` — delivered mid-turn, not yet consumed — and a per-driver constant
`SteerModel::{Fold, Queue}` on the `Harness` trait, chosen by the spike.

On `result`: consume `in_flight` as today. Then **Fold** → consume every id in `steers` with the same outcome;
**Queue** → pop the front of `steers` into `in_flight` (it is now that turn) and leave `pump_queue` blocked, so
nothing else is written until *its* result. If the turn ends with an error, the existing rule applies to every
consumed id: consumed with `error=true`, never redelivered.

Anything that reads or clears `in_flight` must handle `steers` too. Known places: `stop`
(`mod.rs:1462`), `interrupt` (`1497`), `shutdown`'s drain and `turns_in_flight`, and `reap`'s
`requeue_all_undelivered` (`~2120`). **Dependency:** `stop()` today takes the slot and kills the child without
settling `in_flight` — the "stop mid-turn strands the message in `Delivered`" defect from the agent-grid-engine
proposal §2 item 6, still open. Steer multiplies the number of messages that defect can strand, so it should be
fixed first (my recommendation: as its own small PR before steer code).

### 4.3 Single writer and the blocking-write hazard

The write happens in the same delivery function as `pump_queue`, under the slot guard — still one writer.
The new hazard: `pump_queue` only writes when the child is idle and waiting for input; steer writes to a child
that is **busy**. If the child is not draining stdin during a turn, `write_all` on a full pipe (a body may be up
to 256 KiB, larger than a 64 KiB pipe buffer) blocks **while holding the slot guard**, and `interrupt` needs that
same guard to kill the child — the very tool the operator would reach for. Requirements: the steer write has a
short timeout; on timeout it fails the *message* (`last_error`, state stays `queued`, never truncated, §3c #11)
and releases the guard; and the spike (§3) measures whether the CLI reads stdin mid-turn at all. If a
timeout-guarded write in the current structure cannot be made safe, steer should wait for the driver-owned-session
design in `harness-driver-contract.md` §9.2 (one task owns the session, commands over a channel) rather than be
bolted onto `pump_queue`.

### 4.4 Priority lanes and fairness

Steer is not queued and does not enter `next_for_delivery`, so the user-lane 3-in-a-row rule and the 60 s
promotion are untouched; a steer does not increment `consecutive_user`. Cap unconsumed steers per agent
(proposal: 3; `429 too_many_steers`) so the operator cannot grow an unbounded backlog inside one turn.

### 4.5 Interaction with interrupt

Interrupt must consume `in_flight` **and every id in `steers`** with `INTERRUPTED_BY_USER`, never requeue them
(same reason as today: a killed turn may already have committed or pushed). A steer sent to a child that is
being interrupted loses the race harmlessly: the slot lock orders them, and the second sees no turn in flight.
Crash mid-turn (`reap`): steers are consumed with an error (`steer_lost`), **not** requeued — a steering
instruction replayed into a fresh session refers to a turn that no longer exists.

### 4.6 Who may steer

Operator (`from=user`) only, on the engine-secret control plane. **Not** available on `/v1/cli/*`
(agent→agent), ingress endpoints, or scripts. Steer is "interrupt the model's train of thought" — from a
`send` wire or the public internet it is a stronger prompt-injection lever than a queued message (finding 031/035
class). Any agent-originated steer would be a new wire-matrix cell and needs its own adversary review; not
proposed here. Guests cannot (`shared-projects.md`: guests cannot send, interrupt or steer).

### 4.7 Envelope

Same `<AgentPrompt from="user" type="user">` framing, escaping unchanged. Open question (§6): add an
engine-generated `steer="1"` attribute so the model can tell it arrived mid-turn. Cheap and unforgeable (bodies
are escaped) but changes what the model sees, so I would decide it with the spike results in hand.

### 4.8 Discovery, docs, schema

Add `steer` to `FEATURES` **in the same commit** as the field and behaviour (the repo's rule; the id is already
named in the accepted proposal). Report it only when the active driver has a `SteerModel`. Update PROTOCOL.md
(`send`) and ARCHITECTURE §3c #12, and add `SendBody.mode` (an engine-local type, not in the exported
schema). Per-harness support (Codex) is a
`DriverSession::steer` concern in the driver contract and is out of scope.

## 5. Failure modes I would test (each a mutation target)

1. Steer while idle → normal send, `steered:false`.
2. Fold: one `result` consumes both; Queue: two results consume in order, nothing written between.
3. Interrupt with a steer in flight → both consumed `INTERRUPTED_BY_USER`, neither requeued.
4. Crash mid-turn with a steer in flight → consumed `steer_lost`, not requeued.
5. Steer write that blocks → times out, message stays `queued` with `last_error`, `interrupt` still works.
6. Cap: the 4th unconsumed steer → 429.
7. A guest / agent / endpoint / script cannot steer.
8. No `steers` id is left `delivered` after a stop, park, shutdown or reap (the strand class).
9. `mode:"steer"` against an engine without the id is not treated as supported by the reference client.

## 6. Decisions needed

1. **Ruling to amend §3c #12** (the mid-turn rule). Recommendation: yes, explicit operator action only.
2. **Run the spike first** (§3) and paste results here; do not start §4 until it says Fold/Queue and that stdin
   reads mid-turn are safe. If the CLI drops or corrupts mid-turn input, steer is **not buildable on Claude** and
   the honest answer to AgentGrid is "use interrupt + send".
3. Fix the stop-mid-turn strand first (small, independent, already-known defect).
4. Cap on unconsumed steers (3?) and the write timeout (5 s?) — my numbers, not measured.
5. `steer="1"` envelope attribute: yes/no.
6. Should `await_secs` on a steer wait for its own consumption? Recommendation: yes, identical to today's semantics.

## 7. Out of scope

Agent-originated steer; steer for Codex/ACP; more than one live turn per agent; rewriting a message already
delivered; changing the priority-lane rules.
