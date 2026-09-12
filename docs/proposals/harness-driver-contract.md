# Phase 2 scoping: the `HarnessDriver` contract (Codex support)

**Author:** SDK/Engine. **Status:** proposal, nothing built. Anchors verified against `origin/dev`
`4cb2c36`. This is the scoping PM asked for before Codex-harness work starts; §7 has the
recommendation on sequencing.

`docs/proposals/agent-grid-engine.md` §3 already ruled the shape at a high level: replace `Harness`
with `HarnessDriver::launch() -> DriverSession`, methods `send_turn`/`steer`/`interrupt`/`shutdown`,
events `Ready`/`SessionStarted`/`Frame`/`TurnComplete{usage,cost,is_error,interrupted}`/`NeedsAuth`/
`RateLimited`/`Exited`. This document is the part that proposal deferred: what in the *current*
supervisor has to change to host that contract without regressing F008, reaping, or parking, and in
what order.

## 1. Why `Harness` cannot express Codex

`Harness` (`crates/wheel-engine/src/harness/mod.rs:101-128`) is a stateless line transform: `argv`
and `env` describe one spawn, `encode_turn` is a pure string function, `parse_line` maps one stdout
line to one `HarnessEvent` with no memory between calls. That shape fits `claude --print
--input-format stream-json`: one JSON object per line in, one out, no conversation the driver itself
needs to hold.

Codex's app-server protocol is JSON-RPC over stdio with **approvals**: the harness can ask the engine
mid-turn "may I run this command / write this file", and the reply has to go back on the same
connection before the turn continues. That is a request the *driver* must answer, using policy the
supervisor already enforces elsewhere (`--permission-mode bypassPermissions` today refuses this
question by construction, at spawn time, rather than answering it while the child waits). A
stateless `parse_line` cannot hold "there is a pending approval with this id, and the turn is
blocked until it is answered" — there is nowhere to put the id. This is the concrete case behind the
proposal's line "the current trait is a stateless line parser plus argv and cannot express that."

Two consequences that shape the contract, not just this one method:

- The driver needs an object with a lifetime spanning the child process (`DriverSession`), not a
  stateless set of pure functions called by the supervisor. `launch()` returns it; every other
  method is called on it.
- `steer` (send a second turn while one is in flight) and answering an approval are the same shape:
  "write something to the child that is not a fresh turn, while a turn is running." The proposal's
  Phase 1 item 7 already needs this for Claude's own steer support, so the contract should not grow
  a Codex-only side channel for approvals — approvals are `steer`'s first real caller, not a new
  method.

## 2. What in `supervisor/mod.rs` is Claude-specific right now

Read against `origin/dev`, not assumed from the proposal (two anchors below had drifted from what
the plan doc's own anchors would suggest — same lesson as `agent-grid-engine.md`'s own note that
anchors need re-checking against the branch head):

- **One driver for the whole supervisor, not one per agent.** `Supervisor.harness: Arc<dyn Harness>`
  (`:313`) is set once in `Supervisor::new` (`:332`, hardcoded `ClaudeDriver`) and read at every
  spawn (`:842-843`), in `pump_queue`'s `encode_turn` (`:1523`), in `pump_stdout`'s `parse_line`
  (`:1576`, `:1604`), and in `reap`'s `classify_startup_failure` (`:1900`). `agent_cfg.harness` is
  checked once, in `start`, purely as a **refusal gate** (`has_driver`, `:648`) — if it passes, the
  child is spawned with `self.harness` regardless of which harness the node actually names. Today
  that is safe only because `has_driver` admits nothing but `Claude` (`crates/wheel-engine/src/
  harness/mod.rs:20`). The moment a second driver is admitted, this becomes the bug the comment at
  `:644-647` already names: *"the driver is a hardcoded ClaudeDriver... a silent harness
  substitution the operator cannot see."* Fixing the gate's wording is not enough — the field itself
  has to stop being supervisor-wide.
