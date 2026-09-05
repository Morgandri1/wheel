# Deploying web/

Vercel project **root directory = `web/`**. Everything else is defaults; `vercel.json` pins the
package manager and adds the response headers.

## What the operator has to supply

Local email/password is the shipping provider (`AUTH_MODE=local`), so the deploy needs two
variables and no third-party account:

| Variable | Value | Scope | Notes |
|---|---|---|---|
| `NEXT_PUBLIC_AUTH_MODE` | `local` | all | `mock` and `dev` are for local work only. Anything but `local` or `clerk` on a public deploy means no sign-in at all. |
| `NEXT_PUBLIC_API_URL` | `https://wheel-api-production.up.railway.app`, later `https://api.wheel.dev` | all | No trailing slash. It is also baked into the CSP's `connect-src`, so a wrong value blocks every API call at the browser rather than failing at the API. |

`NEXT_PUBLIC_*` values are baked into the client bundle at build time, so changing one needs a
redeploy, not just a restart.

The API needs `AUTH_MODE=local` to match. The web and the API disagreeing about the mode is the
one misconfiguration that looks like a bug rather than a setting: sign-in succeeds, and every
call afterwards 401s.

## What API has to do

CORS must allow the Vercel origin, or the browser blocks every call before it reaches our code:

```
CORS_ALLOWED_ORIGINS=https://<production-domain>,https://<preview>.vercel.app
```

with `x-auth-token` and `x-project-id` in allowed headers (both already are), and
`access-control-allow-origin` actually emitted — an unset allowlist matches nothing and silently
sends no origin header at all, which is what we hit locally. A browser cannot tell a CORS refusal
from an unreachable server, so the symptom in the UI is "Can't reach the API", pointing at the
wrong thing.

If the API sends `Retry-After` on a rate-limited sign-in, it also needs
`access-control-expose-headers: retry-after`, or the browser hides it and the form can only say
"too many attempts" without saying for how long.

## The one cross-origin thing worth checking first

Realtime needs `POST /v1/projects/:id/ws-ticket` to succeed from the browser, then the socket
opens at `.../engine/v1/events?ticket=…`. A browser cannot set headers on a WebSocket handshake,
so the ticket is the only way the socket authenticates. If the board loads but never goes live,
check the ticket call in the network tab before suspecting the engine: a socket that opens and
stays silent is an auth failure, not an engine failure.

## Content Security Policy

Set on every response by `src/middleware.ts` from `src/lib/csp.ts`, with a nonce minted per
request: no inline script, no `eval`, `object-src`/`base-uri`/`frame-ancestors` all `'none'`,
and `connect-src` limited to our own origin plus the API over both https and wss.
(ADVERSARY R7, binding.)

Three consequences worth knowing before someone rediscovers them the hard way:

1. **Every route renders per request** (`export const dynamic = "force-dynamic"` in the root
   layout). A prerendered page is built before any request exists, so its HTML carries no nonce
   and the browser refuses Next's own bootstrap scripts. Measured, not assumed: with
   prerendering, the landing page served 0 nonces and 12 scripts were refused. The cost is that
   page HTML is not CDN-cacheable; static assets still are.
2. **Monaco is served from `/monaco`, not from jsDelivr.** `@monaco-editor/react` fetches the
   editor from a public CDN by default, which means a third party could serve executable code
   into our origin — where it can read the session token. `scripts/copy-monaco.ts` copies the
   editor into `public/` before every dev run and build (`predev` / `prebuild`); `public/monaco`
   is generated and not committed. We found this because the policy blocked Monaco's stylesheet
   while `'strict-dynamic'` was happily letting its script through.
3. **`style-src` keeps `'unsafe-inline'`.** Server-rendered `style` attributes are subject to
   `style-src`, CSP nonces do not apply to style attributes at all, and the exposure is CSS
   injection rather than script execution. Named here so it reads as a decision.

Markdown from a ctx node is rendered through `SafeMarkdown` (`rehype-sanitize`, with `href`
narrowed to http/https/mailto). There is no `dangerouslySetInnerHTML` anywhere in `web/`.
Verified in a browser on a production build: a ctx node containing `<script>`, `<img onerror>`
and a `javascript:` link renders as inert text, with zero CSP violations and zero console errors.

## Where the session token lives, and the tradeoff

In memory, mirrored into `localStorage` under `wheel.session` so a reload does not sign you out.

That mirror is a real exposure and is written down rather than assumed away: **localStorage is
readable by any script running on the origin**, so an XSS anywhere in the app is a stolen session.
A token held only in memory would die with the tab; an `httpOnly` cookie would be unreadable by
script entirely. We are not using the cookie today because the web and the API are on different
origins, which makes a cookie a CSRF surface the API then has to defend, and because that is the
API's call rather than the web's.

What bounds it: the token authorises one user's own projects and nothing else; it expires; and any
401 from any route clears it immediately. If the API later issues an `httpOnly; SameSite=None;
Secure` cookie, `src/lib/local-auth.ts` switches to `credentials: "include"` and drops the mirror
entirely — that is a contained change on this side, and it needs CSRF protection on the API's.

ADVERSARY: this is the paragraph to attack.

## The guard on /app is client-side in local mode

Middleware cannot see a local session — there is no cookie to read at the edge — so `/app` is
gated in the browser by `SessionGate`. That is a routing courtesy, not a security boundary: the
boundary is the API, which refuses every request without a valid `x-auth-token` and 404s projects
you do not own. In `clerk` mode the middleware guard applies as well.

## If the operator moves to Clerk or Privy later

The provider is one setting. Set `NEXT_PUBLIC_AUTH_MODE=clerk`, add
`NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY` (`pk_…`) and `CLERK_SECRET_KEY` (`sk_…`, **server only —
never prefix with `NEXT_PUBLIC_`**), and in Clerk enable email/password plus Google and GitHub,
set the paths to `/sign-in` and `/sign-up`, and add the production domain and `https://*.vercel.app`
to allowed origins or preview builds will not mint a session. The API switches to `AUTH_MODE=jwks`
with the provider's issuer and JWKS URL. `/sign-in` and `/sign-up` serve whichever provider is
configured, so no URL moves and no bookmark breaks.

## Running two modes side by side locally

`NEXT_DIST_DIR` gives a second dev server its own build directory, so a local-mode instance can
run beside the operator's mock-mode one instead of corrupting its `.next` cache:

```
MOCK_PORT=8788 MOCK_ORIGINS=http://localhost:3200 pnpm mock
NEXT_DIST_DIR=.next-local NEXT_PUBLIC_AUTH_MODE=local \
  NEXT_PUBLIC_API_URL=http://localhost:8788 pnpm exec next dev --port 3200
```

The mock implements `/v1/auth/signup|login|logout|me` with a seeded account
(`dev@wheel.dev` / `wheel-dev-password`), a real 5-strike lockout with `Retry-After`, and
identical answers for a wrong password and an unknown email.
