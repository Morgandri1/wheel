# Red-team report — Wheel

System of record (§3c#15): this file + `redteam/findings/*` + `redteam/pocs/*`. Re-read on rebase; messages are
only notifications. Last updated: post-M1.5/M1.6 — the stack boots (docker + process backends) and is deployed
(Railway + Vercel). Most findings are now CONFIRMED by run or CLOSED by verified fix, not design review.

## The one theme, stated first (PM's, and it is the right frame)
**The convincing half is built; the load-bearing half is not.** Again and again the *visible* mechanism is
present and often careful — an SSRF classifier, a query re-check, per-project uids, an envelope escaper — while
the load-bearing half underneath is missing or mis-wired:
- **048** — the SSRF classifier is sound (045), but the network SEGMENTATION it sits within (§5b, finding 003)
  is not deployed: wheel-host is co-located with Postgres+wheel-api, so a sandbox reaches both. And a raw
  script's sockets bypass the SSRF denylist entirely (it guards tool/mcp URLs only). Convincing half: the
  denylist. Load-bearing half: the network. Missing.
- **047** — the query/tool_call re-check is present (`fe8bdcf`/`698cbe8`) and reads as if it closes the
  revocation window, but `require()` validated the caller's wire SNAPSHOT, not live state — so it re-checked
  request-START, not disclosure. Convincing half: the re-check. Load-bearing half: reading live. Was absent;
  now FIXED + CLOSED.
- **037/F007** — per-project uids isolate tenants (verified live), but per-NODE isolation is absent: within a
  project every agent/script shares one uid, so a sibling reads another node's 0600 token file and can
  impersonate it (pm included). Convincing half: uids exist. Load-bearing half: per-node. Missing.
The corollary discipline, applied to everyone INCLUDING me: measure against the claim, never trust it. My own
048 report first asserted egress was "architecturally segmented" — reasoned from §5b's intended topology, not
measured; PM measured and it was the opposite. The measurement wins. Every real defect this campaign came from
running the thing, not reading the thing that describes it.

## Open / load-bearing findings (by severity; full detail in findings/)
| # | Sev | Title | Owner | Status |
|---|-----|-------|-------|--------|
| 003/037 | Critical | Single-uid: per-NODE isolation absent → same-uid sibling reads engine environ (WHEEL_ENGINE_SECRET/VAULT_KEY), token files, vault-exported creds; can impersonate any node | SDK+API | CONFIRMED live (same-uid /proc read; cross-tenant DENIED). Fix = per-node uids (M2/M3). GATES script-exec (impersonation, gate 1). |
| 048 | High | §5b network isolation not deployed: wheel-host co-located with Postgres+wheel-api → sandbox reaches postgres:5432 & wheel-api:8080; SSRF denylist is tool/mcp-only, does not constrain a raw script | API/infra | CONFIRMED (PM host-context measurement). Fix = host in own Railway project + topology deploy-check. GATES script-exec (egress, gate 2). |
| 043 | Critical→mitigated | Unauthenticated endpoint wired to an agent = internet → capable-agent (wheel-dev telegram was mode:none; PM demonstrated live) | SDK/Web | Rotation + Bearer remediation issued; systemic fix = warn at the config (endpoint auth:none + send-wire-to-agent). |
| 006 | High | Capability delegation (grant/place/manage) attenuation must hold | SDK+API | OPEN (§3e, M3). |
| 009 | High | Node-config validation collapsed to one layer, that layer under-tested | SDK | OPEN/partially closed — re-check coverage. |
| 044 | Low | query authorizer Function arm allow-by-default (safe by build flag, not by authorizer) | SDK | FIXED (deny-by-default allowlist + invariant test). |
| 045 | Low | SSRF classifier omits host's own public addresses (§3d(4) deviation) | SDK | OPEN (mitigated; enumerate own addrs + test). |
| 039 | Medium | Ingress rate-limit not per-caller (x-wheel-client-ip never set) | API+SDK | OPEN. |
| 040 | Low | catch_unwind quarantine belt untested → panic=unwind unproven | SDK | FIXED (PoisonDriver test, mutation-checked). |

## Closed / resisted highlights (verified, not asserted)
- **047** CLOSED — live-wires `require()` fix + CI regression (revoke-between-two-checks-on-one-Caller denies).
- **035/034** CLOSED — poison-pill escaper fixed + `catch_unwind` quarantine (040) tested; internet→dead-board
  chain closed at the single sink; ingress routes through it.
- **036** CLOSED — git credentials out of band (GIT_ASKPASS from env, no argv/URL, symlink-safe repair).
- **Token lifecycle** SOUND (live): forge infeasible (256-bit+sha256), rotate-on-start, use-after-delete
  cascade (foreign_keys ON, test-guarded), use-after-rewire live, rename id-keyed, 0600-file-not-env.
- **Cross-tenant fs/socket/environ** SOUND (live two-project process backend): distinct per-project uids,
  0700 dirs, 0600 sockets; host secret root-owned in pid-1 environ, denied to a project uid.
- **wheel query** SOUND (wire-gated + deny-by-default authorizer, one table, no ATTACH/load_extension/cross-table).
- **auth/JWT** SOUND (alg=none/confusion rejected, single-alg pin, kid-flood throttled, non-RSA JWKS skipped).
- **API tenancy + proxy/ingress** RESISTED (owner-predicate load, header scrubbing, no existence oracle).

## Top-3 recommendations
1. **Gate M2 script-execution on the two hardening items, don't ship it ahead of them** (037/038 = gate 1
   impersonation; the egress PoC / 048 = gate 2). A board where a script runs as pm's uid and reaches Postgres
   is not one you turn script-exec on. (PM has wired both gates onto the script-exec ticket — hold that line.)
2. **Land per-node uids** (037) with the cross-uid `/proc`/token-file EACCES tests — the runtime foundation of
   the whole per-node wire/grant model, and the load-bearing half of the isolation story that convinces on the
   surface today. Until it lands, redaction and 0600 perms are accident-guards, not boundaries.
3. **Deploy the network segmentation §5b/003 specifies** (048): wheel-host in its own Railway project, plus a
   deploy check that asserts the host cannot resolve/reach postgres/wheel-api `.railway.internal`. Reachability
   should fail the deploy, not be discovered by a probe. If co-location is unavoidable short-term, MEASURE
   per-uid egress filtering rather than assume it.

## Meta: what worked
Running beat reading every time — the escaper poison (034), the same-uid `/proc` read (037), the ephemeral
root cause (041), 047 (both the finding and catching my own probe's false-alarm before reporting it), 048 (PM's
measurement over my architecture). And "record which engine a finding ran against" (SDK's /healthz build)
distinguished a real defect from a stale image on the very first re-verify. The dogfood reframe surfaced these
before the wake, not after.
