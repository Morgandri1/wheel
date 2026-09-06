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

## Link 5, RE-VERIFIED (PM reconciled it to "ctx reaches the sink via envelope-at-injection"; the code disagrees)
PM pulled the production killing bytes from a CTX node's markdown (nodes table) and reasoned ctx "is injected
into an agent as a message, so it passes through envelope()." I traced the actual delivery, caller-graph
first, and it does NOT:
- `compose_prompt`/`compose_preamble` (preamble.rs) is concat-only — `push_str`/`format!`, no `escape_*`, no
  `envelope`.
- The composed system prompt + ctx is written to `prompt.txt` (supervisor:394-395, plain `std::fs::write`,
  "goes to a file, never argv") and passed to the harness as a FILE arg (:418). `clear_context`
  re-injection (`:889`) calls `start()` → the same file. Never a Message.
- Caller graph of the sink: `escape_envelope_body` ← `Message::envelope()` (message.rs:277) ←
  `encode_turn(&msg.envelope())` at supervisor:545 — the message-DELIVERY loop, and nothing else. No
  ctx/preamble path builds a Message or calls `envelope()`.
**Therefore a poison in a ctx node reaches the child through a FILE, never through the escaper, and cannot
panic at boot.** Link 5 stays NOT-A-LINK. The bytes being present in a ctx node is presence, not the panic
carrier: the same contract text (`— `) is pasted across messages/ctx alike, and the carrier that reconcile
replayed to death was a MESSAGE ROW (link 4), whose body IS delivered through `envelope()`. PM's SINK-LEVEL
quarantine framing is correct and better than a message-table rule — "no content that becomes a message body
may crash a project" — but the SET of "content that becomes a message body" is wheel-msg bodies, operator
chat, and (future) ingress bodies; it does NOT include ctx today. If a reproducing test drives ctx→envelope,
that path is not in the running engine — I want to see it; otherwise the guard belongs on the message-delivery
path + the messages table, and treating ctx as the carrier would harden a surface the escaper never reads.
(Method note: I corrected link 5 to false, weighed PM's reconciliation, and re-verified via the caller graph
rather than accept or reject on authority — the 030 standard, applied to a correction of my own correction.)

## Link 6 — first real consumer named: a TELEGRAM webhook → the PM agent (pin before it ships)
The operator has named the first ingress consumer: a Telegram webhook delivering into the PM agent. This
makes link 6 concrete the day it ships, and it is the SHARPEST instance of endpoint-R4 (finding 031): the PM
agent is (by role) among the most wire-capable agents on the board — send to peers, write to ctx/plans — so a
public webhook body is an internet → most-capable-agent channel. Two overlapping risks on the SAME path: the
034/035 panic sink (a poison body → the PM agent's message → envelope()), and R4 prompt-injection (attacker
text steering the PM agent within its broad wire set). Pre-ship requirements (verify when SDK's ingress lands):
1. The webhook MUST be authenticated, NOT `mode:none`. Telegram supports a `secret_token`
   (`X-Telegram-Bot-Api-Secret-Token` header set at setWebhook) — use `shared_secret`/`hmac` against a wired
   vault (031 R6). A `mode:none` webhook to the PM agent is an open internet → PM-agent injection channel.
2. Verify link 6 itself: does the ingress body reach `envelope()` (the fixed sink)? If yes, the panic is
   defused by the 034 fix — I will confirm reached-and-safe against SDK's code rather than assume.
3. R3: cap the raw body before it becomes a message (≤256 KiB), rate-limit (project, trusted-IP not raw XFF),
   both BEFORE the PM agent is woken. R1: Telegram has no per-update nonce the engine validates by default →
   the PM agent's actions on a webhook must be idempotent or dedup on Telegram's `update_id`.
4. R4: the PM agent's preamble must flag `type=endpoint` bodies as UNTRUSTED EXTERNAL INPUT; and given its
   blast radius, prefer routing the webhook to a MINIMAL-wire intake agent that relays to PM, not straight
   into the agent that can write plans and message everyone.
I will run the full endpoint/ingress live campaign (SSRF is N/A for inbound, but auth-bypass, poison-body →
sink, size/rate/replay, and the R4 injection blast-radius) against this path the moment the ingress code lands.

## Link 6 — RESOLVED: ingress landed, VERIFIED reached-and-safe. The chain is CLOSED.
`crates/wheel-engine/src/api/ingress.rs` is on main. I ran the campaign (source + tests). Verdict:
- **Link 6 is REACHED-AND-SAFE, not reasoned.** Ingress delivers ONLY through `messages::enqueue` →
  `Message::envelope` → the FIXED escaper (034), never formatting an envelope or calling the escaper itself.
  A source-grep regression test (`ingress_delivers_only_through_the_one_envelope_sink`) FAILS if a future edit
  hand-rolls delivery beside the sink. So the ingress body IS routed through the single sink (link 6 confirmed
  reached) and the 034 fix defuses the panic (confirmed safe). **The internet → permanently-dead-board chain is
  closed at the sink for ingress too** — links 1-4 fixed (034 escaper + `catch_unwind` quarantine), link 5 was
  never a link, link 6 reaches the fixed sink.
- Attribution correct (`type=endpoint` by construction, never `user`); secret constant-time-compared and
  redacted from the delivered body; forged `x-wheel-*` stripped at the API edge; body capped while reading;
  reject-before-waking-a-child ordering.
- **One residual, availability only:** no per-caller rate limit (the trusted client-IP header is never set) →
  **finding 039** (Medium). It cannot re-open the dead-board chain; it lets one abuser starve the legitimate
  Telegram→PM provider. Fix is in 039.
- Pre-ship reqs from the link-6 note still stand for the Telegram→PM consumer: Bearer not `None`, and dedup on
  Telegram `update_id` (ingress is provider-agnostic and does not parse Telegram, so replay protection is the
  consumer's job).
