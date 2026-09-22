# 064 — `POST /v1/projects/{id}/builder/turns` was gated by membership only: any Guest/Prompter could spend credentials

- **Severity:** Medium (spend / availability / inventory disclosure; no key-value disclosure). High if a real non-admin member exists on a deployment running the affected build.
- **Owner:** API (`crates/wheel-api/src/routes/builder.rs`, `auth/policy.rs`).
- **Status:** FIX IN REVIEW — #137 (`AdminScope` on the route). Live on `origin/main` (the #135 promotion) until #137 lands. Mutation-verified (below).
- **Method:** source read of engine `builder::{resolve, from_agent, from_vault, candidate_sources}` and the API route; mutation of the fix in a private `CARGO_TARGET_DIR`.

## Defect
The builder turn route took a bare `ProjectScope` (proves membership, not tier). The engine does not check tier either: its `403 policy` is `CredentialError::Policy` (the deployment's api-key-only rule), and its builder handlers take `State` only. The API extractor was therefore the only tier gate, and there was none. `auth/policy.rs` also had no rows for `v1/builder/credential`, so the credential could not be saved by anyone (fixed in the same PR).

## What a Guest or Prompter could do
- select `credential: Agent{node}` for **any** claude agent by UUID (its wired-vault exports, stored token or its own OAuth store) — no wire check, the builder is not a node;
- select `credential: Vault{node}` for **any** vault and spend the `ANTHROPIC_API_KEY` / `CLAUDE_CODE_OAUTH_TOKEN` in it;
- spend the builder's own stored setup-token;
- send a turn with no credential and read the `needs_auth` answer, whose `sources` lists (ids + names) every agent and vault holding a claude credential — same class as finding 054's key-name leak;
- hold the project's single turn permit, so the admin's own turn returns `429 builder_busy`.

Not possible: reading a key value. The child runs `--tools ""`, no MCP, no `WHEEL_*` env, exactly one credential variable.

## Verification
Mutation: revert `AdminScope(scope): AdminScope` → `scope: ProjectScope`; `cargo test -p wheel-api --features sqlite --test tiers -- a_lower_tier_is_refused_every_admin_api_route` FAILS with `a guest reached POST /v1/projects/<id>/builder/turns`; unmutated control passes. The other doors are closed: the generic proxy has no row for `builder/turns` (denied for all tiers), MCP `engine()` goes through `require_engine_tier`.

## Residual (non-blocking, same PR)
The new `engine_v1_routes()` scanner in `policy.rs` reads the engine's router source text and has silent blind spots (`any`/`on`/`head`/`options`, routes after the first `.route_layer(`, `.merge/.nest/.route_service`, `concat!`/string literals in handler args; the `>= 30` guard has slack of one). A tier decision can still be skipped for a route registered in one of those shapes. Fix: assert non-empty method sets and absence of those constructs, or register routes through one helper that both the router and the test read.

## Would falsify this
A tier check in the engine's builder handlers (none found), or evidence the deployment had no non-admin member (severity, not existence).
