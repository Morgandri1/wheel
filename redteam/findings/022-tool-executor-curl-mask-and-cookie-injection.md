# 022 — Tool executor: (1) curl mask misses query/path secrets; (2) cookie-value injection

- **Severity:** Medium ×2. Owner: **SDK/Engine** (`crates/wheel-engine/src/tools/execute.rs` @ 1f1d5e0).
- **Status:** FIXED & VERIFIED e2e. Fixes shipped with the routes (@ 6c371c7): `curl_for.mask` now also
  replaces `encode(secret)` (execute.rs:234) so query/path secrets mask; cookie values are `encode()`d
  (execute.rs:142, comment cites "ADVERSARY 022/2"). Live via `POST /v1/tools/:id/call` dry_run:
  vault query -> `key=<redacted>`, static query -> `tok=<redacted>`, cookie -> `sid=x%3B%20admin%3D...`
  (';' encoded, no injection). Was CONFIRMED on the pure layer (no route yet then). PoC (verbatim-source port,
  runs the actual `encode`/`mask`/cookie-join logic): `redteam/pocs/tool-exec/t_curl_mask_and_cookie.mjs`
  → exit 1. Boundary TB7 (tool nodes / fills), the save_to_vault-class credential surface.
- These are the two SDK asked me to hunt: "a placement where a secret survives into the curl string"
  (the one they "most want"), and "structure out of a value … via a cookie."

## Finding 1 — a query/path static|vault secret SURVIVES (encoded) in the curl string
`build_request` percent-encodes query values (`execute.rs:157` `encode(v)`) and path values
(`:133` `encode(value)`) into `p.url`. `curl_for.mask()` (`:216-224`) redacts by replacing each **raw**
secret string in `p.url` (`:246`). When a `static`/`vault` fill is placed in a **query or path** param and its
value contains ANY non-unreserved char (`/ + = : @ ...` — common in base64/api keys/basic-auth), the URL holds
the **percent-encoded** form while `secrets` holds the **raw** form → the replace finds nothing → the secret
is rendered into the curl string, percent-encoded and trivially `urldecode`-able.
```
query url in curl : https://api.example.com/data?key=sk%2Flive%2Babc%3D%3D      <- secret "sk/live+abc==" NOT masked
path  url in curl : https://api.example.com/t/sk%2Flive%2Babc%3D%3D             <- same
header in curl    : x-api-key: <redacted>                                       <- header IS masked (not encoded) => gap is encoding-specific
```
**Impact:** the whole point of `curl_for` masking is that the string is safe to paste/screenshot/log; a
query- or path-placed credential defeats that. And if the engine surfaces this rendering to the agent
(`wheel tool call --curl` / dry_run), the agent obtains a secret it was never allowed to see — an agent
info-leak (the finding-012 class, now in the engine). Contract §3d rule 3: `--curl` must render static/vault
values **masked**.
**Fix:** mask the encoded form too. In `mask()`, for each secret also replace its percent-encoded spelling:
`out = out.replace(secret, "<redacted>"); out = out.replace(&encode(secret), "<redacted>");`
(or build the curl URL from already-masked components rather than from the finished `p.url`). Add a test with
a secret containing `/ + =` used as a query fill AND a path fill.

## Finding 2 — agent cookie values are not encoded → cookie injection ("structure out of a value")
Cookie values are pushed raw (`:137`) and joined with `"; "` in both `send()` (`:286-287`,
`format!("{k}={v}")` → `req.header("cookie", jar.join("; "))`) and `curl_for` (`:234`). An agent-visible
cookie param whose value is `x; admin=true; role=root` produces the header:
```
Cookie: sid=x; admin=true; role=root      <- two attacker cookies injected
```
The `;` separator is a legal header-value char, so reqwest's `HeaderValue` does NOT reject it (unlike CRLF).
Query and path are encoded to stop exactly this (`x&admin=true` → `x%26admin%3Dtrue`), but cookies were
missed. An agent can add or shadow cookies on the request to the operator's tool endpoint (e.g. flip a
`role`/session cookie). Contract §3d claim 2 / SDK's own statement: getting structure out of a value via a
cookie is a finding.
**Fix:** percent-encode cookie values (cookie-octet: at minimum `; , SP % " \\` and control chars), the same
way query values are encoded — in BOTH `send()` and `curl_for`. Reject/encode CR/LF too (belt).

## Verified-strong in the same review (credit SDK) — NOT findings
- **Fill refusal** (`:75-90`): an arg naming a `static`/`vault`/`hidden` field is REFUSED (not ignored),
  and an invented field is refused. Exact-name match, so a case-variant is refused as "not a field" (an
  agent cannot register a colliding param). Claim 1 holds.
- **Path/query encoding** (`:133/157`): `../../admin` → `..%2F..%2Fadmin`; `x&admin=true` → `x%26admin%3Dtrue`.
  Claim 2 holds for path and query.
