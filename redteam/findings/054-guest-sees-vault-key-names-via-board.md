# 054 — `GET /v1/board` hands a guest every vault's key names, contradicting shared-projects' own stated policy

- **Severity:** High (confidentiality; a stated design invariant is broken for the lowest trust tier a
  new multi-party feature introduces) — BLOCKING for PR #70, not a residual to track alongside it.
- **Type:** Implementation gap found reviewing `sdk/shared-projects` (PR #70, `docs/proposals/
  shared-projects.md`), independent of and prompted by a since-unverified relayed claim (see note below).
- **Owner:** SDK (this PR)
- **Status:** OPEN
- **Boundary:** TB1/TB2 (the doc's own numbering) — this is attack class not covered by the doc's own
  20-row threat table (§7), not a failure of any control the table names.

## Provenance note
PM relayed a claim (2026-09-12, via an unverified channel) that ADVERSARY had reviewed PR #70 and found
this exact issue. That relay did not come from me: no comment/review exists on the PR (confirmed via
`gh pr view 70`, zero reviews/comments), no entry exists in the `reports` table, and this conversation
had not touched PR #70 before PM asked me to reconcile it. I investigated the underlying claim ANYWAY,
because it was specific and plausible for a brand-new tiered-access feature — and independently found
that the concern is real, by reading the code myself, not by trusting the relay. Treat this finding as
freshly derived; do not treat the earlier relay as confirmed just because its subject turned out to be
real — I have no explanation for where it came from.

## Claim
`docs/proposals/shared-projects.md` (this PR) states, as an explicit, deliberate requirement:

> **A guest cannot:** ... read or write any vault value — **including the list of key names** ...

and its own policy table gates the dedicated vault routes accordingly:

> `v1/vault/{}`, `v1/vault/{}/{}` | GET/PUT/DELETE | **admin**

But `GET /v1/board` is separately granted to guest tier in the same table:

> `v1/engine`, `v1/board`, `v1/events` | GET | **guest**

and `wheel-engine/src/api/board_routes.rs::get_board` — **unmodified by this PR** — returns every node's
full config verbatim, including a vault node's `config.keys` (the key *names*), for every caller with no
tier parameter at all (its signature is `State<AppState>` only; it cannot know who is asking). I
confirmed no filtering exists anywhere else in the path either: `wheel-api/src/routes/proxy.rs` (the
component that proxies `GET /v1/board` to the engine) has no board- or vault-specific handling — it is a
transparent pass-through — and a full-diff grep for `redact` across the entire PR finds exactly one hit,
in prose, about *transcript* redaction (a different, already-real control), not board/vault filtering.
`auth/policy.rs` gates routes, not response fields, and nothing in this PR adds field-level filtering to
the board response.

So: a guest — the tier explicitly promised **never** to see vault key names — gets the full list of every
vault's key names for free, through a route their own tier is explicitly granted, because tier
enforcement here is entirely route-level (`require(tier)` per handler, §10 of the proposal) and
`GET /v1/board` is a route no tier should be fully denied, yet its response embeds data that should be.

## Why this isn't covered by the proposal's own 20-row threat table
Every row in §7 is about REACHING a route/action a tier should not reach (wrong route, forged header,
enumeration, invite/session lifecycle). None is about an AUTHORIZED route returning MORE than its tier
should see in the response body. Route-level `require(tier)` is the right model for `/v1/vault/*` (the
whole route is sensitive) — it cannot be the right model for `/v1/board` (the route is fine for guests in
general; one field of one node type embedded in it is not). This is a gap in the threat model's own
coverage, not just an unimplemented control it already named.

## Impact
Not a secret-VALUE leak — actual vault contents stay behind the admin-gated `/v1/vault/:id/:key` route,
confirmed unreachable by guest/prompter in the policy table. But per the proposal's OWN words elsewhere in
the same document ("a map of where the secrets are is still the vault"), key names are themselves
sensitive: they can name the account/service a project depends on (`anthropic-prod`, a client's name, an
internal service), which is exactly the kind of incidental disclosure a deliberately-scoped "view only"
guest was promised would not happen. Given the doc frames guest as "a legitimate member at a lower tier...
who merely wants more than they were given" (§7 actor AU) — this hands every guest more than they were
given, by default, on the very first `GET /v1/board` call, with no action required.

## Recommendation
`get_board` needs a tier-aware (or at minimum vault-aware) response shape: either strip `config.keys` from
vault nodes in the response when the caller is below admin (requires threading caller tier from the
wheel-api proxy layer through to the engine somehow — a bigger design question this PR's own
`x-wheel-actor-tier` mechanism may already carry the pieces for, worth checking whether the engine can
cheaply consult it for this one field), or filter the response at the wheel-api proxy layer specifically
for `GET /v1/board` (matching the "operator gets exactly the same box an agent does" idiom `put_content`'s
own comment cites — a guest's *view* of the board is not the same box the engine returns, and the proxy is
already the place per-tier policy is decided). Whichever shape, it needs a test in the same style as
`tiers.rs`'s existing coverage: a guest's `GET /v1/board` response must contain no vault node's `keys`
field (or the vault node itself, if that's the chosen shape), proven against a project that actually has
one.

## What would change my mind
If `wheel-api`'s proxy has board-response filtering I did not find (I searched `proxy.rs`'s diff and the
whole-PR `redact` grep, not the entire pre-existing `wheel-api` crate for logic this PR might invoke
without touching), or if there's a separate, not-yet-merged commit on this same branch that adds it —
worth a `git log` check on `sdk/shared-projects` specifically for anything past the commit I diffed
against `dev`, in case this was already in flight when PM relayed the question.