- **`Running` does not remember which driver spawned it.** (`:131-156`). `reap` and `pump_stdout`
  both reach back to `self.harness` to interpret the child they are tearing down or reading. Once
  driver selection is per-agent, a `Running` for a Codex agent must be interpreted with the Codex
  driver even after `agent_cfg.harness` changes underneath it (renaming a node's harness while it is
  running is not a thing today, but the exit path must not assume `self.harness` still describes the
  child that is dying).
- **Resume is an argv flag, decided by the caller.** `SpawnSpec.resume: Option<String>`
  (`harness/mod.rs:34`) is threaded from `board::agent_state(..).session_id` (`:634-635`) straight
  into `spec`, and `ClaudeDriver::argv` turns it into `--resume <id>` (`harness/claude.rs:52-55`).
  That is a Claude CLI convention, not a property every harness shares. Codex's app-server resumes a
  conversation as a protocol operation *after* the process is already up, not as a launch argument.
  `launch()` needs to take the resume hint and let the driver decide what to do with it, rather than
  the supervisor building an argv token that only happens to mean the right thing for one driver.
- **The exit-classification call is keyed on the dying driver, not a fixed one.** `reap`'s
  `self.harness.classify_startup_failure(None, &output)` (`:1900`) has the same problem as the first
  bullet: it must run against the driver that was actually spawned, via whatever `Running` (or its
  replacement) carries forward from `launch()`.
- **F008's guarantee is currently structural, not contractual — and that has to survive.** The
  session match (`session_matches`, checked at `:1633` for `Text` and `:1651` for `Result`) works
  today because `ClaudeDriver::parse_line` reads `session_id` off the *top level* of one whole-line
  JSON object, and a tool a Claude agent runs cannot make its own stdout become a top-level line on
  the harness's own stdout pipe — the CLI nests tool output inside JSON string fields structurally,
  which is what lets `parse_line` be a pure function of one line at all. A JSON-RPC driver has the
  same shape (one framed message in, its own `id`/method fields), so the guarantee ports, but it
  ports **per driver, proven per driver** — see §4. The contract must not let a driver author believe
  the guarantee is inherited from the trait; it is proven again for each implementation, the same way
  `ClaudeDriver::parse_line` earns it today by construction, not by declaration.

## 3. The contract

```rust
pub trait HarnessDriver: Send + Sync {
    fn kind(&self) -> wheel_core::Harness;

    /// Spawn the child and return a live session. Replaces `argv`/`env`/the
    /// supervisor building `Command` directly (`:842-853`) for THIS driver;
    /// `child_command` (`:276`) stays the only thing that clears/repopulates
    /// the environment — `launch` receives an already-`env_clear`ed
    /// `tokio::process::Command` and only adds to it, it does not construct one.
    async fn launch(
        &self,
        cmd: tokio::process::Command,
        spec: &SpawnSpec,
    ) -> std::io::Result<Box<dyn DriverSession>>;
}

#[async_trait]
pub trait DriverSession: Send {
    /// Write one turn. Replaces `Harness::encode_turn` + the direct
    /// `running.stdin.write_all` in `pump_queue` (`:1523`, `:1539`) for this
    /// driver — encoding and writing move together because a JSON-RPC driver
    /// picks the request id at write time, and the supervisor must not
    /// pick ids for a protocol it does not own.
    async fn send_turn(&mut self, envelope: &str) -> std::io::Result<()>;

    /// Write something that is not a fresh turn while one is in flight: a
    /// steer, or an approval reply. One method, because both are "the driver
    /// decides what un-blocks the child," not two unrelated features (§1).
    async fn steer(&mut self, envelope: &str) -> std::io::Result<()>;

    /// Cancel whatever is in flight without losing the session. Replaces
    /// `Running::kill` for the "interrupt" call site (`:1392`); `stop`/
    /// `shutdown`'s process-group SIGTERM/SIGKILL stay supervisor-level,
    /// because they are about the OS process, not the protocol.
    async fn interrupt(&mut self) -> std::io::Result<()>;

    /// The next event, or `None` on EOF. Replaces the `BufReader::lines()` +
    /// `parse_line` loop in `pump_stdout` (`:1587-1604`) — a JSON-RPC driver
    /// owns its own framing (Content-Length headers, in Codex's case) instead
    /// of the supervisor assuming one JSON value per newline.
    async fn next_event(&mut self) -> DriverEvent;

    async fn shutdown(&mut self);
}

pub enum DriverEvent {
    Ready,
    SessionStarted { session_id: String },
    Frame { session_id: Option<String>, text: String },
    ApprovalRequested { id: String, summary: String },
    TurnComplete { session_id: Option<String>, is_error: bool, text: Option<String>,
                   turns: Option<u64>, cost_usd: Option<f64> },
    NeedsAuth,
    RateLimited { session_id: Option<String>, status: String, window: Option<String>,
                  utilization: Option<f64>, resets_at: Option<i64> },
    Exited { code: Option<i32> },
    /// Exactly today's `HarnessEvent::Unknown`: never fatal, always logged.
    Unknown { raw: String },
}
```

