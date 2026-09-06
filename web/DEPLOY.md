# Deploying web/

## Environment variables — the complete list

`web/.env.example` is the copy-paste version of this table. Nothing in `web/` reads an
environment variable that is not listed here.

**Every `NEXT_PUBLIC_*` value is inlined into the client bundle at build time.** Two consequences
that bite people: changing one requires a *redeploy*, not a restart; and it is readable by anyone
who opens the bundle, so a secret must never carry that prefix.

### Required in every deployment

| Variable | Value | Where it is read | If it is wrong |
|---|---|---|---|
| `NEXT_PUBLIC_API_URL` | `https://wheel-api-production.up.railway.app`, later `https://api.wheel.dev`. **No trailing slash** | `src/lib/api.ts`, `src/middleware.ts` (CSP), endpoint inspector's public-URL fallback | It is baked into the CSP's `connect-src`, so the *browser* blocks every API call before it is sent. The network tab shows a CSP violation, not a failed request. |
| `NEXT_PUBLIC_AUTH_MODE` | `local` | `src/lib/auth.ts`, `src/lib/local-auth.ts`, `src/middleware.ts`, `src/lib/csp.ts` | See the footgun below — an unrecognised value fails *silently*. |

Defaults if unset: `NEXT_PUBLIC_API_URL` falls back to `http://localhost:8787` (the mock) and
`NEXT_PUBLIC_AUTH_MODE` to `mock`. Both defaults are correct for a laptop and wrong for a deploy,
and neither announces itself — a production build with no environment at all comes up pointing at
a mock server on localhost that isn't there.

### `NEXT_PUBLIC_AUTH_MODE` values

| Value | What it does | Needs |
|---|---|---|
| `mock` | Constant token against the bundled mock server (`pnpm mock`). No sign-in screen. | — |
| `dev` | The real API with `WHEEL_ENV=dev`, using a pre-minted token. | `NEXT_PUBLIC_DEV_TOKEN` |
| `local` | Email/password sessions issued by the API itself. **This is what we deploy.** | API on `AUTH_MODE=local` |
| `clerk` | A real Clerk session. | the two Clerk keys below |

**The footgun.** The mode is read as a plain string with no validation, so a typo — `locol`,
`Local`, a trailing space — is not rejected anywhere. What happens instead: the sign-in page still
renders (it falls through to the local form), the sign-in request still succeeds, and then no
token getter is ever registered, so *every* call afterwards 401s and the app bounces you back to
sign-in. It reads as "the API is broken" or "sign-in doesn't stick". It is a spelling mistake.

The same symptom, from a different cause, is the web and the API disagreeing about the mode. If
sign-in succeeds and everything after it 401s, check both these before anything else.

### Conditional

| Variable | When | Notes |
|---|---|---|
| `NEXT_PUBLIC_DEV_TOKEN` | `AUTH_MODE=dev` only | Mint with `infra/dev/e2e.py`'s `mint()`. It lands in the client bundle, so it is never a production credential. |
| `NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY` | `AUTH_MODE=clerk` only | `pk_…`. Public by design. |
| `CLERK_SECRET_KEY` | `AUTH_MODE=clerk` only | `sk_…`. **Server only — never prefix with `NEXT_PUBLIC_`.** |

### Set by the platform, not by us

| Variable | Notes |
|---|---|
| `NODE_ENV` | Vercel sets `production`. It selects the production CSP (drops `'unsafe-eval'`, adds `'strict-dynamic'`) and it is why `vercel.json` installs with `--prod=false`: pnpm skips devDependencies when this is `production`, and without `typescript` installed Next never loads `tsconfig.json`, so the `@/*` path alias goes unregistered and the build dies on `Module not found: Can't resolve '@/lib/events'` — a path error that is really a missing dependency. |

### Local development only

| Variable | Default | Notes |
|---|---|---|
| `NEXT_DIST_DIR` | `.next` | A second dev server needs its own build directory. Two `next dev` sharing one `.next` corrupt each other's cache, and the symptom is a 500 on the server you were not touching. QA's Playwright config relies on this to run mock-mode and local-mode servers side by side. |

### The mock server only (`pnpm mock`) — never set in a deployment

