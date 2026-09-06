# 035 — Chain: (internet →) agent/operator → poison message → escaper panic → permanently dead board

- **Severity:** High. TODAY: an AGENT (or the operator) can permanently dead-board its own project with one
  message — verified, agent-reachable, survives every restart. WHEN ENDPOINT INGRESS LANDS: the same sink is
  reachable from an unauthenticated public HTTP body, making it internet → permanently-dead-board. Owner:
  SDK/Engine (`message.rs` escaper — the single sink; and the endpoint→agent delivery being built now).
  Boundary TB5. Builds on finding 034 (the panic) and 032 (per-project blast radius).
- **PM asked me to say plainly which links I VERIFIED vs REASONED — as with 030, where a High rested on an
  untraced default. I did; one of PM's posited links (the ctx node) is CORRECTED to false below.**

## The sink (one place; fixing it breaks every variant)
`escape_envelope_body` (message.rs:183-214) panics on a `<`/`</` followed by ~4+ multi-byte chars straddling
byte `name_at + TAG.len()` (finding 034). **VERIFIED** in source + PoC `redteam/pocs/poison/t_escape_bytepanic.mjs`.

## The chain, link by link (VERIFIED / REASONED / CORRECTED)
1. **Sink panics** — VERIFIED (034).
2. **A delivered message runs the sink** — VERIFIED. `Message::envelope()` (message.rs:266) calls
   `escape_envelope_body(&self.body)` (:277); the supervisor delivers a turn via
   `self.harness.encode_turn(&msg.envelope())` (supervisor/mod.rs:545). So the panic fires when the message is
   written to the child — at delivery.
3. **The poison message is persisted and re-delivered across restarts** — VERIFIED/OBSERVED. Messages persist
   in sqlite; the queue/reconcile re-delivers on start (034); the operator's board went down *permanently*
   today, which is the persistence+replay observed in production. So the DoS is permanent, not transient.
4. **An AGENT — not only the operator — can author it** — VERIFIED. `wheel msg <peer> "<————"` over an
   agent→agent `send` wire stores the raw body; delivery (link 2) escapes it → panic. The operator chat box is
   the other author (what fired today); an agent is an equal author. Blast radius per 032: the sender's own
   project's engine crash-loops permanently; other projects are fine (messages are per-project — an agent
   cannot message across projects).
5. **CORRECTED — the ctx-node variant does NOT reach a boot panic.** PM posited: an agent `wheel write`s the
   poison into a ctx node it is wired to, and it hits the escaper at boot. I traced it: `wheel write <ctx>`
   replaces the markdown, and the boot/spawn re-render is `preamble.rs:139-140`:
   `out.push_str(&format!("\n\n# Context: {name}\n{markdown}"))` — a PLAIN concat, no escaper, no byte-slice.
   `format!` on any `&str` cannot panic on char boundaries. So a poison stored in a ctx node is injected
   verbatim into the system prompt and does NOT panic the engine at boot. **This link is FALSE.** (A poison in
   ctx is still an attacker-controlled prompt-injection payload in the agent's context — a separate concern,
   endpoint R4 / prompt-injection, not a boot-panic DoS.)
6. **REASONED / FUTURE — the internet entry point.** The engine's ingress → endpoint → agent-message delivery
   is NOT built yet: only `EndpointConfig`/`EndpointAuth` (wheel-core) and the create-time validator
   (board.rs) exist; no engine handler turns an `/ingress/*` hit into a delivered message. SDK is building it
   now. It COMPLETES the chain iff the endpoint→agent delivery constructs a `Message` and delivers it through
   `envelope()` (contract §3: an ingress hit "is delivered as a message" with `from=<endpoint> type=endpoint`)
   — which routes the attacker-controlled body straight through the link-2 sink. With `mode:none` (public,
   default) that is UNAUTHENTICATED internet → the panic. I have NOT seen that code (it does not exist yet),
   so this link is reasoned from the contract, not verified.

## Net, stated plainly
- **Verified, exploitable NOW:** agent or operator → poison message → permanent per-project dead board.
  (Links 1-4 all verified/observed.)
- **Reasoned, arrives with endpoints:** unauthenticated internet body → the same sink, iff SDK builds the
  endpoint delivery on the `Message` envelope (link 6). If they do, and the sink is unfixed, it is a public
  internet → permanently-dead-board chain with no auth.
- **Corrected, NOT a link:** the ctx-node write does not cause a boot panic (link 5) — the preamble is a safe
  concat; I checked rather than assumed.

## Fix (the leverage is the sink)
Fix `escape_envelope_body` once (`char_indices()`/`is_char_boundary` guard, finding 034) and every carrier —
message, wheel msg, and the future ingress body — is safe at once, because they all funnel through
`envelope()` (link 2). Belt: apply finding 034's quarantine rule (a message whose body cannot be escaped is
skipped/dead-lettered, logged, and the engine reaches a serving state without it, every boot) so no single
stored body can crash-loop a project even if a new sink appears. And when SDK lands the endpoint delivery,
send me the code — I will verify link 6 (does the ingress body reach `envelope()`) rather than leave it
reasoned.
