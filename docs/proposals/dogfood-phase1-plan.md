# Phase 1 dogfooding plan — prioritized, owned, gated (PM, 2026-09-07)

Synthesized from three grounded reads at `4f619c5`: Web (UI operability), SDK (engine/script),
API (`docs/proposals/dogfood-api-gaps.md`). Two "blockers" were retired on inspection, not built:
agent-state-stuck-`starting` (already fixed in deployed `0b81bc0`; agents park correctly; pm's NULL
session is affirmative proof the ephemeral fix works in prod) and turns=0 (correct value, not a gap —
unmeasured until a turn completes on a cloud agent). Comms discipline: assignments carry a SHA; git
only. Every item lands with ADVERSARY review + QA >=90% per standing rules.

## P0 — PATCH silently deletes agent config (destructive, blocks safe editing)
`patch_node` REPLACES `node.config` wholesale (API §3, proven through wheel-core): a UI that PATCHes
only the fields it has controls for erases the ones it does not — workspaces, budget, idle_timeout ->
None, run_on_startup true->false, 200 and no warning. Dogfooding IS editing agents in the UI, so this
is actively destructive on the core path. FIX (SDK owns the handler decision, Web owns the caller):
either PATCH gains merge semantics, or the UI does read-modify-write and sends the whole config back.
One or the other MUST land before any new agent-config control ships. Not both blindly — SDK rules the
handler, Web conforms. This ranks above the missing controls because it breaks agents that already work.

**RULED (SDK, 2026-09-07): PATCH gains MERGE semantics.** The handler merges the incoming partial
config into the stored config, then deserialises and VALIDATES the whole result before storing — an
invalid merge is a 400 naming the field, never a stored half-config; `type` stays immutable. Handler-level,
so it protects every caller by construction. Web builds the panel against merge now. SDK lands the handler
before any new agent-config control ships. SEQUENCING NOTE: if the DEPLOYED save currently sends a partial
PATCH (Web checking), the merge handler once DEPLOYED is the fix for a LIVE destructive bug, not just a gate
on new controls — so it ships promptly, not merely before-new-controls.

## P1 — Script execution (the keystone capability gap)
No engine runtime (SDK) AND no control-plane route to invoke it (API §4.3: `wheel run` is CLI-plane
only). SDK is scoping the runtime in docs/proposals/ (concurrency-cap + shared-store levers are
REQUIREMENTS; shared-store pre-satisfies part of the Phase-2 optimization directive). API adds the
control-plane route so a script is runnable from the board, not just the CLI. Acceptance: ADVERSARY's
saved egress PoC (redteam/pocs/egress/) proves a raw script cannot reach the private network;
MAX_SCRIPT_OUTPUT_BYTES enforced; QA >=90%. Owners: SDK (runtime+route contract) + API (control-plane
exposure) + ADVERSARY (egress gate) + QA.

## P1 — Agent-creation UI: workspaces / budget / idle_timeout (Web)
API already exposes the writes (API §2); this is purely a Web UI task — add the three controls so a
working wheel-dev-style agent can be CREATED in the UI, not only edited. GATED on P0: the create/edit
panel must read-modify-write (or rely on merge PATCH) so it cannot trip the silent-delete trap. Owner: Web.

## P2 — Chest is invisible from the board (API §4.1)
No control-plane routes for chest ls/get/put; access is per-node-token CLI-plane only, and the proxy
injects the engine secret, so there is no operator-authenticated way in. To dogfood Chest, add
control-plane chest routes WITHOUT collapsing the per-node-token boundary into the engine-secret layer
— a security-sensitive design. Owners: API + SDK, ADVERSARY on the auth boundary. Table-row write from
the board (API §4.2) rides the same decision; lower urgency (agents write via CLI plane already).

