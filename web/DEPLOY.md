# Deploying web/

## The shape of it

The browser talks to exactly one thing: this app's server. That server is the only thing that
talks to the Wheel API on the browser's behalf.

```
browser ──same origin──▶ wheel-web (Next route handlers) ──WHEEL_API_URL──▶ wheel-api ──▶ host / engines
```

| Route | What it does | Where |
|---|---|---|
| `/api/wheel/v1/projects/…` | Authenticated proxy to the API's project routes | `src/lib/api-proxy.ts` |
| `/api/session`, `/api/session/{login,signup,logout,password}` | Local-mode sessions, held as an httpOnly cookie | `src/lib/session-routes.ts` |
| `/api/wheel/projects/:id/events` | The engine event socket, relayed as Server-Sent Events | `src/lib/event-relay.ts` |
| `/api/wheel/probe` | The endpoint panel's "send test", run from the server | `src/lib/ingress-probe.ts` |

What follows from that:

- **The API does not have to be reachable from the internet** — only from this server. Bind it to
  loopback (`npx wheel-web` beside `wheeld`), keep it on a private network (compose, k8s), or leave
  it public (Railway + Vercel). The browser behaves identically in all three.
- **The API's address is never in the bundle, the CSP or any response.** `WHEEL_API_URL` is
  server-only. `src/lib/client-boundary.test.ts` fails if any module the browser loads names it, and
  `import "server-only"` makes the build fail if a client module imports the server config.
- **The browser holds no token in any mode.** See "Where the session lives" below.
- **The one public thing is endpoint ingress.** `/p/<project>/…` is what webhook senders hit, so it
  must be reachable wherever you want webhooks. The API names that address (`project.ingress_base_url`,
  from its `PUBLIC_BASE_URL`); the web only displays it and never builds one itself.

## Environment variables — the complete list

`web/.env.example` is the copy-paste version of this section. Nothing in `web/` reads an
environment variable that is not listed here.

### Server-only, read at run time

Change one and restart; nothing is rebuilt. None of these ever reaches the browser.

| Variable | Default | Notes |
|---|---|---|
| `WHEEL_API_URL` | `http://127.0.0.1:8080` (none in the Docker image) | Where this server reaches the API. Falls back to `NEXT_PUBLIC_API_URL` so an existing deployment keeps working — but that fallback is inlined when the server is built, and `WHEEL_API_URL` is not. A value that is not an http(s) URL stops the server from starting. `127.0.0.1` rather than `localhost`: Node may resolve `localhost` to `::1` while the API listens on IPv4 only. The image has no default and refuses to start without one: `127.0.0.1` inside a container is the container. |
| `WHEEL_AUTH_MODE` | `mock` | `mock` · `dev` · `local` · `clerk` — see below. Falls back to `NEXT_PUBLIC_AUTH_MODE`. The server hands the mode to the page, so one build serves any mode. |
| `WHEEL_DEV_TOKEN` | — | The token `dev` mode presents. Replaces `NEXT_PUBLIC_DEV_TOKEN`, which put the token in the bundle. Mock mode never sends it. |
| `WHEEL_ALLOW_INSECURE_AUTH` | off | `1` lets a production server run `mock` or `dev`, the modes where one credential serves every visitor. Without it a production server refuses to start in either — and an unset `WHEEL_AUTH_MODE` is `mock`. |
| `WHEEL_PUBLIC_ORIGIN` | — | The origin browsers use, e.g. `https://wheel.example.com`. **Set it behind any TLS-terminating proxy, and for any address but localhost.** Without it (and without a trusted proxy) the `/api` routes answer only a loopback `Host`. See "The trust model". Anything but a bare http(s) origin stops the server from starting. |
| `WHEEL_TRUST_PROXY` | off (on when `VERCEL=1`) | `1` to take the public origin from `X-Forwarded-Proto` / `X-Forwarded-Host` — the rightmost value, the one the nearest proxy wrote. Only for a proxy that overwrites them, with this server reachable through it alone. `WHEEL_PUBLIC_ORIGIN` wins over both. |
| `WHEEL_PROXY_BODY_LIMIT_BYTES` | `5242880` | Request bodies over this are refused with 413 while streaming, before they are buffered. Keep it equal to the API's `INGRESS_BODY_LIMIT_BYTES` (5 MiB by default), which is the limit a chest upload actually meets. |
| `CLERK_SECRET_KEY` | — | clerk mode only. **Never prefix with `NEXT_PUBLIC_`.** |

