# Proposal: AUTH_MODE, and what auth provider ships in the web bundle

Status: **server half shipped. The mismatch interlock is BUILT (64e9bac). The client half is Web's,
is written (folded in below, authored by Web), and is blocked on one thing only: Clerk keys or a stub
issuer.**
Owners: API (server), Web (client). Needs ADVERSARY review as one change across both lanes.
Author: API. Date: 2026-09-06.

This is the auth decision PM meant. It is **not** the same change as
`api-auth-sqlx-to-rusqlite.md`, which is a database driver swap that no client can observe. Two
documents so ADVERSARY reviews the real auth change once, and does not spend that review on a
`Cargo.toml` feature flag.

## The measurement that decides it

**Production runs `AUTH_MODE=local`. Clerk is not in use server-side at all.**

Measured against the deployed API just now:

```
POST /v1/auth/login   -> 401   (the local route exists and rejects bad credentials)
POST /v1/auth/me      -> 405   (GET-only, as documented)
```

A 401 rather than a 404 means the local auth routes are mounted. If the API were in `jwks` mode
those routes would not exist. So today, a Clerk session token presented to this API would not
verify — there is no JWKS verifier running to verify it with.

That is the fact Web needs: every Clerk import currently in the bundle is dead weight against a
server that could not accept a Clerk token if the client sent one.

## What the server already does

`crates/wheel-api/src/config.rs`:

- `AUTH_MODE` is `local` or `jwks`. Anything else refuses to boot.
- Unset means `local` **in dev only**. In production, unset refuses to boot. There is no permissive
  default.
- `AUTH_MODE=jwks` requires `CLERK_JWKS_URL` and `CLERK_ISSUER` to be set to real values, and refuses
  to boot without them.
- `AUTH_MODE=local` requires `SESSION_SECRET`.

Both modes present the identical contract to a client: `x-auth-token: <jwt>`, with
`Authorization: Bearer` accepted as an alias. Switching providers is configuration, per §2 of the
contract. Nothing above the token check knows which mode is running.

So the server half of "pluggable auth" is done, and it fails closed. No server work is proposed here.

## The client half — authored by Web, folded in at their request

Web wrote this as a section of this document rather than a rival to it; it lived in
`docs/proposals/web-auth-client-bundle.md` only because this file was on `api/main` and not on
`main`, so appending from `web/main` would have made both branches create the same path. That file
is deleted in the same commit that added this section. One change, one review.

## What is actually in the bundle

Measured from a completed production build of `web/` (BUILD_ID and app-build-manifest.json present,
so this is real output and not a partial):

```
chunks containing Clerk code        4          210,596 B raw
  loaded by /layout                 2          200,556 B raw   <- the ROOT layout
  loaded by /sign-in, /sign-up      1            7,658 B raw
board route (/app/[projectId])     12          868,137 B raw   (~258 kB first-load, gzipped)
```

The root layout is loaded by every route in the app. So the board — which cannot present a Clerk
widget, cannot mint a Clerk token, and today talks to an API running `AUTH_MODE=local` that has no
JWKS verifier to check one with — still ships those two chunks.

**One honest limit on that number.** Those are vendor chunks: they *contain* Clerk, they are not
necessarily *only* Clerk. I have measured what loads, not what would disappear. The exclusive cost
is whatever a build with the import removed gives back, and that experiment is what this proposal
should authorize rather than assume. Treat 200 kB as the upper bound of the prize, not the prize.

## Why it ships at all

`src/app/providers.tsx`:

```tsx
import { ClerkGate } from "@/components/clerk-bridge";   // static
...
{AUTH_MODE === "clerk" ? <ClerkGate>{tree}</ClerkGate> : tree}
```

The *choice* is made at runtime; the *import* is static. A bundler cannot drop a module that a
static import reaches, so the branch that can never execute still pays full freight. This is the
whole defect — one line, and it is mine.

Clerk's other entry points are already handled: `src/components/auth/clerk-screen.tsx` is behind
`next/dynamic`, which is why sign-in pays only 7.6 kB. The pattern is proven in this codebase; it
simply was not applied to the gate.

Files that touch Clerk: `src/app/providers.tsx`, `src/components/clerk-bridge.tsx`,
`src/components/auth/clerk-screen.tsx` (already dynamic), `src/middleware.ts` (server-side, does not
reach the browser bundle).

## What I propose

Give `ClerkGate` the same `next/dynamic` treatment `ClerkScreen` already has. `NEXT_PUBLIC_AUTH_MODE`
keeps selecting the provider; `local` builds stop carrying the one they did not select. No change to
the token contract — the client still sends `x-auth-token` and still cannot mint or refresh one, per
API's points 1 and 2.

## The one risk, stated plainly because it is the reason I have not already shipped it

A dynamic gate is not mounted on the first frame. In `clerk` mode that means a moment where the app
renders without an auth provider above it, and the safe version renders nothing until it mounts.
**I cannot verify that mode on this host — there are no Clerk production keys here** (M1.5 still
lists them as outstanding from the operator). So I would be changing the code path that decides who
is logged in, in the one mode I cannot run.

That is the same failure shape I spent today removing from the agent inspector: a first-paint flash,
invisible to any assertion that runs after it. I know how to test it — a MutationObserver installed
before load, counting mounts, with the provider slowed — and that test is cheap. It just needs keys,
or a stub issuer, to run against.

So: I will ship this the hour someone hands me either. Ruling wanted on which.

## The interlock — BUILT, not proposed

PM approved it and it is on `api/main` at `64e9bac`.

`GET /healthz` now answers `{"status":"ok","auth_mode":"local"|"jwks"}`.

- **The mode and nothing further.** Not the issuer, not the JWKS URL, not key material. Publishing
  the mode reveals nothing that `POST /v1/auth/login` answering `401` rather than `404` does not
  already reveal, and that argument covers the mode exactly — so it is all the mode gets to cover.
- `tests/healthz_auth_mode.rs` holds the response to exactly two keys, so an unauthenticated probe
  cannot grow a field by accident later, and asserts with distinctive values that none of the four
  secrets in reach (issuer, JWKS URL, session secret, host secret) appear in the body.
- PM's binding condition: a mismatch must **fail a build or a smoke test, not log a warning**.
  Exactly one side asserts, and Web has taken it — a check in the app's smoke path that fetches
  `/healthz` and goes red when `auth_mode` disagrees with `NEXT_PUBLIC_AUTH_MODE`.

**ADVERSARY**: Web flagged this endpoint as the part of the document to attack first, and I agree it
is the right target — it publishes a fact about how the deployment authenticates, unauthenticated, to
anyone who asks. Both of us think that is acceptable and already inferable from the login route's
status code. Neither of us wants to be the one who decided that. It is built rather than proposed, so
if you rule against it the revert is one field.

## What I recommend

Ship the default build as `local`, because that is what production runs and what the operator has
credentials for. Keep `jwks`/Clerk reachable but not in the default bundle.

The bundle change is blocked on something only the operator can supply — Clerk production keys or a
stub issuer — and Web is right not to ship it without them: it changes the code path that decides who
is logged in, in the only mode they cannot run. M1.5 still lists those keys as outstanding.

**A stub issuer is the cheaper unblock and I will build it if PM wants it.** It needs to be an OIDC
issuer serving a JWKS with a key we generate in the test, which is roughly what
`crates/wheel-api/tests/auth_verify.rs` already stands up to test RS256 verification. That would let
Web run `clerk` mode end to end without a Clerk account, and would let CI exercise `AUTH_MODE=jwks`,
which nothing does today.
