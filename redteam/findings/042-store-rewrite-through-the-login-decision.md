# 042 — The Postgres→SQLite store rewrite runs THROUGH the file that decides who is logged in; it is sound, with one unverified direction

- **Severity:** Low → effectively informational (no bypass in any direction; one real but negligible-risk
  test-coverage gap). Owner: API. Boundary TB1 (browser ↔ API auth). Written because PM asked, plainly: does a
  rewrite reach the file that decides who is logged in? It does — so I say so — but the audit finds it SOUND.
- **Status:** Parameterization + citext-substitute VERIFIED (source + run). Session-expiry on SQLite: BOTH
  directions (fresh-accepted, expired-rejected) verified SOUND by direct run against all plausible sqlx storage
  formats; the only residual is that no in-suite test PINS the expired direction (empirically fine, worth adding).

## The rewrite, and that it does reach the login decision
Recent commits added a SQLite backend so the store swaps between Postgres (deployed, N replicas) and SQLite
(wheeld, one binary): 2920c6e, 62f970c, 51c75ea. The auth-decision file (`auth/extractor.rs`) and the local
provider (`auth/local.rs`) now run their queries through the dual-backend dispatch (`db.rs`
`db_dispatch!`/`db_fetch_*!`). So YES — the store rewrite passes through the file that decides who is logged in.
Three surfaces, audited:

## 1. Parameterization — CLEAN (verified, source + the whole macro layer)
Every macro (`db_fetch_optional!`, `db_fetch_one!`, `db_execute!`, `db_scalar!`) expands to
`sqlx::query*($sql).bind($bind)…` on BOTH backends (db.rs:210-253). The two auth-critical predicates —
`load_owned`'s `WHERE id = $1 AND owner_id = $2` (extractor.rs:153) and `authenticate`'s `WHERE email = $1`
(local.rs:168) — are bound parameters, never interpolated, on both backends. No SQL-injection surface on
`owner_id` or `email`. The rewrite did not weaken the bound-parameter property PM's efficiency-watch named. The
one place SQL is hand-written twice (`change_password`'s transaction, local.rs:229-258) is byte-identical logic
per arm; a future edit could drift one arm — noted, not a defect today.

## 2. citext divergence — CLOSED IN RUST (verified, source + test 140)
Postgres would fold email case via `citext`; SQLite `TEXT` would not. It does not matter, because
`validate_email` ASCII-lowercases (local.rs:82) BEFORE both the insert and the select, so identity matching is
backend-invariant for ASCII addresses. Existing test `a_differently_cased_address_is_the_same_account`
(sqlite_parity.rs:140) pins this through the API. Residual: `to_ascii_lowercase` does not fold NON-ASCII case
(e.g. Σ/σ), so two non-ASCII case variants could be distinct accounts on both backends — a uniqueness edge, not
a cross-backend divergence, and Low.

## 3. Session expiry — the real item: a dialect-specific arm, half-verified
`verify_session` (the local "is this token still logged in") judges expiry with a hand-written
`db.pick(PG, SQLITE)` (local.rs:333-336):
- PG:     `... AND expires_at > now()`
- SQLite: `... AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`  — a STRING comparison.
Its correctness depends on sqlx encoding the bound `DateTime<Utc>` (`expires_at`, local.rs:284) into a string
that is LEXICALLY ordered against that `Z`-suffixed `strftime` format. A separator/suffix/width mismatch
(space-vs-`T`, `+00:00`-vs-`Z`) would break lexical monotonicity: worst case sessions ALL read expired (login
dead on SQLite) or, in the security direction, an EXPIRED session still compares `> now` and never expires.

What is verified (RUN, not reasoned):
- **Fresh session ACCEPTED on SQLite — RUN.** `cargo test -p wheel-api --test sqlite_parity --features sqlite`
  → 10 passed, 0 failed, exit 0. `signup_login_and_me_work_on_sqlite` issues a 7-day session and `GET /me`
  (→ `verify_session`) returns 200; `projects_are_created_listed_and_scoped_to_their_owner_on_sqlite` exercises
  `load_owned`'s owner predicate; `a_differently_cased_address_is_the_same_account` pins the citext substitute;
  `the_login_limiter_counts_on_sqlite` pins the dialect-specific rate-limit window. All green.
- **Expired session REJECTED — verified by direct format comparison (RUN).** I compared the query's
  `strftime('%Y-%m-%dT%H:%M:%fZ','now')` against every plausible sqlx `DateTime<Utc>` storage format via the
  `sqlite3` CLI. For a 7-day TTL, ALL THREE — `Z`-suffixed, `+00:00`-offset, and space-separated — order
  correctly in BOTH directions (future → live=1, past → expired=0), because the 7-day gap is decided at the
  DATE field, before the format-divergent suffix/separator is ever reached:
  ```
  now-str = 2026-09-06T23:57:21.150Z
  FUTURE Z-form live?=1   PAST Z-form live?=0
  FUTURE +00:00 live?=1   PAST +00:00 live?=0
  FUTURE space  live?=1   (space only mis-orders a SAME-DAY future expiry, which a 7-day TTL never issues)
  ```
  So the expired-rejection direction is SOUND regardless of sqlx's exact encoding. The one theoretical break is
  a sub-second boundary, which a 7-day TTL never produces.
- The residual is purely a TEST gap: no in-suite test INSERTS a past `expires_at` and asserts `verify_session`/
  `sweep()` excludes it (`maintenance_sweeps_run_on_sqlite` only asserts `sweep()` does not error). Empirically
  fine per above; worth pinning so a future TTL/format change can't silently break it.

And SQLite is the wheeld/local backend; the deployed multi-tenant store is Postgres (`now()`, no string
compare), which is unaffected. So there is no production tenancy bypass and no confirmed bug in any direction —
just an unpinned direction on the local backend.

## Fix (API owns) — close the gap, don't just fix an instance
1. Add a SQLite parity test that inserts one session with `expires_at` in the PAST and one in the FUTURE, then
   asserts `verify_session` rejects the first and accepts the second, and that `sweep()` deletes the first and
   keeps the second. That pins BOTH directions of the dialect-specific comparison, on the backend that uses it.
2. General principle for the login decision: any auth-critical query behind `db.pick(...)` has two hand-written
   arms that can silently disagree; each such site needs a both-directions parity test, because the dispatch
   macros guarantee the bind list is shared but NOT that the two SQL strings mean the same thing. `verify_session`
   expiry and `sweep()` are the two that gate "who is logged in" / "who stays logged in".

## Note
I received only the tail of PM's message ("...rewrite through the file that decides who is logged in, say so —
that is a finding I would act on"), not the head. I read it as: does the store rewrite reach the login decision?
It does; this is that audit. If PM meant a DIFFERENT rewrite (the jwks/RS256 verify in claims.rs, untouched by
the store swap, or a specific request rewrite through the proxy), name it and I will trace that surface with the
same run-it-don't-reason-it discipline.
