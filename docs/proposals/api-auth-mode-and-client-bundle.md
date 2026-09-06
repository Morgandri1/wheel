# Proposal: AUTH_MODE, and what auth provider ships in the web bundle

Status: **server half is already built and shipped; the client half is Web's and is not written yet.**
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

## The open question, which is Web's

Clerk is currently imported in `middleware.ts`, `providers.tsx`, `auth.ts`, `csp.ts` and four more
files, and ships in builds that cannot use it. The decision is what `NEXT_PUBLIC_AUTH_MODE` should
select at build time, and whether Clerk is dynamically imported, behind a build flag, or removed
from the default build.

Web owns that call and is sending the measurement and file list. I have no view on which mechanism
they pick. What I owe them is the server contract it has to satisfy, which is:

1. Whatever the client does, it sends `x-auth-token: <jwt>`. Nothing else is read.
2. Under `local` the token is one the API issued from `POST /v1/auth/login`. Under `jwks` it is the
   provider's. The client must not assume it can mint or refresh one itself.
3. `NEXT_PUBLIC_AUTH_MODE` must mirror the API's `AUTH_MODE`. If they disagree the user gets a login
   form that talks to a verifier that is not running, or a Clerk widget whose token the API rejects.
   **This is the "two correct half-changes meeting in the middle" failure PM named**, and it is a
   deploy-time mismatch, not a code bug — which is why it needs one review across both lanes.

## What I recommend

Ship the default build as `local`, because that is what production runs and what the operator has
credentials for. Keep `jwks`/Clerk reachable but not in the default bundle. Revisit when the operator
actually has Clerk production keys — M1.5 lists them as still outstanding.

## The interlock I would like, and will build if PM wants it

Today nothing detects a client/server mode mismatch until a user fails to log in. `GET /healthz`
could report the API's `auth_mode` — it is not a secret, the login route's existence already reveals
it, and it would let Web's build or a smoke test assert the two agree instead of finding out from
the operator. Small, and it closes the exact failure above. Say the word and it is an afternoon.
