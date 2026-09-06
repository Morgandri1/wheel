# 021 — Poisoned/session expiry on a SHARED vault strands peer agents — real blast radius, NOT agent-reachable

- **Severity:** Low (operator foot-gun / availability). **No agent-reachable vulnerability.** One hardening rec.
- **Owner:** SDK/Engine (`api/agent_routes.rs save_credential_to_vault`, `supervisor lapsed_credential`,
  `vault.rs put_with_expiry/credential_detail`).
- **Status:** Source-verified @ HEAD (SDK's refined target 2 — the multi-agent surface). No live planting path
  exists for an untrusted agent, so no live PoC is possible from the agent threat model; the peer blast
  radius is confirmed in code.
- **Boundary:** TB5/TB6 (child↔engine, credential subsystem), multi-tenant-within-project (peers on a board).

## The question (SDK)
Can a poisoned/expiring credential on a SHARED vault strand PEER agents that are otherwise fine, and can the
freshness-floor-free `auth_status` be turned into peer impact?

## Blast radius — REAL (code-confirmed)
`supervisor::lapsed_credential(agent)` → `vault::credential_detail(agent)` finds the vault the agent reads
that supplies the recognised credential key and returns its expiry; if `expires_at <= now`, that agent
refuses to start (`needs_auth`, naming the vault). This runs PER AGENT against the SHARED vault, so an
expired credential in a vault read by N agents strands ALL N — not just the one that authed. Confirmed.

## But NOT agent-reachable — the planting path is operator-only
The only routes that can set a vault credential's expiry:
- `save_credential_to_vault` (from `auth/complete` with a session/paste-code credential) — stores the
  credential's own parsed expiry (`agent_routes.rs:696` `found.expires_at.and_then(millis_to_timestamp)`).
- `PUT /v1/vault/:id/:key` — its body (`PutValue`) has **only `value`, no `expires_at`** → stored with
  `expires_at=None` (durable-treated). So PUT cannot plant an expiry at all.
Both `auth/complete` and `PUT` are the **engine-secret / host realm** (operator, via API→host→engine).
**Agents have NO vault write** — the wire matrix makes agent→vault read-only, and there is no `/v1/cli`
vault-write route. So an untrusted board agent cannot plant a poisoned expiry on any vault, shared or not.
The multi-agent expiry DoS is therefore outside the agent threat model.

`millis_to_timestamp` uses i128 math + range-checked `from_unix_timestamp_nanos(..).ok()` → an absurd/
overflowing millis value yields `None` (treated as no-expiry), never a panic. A past value → self-`needs_auth`
(fail-closed, recoverable). Confirmed with SDK.

## auth_status has no peer impact
`auth_status` reads the agent's OWN credential store with no freshness floor — for DISPLAY only. The spawn
gate (`lapsed_credential`) reads the VAULT, not the store. So an agent lying about its own store expiry
misleads the UI about itself and nothing else; it cannot change a peer's start decision (peers gate on the
vault). The two paths are separate — no finding.

## Hardening recommendation (Low — the operator foot-gun)
`save_credential_to_vault` for the paste-code/session path stores a SESSION credential's real expiry into
the target vault and only adds a `warning` when `!is_long_lived`. The `setup_token` path already REFUSES a
non-durable credential precisely so it "will not expire underneath five other agents." The same rationale
applies to `save_to_vault` when the target vault is SHARED (has readers other than the caller): a warning is
easy to miss and the blast radius is every peer. Recommend: when `save_to_vault` targets a vault with any
reader other than the caller, REFUSE a non-durable/session credential (or require an explicit override),
mirroring the setup_token stance — closing the multi-agent-strand foot-gun by construction rather than by an
ignorable warning. Not a vulnerability; a sharp edge on the multi-agent surface.
