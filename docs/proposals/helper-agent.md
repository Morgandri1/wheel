# Proposal: board-scoped helper agent

Status: **draft, for adversary + PM review**. Adversary already sent 5 review anchors ahead of this draft
existing (relayed by PM 2026-09-09); each is addressed explicitly below, not left for a second review pass to
discover.
Author: SDK. Date: 2026-09-10. Answers `docs/wow-agent-brief.md` task 5.

## Scope warning, up front

Unlike tasks 1 (portals) and 8 (loop node), this is not "wire up an existing mechanism." Grepping the entire
codebase for `place`, `grant`, `revoke`, `owner_node`, `may_place` finds **zero matches outside
`docs/ARCHITECTURE.md`** — §3e's whole "an agent can place/grant/manage nodes" surface is documented as M2 and
does not exist in code. A helper agent needs that surface to do anything (place/update/remove nodes, wire/grant,
inspect the board), so this proposal is really two things stacked: (A) the generic place/grant/manage engine
primitives §3e already promises every sufficiently-privileged agent, and (B) the helper-specific behavior built
on top of them (auto-wire-to-everything, the precise-prompt requirement, containment). **Recommend building and
reviewing (A) as its own PR before (B)** — (A) is independently useful (any future `may_place: true` agent needs
it) and separating them means adversary reviews general capability-grant logic once, then reviews the helper's
ADDITIONAL blast radius on top of already-reviewed primitives, rather than both at once.

## Design

### What "global read/write" actually is: real materialized wires, not a bypass