### `WHEEL_AUTH_MODE` values

| Value | What the server presents to the API | Needs |
|---|---|---|
| `mock` | A constant token, for the bundled mock (`pnpm mock`). No sign-in screen. | — |
| `dev` | `WHEEL_DEV_TOKEN`, against the real API with `WHEEL_ENV=dev`. | `WHEEL_DEV_TOKEN` |
| `local` | The session cookie. Email/password sessions issued by the API. **This is what we deploy.** | API on `AUTH_MODE=local` |
| `clerk` | Clerk's session token, read on the server with `auth().getToken()`. | the two Clerk keys |

**An unrecognised value now fails loudly.** It used to be read as a plain string, so `locol` or
`Local` rendered a sign-in page that "worked" and then 401'd on everything. Now the server refuses
to start and logs `WHEEL_AUTH_MODE="locol" is not one of: mock, dev, local, clerk.` Leading and
trailing whitespace is forgiven; case is not. Every setting is checked when the server starts
(`src/instrumentation.ts`), which logs one line — `wheel-web: API … · auth … · public origin: …` —
or refuses to come up.

The web and the API disagreeing about the mode still looks like "sign-in succeeds and everything
after it 401s". `pnpm check:auth-mode` (with the web server's env) compares the two.

**`dev` and `mock` make this server an authenticated proxy for anyone who can reach it**: it
presents one credential for every visitor. So a production server (`NODE_ENV=production`, which
`next start`, the standalone server and the image all set) refuses to start in either mode — and
mock is what an unset `WHEEL_AUTH_MODE` means — unless `WHEEL_ALLOW_INSECURE_AUTH=1` says an open
server is meant. Mock presents only the mock's fixed token, never `WHEEL_DEV_TOKEN`.

### Build-time (`NEXT_PUBLIC_*`)

Inlined into the bundle when it is compiled: changing one needs a rebuild, and anyone can read it.

| Variable | When | Notes |
|---|---|---|
| `NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY` | clerk mode only | `pk_…`. Public by design. |
| `NEXT_PUBLIC_API_URL`, `NEXT_PUBLIC_AUTH_MODE` | legacy | Read by the server only, as fallbacks for the two `WHEEL_` names. No client code references either. |

`NEXT_PUBLIC_DEV_TOKEN` is **gone**. Setting it does nothing; use `WHEEL_DEV_TOKEN`.

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
| `MOCK_PORT` | `8787` | Must match the port in `WHEEL_API_URL`. `pnpm dev:mock` points the web server at `http://127.0.0.1:8787` unless `WHEEL_API_URL` is already set. |
| `MOCK_ORIGINS` | `http://localhost:3000,http://127.0.0.1:3000` | CORS allow-list. Only matters to a browser calling the mock directly, which the web app no longer does. |
| `MOCK_BULK_NODES` | `0` | Seeds N extra nodes, for testing the board at scale. |

## Where the session lives — the trust model

ADVERSARY: this is the section to attack. It is the one statement of the model; the code comments
point here rather than repeating it.

**local mode.** The API issues an HS256 session JWT at `/v1/auth/login|signup`. This server puts it
in a cookie and returns only `{user}` to the page:

