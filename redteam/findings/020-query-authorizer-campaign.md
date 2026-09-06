# 020 — `wheel query` rusqlite authorizer (read-only SQL scoped to one table): RESISTED 36/36

- **Severity:** Informational (no vuln). Verified-secure record for the SQL surface I pre-reviewed
  (`redteam/reviews/table-query-authorizer-testplan.md`).
- **Owner:** SDK/Engine (`crates/wheel-engine/src/db/tables.rs` `query`/`authorize`, `api/cli_routes.rs` `query`).
- **Status:** VERIFIED RESIST — live vs `wheel-engine:dev` @ HEAD (image 22:38Z, post-dates the query code).
  PoC: `redteam/pocs/table-sql/run_query_campaign.sh` + `t_query_authorizer.py`. Container removed.
- **Boundary:** TB4/TB5 (an untrusted agent's arbitrary SQL against the shared sqlite file).

## Result — 36/36 resisted
Legit `SELECT * FROM t_self` and `count(*)` succeed (authorizer isn't just "deny all"). Every attack denied:
- **ATTACH** (1a/1b) → "only one statement per query" / not authorized.
- **PRAGMA** statement forms (table_info/database_list/query_only=OFF/writable_schema=ON) → not authorized;
  **and the table-valued `pragma_*` function bypass I flagged (2e/2f)** (`SELECT * FROM pragma_table_info(..)`,
  `pragma_database_list`) → "access to pragma… prohibited". The Read on the pragma vtable is caught.
- **Cross-table reach** (JOIN, FROM-subquery, WHERE-subquery, correlated subquery, CTE, UNION, `main.t_other`,
  `t_other AS t_self`, `sqlite_master`, `sqlite_schema`, `t_self, sqlite_master`) → all "access to t_other/
  sqlite… prohibited". The scope is enforced per-`Read` in the authorizer, so subqueries can't slip past.
- **Writes** (INSERT/UPDATE/DELETE/DROP, even on the own table) → not authorized (read-only conn + authorizer).
- **Trigger/view** (CREATE TEMP TRIGGER, CREATE TEMP VIEW over t_other) → refused.
- **Extensions/functions**: `load_extension` → denied by name; `readfile`/`writefile` → "no such function"
  (not compiled into the bundled build).
- **Case-insensitive reject** (`T_OTHER`, `sqlite_MASTER`) → denied (identifiers are case-insensitive; the
  allow rule matches case-insensitively, so odd case can't dodge it).
- **Stacked statements** (`SELECT 1; DROP…`, comment-led `/* */ ATTACH…`) → "only one statement per query"
  (refused, not silently ignored).
- **DoS**: recursive-CTE row-bomb returns a BOUNDED 200 capped at `MAX_ROWS=10_000` (rows fetched one at a
  time and counted, so a cartesian/recursive expansion costs 10k rows, not all); a `progress_handler`
  enforces a 5 s `QUERY_TIMEOUT`; `randomblob(1e9)` → "a single value exceeded 8 MiB" (`set_limit`); total
  response capped at 16 MiB. No OOM/hang.

## Why it holds (credit to SDK's design)
`Connection::open_with_flags(SQLITE_OPEN_READ_ONLY)`; a **default-DENY** authorizer (`Select`/`Read`-of-the-
one-table/`Recursive`/`Function`-except-`load_extension` allowed, everything else `Deny`, so a future sqlite
action can't silently widen it); `has_extra_statement` refusal; `stmt.readonly()` gate; per-value
`set_limit`; `MAX_ROWS` as a fetch cap (not a post-hoc truncation); `progress_handler` deadline. The user
SQL runs on its own connection, never holding the engine's writer, so a slow query can't stall delivery.

No changes recommended. This surface is ready for the tool-importer work that builds on it.
