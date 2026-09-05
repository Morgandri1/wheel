# 014 — Project lifecycle not serialized: `start‖delete` returns 500 (possible orphaned sandbox)

- **Severity:** Low–Medium (S3: deterministic 5xx + possible host-side resource/secret orphan)
- **Owner:** API (`crates/wheel-api` projects lifecycle + `crates/wheel-host`)
- **Status:** CONFIRMED (live, infra/dev, WHEEL_ENV=dev, 2026-09-05). PoC:
  `redteam/pocs/api-tenancy/t_lifecycle_toctou.py`.
- **Boundary:** TB3 (API ↔ sandbox lifecycle / secret custody).

## Claim
Concurrent `POST /v1/projects/:id/start` and `DELETE /v1/projects/:id` for the same project is not
serialized. The start handler **deterministically** returns `500 {"code":"internal","message":"An
unexpected error occurred."}` while the delete returns 204 and the project row ends deleted (final GET
→ 404). 5/5 runs identical.

```
run0..4: start=500  delete=204  final_GET=404  {"code":"internal",...}
```

## Why it matters
1. **Unhandled race, not a clean refusal.** The correct outcome is a definite 409 (busy) / 404
   (gone), not an internal 500 — a 500 means the handler hit an unexpected state (e.g. the project row
   vanished mid-start), i.e. lifecycle ops share no per-project lock/serialization.
2. **Possible orphaned sandbox (unconfirmed — needs host-side check).** If `start` had already asked
   the host to spawn the engine when `delete` removed the project row, the host sandbox/engine may be
   left RUNNING with no owning row — a resource leak AND a live engine still holding its
   `WHEEL_ENGINE_SECRET` + vault key in memory with nothing tracking it for teardown. I cannot see the
   host's sandbox table from the API surface (`:7100` is bearer-gated), so this is flagged as a
   hypothesis to confirm host-side, not asserted.

## What is NOT broken (checked same run)
- Concurrent 4× start is idempotent (all 200, final running).
- Use-after-delete: proxy to a deleted project's engine → 404 (never reaches a live engine).
- start/stop/restart churn stays coherent (no 5xx, definite final state).

## Proposed fix (API)
Serialize lifecycle transitions per project (a per-project async lock or a DB row-state guard:
`stopped→starting→running→stopping→stopped`, with `deleting` terminal). `start` on a deleting/deleted
project → 409/404, never 500. On `delete`, tear down the host sandbox idempotently AFTER marking the
row, and have the host reconcile-sweep any sandbox whose project row is gone (belt-and-braces against
exactly this orphan). Add the host-side orphan check to confirm/deny item 2.