| Attribute | Value | Why |
|---|---|---|
| name | `wheel_session`, or `__Host-wheel_session` over https | A browser accepts `__Host-` only from a secure origin, with `Path=/` and no `Domain`, so a sibling subdomain cannot plant or overwrite it. Over https only the `__Host-` name is read. |
| `HttpOnly` | always | Page script cannot read it, so an XSS cannot carry the session away. |
| `SameSite` | `Lax` | Not sent on cross-site subrequests or cross-site POSTs. |
| `Secure` | whenever the PUBLIC origin is https | Behind a proxy that is known only from `WHEEL_PUBLIC_ORIGIN` or a trusted proxy's `X-Forwarded-Proto`; a forged header from anyone else earns nothing. |
| `Path` | `/` | |
| `Max-Age` | seconds until the API's `expires_at` (or the JWT's `exp`) | A session the API has already expired is refused rather than set. |

**Only a live-looking cookie is presented.** The value must be shaped like the API's session JWT and
not past its own `exp`; anything else is treated as no session — and cleared — before a body is
read or an upstream socket dialled. The signature is the API's to check; this only stops a garbage
cookie from costing anything.

`GET /api/session` asks the API's `/v1/auth/me` who the cookie belongs to, and only its explicit
`{user: null}` signs the browser out: a 5xx, a timeout or no answer at all shows "Can't reach the
server" and retries (1 s → 30 s) rather than bouncing a returning user to sign-in. Logout clears
the cookie even when the API cannot be reached. A password change clears it, because the API has
revoked every session. Any 401 from the API clears it on the way back. The old `localStorage`
mirror (`wheel.session`) is deleted.

**Session and probe calls to the API give up after 5 s** (a 504 `api_timeout`, distinct from the 502
`api_unreachable`), so a sign-out or a session check cannot hang for as long as the platform
allows. Proxied board calls are bounded only by the browser's own request.

**Bodies are JSON or refused.** The session routes and the probe take only `application/json`
(415 otherwise), so a form post from anywhere cannot be read as a sign-in.

**clerk mode.** Clerk keeps its own session cookie on this origin; this server reads the token with
`auth().getToken()` from `@clerk/nextjs/server` and attaches it. The browser never handles it.

**CSRF.** Every non-GET to a proxy, session or probe route must come from this app's PUBLIC
origin. When the request carries `Origin` (every browser sends it on a state-changing request), it
must equal the public origin — scheme, host and port. With no `Origin`, `Sec-Fetch-Site:
same-origin` is accepted instead. Anything else is a 403, and so is any request of any method the
browser labels `Sec-Fetch-Site: cross-site`. SameSite=Lax is the first lock; this is the second,
and the only one in dev/mock mode.

