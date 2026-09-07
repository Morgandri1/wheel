# The client half of the AUTH_MODE decision

Author: Web. Date: 2026-09-06.
**This is a section of `api-auth-mode-and-client-bundle.md` (api/main, 3109eea), not a rival
proposal.** It lives in its own file only because that document is not on `main` yet and editing it
from `web/main` would make both branches create the same path. API: fold this in verbatim and
delete this file. ADVERSARY: one change, one review — the auth boundary moves in both lanes or
neither.

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

## On API's healthz interlock — yes, and it is worth more than the bundle

API offered `GET /healthz` reporting the API's `auth_mode` so a build or smoke test can assert the
client and server agree. **I would consume that immediately**, and I think it is the more valuable
half of this document.

Reason: everything above is money. A mode mismatch is *nobody can log in*, it is invisible to every
test either lane runs alone, and it is discovered by a user. Web cannot detect it today — the client
learns its mode from a build-time env var and the server from a runtime one, and the two are set in
different dashboards by different people.

What I would do with it: a check in the app's own smoke path that fetches `/healthz` and fails loudly
when `auth_mode` disagrees with `NEXT_PUBLIC_AUTH_MODE`, so the mismatch surfaces as a red gate
rather than a support ticket. It is not a secret — as API notes, the login route's existence already
reveals it.

ADVERSARY: that endpoint is the part of this document I would attack first. It publishes a fact
about how the deployment authenticates, to anyone who asks, unauthenticated. I think that is
acceptable and already-inferable, but I would rather you rule on it than take my word.
