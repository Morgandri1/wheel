# 039 — Public ingress has no per-caller rate limit: one abusive IP starves the legitimate webhook (e.g. Telegram→PM)

- **Severity:** Medium (availability). The auth, poison-safety, attribution and secret handling of ingress are
  all SOUND (see the link-6 verdict below); this is the one real gap. Owner: API (set/forward the client IP) +
  SDK/Engine (the engine limiter is inert without it). Boundary TB1/TB5 (public internet → sandbox). Timely: the
  Telegram→PM webhook (035 link 6) is the first real consumer, and this is exactly "my phone can't reach my
  board because someone is spamming the public URL."
- **Status:** CONFIRMED by source trace across all three layers (API, host, engine). Live-repro stageable once
  the stack is up; the defect is structural (a header that is trusted but never set) and provable from code.

## What I found — two rate limiters, BOTH per-project, NONE per-caller
1. **API layer** (`crates/wheel-api/src/routes/ingress.rs:52`): `ingress_limiter.check(&db, &project_id)` —
   counted in Postgres (holds across replicas, good) but keyed **only on `project_id`**. No client identifier.
2. **Engine layer** (`crates/wheel-engine/src/api/ingress.rs:139-151`): a per-caller `RateLimiter` keyed on the
   `x-wheel-client-ip` header, which the module documents (lines 49-55) as "set by the host after it has seen
   the peer address" and deliberately NOT `X-Forwarded-For` (because a client can forge XFF). Correct design —
   **except no layer ever sets `x-wheel-client-ip`.** A whole-repo grep finds it only in the engine's own read
   and comments; nothing writes it. The API's ingress path calls `hop::sanitize_for_upstream(headers, ["x-wheel-"])`
   (ingress.rs:63), which STRIPS every inbound `x-wheel-*` (so a client cannot forge it — verified, the
   `ingress_drops_forged_wheel_headers` test proves it) and then adds only `x-wheel-ingress: 1` — never the
   client IP. The API never even captures the peer address (no `ConnectInfo`).

So the engine limiter always takes its `unwrap_or_else(|| "unattributed")` branch (ingress.rs:143-148): **one
shared bucket for the entire project's public URL.** Both limiters are per-project; per-caller limiting exists
in the design but is dead in production.

## Impact
- **DoS of the legitimate provider.** `RATE_LIMIT=60 / 60s` (engine) and the API's per-project budget are
  SHARED across all callers. One attacker at ~1 req/s exhausts the project's budget; the legitimate Telegram
  callback then gets 429/limited. The operator's phone-to-board channel goes dark while an attacker spends
  nothing but a loop. The module's own stated purpose — "the cost control on a public URL" — is met for total
  cost but fails at its real job: it cannot tell an abuser from the provider, so it lets the abuser starve the
  provider.
- **Rate limit runs before auth (correctly), so a Bearer endpoint is as DoS-able as a None one.** Steps are
  route → rate-limit → body → auth (ingress.rs:104-166). That ordering is right (don't do a vault read for an
  unauthenticated flood), but combined with the shared bucket it means unauthenticated wrong-secret noise on the
  Telegram endpoint's path consumes the same budget the authenticated provider needs. Auth does not protect
  availability here.
- Not a bypass or a spoof: the shared bucket still caps TOTAL project cost, and forged `x-wheel-*` is stripped,
  so an attacker cannot mint fresh identities to exceed the total. The harm is purely that a good caller and a
  bad caller are indistinguishable to the limiter.

## Fix
The API is the public edge (api.wheel.dev); it is the only layer that sees the real peer. It should:
1. Capture the peer IP (`ConnectInfo<SocketAddr>`, or a SINGLE trusted upstream-proxy hop's XFF if Vercel/an LB
   fronts it — decide which and document it), and set `x-wheel-client-ip: <peer>` AFTER the `x-wheel-` strip, so
   the value the engine trusts is one the API actually vouches for and a client cannot forge (the strip already
   guarantees the latter).
2. Additionally key the API's own Postgres limiter on `(project_id, client_ip)` (with a per-project ceiling on
   top), so per-caller limiting holds across replicas too — the engine's in-memory per-caller limiter is
   per-replica/per-engine and is a second line, not the primary.
Once the header is set, the engine limiter (already written and tested) starts doing its job with no engine
change. Without step 1 the engine's `x-wheel-client-ip` code is dead.

## Verify after mitigation
- A flood from IP A (past the budget) returns 429 to A while a concurrent hit from IP B (the "provider")
  succeeds — the exact property the engine's own `the_rate_limiter_stops_a_caller_past_the_window_budget` test
  asserts in isolation, now true end-to-end through the API.
- A client sending `x-wheel-client-ip: 9.9.9.9` still gets keyed on its real peer (the forged one is stripped).

## Note (already in 031, restated for the Telegram consumer)
Auth `None` is a valid config but must NOT be used for the Telegram→PM endpoint (use Bearer via the Telegram
`x-telegram-bot-api-secret-token`, which the engine's `authenticate` already accepts). And replay/idempotency is
the consumer's job — ingress is provider-agnostic by design and does not parse Telegram, so the PM agent must
dedup on Telegram's `update_id`.