**Adversary anchor 1, addressed directly**: a `helper: bool`-style shortcut that skips the wire check for this
agent type is a hard block, full stop — regardless of how tight the system prompt is, a capability the wire
model cannot see is a capability the UI cannot show or the operator revoke, which fails §3 at its root ("the
wire matrix still governs"). This proposal creates **real rows in the `wires` table**, kept live by two hooks:

- **On helper creation**: enumerate every existing node, and for each, create every `(Agent, <node type>,
  WireType)` wire `wheel_core::wire::allowed_wires()` says is legal from an agent to that type — filtered by the
  vault carve-out below. `allowed_wires()` already exists (`wire.rs:183`) and is exactly "every wire type a wire
  matrix update would need to reason about," so this reuses the matrix's own enumeration rather than
  hand-listing per-type rules that could drift from it.
- **On any future node creation**: a hook in `db::board::create_with` (`board.rs:86`, the single choke point
  every node insertion already goes through — `api/board_routes.rs:65`'s `create_node` and any future `place`
  tool alike) checks whether any agent on the project has the helper role, and if so, wires it to the new node
  the same way, immediately, in the same transaction as the node's own creation.
- **On node deletion**: no new work — `DELETE /v1/nodes/:id` already cascades wires (contract §4), so a helper's
  wire to a deleted node disappears for free.

Every one of these wires has `granted_by` set (the existing audit column, `board.rs:415,460` — currently
recorded but unused for attenuation) to the helper's own id, so `wheel connections`/the UI can show and the
operator can `remove_wire` any one individually, exactly like a hand-drawn wire. "Global" is a real, visible,
per-wire-revocable set from the moment it's created — never an invisible flag checked at authorization time.

**Said explicitly (adversary asked this be confirmed rather than assumed):** the hook above only ever creates
wires OUTGOING from the helper — `(Agent, <other node>, WireType)` — never the reverse `(Ctx, Agent, Send)`
injection direction a ctx node's own wire would need to auto-inject its markdown into the helper's preamble. This
is intentional, not an oversight of the enumeration: "global read/write" and "inject everything into my context"
are different capabilities with different costs (the latter is unbounded preamble bloat as ctx nodes accumulate,
and a helper does not need every ctx's content sitting in its context window to `wheel read <ctx>` one on
demand). The helper gets on-demand data access to every ctx node's content, exactly as proposed; it does not get
every ctx node's content permanently baked into its own system prompt.

### Vault is a hard-coded exclusion, not an assumed carve-out

**Adversary anchor 3, addressed directly**: the auto-wire hook above **unconditionally skips `NodeType::Vault`**
in both the initial enumeration and the future-node hook — not a configurable option, not something the helper's
role can opt into, a `match node_type { NodeType::Vault => continue, ... }` a reviewer can see has no escape
hatch. "Global read/write" as a phrase in the brief is scoped by this proposal to mean "every node whose wire
matrix cell result an operator would unsurprisingly expect a workflow-building assistant to touch" — a
credential store is never one of those, and this is asserted in code, not left to the helper's prompt to
self-restrain (§2's whole point: nothing relies on an agent restraining itself). An operator who deliberately
wants the helper to read one specific vault still can, by hand-drawing that one wire exactly as they would for
any other agent — that is an explicit, visible, single decision, categorically different from an automatic
carve-in.

### Manage capability: an allowlist (position + lifecycle only), never any peer config content

**Adversary anchor 2, addressed directly**: if `agent → agent, write` (manage: start/stop/restart/update/remove,
per §3e) is in the helper's wire set, "update" on a peer agent includes changing that peer's `system_prompt` —
and the operator's precise-prompt requirement constrains what the HELPER itself does, not content it might write
into someone else's config after inferring what the user "probably" wants that other agent's prompt to say. The
engine cannot verify "the helper copied the user's words verbatim into agent X's prompt" versus "the helper
paraphrased/extended them" — that is not a checkable property at the API boundary.

**Proposed resolution: the helper's `manage` wire is real but field-scoped, by an ALLOWLIST, not a denylist**
(PM's review — closing an ambiguity in an earlier draft of this section, which named only `system_prompt` as
refused and left every other config field's status implicit). Anchor 2's concern is not specific to prompts: a
helper inferring what it "probably" should write into a peer's `budget`, `workspaces`, `harness`, or any other
config field is the identical unverifiable-inference problem, just aimed at a different key. Naming prompts
alone and leaving the rest implicit would have reintroduced the same gap one field at a time as this ships.

**The allowlist, for a peer agent the helper does not own (did not itself `place`), is exactly two things:**
`position` (layout only — no semantic content) via `PATCH /v1/nodes/:id`, and the four pure-lifecycle POST
routes (`start`/`stop`/`restart`/`clear`, which carry no body the helper could inject content through at all).
**Everything else in `config` — `system_prompt`, `budget`, `workspaces`, `harness`, `model`,
`idle_timeout_secs`, `ephemeral_context`, `run_on_startup`, all of it — is refused (400) on a PATCH to a peer
agent the helper does not own,** enforced the same place `patch_node`'s existing merge-patch validation already
runs (`board_routes.rs`), as an additional check gated on "caller is a helper AND target is not
`owner_node == caller`". An ordinary agent's `write` wire to another agent is unaffected — this restriction is
specific to the helper role, because the helper is the only agent type this proposal grants blanket
manage-everything to, and blanket capability is exactly the case an allowlist (not a growing denylist) has to
bound. A helper managing an agent it PLACED ITSELF (via its own `place` call, this session) may still set that
new agent's FULL config at CREATION time — the risk the anchor names is rewriting an EXISTING peer's identity
mid-operation, not authoring a new one the user asked for.

### Placement: a real, unavoidable consent gate

**Adversary anchor 4, addressed directly**: same weight as the `board_apply.rs` consent-list pattern already in
the codebase (`c.grant.push("allow_patch"/"allow_wire")`, API-owned) — a UI dialog is Web's half, but the ENGINE
side must not be satisfiable by an unconfirmed request, or a scripted/automated client bypasses the dialog
entirely. Proposed: `POST /v1/nodes` for `AgentConfig.role: Helper` requires a body field `confirmed_global_access:
true` with **no default** (the field's absence is refused with 400, not treated as `false`) — this makes the
consent a server-side fact the request itself must carry, not a client-side nicety that only the UI happens to
enforce. Web's dialog sets this after showing the real, current wire-count-if-placed-now number, not a generic
warning string.

### Rate limiting the helper's own place/grant calls

**Adversary anchor 5, addressed directly**: this is a distinct risk from `manage` and from the per-project node
cap (§3e default 50, which bounds TOTAL nodes ever created, not the RATE — a misprompted helper could exhaust it
in one turn). Proposed: reuse the exact shape of `api/ingress.rs::RateLimiter` (`ingress.rs:61-84`, a
sliding-window counter that already exists and is already tested), keyed by the calling node's id instead of an
IP, applied specifically to the helper's `place`/`grant` tool calls with a fixed, conservative default (proposed:
10 place/grant calls per 60s — generous for "build me a three-node pipeline," tight enough that a misprompted
loop hits the ceiling in the same turn it started misbehaving rather than the same project run). Not
configurable in v1, matching the ingress limiter's own precedent of a fixed constant rather than a per-node
knob a compromised agent could raise on itself.

### One discriminant field, not a template

**Brief's open question, answered**: a template (task 3) only pre-wires what existed AT INSTANTIATION — it
cannot satisfy "auto-covered as nodes are added after it's placed," which is the actual hard requirement here.
This has to be a real, first-class engine behavior triggered by the node's own config, so: a new
`AgentConfig.role: Option<AgentRole>` field (`AgentRole::Helper` the only variant today, headroom for a future
second board-provided role without another top-level `AgentConfig` field), defaulting to `None` — an ordinary
agent's config is one field longer and otherwise unchanged; every existing test and board is unaffected.

### The system prompt itself

The operator's ruling is that this is load-bearing, not descriptive — so here is the actual text, prepended
(never appended, so a user-edited `system_prompt` cannot push it out of the model's attention window) whenever
`role: Helper` is set, by `wheel_core::compose_system_prompt` (`preamble.rs:135`) before the node's own
`system_prompt`:

```
You are a board-building assistant with broad read/write access to this project's nodes and wires. This access
exists so you can act on explicit instructions without per-node wiring — it is not permission to act beyond
what the current instruction asks.

Rules, in order of priority:
1. Do exactly what this turn's message asks. Nothing more.
2. If the request is ambiguous about scope (which nodes, how many, what config), ask rather than guess broader.
3. Never take an action the user did not ask for in THIS turn, even if it looks like an obvious next step, a
   related cleanup, or something you assume they would also want. If you believe an additional action would
   help, name it and stop — do not perform it.
4. Never modify, wire, rename, or remove a node the current instruction does not name or clearly identify.
5. Prefer the smallest set of tool calls that satisfies the instruction over a compound or batch operation you
   would have to infer sub-steps for.

You will be given a new instruction for anything further. Finishing early and reporting what you did is always
correct; finishing more than asked is not.
```

This is the generation RULE (a fixed block, not templated per-project) — every helper on every board gets the
identical text, so a security review of this prompt reviews every helper that will ever exist, not one instance
of it.

## Open design questions this proposal is taking a position on

- **Project-level flag vs. real auto-generated wires?** Real wires, kept live via create-time hooks — settled
  above (anchor 1).
- **Does write-access extend to other agents (manage)?** Yes, but bounded by an ALLOWLIST — `position` plus the
  four lifecycle routes only, on a peer the helper does not own; every other config field is refused, not only
  `system_prompt` — settled above (anchor 2).
- **One node type/kind, or a template?** A discriminant field (`AgentConfig.role`), not a template — templates
  cannot auto-cover future nodes — settled above.
- **How is it kept from being the most dangerous node on the board?** The sum of: real per-wire revocability
  (anchor 1), a hard-coded vault exclusion (anchor 3), field-scoped manage (anchor 2), a server-enforced consent
  gate (anchor 4), a rate limit on its own place/grant calls (anchor 5), and a fixed, auditable system prompt.
  None of these alone is sufficient; the proposal does not claim any single one is.

## Non-goals

- Not building the FULL §3e surface speculatively — only enough of place/grant/manage for the helper's own tool
  surface to function. A general-purpose `may_place` for arbitrary agents is real future work this proposal's
  primitives make easier, not something this PR set needs to finish.
- No cross-project helper capability — a helper's blanket access is this project's nodes only; nothing here
  interacts with the portals proposal.
- No per-project configurability of the rate limit or the system prompt text in v1 — both are fixed, matching
  this codebase's existing precedent (`ingress.rs`'s constants) for controls where a compromised or misprompted
  agent must not be able to raise its own ceiling.

## Owners

SDK: the generic place/grant/manage primitives (phase A), the helper role field + auto-wire hooks + vault
exclusion + field-scoped manage + the rate limiter + the fixed system prompt (phase B). Web: placement flow
with the real consent dialog (showing the actual node/wire count this will create), a distinct "global access"
badge wherever a helper appears on the board, and a revoke UI that lists its wires individually (not a single
"revoke everything" action, so a partial revoke — e.g. keep manage, drop write-to-a-specific-table — stays
possible). API: none identified as new surface; placement still goes through the existing project API's node
creation path.
