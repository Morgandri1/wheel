# 052 — The authenticated engine proxy does not strip the `x-wheel-*` namespace

- **Severity:** Low as shipped (latent — nothing consumes these headers on this path today).
  **High the moment any trust marker is added to that namespace**, which is what
  `docs/proposals/shared-projects.md` does.
- **Owner:** API (`wheel-api`).
- **Status:** FOUND and FIXED in the same change (`sdk/multiplayer-identity`). Filed separately
  rather than folded into the feature, because a defect that only ever appears as a line in a
  feature diff is a defect nobody reviews.
- **Boundary:** TB2 (API ↔ host ↔ engine) — "header smuggling: client-sent `x-wheel-*` surviving
  the proxy hop into the engine", already named in `THREAT-MODEL.md:52` as a thing to check.

## Claim

`routes/proxy.rs`'s `forward_http` sanitised the client's headers with an **empty** forbidden-prefix
list:

```rust
// crates/wheel-api/src/routes/proxy.rs:199 (before)
    let headers = hop::sanitize_for_upstream(req.headers(), &[]);
```

`sanitize_for_upstream` takes `extra_forbidden_prefixes` precisely so a caller can drop a namespace,
and its doc-comment says so — "lets the ingress path additionally drop every `x-wheel-*` header, so
a public caller cannot forge the trust markers we ourselves add". The **public** ingress route passes
it (`routes/ingress.rs:36,68`). The **authenticated** engine proxy did not.

`wheel-host` then relays whatever it is given: it removes nine fixed names
(`crates/wheel-host/src/proxy.rs:162-174`) and `x-wheel-` is not among them.

So an authenticated tenant could set any `x-wheel-*` header on
`ANY /v1/projects/{id}/engine/{*rest}` and it arrived at their project's engine byte-for-byte,
indistinguishable from one the API had set.

## Impact, stated honestly

**Not exploitable as shipped.** The engine reads exactly two headers in this namespace —
`x-wheel-client-ip` and `x-wheel-secret` — and both are read only in `api/ingress.rs`, which serves
the public `/ingress` router, *not* the `/v1` control plane the authenticated proxy reaches.
`x-wheel-ingress` is set by the API and read by nobody. There is therefore no privilege gain
available today, and this is a latent hole rather than a live one.

It matters because of what it becomes. The namespace exists to carry markers the engine may trust
*because only the API can set them*. The moment anything on the control plane reads one — an actor
id, a role, a project marker — a tenant can assert it about themselves. Multiplayer M1 introduces
exactly that: `x-wheel-actor-id`, `x-wheel-actor-tier`, `x-wheel-actor-credential`. Without this fix
a guest sets `x-wheel-actor-tier: admin` and the engine believes it.

Two properties made it easy to miss, and both are worth naming:

- **The safe version already existed**, one file away, with a comment explaining why. The ingress
  path was written defensively and the proxy path was written first; nothing connected them.
- **The failure is invisible without a consumer.** No test could have caught it by asserting on
  behaviour, because no behaviour depended on it. It is only findable by reading the two call sites
  of one function and noticing they disagree.

## Fix

Strip the namespace, then set our own, on **every** path the API reaches an engine by — there are
three, and they build headers three different ways:

1. `forward_http` — `hop::sanitize_for_upstream(req.headers(), &["x-wheel-"])`.
2. `bridge_websocket` — builds a *fresh* request, so nothing is inherited and nothing is stripped;
   markers must be added explicitly.
3. `HttpBoardClient` (`routes/board_apply.rs`) — bypasses `routes/proxy.rs` entirely; used by
   `board/apply` and `instantiate`.

All three now go through `http::actor::set_actor` / `sanitized_with_actor`, so the strip and the set
are one function rather than three call sites where two would eventually be a version behind.

## Verify

`crates/wheel-api/tests/tiers.rs`:

- `a_forged_actor_header_never_reaches_the_engine` — sends every marker plus `x-wheel-ingress`,
  `x-wheel-client-ip` and an unknown `x-wheel-*`, on a route the caller *is* allowed, and asserts
  what the engine received is the server's value. Note it asserts the header count is **one**:
  `HeaderMap` holds multiple values per name, so "ignored" would leave the forgery sitting beside
  the real one, and only a count catches that.
- `a_forged_tier_does_not_open_a_route` — the forgery buys nothing.
- `the_board_apply_path_carries_the_actor_and_refuses_lower_tiers` — the third path, which a fix
  applied only at the proxy would have missed.

Mutation-checked: restoring `&[]` at `forward_http` turns the first two red; removing the explicit
headers from `bridge_websocket` or `HttpBoardClient` turns the corresponding assertion red.

## Residual

`wheel-host` still relays the namespace rather than stripping it itself. That is deliberate — it is
how the API's markers reach the engine at all — but it means the API is the *only* thing standing
between a client and the engine's trust markers. Anything else that ever forwards into a project's
engine inherits this obligation. A second layer in the host (strip the namespace, re-add nothing)
would need the host to know which markers are legitimate, which is knowledge it does not have and
should not acquire.