| Variable | Default | Notes |
|---|---|---|
| `MOCK_PORT` | `8787` | Must match the port in `NEXT_PUBLIC_API_URL`. |
| `MOCK_ORIGINS` | `http://localhost:3000,http://127.0.0.1:3000` | CORS allow-list. A browser cannot distinguish a CORS refusal from an unreachable server, so a missing origin here surfaces in the UI as "Can't reach the API" — pointing at the wrong thing. |
| `MOCK_BULK_NODES` | `0` | Seeds N extra nodes, for testing the board at scale. |

## What the operator has to do in Vercel

Project **root directory = `web/`**. Everything else is defaults; `vercel.json` pins the package
manager and adds the response headers. Set `NEXT_PUBLIC_AUTH_MODE=local` and `NEXT_PUBLIC_API_URL`
for all environments, and redeploy after any change to either.

## `npx wheel-web` — running the board without a build

The same app ships as a package that needs no toolchain: Next's standalone server, prebuilt.

```
npx wheel-web --api https://api.wheel.dev      # or WHEEL_API_URL
npx wheel-web --port 3400 --api http://localhost:8080
```

Build and assemble it with `pnpm build:pkg && pnpm pack:pkg`; the publishable tree lands in
`dist-pkg/` (gitignored) and is published as `wheel-web`, versioned with the API.

**The API URL is resolved at run time, not baked in.** This is the whole design constraint, and
it is easy to get wrong: `NEXT_PUBLIC_*` values are inlined into the bundle when it is compiled,
so a prebuilt package cannot read one from the user's environment. Shipping `WHEEL_API_URL` as a
`NEXT_PUBLIC_` variable would give the package a single option that silently does nothing. Instead
the server resolves it per request (`src/lib/runtime-config.ts`) and hands it to the client before
the first fetch; the build-time value stays as the fallback, which is what keeps the Vercel
deployment behaving exactly as before. The middleware builds the CSP from the same resolution, so
the policy can never name a different API than the app is calling.

`NEXT_PUBLIC_AUTH_MODE=local` genuinely is fixed at build time and is baked into the package —
there is no identity provider in this build to configure.

Three traps in this pipeline, all of which produce a package that looks fine:

1. **Static assets go under the dist-dir the build used**, not a hardcoded `.next`. Build with
   `NEXT_DIST_DIR=.next-pkg` and copy into `.next/static` and every chunk 404s: the HTML is
   server-rendered so the page appears, but React never hydrates and nothing is clickable. Nothing
   looks broken until you click. `pack:pkg` now fails if the chunk directory is missing.
2. **The manifest must not say `"type": "module"`.** Next's standalone `server.js` is CommonJS and
   calls `require()`; marked as ESM it dies on its first line. The bin is `.mjs`, which is ESM by
   extension and needs nothing from the manifest. `pack:pkg` fails on this too.
3. **`.next/static` and `public/` are not part of standalone output** — it assumes a CDN serves
   them. A locally-run package has no CDN, so the packer copies both.

## What API has to do

CORS must allow the Vercel origin, or the browser blocks every call before it reaches our code.
The Vercel project is **`wheel-2708`**, so the production origin is:

```
CORS_ALLOWED_ORIGINS=https://wheel-2708.vercel.app
```

Preview deployments get their own origin per branch,
`https://wheel-2708-git-<branch>-<team>.vercel.app`, and are **deliberately not allow-listed**:
a wildcard `https://*.vercel.app` would let any Vercel app on the internet call this API with a
user's token, which is a far worse trade than previews that cannot reach the API. Add specific
preview origins when a preview genuinely needs one.

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
   into our origin — where it can read the session token. `scripts/copy-monaco.mjs` copies the
   editor into `public/` before every dev run and build (`predev` / `prebuild`); `public/monaco`
   is generated and not committed. We found this because the policy blocked Monaco's stylesheet
   while `'strict-dynamic'` was happily letting its script through.

   It is `.mjs`, not `.ts`, deliberately: `prebuild` runs inside the production install, so a
   build step that needs `tsx` needs a devDependency to build — which is the exact failure mode
   `--prod=false` exists to prevent. Do not "modernise" this one back to TypeScript.
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