- **Header CRLF:** headers go through `req.header(k, v)` → reqwest typed `HeaderValue`, which rejects control
  chars, so a `\r\n` value errors at send rather than injecting. **Reasoned, not demonstrated** (no route +
  SSRF denies local targets); worth a live confirm when routes land, per SDK's "rather broke than assumed."
- **SSRF (`send`/`resolve_and_check`):** literal/suffix `host_is_denied` first, then `lookup_host` once +
  `reqwest ...resolve(host, addr)` pins the connection to the validated address (rebinding), redirects
  followed MANUALLY (`Policy::none()`) and re-checked per hop, `MAX_REDIRECTS`, and the body/credentials are
  sent on the **first hop only** (`:292-296`) so they never follow an off-origin redirect. Streamed response
  cap (`:337-346`), not content-length-trusting. Strong; full live verification pending the HTTP route (must
  still confirm resolve_and_check checks EVERY resolved address, and the v6 6to4/NAT64/Teredo embeddings from
  the pre-review testplan).

## Also flagged (Low, SDK-admitted) — body keeps agent JSON type
`original_or_string` (`:175-180`) forwards an agent body field's own JSON type, so an agent can send an
object/array where the spec implied a string (no schema validation yet). It cannot clobber a `static`/`vault`
body fill (those are separate top-level keys, `:139`), but it injects arbitrary JSON structure into the
outbound body → downstream injection (e.g. NoSQL operator objects) depending on the target API. Validate
agent body values against the parameter schema.

## Update (2026-09-06) — header-CRLF now EXPLICITLY defended (was reasoned-only); cookie fixed
Header-CRLF-at-send, which I'd flagged as reasoned-not-demonstrated (reqwest-only), is now belt-and-suspenders
(commit 054335b): `execute.rs:328` bails before setting a header if its value contains `\r`, `\n`, or `\0` —
"header {k} contains a line break or a null, which would let its value add another header" — naming the header,
on top of reqwest's own rejection. Agent cannot inject a header NAME (names come from operator-defined
`op.params`, not args). SDK's regression test `a_header_value_cannot_inject_another_header` asserts on a REAL
parsed header line (`has_header_line`), NOT a substring — correct, because an ENCODED value would substring-match
and give a false pass. Cookie values are now `encode()`d (`ParamLocation::Cookie => encode(value)`), closing
022/2. Both halves of 022 are fixed. NOTE: a live end-to-end run of the :328 bail through `/v1/tools/:id/call`
is still gated on finding 027 (no creatable allowlisted target), but the defense is explicit + unit-tested, so
this is no longer a critical gap — only the redirect-no-replay/cap/timeout e2e-via-route remain 027-blocked.

## send() ALLOWED-PATH VERIFIED LIVE (2026-09-06) — via a hostname allowlist entry
PM's insight unblocked this without the 027 fix: a HOSTNAME passes create-time validation (`host_is_denied`
is a literal/suffix check; `echo.test` is neither), is allowlisted by that literal (`WHEEL_TOOL_ALLOW_HOST=
echo.test:18080`), and resolves via `/etc/hosts` to a loopback witness server. All loopback → RoE-safe.
PoC: `redteam/pocs/tool-exec/run_allowed_via_hostname.sh` + `srv.py`. Results (image 00:55, HEAD 2a50695):
- **header-CRLF at send → REJECTED**: `X-Try: a\r\nX-Injected: pwned` → "builder error: failed to parse
  header value"; no `X-Injected` reached the witness. reqwest attempted to set the header and rejected the
  CRLF, so the header path is live and the defense holds.
- **per-hop revalidation**: a 302 to `http://169.254.169.254/` → hop 2 REFUSED ("169.254.169.254 is not a
  reachable destination"). A 302 to an unallowlisted `127.0.0.1:19999` → REFUSED. Relative redirects resolve
  against the origin (chain followed on-host).
- **redirect limit**: a 4-redirect chain → "too many redirects (limit 3)".
- **body-not-replayed on redirect**: POST with body `BODYSECRET`, 302 to a second allowlisted hop → hop-2
  witness received `method=POST, body="", headers={accept,host}` — the body did NOT follow the redirect.
  (Credential/body safety across an off-origin redirect.)
- **5 MiB cap**: a 6 MiB response → "response exceeded 5 MiB". **30s timeout**: a 35s server → errored at ~30s.
NOTE for SDK (minor, UX not security): the CRLF rejection surfaces as reqwest's opaque "builder error:
failed to parse header value" — the clearer `:328` message ("header X-Try contains a line break or a null…")
does NOT surface, so reqwest's `.header()` preempts the explicit check. If the clear message is wanted, run
the `:328` scan before `req.header()`. Defense is intact either way.
