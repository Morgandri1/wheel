# Deploying web/

Vercel project **root directory = `web/`**. Everything else is defaults; `vercel.json` pins the
package manager and adds the response headers.

## What the operator has to supply

| Variable | Where it comes from | Scope | Notes |
|---|---|---|---|
| `NEXT_PUBLIC_AUTH_MODE` | set to `clerk` | all | `mock` and `dev` are for local work only. Anything other than `clerk` on a public deploy means no sign-in at all. |
| `NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY` | Clerk dashboard → API keys | all | Starts `pk_test_` / `pk_live_`. Public by design. |
| `CLERK_SECRET_KEY` | Clerk dashboard → API keys | all | Starts `sk_`. **Server only — never prefix with `NEXT_PUBLIC_`.** |
| `NEXT_PUBLIC_API_URL` | API's Railway domain, later `https://api.wheel.dev` | all | No trailing slash. |

`NEXT_PUBLIC_*` values are baked into the client bundle at build time, so changing one needs a
redeploy, not just a restart.

## What the operator has to do in Clerk

1. Enable email/password, Google and GitHub.
2. Paths: sign-in `/sign-in`, sign-up `/sign-up` (the routes exist at those paths).
3. Add the Vercel domains — the production domain and `https://*.vercel.app` for previews —
   to Clerk's allowed origins, or the session will not mint on preview builds.

## What API has to do

CORS must allow the Vercel origin, or the browser blocks every call before it reaches our code:

```
CORS_ALLOWED_ORIGINS=https://<production-domain>,https://<preview>.vercel.app
```

with `x-auth-token` and `x-project-id` in allowed headers (both already are), and
`access-control-allow-origin` actually emitted — an unset allowlist matches nothing and silently
sends no origin header at all, which is what we hit locally.

## The one cross-origin thing worth checking first

Realtime needs `POST /v1/projects/:id/ws-ticket` to succeed from the browser, then the socket
opens at `.../engine/v1/events?ticket=…`. A browser cannot set headers on a WebSocket handshake,
so the ticket is the only way the socket authenticates. If the board loads but never goes live,
check the ticket call in the network tab before suspecting the engine: a socket that opens and
stays silent is an auth failure, not an engine failure.
