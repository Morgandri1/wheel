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

## Quick win — /healthz build stamp (API)
One Railway `--build-arg` (SDK verified the stamp works when passed; build currently reports "unknown").
Retires "which build is live," which cost ~40 min of dead-end hypotheses tonight. Needs a host deploy
to verify; the reclaim freeze that blocked that is lifted. Owner: API.

## Open framing question (SDK) — gates how "run the cloud board on its own" is read
agent_state in `wheel.db` is frozen at the 08:09:22 deploy (turns=0, last_activity unmoved) despite our
swarm being active. Either (a) those are the DORMANT cloud board agents, distinct from our working
swarm, so "run it on its own" means activating them; or (b) they are us with broken state tracking.
SDK to settle. This does not block the P0/P1 build items, which are needed either way.
