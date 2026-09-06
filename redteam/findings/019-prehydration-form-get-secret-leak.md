# 019 — Pre-hydration native form submission leaks secrets to the URL (sign-in) + full form sweep

- **Severity:** was **HIGH (S1-class)** for the sign-in form (a password in the URL → history, Referer,
  edge logs); **RESOLVED** across every form. Sweep result below.
- **Owner:** Web. Origin finding + sign-in fix by Web (7b1af52); remaining forms hardened live during this
  sweep (HEAD bd38a95).
- **Status:** VERIFIED RESOLVED by code audit @ bd38a95. Boundary TB8 (Web).

## The class
A React form rendered at first paint can be submitted BEFORE hydration runs `e.preventDefault()`. The
browser then performs a NATIVE submission using the form's `method` (default **GET**) and `action`
(default = current URL). With no `method`, every named field — including a `type="password"` — lands in
the **query string**: browser history, the `Referer`, and Vercel/edge access logs. Web hit this on sign-in.

## Sweep — every `<form>` and every secret-bearing input in `web/src`
**All four forms now declare `method="post"`** (a pre-hydration submit becomes a POST body, never a URL):
| form | file | secret field? | method="post" |
|------|------|---------------|---------------|
| Sign-in / Sign-up | `auth/auth-screen.tsx` | password | ✅ (+ Web's no-submit-before-mount guard) |
| New project | `app/app/page.tsx` | none (name only) | ✅ (dialog-gated, not first-paint) |
| Tool base_url | `board/canvas.tsx` | none (public URL) | ✅ (state-gated) |
| Vault key/value | `inspector/vault-panel.tsx` | **vault secret value** | ✅ (+ inputs carry NO `name`; value is `type=password autoComplete=off`) |

**Secret inputs that are NOT in any form** — auth code / setup-token / api-key (`inspector/auth-flow.tsx`),
OAuth (`inspector/oauth-panel.tsx`): these have **no `<form>`**, and their only render ancestor
(`inspector/index.tsx`, the inspector shell) has no `<form>` either, nor does the board page. A lone input
outside any form has no implicit/native submission, so there is **no pre-hydration GET path** for those
credentials at all (stronger than method=post — there is simply nothing to submit). They POST via
`fetch`/`agent.authComplete` with the token in the `x-auth-token` header.

**Password change:** no dedicated UI present in `web/src` (M3 per contract) — nothing to sweep yet.

## Other checks PM asked for
- **Session token in a URL:** never. It rides in the `x-auth-token` header (`lib/api.ts`); the token never
  appears in a URL, Referer, or WS path. The only sensitive value in any URL is the ws-ticket (by design,
  §5) — single-use, 30 s, (user,project)-bound, hashed at rest (finding 010 update; F010 verified).
- **Referrer-Policy:** `strict-origin-when-cross-origin` (`vercel.json`, all routes). Correct — cross-origin
  Referer carries only the origin, never path/query. NOTE: this is not what fixes the GET-leak (`method=post`
  is); it's the second layer that keeps any URL value from reaching a third party. CSP `connect-src` is
  API-only, so no third-party subresource exists to leak a Referer to anyway.
- **API logs query strings?** No. `proxy.rs`/`ingress.rs` read `req.uri().query()` only to FORWARD it
  upstream; the `tracing::warn!` calls log `error` only, never the uri/query. The ws-ticket is not written to
  our application logs. (Platform edge logs may record request lines; the ticket there is single-use/30s/
  hashed → dead on capture.)

## Verdict
Sign-in S1 fixed; the whole form surface now has the dangerous GET default unreachable. No residual
disclosure path found. `method="post"` is sufficient for the security property (non-disclosure); the
hydration guard is a UX/defense-in-depth measure, warranted on the first-paint auth form (done) and optional
on the dialog/state-gated others (their residual is lost input, not disclosure).
