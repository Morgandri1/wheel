# 048 — §5b network isolation is NOT deployed: wheel-host shares one Railway project with Postgres and wheel-api

- **Severity:** High (the network segmentation the threat model relies on is absent; reachability PROVEN, only
  credentials stand between it and use). Owner: API / infra (§5b deployment topology, `infra/railway/`).
  Boundary TB1/TB4b (sandbox → internal services). Measured by PM on the live deployment; I formalize it, and I
  CORRECT my own earlier claim that egress was "architecturally segmented" — see the note.
- **Status:** CONFIRMED by PM's live measurement from the wheel-host container: `postgres.railway.internal:5432`
  AND `wheel-api.railway.internal:8080` accept TCP connections. No `DATABASE_URL` on the host, so no credentials
  were found — **reachability is the finding.**

## What §5b promises vs what is deployed
§5b (deployment topology) is explicit: wheel-host runs "in its OWN Railway project (separate private network
from the API + Postgres — ADVERSARY finding 003)… private networking does not cross projects," and therefore
"Agents inside sandboxes therefore **cannot reach Postgres or API internals at all** — only the host's own
`:7100` (bearer-gated) and the public internet." That guarantee is the network layer of the containment story.

The DEPLOYED topology does not match: wheel-host is in the SAME Railway project as Postgres and wheel-api, so
their private-network names resolve and their ports accept connections from the host container. The
segmentation §5b describes — and finding 003 required — is not in place.

## Impact
The network layer that was supposed to contain a compromised sandbox is gone. Anything that gets code
execution in a sandbox on this host (a prompt-injected or malicious agent — the whole §2 premise — once
agent/script execution is exercised; script exec is not wired on 0b81bc0, finding 047-adjacent) has raw TCP
reachability to:
- `postgres.railway.internal:5432` — the tenant database. Reachability without creds is not yet read access,
  but it removes the segmentation that was the reason a sandbox could not attack Postgres at all; any credential
  leak (037's same-uid vault exposure, a mis-scoped env, a future `DATABASE_URL` on the host) becomes a direct
  DB path, and an unauthenticated/again-CVE Postgres is now in reach.
- `wheel-api.railway.internal:8080` — the API's INTERNAL port, which may expose host/admin or inter-service
  endpoints not hardened to the same degree as the public edge (the public edge assumes only trusted callers
  reach the internal port). A sandbox reaching it is exactly the confused-deputy surface §5b removed by design.
The mitigant today is only "no creds found on the host" — i.e. the exposure is gated by credential secrecy, not
by network isolation, which is precisely the posture §5b/003 was written to avoid.

Note the ADJACENT boundaries that DO hold (PM measured, and my process-backend probe agrees), so this finding is
scoped to the network layer only:
- Host secrets (`RAILWAY_API_TOKEN`, `GH_TOKEN`, `WHEEL_HOST_SECRET`) live in pid 1's environ, root-owned;
  uid 21088 is DENIED `/proc/1/environ`. The host secret is not exposed to a sandbox uid via /proc.
- Per-project uids differ → cross-tenant filesystem/socket/environ isolation holds (my two-project probe).
- The absent per-NODE boundary is 037 (intra-project, same uid), a separate finding.

## Fix (API / infra)
Deploy wheel-host in its OWN Railway project, separate private network from Postgres + wheel-api, as §5b and
finding 003 specify — so `*.railway.internal` for Postgres/API does not resolve from the host at all. Until
then the interim posture must not pretend the network isolates: keep no `DATABASE_URL`/API-internal creds on the
host (already the case — the reason PM found no creds), and treat the sandbox→internal path as open. If
same-project deployment is unavoidable short-term, per-uid egress filtering (nftables `owner` match / per-project
netns — the §5b capability spike) is the only substitute, and its presence/absence should be MEASURED, not
assumed. A deploy check should assert wheel-host cannot resolve/reach `postgres.railway.internal` /
`wheel-api.railway.internal`, so a regression in topology fails the deploy rather than silently re-opening this.

## Note — my correction, owed
In my prod-pass report I wrote that raw egress to Postgres/API was "denied by network SEGMENTATION,
architecturally (§5b separate Railway networks)." That was REASONED FROM THE CONTRACT (§5b's intended topology),
not measured — the exact error this campaign existed to catch, turned on me. My throwaway-project egress probe
could not run (script execution unimplemented on 0b81bc0), so I fell back to the architecture instead of holding
the question open, and the architecture describes the intended deployment, not the actual one. PM measured the
real host container and it is the opposite: co-located, reachable. The measurement wins; my claim was wrong.
This is why the egress question needed a run (or PM's host-context measurement), not a reading — and why the
egress PoC stays wired to the M2 script-execution ticket so the AGENT-uid reachability is measured too, not
inferred from the host's.
