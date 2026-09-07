# 043 — An unauthenticated endpoint wired to an agent is an unwarned internet → capable-agent path (LIVE on wheel-dev)

- **Severity:** the SYSTEMIC gap (no warning on a dangerous-by-construction config) is **High**; the LIVE
  instance on wheel-dev (the `/telegram` endpoint at `auth:none` wired to the PM agent) is **Critical** and
  being mitigated now. Owner: Web (endpoint panel warning) + API/Engine (board-state flag at wire/create).
  Boundary TB1/TB5 (public internet → sandbox) fused with TB4 (037). Companion to 031 (endpoint R4), 035 (the
  internet→agent chain), 037 (the /proc crown-jewel leg — CONFIRMED), 002 (agent = untrusted RCE).
- **Status:** LIVE and DEMONSTRATED. PM proved it by accident: a plain curl from a laptop with NO auth headers
  hit the public API `/p/<project>/telegram`, returned 202, reached the PM agent, which replied to the operator
  over Telegram. That was the P0 verification; it was also an unauthenticated internet client injecting content
  into a `bypassPermissions` agent.

## The systemic gap (what PM asked to have said out loud)
`EndpointAuth::None` is `#[default]` — the operator's explicit, correct design: an endpoint must be trivially
usable as a webhook with any provider. Not proposing to change that. The gap is that NOTHING anywhere warns
that an `auth:none` endpoint carrying a `send` wire to an AGENT is an open internet → capable-agent channel.
The two safe-looking defaults compose into a dangerous one silently:
- endpoint auth defaults to `none` (fine alone — an endpoint with no consumer is inert), and
- an endpoint→agent `send` wire delivers every hit as a message to a `bypassPermissions` agent (fine alone — if
  the endpoint were authenticated),
- but `none` + `→agent` = "anyone who can reach the URL can put a turn into an agent that runs arbitrary code,"
  and no layer says so at the moment the wire is drawn.

## The full chain, now open end-to-end (not hypothesized)
1. Unauthenticated POST to `https://api.wheel.dev/p/<project_id>/telegram` → 202, delivered as a message to the
   PM agent. **Demonstrated** (PM's curl).
2. The PM agent is `bypassPermissions` and among the MOST-wired agents on the board (send to peers, write to
   ctx/plans). Attacker text in the body is a prompt-injection payload (031 R4).
3. Injected agent runs `cat /proc/<engine>/environ` → `WHEEL_ENGINE_SECRET` (wire-matrix bypass = control-plane
   godmode) + `WHEEL_VAULT_KEY` (decrypt every vault). **037's /proc leg is CONFIRMED by run** (same-uid sibling
   read; PoC in redteam/pocs/child-isolation/). Plus 037's other carriers (sibling child environ = vault values,
   wheel.db, node token files, creds dirs).
Net: one unauthenticated internet request begins a chain to total project compromise. This is exactly the
035 "internet → capable agent" shape, fused with 037, with link 1 now demonstrated OPEN.

## Likelihood nuance (stated precisely, not to soften it)
The URL carries `project_id`, a v4 UUID that is not enumerable (unknown project → 404, no oracle). So an
attacker must LEARN the URL. But the URL is shared with Telegram's infrastructure (setWebhook), and lives in
config, logs, referrers, and proxies — URL-secrecy is NOT an authentication boundary and leaks by design. So:
treat the channel as public for severity; likelihood is gated only by whether the UUID has leaked, which is a
matter of time and not a control. This tempers "arbitrary internet client" to "any client that has the URL" —
it does not make the channel authenticated.

## Rating (the question PM asked): open until CLOSED — "one action away" does not change it
A mitigation that is available but not applied is not a mitigation. Severity is the system's state now, not the
remediation effort. The exploit works now (demonstrated). And the "one operator action" is in fact four
all-or-nothing steps (secret→vault, endpoint→vault read wire, auth→Bearer{vault_ref}, setWebhook secret_token)
with a downtime failure mode if 1-3 land without 4 — awaiting a human. That makes "available" weaker, not
stronger. LIVE/Critical until the config is actually changed.

## Fix — three complementary breaks at different links; apply the fastest now
Any ONE of these breaks the chain, at a different link:
- **Endpoint Bearer (fastest, config-only, operator).** Set the `/telegram` endpoint to `Bearer{vault_ref}` with
  Telegram's `x-telegram-bot-api-secret-token` (the engine's `authenticate` already accepts it, constant-time).
  Breaks link 1. This is the quickest single close and should get the operator's immediate go-ahead.
- **#17 (scrub secrets from the engine environ).** Breaks the crown-jewel leg (link 3) — the agent still reads
  environ, but the two secrets are no longer there. Leaves 037's other five carriers.
- **Per-node uids (§2/F007).** Breaks the sibling read for ALL 037 carriers. The structural fix; stays top
  priority.
The SYSTEMIC fix (this finding): don't change the `none` default; WARN at the dangerous configuration —
- Web endpoint panel: when an endpoint has `auth:none` AND a `send` wire to an agent, show "this exposes
  <agent> to unauthenticated input from anyone who can reach the public URL."
- API/Engine: surface the same as a board-state flag at wire creation / on `GET /board`, so the warning exists
  even for boards built via the API or `wheel.toml`, not only the UI.

## Post-close remediation — ROTATE, and rotate the right things (order matters)
Closing the chain (Bearer / #17 / per-node uids) stops FUTURE reads; it does not un-leak what the open window
already exposed. So after the carrier is closed, rotate — but rotate the VALUES, not just the keys:
- **The vault VALUES (account credentials) are compromised, not merely the vault_key.** 037 item 2: vault values
  are exported into each child's env (mod.rs:531-533), so a same-uid sibling could read another child's environ
  and get the plaintext credential DIRECTLY; and `WHEEL_VAULT_KEY` decrypts the at-rest ciphertext. A new
  vault_key does NOT help a credential whose plaintext already leaked. So regenerate the actual account tokens
  (Anthropic OAuth, OpenAI/Codex key, any others). The GitHub PAT was already revoked — do the same for the rest.
- **`WHEEL_ENGINE_SECRET`: cheap** — change the value in the API's `project_secrets` and restart the engine; no
  data migration.
- **`WHEEL_VAULT_KEY`: expensive, as suspected** — it encrypts vault values at rest, so a true rotation is a
  decrypt-all-old / re-encrypt-all-new migration (both keys present, transactional) that likely does not exist
  yet. Pragmatic path for a small board: generate a new key, discard old ciphertext, operator re-PUTs each
  credential (values are write-only and operator-held anyway) — which COMBINES with rotating the values above
  into one operation and needs no migration code.
- **Order:** rotate AFTER the carrier is closed (specifically after #17 scrubs the two from environ), never
  before — rotating into a still-open exposure re-exposes the new secret immediately. Gate rotation on #17, not
  on per-node uids (which is far off and must not hold up rotation).

## Note
Credit to PM: insisting on CONFIRMING the endpoint's auth mode rather than assuming Bearer is what turned a
false "latent" rating into the true "live" one. The conditional was right; the fact was the other way.
