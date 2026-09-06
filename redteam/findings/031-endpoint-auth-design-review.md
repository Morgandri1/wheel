# 031 — DESIGN REVIEW: endpoint auth (none | shared_secret | hmac), the public-ingress → agent surface

- **Type:** Red-team design review (PM requested; SDK builds to this). Spec: docs/ARCHITECTURE.md "Endpoint
  auth" @ f13a7c2. Owner: SDK/Engine (ingress → endpoint node → auth → message delivery). Boundary TB1/TB5.
- **Verdict:** The shape is sound — optional auth, secret in a WIRED vault (never inline / never in board
  JSON, events, export), `ip_allow` additive-not-substitute, bare-401. **Approve the shape**, with the
  REQUIREMENTS below; three are load-bearing (R1 replay/timestamp, R3 size-before-HMAC, R4 the mode:none →
  wheel-write-agent injection path). Working from the spec as received (the invariants line was truncated at
  "Invariants reg…"); if the full invariants already cover R2/R5/R6 mark them done.

## R1 — Replay (LOAD-BEARING)
`shared_secret` is a static credential → **every request is replayable**; the secret in header/query/path
does not bind the request. `hmac` is replay-resistant ONLY if the provider's TIMESTAMP is validated:
- `stripe` (`t=…,v1=HMAC(t.body)`) and `slack` (`v0:ts:body` + `x-slack-request-timestamp`) carry a timestamp
  → the engine MUST verify HMAC over the timestamped base string AND reject a timestamp outside a tolerance
  window (Stripe/Slack use ~5 min). If it only checks `HMAC(body)`, both replay freely.
- `github` (`x-hub-signature-256 = HMAC(body)`) has NO timestamp → inherently replayable; GitHub relies on the
  app deduping `X-GitHub-Delivery`.
- `hmac_sha256` (raw) — body only, replayable.
**Require:** for the timestamped schemes, verify and window the timestamp (a signature is not enough). For the
rest, the delivery is replayable by design → an endpoint→agent that MUTATES (wheel write) must be idempotent,
or add an optional idempotency/delivery-id dedup. Document this next to `scheme` so an operator wiring a
mutating agent to a github/shared_secret endpoint knows a replay re-fires it.

## R2 — Timing
Every secret/signature comparison MUST be constant-time: `shared_secret` value compare, and the HMAC
compare (computed vs presented). A non-constant-time `==` leaks the secret/signature byte-by-byte over many
requests (the endpoint is public and unrate-limited-per-guess unless R3 holds). Reuse the API's
`constant_time_eq`. (Likely already an invariant — confirm it covers the HMAC compare, not just shared_secret.)

## R3 — Size / rate, BEFORE the HMAC and BEFORE waking the agent (LOAD-BEARING)
`hmac` is computed "over the RAW body before any parsing" → the engine must BUFFER the raw body to sign it.
**Cap the raw body BEFORE buffering/HMAC** (stream to a ceiling, reject over it) or a large body is a memory
DoS that costs nothing to send and does not even need a valid signature. The cap must also be ≤ the
message-body limit (§3c#6, 256 KiB) since the body becomes a delivered message. Rate limit + size cap must be
enforced **before the agent is woken** (waking an agent is the expensive/attackable step, and on a public URL
the limit is the only cost control). Rate-limit key = (project, trusted-client-IP) — NOT raw XFF (Railway
lets a client prepend it; the auth-review R4 lesson). ip_allow narrows but never replaces the limit.

## R4 — Prompt injection from the body into a wheel-write agent (LOAD-BEARING, and the mode:none sharp edge)
The ingress body is attacker-controlled and is delivered into the wired agent's context as
`type=endpoint`. Auth changes WHO can inject (a secret-holder / valid-signature sender), not that the body is
untrusted content. Two things:
1. **`mode:none` (public, the default) wired `send`→ a wheel-write agent = an open internet→agent
   prompt-injection channel.** Anyone on the internet can put text into that agent's turn, and the agent can
   then `wheel write` to any node it is wired to (ctx/table), `wheel msg` any agent it can send to, and
   `wheel secret get` any vault it reads. The blast radius is the agent's wire set. This must not be
   create-blocked (operator's choice, per your ruling) but it MUST be **loud**: the UI has to mark an agent
   reachable from a public/unauthenticated endpoint as internet-exposed, and the preamble must flag
   `type=endpoint` bodies as UNTRUSTED EXTERNAL INPUT (the envelope already prevents attribution-forgery; it
   does nothing about content).
2. Recommend the docs steer ingress→agent wiring to a MINIMAL-wire agent (no vault read, no send to a
   secret-holding peer) unless the operator accepts the exposure. This is the save_to_vault-class leverage
   applied to ingress: the endpoint's real capability is the union of its agent's wires.
The envelope framing is necessary but NOT sufficient here; it is the wire set that bounds the damage.

## R5 — Endpoint name attacker-visible
The path is inherently visible (the caller hits `/p/<project uuid>/<path>`); the project id is a uuid (not
enumerable). The endpoint NODE NAME (the `from=<endpoint name>` in the envelope) is agent-side and must NOT
leak to the caller. A failed auth must be a **bare 401 with no body** that never says which part failed or
whether the endpoint/path exists (avoids an oracle for enumerating paths/names/config). Low, but pin the
bare-401 invariant and "no node name in any ingress response/error."

## R6 — Stolen secret + rotation
A stolen `shared_secret` (or a leaked hmac `secret_key`) lets the attacker invoke/forge → blast radius = the
agent's wire capabilities (R4). Rotation is clean because the secret lives in a WIRED VAULT: `PUT /v1/vault`
overwrites → the old value is immediately invalid (no lingering grace token). Two recommendations: (a) a
DISTINCT `secret_key` per endpoint, so rotating one does not silently break others sharing a vault key; (b)
since there is no key-versioning, document that rotation is atomic-swap (the sender must update at the same
moment) — acceptable for v1. Confirm the presented credential never lands in the delivered message,
transcript, or log (your invariant) — including the hmac signature header and any `Authorization`/query
secret; strip them from the `{method, path, headers, body}` delivered to the agent.

## R7 — Is `ack`-with-no-send genuinely safe?
Yes, for the case that literally means it: an endpoint with **response_mode `ack` AND no outgoing wire** is
inert — it returns a bare 200 and delivers to no one, a safe public health/ping surface. But `ack` controls
only the RESPONSE, not delivery: an `ack` endpoint that DOES have a `send` wire to an agent still delivers the
body (R4 applies in full — the caller just doesn't see the agent's output). So the safety is in the WIRE, not
in `response_mode`. Pin the wording: "ack" ≠ "no-send"; the inert-safe case is "no outgoing wire."

## Summary
Approve the auth shape. Build to: R1 (timestamp-window the hmac schemes; idempotency note for the rest),
R2 (constant-time incl. HMAC), R3 (cap raw body before HMAC + rate/size before waking the agent, IP not from
raw XFF), R4 (loud UI + preamble for public/unauth → wheel-write agent; minimal-wire ingress agents), R5 (bare
401, no node name leak), R6 (per-endpoint secret; strip the presented credential from the delivered message).
The one I would not ship without is R3+R4 together: a public `mode:none` endpoint on an unbounded body wired
to a capable agent is a no-cost internet→board injection-and-DoS primitive.
