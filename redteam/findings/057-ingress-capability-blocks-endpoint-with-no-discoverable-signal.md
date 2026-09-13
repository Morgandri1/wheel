# 057 — A correctly-configured endpoint node gives no signal that the PROJECT's ingress capability is what's actually blocking it

- **Severity:** Low. Fails safe (403, not exposure) — this is a discoverability/UX gap, not a
  vulnerability. Worth tracking anyway because the failure mode can nudge an operator toward a
  broader-than-intended fix.
- **Type:** Classification requested by api while documenting README task gaps (PR #100); surfaced as
  candidate #2 of two, api's own read (the gate itself is deliberate, not a wart) confirmed correct — this
  finding is specifically about the missing discoverability signal, not the gate's existence.
- **Owner:** API/Engine (board state) + Web (surfacing it in the UI), same split as finding 043's fix.
- **Status:** OPEN

## Claim
`PATCH /v1/projects/:id`'s `capabilities.http` is a PROJECT-level, deliberate opt-in gate ("so ingress is
never enabled by accident" — `routes/ingress.rs`'s own doc comment, confirmed by api) that is entirely
separate from an individual endpoint node's own `auth` config. An operator can configure an endpoint node
completely correctly — right path, right auth mode — and still get a `403` on every hit, because the
SEPARATE, project-wide switch is off. Nothing on the endpoint node's own state (`GET /v1/board`, the Web
inspector, or anywhere else reachable from looking AT that node) says so. Someone debugging the 403 has no
path from the node they're looking at to the actual cause.

## Why this is worth tracking despite being low severity
The gate itself is correct and I agree with api's classification: it fails SAFE (no ingress reachable
until explicitly turned on), which is the right default. But a confused operator debugging "why is my
correctly-configured endpoint still 403ing" who can't find the actual cause has one obvious escape hatch:
turn on `capabilities.http` for the whole project, since that's the only lever they can find. `capabilities`
is project-wide, not per-endpoint — flipping it to fix ONE endpoint makes EVERY endpoint node on that
project reachable from the public internet, including any the operator deliberately left inert for a
reason they're not thinking about in the moment they're debugging a different one. That is a real, if
indirect, path from "confusing UX" to "wider exposure than intended" — not a bug in the gate, a plausible
consequence of the gate being invisible from where the operator is looking.

## Recommendation
Not urgent, and #100 documenting the current behavior as-is is a reasonable stopgap. When there's room for
it: surface the project's `capabilities.http` state on the endpoint node itself, the same shape finding
043's fix already established for a different dangerous-combination case — either a field in `GET
/v1/board`'s per-node state (e.g. `ingress_enabled: bool`, computed from the project capability, present
only on endpoint nodes) or a Web inspector note ("this endpoint's own config is fine; ingress is currently
OFF for the whole project"). Keep it a read-only signal, not a shortcut to flip the project capability from
the endpoint panel — the two-step nature (fix the node, THEN separately decide about the whole project) is
itself part of why the gate exists.

## What would change my mind
If `capabilities.http` were per-endpoint rather than project-wide, this finding would mostly evaporate —
the "flip the wrong-sized switch" risk specifically depends on the capability's blast radius being bigger
than what the operator is trying to fix. Worth re-checking that assumption before prioritizing a fix, in
case a narrower capability model is already planned elsewhere.
