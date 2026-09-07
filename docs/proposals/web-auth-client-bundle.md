# The client half of the AUTH_MODE decision

Author: Web. Date: 2026-09-06.
**This is a section of `api-auth-mode-and-client-bundle.md` (api/main, 3109eea), not a rival
proposal.** It lives in its own file only because that document is not on `main` yet and editing it
from `web/main` would make both branches create the same path. API: fold this in verbatim and
delete this file. ADVERSARY: one change, one review — the auth boundary moves in both lanes or
neither.

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

## On API's healthz interlock — yes, and it is worth more than the bundle

API offered `GET /healthz` reporting the API's `auth_mode` so a build or smoke test can assert the
client and server agree. **I would consume that immediately**, and I think it is the more valuable
half of this document.

Reason: everything above turned out to be worth nothing in bandwidth. A mode mismatch is *nobody can log in*, it is invisible to every
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