## Quick win — /healthz build stamp (API) — RETIRED, NOT FIXABLE AT THIS LAYER
API falsified this by checking both mechanisms (SHA 17398c5): (1) Railway's service-settings GraphQL type
(ServiceInstanceUpdateInput) has NO build-args field — a platform limit, not our schema; (2) the workaround
`GIT_SHA=${{RAILWAY_GIT_COMMIT_SHA}}` resolves to the EMPTY STRING (measured), and baking that is a TRAP —
`build:""` reads as a bug, worse than `"unknown"` which declines honestly. Tested reversibly with
--skip-deploys, confirmed empty, deleted; production never built with it. `build:"unknown"` is the END
STATE. Confirm "which build is live" by verifying a deploy actually REBUILT (railway deployment commit meta
+ rebuild), NEVER a runtime stamp — the runtime value names the commit that TRIGGERED the deploy, not the
one the binaries compiled from (false confirm, SDK rejected it). Lesson: the plausible 5-line fix was
impossible at the layer; check, do not reason from docs. Both docs (docs/API.md, infra/railway/README.md)
now state build:"unknown" is final.
## Open framing question (SDK) — gates how "run the cloud board on its own" is read
agent_state in `wheel.db` is frozen at the 08:09:22 deploy (turns=0, last_activity unmoved) despite our
swarm being active. Either (a) those are the DORMANT cloud board agents, distinct from our working
swarm, so "run it on its own" means activating them; or (b) they are us with broken state tracking.
SDK to settle. This does not block the P0/P1 build items, which are needed either way.

## REFRAME (SDK, 2026-09-07) — "run on its own" = move the loop, not polish what exists

The wheel.db agents are the DORMANT cloud board — a mirror of us that is switched off (answer (a)).
We do the work on yoke, on a laptop; the board does nothing on its own behalf. So "run the cloud board
on its own" is not a polish task on top of what exists — it is MOVING the development loop from this
swarm to that board. Script execution is the keystone rather than a feature precisely because those
agents cannot run anything on their own behalf and their preamble promises a capability that does not
exist.

The consequence, and the strategic pivot: every gap we find by speculating from outside is a gap WE do
not feel, because we are not the ones living on the board. Real friction is found by living on it, not by
building against a guess. Therefore the priority shifts from "build the capabilities first, then dogfood"
to "wake the board sooner on real work, and let the first thing that breaks drive the build order."

Minimum-to-wake is small: the cloud agents can already clone/edit/commit/push via their harness (the
wheel-on-wheel CI proves the flow). The one prerequisite that must not be skipped is the P0 merge-PATCH
fix, so activating them and editing their configs cannot silently destroy config. Beyond that, we wake
and observe rather than pre-build Script/Chest/UI speculatively.

OPERATOR-DOMAIN DECISION (not PM's to take alone): actually activating the cloud board to self-drive
re-raises the duplication the operator halted earlier ("doing the same job as you"). The resolution is
to MOVE the loop, not run both — this swarm's role becomes bootstrapping the board to make itself
unnecessary. Pending the operator's go on that flip.

## DECISION (operator, 2026-09-07): INCREMENTAL WAKE

Move the loop to the board dogfood-driven, not build-first. After the merge-PATCH prereq: wake ONE cloud
agent on one small real task, observe what breaks, let that drive the build order — iterate agent by agent.
The laptop swarm stays primary meanwhile, so duplication is bounded to one agent/task and the step is
reversible. The first wake doubles as SDK's turn-completion discriminator (consumed? left running? "could
not record spend" in log? turns increment?), so it settles the frozen-state question with a live turn.

## PRIORITY (operator, 2026-09-07): CODEX IS NOT A LAUNCH GATE
Launch and dogfood are Claude-only. Codex completeness does not gate anything.
- The codex GUARD (honest refusal of a codex node, live + behaviour-verified) is the CORRECT launch state —
  it requires codex to not silently run, not to work. Keep it.
- Building the codex DRIVER is M2/post-launch, deprioritised, never a launch gate. No engine effort on a
  codex driver before launch.
- Nothing about codex blocks the wake, the dogfood, or launch. The real not-yet-proven items remain
  BUG-037 (turn/spend accounting), --resume success, and BUG-036 redelivery — codex is not among them.