Notes on the mapping from today's types, since the proposal names the events but not how they
replace what exists:

- `ApprovalRequested` is new. Nothing in `HarnessEvent` has it, because Claude's
  `bypassPermissions` refuses the question before the child ever asks. It is Codex-only in practice
  today, but it is a top-level event, not a Codex-specific method, so a future driver with the same
  need (ACP, per the port order) does not grow its own variant.
- `Exited` replaces the supervisor inferring death from stdout EOF (`pump_stdout`'s `while let Ok
  (Some(line)) = lines.next_line()` ending, `:1597`) plus a separate `child.wait()` in `reap`
  (`:1870`). Making it an event the driver emits (rather than the supervisor noticing EOF) is what
  lets a driver whose framing is not line-based — Codex's Content-Length-prefixed frames — signal
  exit without the supervisor knowing anything about its wire format.
- `classify_startup_failure` is dropped as a *separate* trait method. Under the old contract it
  existed because a dead child leaves the supervisor holding only raw stdout/stderr text and no
  parsed opinion about it. Under the new one, `next_event` keeps yielding until `Exited`, so a
  driver that recognises "not logged in" in its own protocol emits `NeedsAuth` directly, the same
  way it already recognises a `TurnComplete`. `reap` then classifies from the **event history**
  (did we see `NeedsAuth`? did we ever see `SessionStarted`?) instead of re-deriving it from raw
  text after the fact. This removes one Claude-shaped assumption (`ROOT_REFUSAL` string-matching
  against combined stdout+stderr, `harness/claude.rs:19-24`) from being something every future
  driver's exit path re-implements — it becomes something `ClaudeDriver::next_event` does once, in
  the one place that already reads its lines.

## 4. Registry, not a supervisor-wide field

`Supervisor.harness: Arc<dyn Harness>` (`:313`) is replaced by:

```rust
struct DriverRegistry(HashMap<wheel_core::Harness, Arc<dyn HarnessDriver>>);
impl DriverRegistry {
    fn get(&self, h: wheel_core::Harness) -> Option<&Arc<dyn HarnessDriver>> { self.0.get(&h) }
}
```

`has_driver` (`harness/mod.rs:20`) becomes `registry.get(harness).is_some()` — one lookup, so node
creation, `start`'s refusal gate, and `GET /v1/engine`'s `harnesses` list stay the "one answer"
property the doc comment at `:18-20` already asserts, just sourced from the registry instead of a
hardcoded `matches!`.

`start` (`:607`) looks up `registry.get(agent_cfg.harness)` — already has `agent_cfg` in scope at
`:648` where the refusal check runs today — and spawns *that* driver, not `self.harness`. `Running`
gains a field carrying the `Box<dyn DriverSession>` it is driving (replacing the raw `stdin: ChildStdin`
+ `child: Child` pair at `:137-138`, since a `DriverSession` now owns the process it launched — the
supervisor still owns `pgid` for process-group signals, which stays outside the trait because SIGTERM/
SIGKILL to a group is an OS-level operation `stop`/`shutdown`/`interrupt`'s kill path perform directly,
not a protocol concept). `reap`, `pump_queue`, and the read loop all call methods on `running.session`
instead of reaching for `self.harness`, which is exactly what fixes the "dying child interpreted by
the wrong driver" gap in §2.

## 5. F008, made a conformance suite instead of a Claude-only property

`session_matches` (used at `:1633`, `:1651`) does not change: a `TurnComplete`/`Frame`'s
`session_id` is still checked against the one recorded from `SessionStarted`, and a mismatch is
still logged and dropped, never ends a turn. What changes is that this guarantee currently has
exactly one proof, `ClaudeDriver::parse_line`'s tests (`harness/claude.rs`, e.g.
`an_init_without_a_session_id_is_not_treated_as_an_init`), and no way to require the same proof from
a second driver.

**Proposed: a driver-parametrised test module**, `crates/wheel-engine/src/harness/conformance.rs`,
exported as a function every driver's own test module calls with itself:

```rust
pub fn assert_forged_result_is_never_top_level(driver: &impl HarnessDriver, ...) { ... }
```

At minimum it proves, against a fake child the test controls: (a) a line/frame the child's own
*tool* would produce, containing something shaped like a top-level completion event, does not reach
`next_event` as `TurnComplete` when it appears nested inside a JSON string value rather than as the
frame's own top-level shape (`ClaudeDriver`'s version of this is implicit in `serde_json::from_str
::<Value>(trimmed)` reading `v.get("type"))` — a nested occurrence is inside a string leaf, never a
sibling key, so it is structurally inert; the conformance test is what makes a *future* driver prove
the same rather than assume it); (b) a `TurnComplete`/`Frame` whose `session_id` does not match the
one from `SessionStarted` is surfaced as `Unknown`-shaped noise, never as the real event; (c) a
`SessionStarted` with no session id is not treated as one (mirrors
`an_init_without_a_session_id_is_not_treated_as_an_init`). Codex's own port adds whatever is specific
to JSON-RPC framing (a `result` reply whose `id` does not match an outstanding request, for
instance) as driver-specific cases alongside the shared ones, not instead of them.

This is the concrete answer to "touches F008" in the proposal's Phase 2 paragraph: F008 is not
weakened by the refactor, but its proof stops being singular. Bringing Codex in without this suite
would mean the second driver's forged-result defence rests on nothing but the author's care, which
is exactly the gap ADVERSARY review should be asked to close before Codex's own driver lands (see
§7).

## 6. Reaping and parking under the new contract

- **Reaping.** `reap` (`:1847`) currently distinguishes "started, then exited" from "never started"
  via the `initialised` flag set on the first `Init` (`pump_stdout`, `:1607`) and classifies the
  latter from raw output text. Under `DriverEvent`, `initialised` is set on `SessionStarted` exactly
  as before, and classification reads the event stream instead of the text — see §3's note on
  dropping `classify_startup_failure`. The `already_diagnosed` short-circuit (`:1889-1892`, "don't
  overwrite a `NeedsAuth`/`Error`/`BudgetExhausted` a `Result` already set") is unchanged; it reads
  `AgentStatus` from the DB, which is driver-agnostic already.
- **Idle-parking / resume.** Parking itself (`stop` that keeps `session_id`, doc comment `:1093`) is
  unchanged — it is a supervisor-level decision, not a driver one. What changes is who turns a kept
  `session_id` into a resumed conversation: today `start` puts it straight into `SpawnSpec.resume`
  and only `ClaudeDriver::argv` reads it (§2). Under the new contract, `launch(cmd, spec)` receives
  `spec.resume` and the *driver* decides — `ClaudeDriver` still appends `--resume <id>`; a Codex
  driver instead performs whatever the app-server calls "attach to conversation N" as its first
  protocol exchange inside `launch`, before returning the `DriverSession`. The supervisor's contract
  with every driver stays the same: "if `resume` is `Some`, hand back a session already resumed, or
  fail `launch` with a reason" — it does not need to know which of those two shapes it is.
- One sharp edge worth flagging now rather than at implementation time: `reap`'s "a resumed session
  that never initialised is a dead session, clear it" logic (`:1964-1976`) assumes resume failure
  shows up as "never got to `SessionStarted`." If Codex's resume is a protocol call *inside*
  `launch()` rather than an argv token the process either honours or ignores, a failed resume might
  need to fail `launch()` itself (returning `Err` before a `DriverSession` even exists) rather than
  spawning successfully and then never emitting `SessionStarted`. Both must clear the stored session
  id the same way; this is a case to pin with a test on the Codex port, not something the trait
  needs to resolve today since Claude's own resume genuinely is "spawn, then maybe never init."

## 7. Sequencing and recommendation

The proposal already ruled Phase 2 lands "behind a green Phase 1 suite, not alongside it," because
this refactor is the one that touches reaping and F008. I'd add one more ordering constraint from
having now read the current code closely: **the registry + `Running` changes in §4 should land as
their own PR, re-hosting `ClaudeDriver` under the new trait with no behaviour change, before any
Codex code exists.** That PR is entirely mechanical risk (does the existing Claude test suite still
pass unchanged) with zero new-protocol risk, so it is the right place for ADVERSARY's required
review of "does F008 still hold" to happen — reviewing it mixed in with Codex's actual app-server
integration would ask one review to separate "did the refactor regress Claude" from "is Codex's
approval-reply path safe," which are different questions with different reviewers' worth of
attention.

Concretely, three PRs in order, each gated on the last being green and merged:
1. `HarnessDriver`/`DriverSession`/`DriverEvent` + the conformance suite (§5) + registry (§4),
   `ClaudeDriver` ported with behaviour unchanged. No Codex code. This is where "port order: Claude
   first" (proposal §3) is satisfied.
2. Codex app-server driver: `launch`/`next_event` framing, `ApprovalRequested` → policy (what does
   Wheel answer, and does that answer come from `agent_cfg.permission_mode` per Phase 1 item 4, or a
   fixed refusal until AgentGrid's UI can surface the prompt? — **open question for PM/ADVERSARY**,
   not decided by this document), resume-as-protocol-call (§6's sharp edge, pinned with a test).
3. `has_driver`/`GET /v1/engine`'s `harnesses` list flips Codex from refused to offered, in the same
   commit as whatever test proves it end-to-end (mirrors the Phase 0 rule that a feature id and its
   route/field land together).

**On priority relative to my current queue:** PR 1 above is safe to start now — it is a pure
refactor of code I already know well (this session's reading of `supervisor/mod.rs` and
`harness/mod.rs`/`claude.rs` above), it has no dependency on anything currently blocked on review,
and doing it while PR #60/#70's review is elsewhere pending is exactly the kind of unblocked work
this window is for. I'd treat PR 2 (the actual Codex protocol work) as the part that should wait for
an explicit go-ahead, since it is where the "fairly high priority" framing matters most and where
ADVERSARY's bandwidth should be reserved rather than split across it and #60/#70 at the same time.
I'll start PR 1 next unless told otherwise.

## 8. Open questions

- **Approval policy** (§7, item 2): what does the engine answer when Codex asks for a permission it
  would need a human for, given headless children cannot block on a prompt (the same reasoning
  behind Claude's `bypassPermissions`, `harness/claude.rs:35-37`)? Candidates: always allow (matches
  Claude's current posture, extends the "sandbox is the boundary" security framing to Codex without
  a new carve-out), or refuse-and-report (safer default, but makes Codex agents functionally
  crippled until AgentGrid can surface approvals, which is Phase 4 canvas work — Phase 2 for engine,
  much later for UI). I'd default to always-allow for consistency with the existing security
  framing, but this is exactly the kind of credential/policy-shaped call §1 of the contract doc says
  needs ADVERSARY review before merge, not a default I should just pick.
- **Does `DriverSession` need `Clone`/`Sync`, or is `&mut self` on every method sufficient?** Current
  draft assumes the supervisor already serialises access per agent via the slot's `AsyncMutex`
  (`AgentSlot`, `:197`), so a driver never needs interior mutability of its own — worth confirming
  this holds once `steer` and `interrupt` are both real methods that can race against `next_event`'s
  read loop for the same session.