The public origin is, in order: `WHEEL_PUBLIC_ORIGIN`; the rightmost `X-Forwarded-Proto` +
`X-Forwarded-Host` — the values the nearest proxy wrote — when `WHEEL_TRUST_PROXY` is on;
otherwise the connection's own scheme and `Host`. A forged `X-Forwarded-*` from a peer nobody
declared trusted is ignored, and so is the scheme Next derives from it (Next fills those headers
only when absent, so a client's own survive). When a browser says a request is same-origin but its
`Origin` is not the one computed, the server logs `…set WHEEL_PUBLIC_ORIGIN` — that is a proxy
nobody told it about, not an attack.

**DNS rebinding.** With neither `WHEEL_PUBLIC_ORIGIN` nor a trusted proxy, the `/api` routes answer
only a loopback `Host` (`localhost`, `127.0.0.0/8`, `[::1]`): any other name may be an attacker's,
rebound to this server, and a page on it is "same-origin" to itself. With `WHEEL_PUBLIC_ORIGIN`
set, a rebound page's `Origin` is not yours and its writes are refused; its reads carry no cookie.

**What an XSS can still do:** act as the user through same-origin calls while the page is open. It
can no longer take the session away with it. The CSP (no inline script, per-request nonce,
`connect-src 'self'`) is what bounds that.

**The boundary is still the API.** It verifies the JWT and asserts ownership on every call; a
project you do not own is a 404. Nothing this server does replaces that — it only decides which
requests are allowed to reach the API at all.

## What the proxy refuses

All in `src/lib/proxy-rules.ts`, each one a unit test:

- **Any route that is not one of the board's.** A positive list, compared on the raw path:
  `/v1/projects`, `/v1/projects/:id`, `…/:id/{start,stop,restart}`, `…/:id/board/apply` and
  `…/:id/engine/v1/…`. `/v1/auth/*` in particular is never proxied: answered through a generic
  proxy, a login would hand the JWT to page script. Nor is ws-ticket, however it is spelled
  (`ws%2Dticket`, `ws-ticket;x`) — it is simply not on the list.
- **Traversal.** Each segment is decoded exactly once, and refused if what is left is `.` or `..`,
  or holds a `%` (a second layer of encoding, `%252e%252e`, aimed at a hop that decodes again), a
  slash, a backslash or a control character; so are empty segments, `;` and malformed escapes. The
  parsed target is then checked again to be sure it is exactly the path that was validated. (The
  API hardens its own hops separately — this is the second lock.)
- **Headers.** Only `x-project-id` (a bare id — a folded duplicate is refused) and `content-type`
  (printable ASCII) are forwarded. The browser's cookies, a browser-supplied `x-auth-token` or
  `authorization`, and any `x-forwarded-*` never leave. The response comes back with a short
  allow-list of headers; `set-cookie`, `location`, `content-length` and `content-encoding` do not.
- **Rendering on this origin.** Every proxied response carries `x-content-type-options: nosniff`,
  and anything that is not JSON also gets `content-security-policy: sandbox` and
  `content-disposition: attachment`: a chest can hold HTML, and rendered here it would run with the
  user's session.
- **Oversized bodies.** A declared `content-length` over the cap is refused without reading. An
  undeclared body is streamed to the API through a counter — never buffered here — which errors
  the stream the moment it passes the cap, and the caller gets a 413.
- **SSRF.** The target origin is `WHEEL_API_URL` and nothing else. The Host header, a forwarded
  host, the URL the request arrived on and anything in the query cannot change it, and redirects
  from the API are not followed.

## The guard on /app

In local mode, middleware sends a visitor with no session cookie from `/app…` to `/sign-in`
(keeping `?next=`). It looks only for a cookie shaped like a live session. `SessionGate` then asks
`/api/session` whether the session is alive and redirects if it is not. Both are routing
courtesies; the API is the boundary. In clerk mode Clerk's middleware guard applies instead.

## Realtime

The board opens an `EventSource` at `/api/wheel/projects/:id/events`. The relay opens the engine's
socket at `${WHEEL_API_URL}/v1/projects/:id/engine/v1/events` with the session in the
`x-auth-token` header — the form the API already accepts from non-browser clients — so no ticket is
minted and no credential is ever in a URL. Each frame is relayed verbatim as one SSE `data:` event.

- `: keepalive` every 15 s, so idle proxies do not cut the stream.
- `event: wheel-open` once the upstream socket is really connected; the board shows "Live" then,
  not when the HTTP response arrives.
- `event: wheel-error` with `{"status": …}` when upstream refuses or fails. A 401 signs the UI out;
  anything else reconnects with backoff (0.5 s → 15 s, reset on a real open). EventSource's own
  retry is never used, because it has no backoff and no idea that a 401 is final.
- The relay has no replay. When a stream reopens after any gap — a reader more than 1 MiB behind is
  disconnected, the platform cuts it, or it reaches its 15-minute lifetime — `events.ts` calls
  `onResync` and the board refetches, exactly as it does for the engine's own `lagged`. The
  lifetime is also why a session revoked at the API stops streaming within 15 minutes.
- The upstream socket closes when the browser goes away. A garbage or expired cookie never dials
  one. At most 8 streams per session, 32 per client address (only as trustworthy as the proxy that
  recorded it) and 1024 per server process are open at once; past that the answer is 429 and the
  client backs off.

If the board loads but never goes live, look at the `events` request in the network tab: an
immediate `wheel-error` says exactly what upstream answered.

`POST /v1/projects/:id/ws-ticket` stays on the API for other clients; the web no longer calls it.

## Content Security Policy

Set on every response by `src/middleware.ts` from `src/lib/csp.ts`, with a nonce minted per
request: no inline script, no `eval`, `object-src`/`base-uri`/`frame-ancestors` all `'none'`, and
`connect-src 'self'` — plus Clerk's hosts in clerk mode, and the dev server's hot-reload socket in
development. The policy never names the API. (ADVERSARY R7, binding.)

Three consequences worth knowing before someone rediscovers them the hard way:

1. **Every route renders per request** (`export const dynamic = "force-dynamic"` in the root
   layout). A prerendered page is built before any request exists, so its HTML carries no nonce
   and the browser refuses Next's own bootstrap scripts. Measured, not assumed: with
   prerendering, the landing page served 0 nonces and 12 scripts were refused. The cost is that
   page HTML is not CDN-cacheable; static assets still are.
2. **Monaco is served from `/monaco`, not from jsDelivr.** `@monaco-editor/react` fetches the
   editor from a public CDN by default, which means a third party could serve executable code
   into our origin — where it can act with the user's session. `scripts/copy-monaco.mjs` copies the
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

## Vercel

Project **root directory = `web/`**. `vercel.json` pins the package manager and adds the response
headers. Set `WHEEL_AUTH_MODE=local` and `WHEEL_API_URL` (server-only) for all environments; the
existing `NEXT_PUBLIC_AUTH_MODE` / `NEXT_PUBLIC_API_URL` keep working as fallbacks until then,
though those are read at build time and need a redeploy to change.

- **Proxy trust.** Vercel sets `VERCEL=1`, and `WHEEL_TRUST_PROXY` defaults on when it does: the
  edge always sets `X-Forwarded-Proto` / `-Host` and a function cannot be reached around it, so
  production and preview domains alike are compared correctly with no extra setting.
- **Runtime.** Every route handler here declares `runtime = "nodejs"`; the events relay needs
  Node's networking for its WebSocket client and would not run on the Edge runtime.
- **Stream duration.** A Vercel function has a maximum duration set by the plan (300 s by default
  with fluid compute). The events stream is cut when it is reached; EventSource reports an error,
  `events.ts` reconnects with backoff and the board refetches. Expect a brief "Reconnecting" at
  each limit — events are hints to refetch, and messages and logs are persisted, so nothing that
  the board shows is lost.
- **Body size.** Vercel caps a request body at 4.5 MB, below the proxy's 5 MiB default, so a chest
  upload between the two fails at the platform with a 413. Self-hosted deployments have no such cap.
- **The API sees Vercel's egress addresses, not users'.** Its auth limits are keyed per email
  (login) and globally (signup), not per IP, so a shared source address does not merge anyone's
  limit with anyone else's.
- **CORS.** The API no longer needs the Vercel origin in `CORS_ALLOWED_ORIGINS` for the web. Leave
  it until this release is live, then it can go.

## `npx wheel-web` — running the board without a build

The same app ships as a package that needs no toolchain: Next's standalone server, prebuilt.

```
npx wheel-web                                   # API at http://127.0.0.1:8080, local auth
npx wheel-web --port 3400 --api http://127.0.0.1:8080
npx wheel-web --public-origin http://192.168.1.5:3000   # opened from another machine
```

Without `--public-origin` (or `WHEEL_PUBLIC_ORIGIN`) the server's `/api` routes answer only on
localhost — see "DNS rebinding" above.

Build and assemble it with `pnpm build:pkg && pnpm pack:pkg`; the publishable tree lands in
`dist-pkg/` (gitignored) and is published as `wheel-web`, versioned with the API.

The API URL and the auth mode are both read by the server when it starts; nothing about either is
baked into the bundle, so one package works against any API. The bin sets `WHEEL_AUTH_MODE=local`
unless the environment says otherwise. Since the browser never talks to the API, `wheeld` can
listen on `127.0.0.1` only and still serve a board opened from another machine through this server
(given `--public-origin`).

Three traps in this pipeline, all of which produce a package that looks fine:

1. **Static assets go under the dist-dir the build used**, not a hardcoded `.next`. Build with
   `NEXT_DIST_DIR=.next-pkg` and copy into `.next/static` and every chunk 404s: the HTML is
   server-rendered so the page appears, but React never hydrates and nothing is clickable. Nothing
   looks broken until you click. `pack:pkg` fails if the chunk directory is missing.
2. **The manifest must not say `"type": "module"`.** Next's standalone `server.js` is CommonJS and
   calls `require()`; marked as ESM it dies on its first line. The bin is `.mjs`, which is ESM by
   extension and needs nothing from the manifest. `pack:pkg` fails on this too.
3. **`.next/static` and `public/` are not part of standalone output** — it assumes a CDN serves
   them. A locally-run package has no CDN, so the packer copies both.

## Docker — `docker/Dockerfile.web`

```
docker build -f docker/Dockerfile.web -t wheel-web .
docker run --rm --read-only -p 3000:3000 -e WHEEL_API_URL=http://api:8080 wheel-web
```

A standalone build (`WHEEL_STANDALONE=1`) on `node:22-bookworm-slim`, running as uid 10001 and
listening on `0.0.0.0:3000` inside the container. `HEALTHCHECK` fetches `/version.json`. The files
are root-owned and never written, so the container runs with a read-only root filesystem.

| Runtime env | Default in the image |
|---|---|
| `WHEEL_API_URL` | **none** — required; the container exits 64 with a sentence if it is unset. In compose, the API service, e.g. `http://wheeld:8080` |
| `WHEEL_AUTH_MODE` | `local` |
| `WHEEL_PUBLIC_ORIGIN` | none — set it behind the proxy, e.g. `https://wheel.example.com` |

The build context is the repo root, like the other images; `docker/Dockerfile.web.dockerignore`
narrows it to `web/` (BuildKit reads it in place of the root `.dockerignore`, which excludes
`web/`). Terminate TLS in front of the container and set `WHEEL_PUBLIC_ORIGIN`, or the cookie is
written without `Secure` and the CSRF check compares `Origin` with the container's own address.
Do not publish port 3000 beyond the proxy.

## Behind a TLS-terminating proxy — one box, Caddy in front

The layout the VPS kit (`infra/vps/*`) builds:

```
                    ┌── /v1/*, /p/* ──▶ wheeld   (binds loopback; the API and public ingress)
https://domain ─ Caddy
                    └── everything else ──▶ wheel-web :3000 ──WHEEL_API_URL──▶ wheeld
```

| Setting | Compose | Bare metal |
|---|---|---|
| `WHEEL_API_URL` | `http://wheeld:8080` | `http://127.0.0.1:8080` |
| `WHEEL_PUBLIC_ORIGIN` | `https://<domain>` | `https://<domain>` |
| `WHEEL_AUTH_MODE` | `local` (the image default) | `local` (the `npx` default) |

- **Every web route is under `/api/`, `/app`, `/sign-in`, `/sign-up`, `/version.json` or a static
  path.** None is under `/v1` or `/p`, so Caddy's split cannot shadow one.
- **Set `WHEEL_PUBLIC_ORIGIN`** rather than `WHEEL_TRUST_PROXY`: it does not depend on header
  hygiene, and it closes DNS rebinding. (`WHEEL_TRUST_PROXY=1` also works behind Caddy, which
  overwrites `X-Forwarded-*` from clients by default — but only if port 3000 is reachable through
  Caddy alone.)
- **SSE through Caddy.** The events stream is `text/event-stream` with `cache-control: no-cache,
  no-transform` and `x-accel-buffering: no`, and it writes a heartbeat the moment it opens and
  every 15 s. Caddy flushes event streams immediately; nothing needs configuring.
- **The browser can reach `/v1` on this origin too**, since Caddy routes it to wheeld, and it will
  send the web's session cookie there. That is harmless only because the API authenticates by
  `x-auth-token` and never by cookie — which must stay true.
- `project.ingress_base_url` should be `https://<domain>/p/<id>` — wheeld's `PUBLIC_BASE_URL`.

## What API has to do

Nothing new is required: the events route already accepts `x-auth-token` as a header.

- `project.ingress_base_url` must be the PUBLIC address of ingress (`PUBLIC_BASE_URL`), because the
  web no longer knows any API address to build one from. Until the API sends one, the panel says
  the URL appears once the project has started.
- CORS for the web origin can be removed once this ships. `access-control-expose-headers:
  retry-after` no longer matters to the web either: the server reads `retry-after` and passes it on.

## If the operator moves to Clerk or Privy later

The provider is one setting. Set `WHEEL_AUTH_MODE=clerk`, add `NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY`
(`pk_…`, needs a rebuild) and `CLERK_SECRET_KEY` (`sk_…`, **server only — never prefix with
`NEXT_PUBLIC_`**), and in Clerk enable email/password plus Google and GitHub, set the paths to
`/sign-in` and `/sign-up`, and add the production domain and `https://*.vercel.app` to allowed
origins or preview builds will not mint a session. The API switches to `AUTH_MODE=jwks` with the
provider's issuer and JWKS URL. `/sign-in` and `/sign-up` serve whichever provider is configured,
so no URL moves and no bookmark breaks.

## Running two modes side by side locally

`NEXT_DIST_DIR` gives a second dev server its own build directory, so a local-mode instance can
run beside the operator's mock-mode one instead of corrupting its `.next` cache:

```
MOCK_PORT=8788 pnpm mock
NEXT_DIST_DIR=.next-local WHEEL_AUTH_MODE=local \
  WHEEL_API_URL=http://127.0.0.1:8788 pnpm exec next dev --port 3200
```

The mock implements `/v1/auth/signup|login|logout|me` with a seeded account
(`dev@wheel.dev` / `wheel-dev-password`), a real 5-strike lockout with `Retry-After`, and
identical answers for a wrong password and an unknown email.

## Releases are fired by CI, not inferred from the deployed commit

`vercel.json`'s `ignoreCommand` can only inspect the commit Vercel is deploying. On a shared main
that commit is usually **not** the release commit — another lane lands on top within minutes, that
commit's version equals its parent's, and the release is skipped. It then sits on main deployed to
nobody, and the only way to notice is to go looking. That is what happened to 0.4.1.

A push RANGE does not have that problem, so CI decides:

```yaml
# .github/workflows/ci.yml — on push to main (owner: QA)
- name: Deploy web when the version changed
  if: github.ref == 'refs/heads/main'
  env:
    VERCEL_DEPLOY_HOOK: ${{ secrets.VERCEL_DEPLOY_HOOK }}
  run: web/scripts/deploy-if-released.sh "${{ github.event.before }}" "${{ github.sha }}"
```

`web/scripts/deploy-if-released.sh` compares `web/package.json`'s version at each end of the push
and fires a Vercel deploy hook only when it changed. It **fails the build** if the version changed
and `VERCEL_DEPLOY_HOOK` is unset — a release that silently does not ship is the bug being fixed, so
that case must be loud.

**Sequencing — do not reorder these:**

1. Operator creates a Vercel deploy hook and stores it as the `VERCEL_DEPLOY_HOOK` repo secret.
2. QA lands the CI step above.
3. A release proves the hook fires (`/version.json` reports the new version).
4. **Only then** set `git.deploymentEnabled` to `false` for `main` in `vercel.json`.

Doing step 4 early leaves nothing deploying at all: git triggers off, hook not yet wired. Until
step 3, both paths are live and a release may build twice — wasteful, but visible, which is the
right side to fail on while the two halves are being connected.
