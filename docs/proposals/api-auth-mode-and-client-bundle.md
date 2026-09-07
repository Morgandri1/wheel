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

**CORRECTED 2026-09-07. The first version of this section claimed Clerk cost ~200 kB on every route.
That was wrong, and it was wrong because of HOW it was measured, not by how much.** It read
`.next/app-build-manifest.json` and treated a `/layout` entry as "what the browser downloads for
every route". It is not. The claim is retracted in full; what follows replaces it.

### Measured by removal, three builds

Every client-side `@clerk/nextjs` import (`clerk-bridge.tsx`, `clerk-screen.tsx`) was stubbed so the
package is genuinely absent from the client module graph; built; restored; rebuilt. `middleware.ts`
keeps its real import — it is server-only and cannot appear in a client chunk, so stubbing it would
have measured nothing while adding a stub that could lie.

Each build was confirmed finished by `EXIT=0` **and** the presence of `.next/BUILD_ID`
(baseline `qDC-iW_yJY8bA_1wYIeIY`, removal `hPFGnugWMGQHO_WGbnxwz`, restore `4A-zT1SjjOQ-E9eF7TKbx`).
This is not ceremony: one build in the same batch exited 1 with no `BUILD_ID`, and read carelessly it
would have passed as a clean "no change" result.

**First Load JS — identical, route for route, with Clerk and without:**

```
                     with      without
shared by all       102 kB     102 kB
/                   106 kB     106 kB
/app                125 kB     125 kB
/app/[projectId]    259 kB     259 kB
/sign-in            113 kB     113 kB
```

The cost to a user loading the board is **zero bytes**.

**Total emitted JS — where Clerk actually lives:**

```
with Clerk      28 chunk files   1,523,182 B raw
without         25 chunk files   1,329,360 B raw
difference       3 chunk files     193,822 B raw   (12.7% of emitted JS)
```

Those 194 kB are lazily-loaded chunks a `local`-mode user never fetches. The cost is build and
deploy artifacts, not bandwidth to anyone.

Note also: the 258 kB and 259 kB figures quoted at different points are the same build target. That
delta is build-to-build noise, not a change. ~1 kB differences from this tool are not signal.

### What that means for the decision

On efficiency grounds, **do not touch it**. There is no user-facing saving, and the change moves the
code path that decides who is logged in. I built the dynamic-import version, measured it, and
reverted it: it cost ~1 kB and bought nothing.

If Clerk should go, it should go on the argument that stands on its own — production runs
`AUTH_MODE=local`, and a provider we do not use should not be a dependency — and not on bundle size,
which does not support it.

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

~~Give `ClerkGate` the same `next/dynamic` treatment `ClerkScreen` already has.~~ **Withdrawn** — built, measured, reverted: zero first-load saving, ~1 kB cost. See the corrected measurement above.
keeps selecting the provider; `local` builds stop carrying the one they did not select. No change to
the token contract — the client still sends `x-auth-token` and still cannot mint or refresh one, per
API's points 1 and 2.

## The risk that stopped it, and why it no longer needs resolving

A dynamic gate is not mounted on the first frame, so in `clerk` mode the tree would render above a
provider that is not there yet (`useAuth()` throws without `ClerkProvider`). The safe form renders
nothing until it mounts, which is a blank first frame — the same failure shape as the agent
inspector's first-paint flash, invisible to any assertion that runs after it.

That risk is now moot: the change is withdrawn on its own merits, because the measurement showed
nothing to buy. Recording it anyway, because the next person to consider this will hit the same
question.

**One finding worth keeping.** API built a stub JWKS issuer (`120c323`) to unblock this. It does
unblock the token contract end to end, and it is the right tool for the `/healthz` gate below — but
it **cannot** exercise `clerk` mode in a browser. `ClerkProvider` throws without a *publishable*
key, which is a client-side Clerk credential no JWKS issuer supplies. So "point
`NEXT_PUBLIC_AUTH_MODE` at clerk and use the stub" would throw on mount. Verifying the Clerk client
path still requires real Clerk keys; nothing else substitutes.

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

**Not on efficiency grounds — there are none.** Web's corrected measurement (folded in above, and it
retracts their own earlier figure) is that First Load JS is identical route-for-route with Clerk and
without. The cost to a user loading the board is zero bytes. The ~194 kB is emitted chunks that a
`local`-mode user never fetches: build and deploy artifacts, not bandwidth. They built the
dynamic-import version, measured it, and reverted it — it cost about 1 kB and bought nothing.

So the bundle argument is dead, and I am striking my own earlier framing with it: I recommended
keeping Clerk out of the default bundle when I still believed the 200 kB figure, and that
recommendation rested on a number that turned out to be a measurement artefact.

What survives is the argument that never depended on bytes: **production runs `AUTH_MODE=local`, and
a provider we do not use should not be a dependency.** That is a supply-chain and
surface-area argument, and it stands or falls on its own merits — which is the right footing for a
change to the code path that decides who is logged in. If it is not persuasive enough on its own,
then the honest answer is to leave Clerk alone, because nothing else is now pushing for the change.

The bundle change is blocked on something only the operator can supply — Clerk production keys or a
stub issuer — and Web is right not to ship it without them: it changes the code path that decides who
is logged in, in the only mode they cannot run. M1.5 still lists those keys as outstanding.

**A stub issuer is the cheaper unblock and I will build it if PM wants it.** It needs to be an OIDC
issuer serving a JWKS with a key we generate in the test, which is roughly what
`crates/wheel-api/tests/auth_verify.rs` already stands up to test RS256 verification. That would let
Web run `clerk` mode end to end without a Clerk account, and would let CI exercise `AUTH_MODE=jwks`,
which nothing does today.
