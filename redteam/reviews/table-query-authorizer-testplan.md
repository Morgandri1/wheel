# Test plan — `wheel query` / rusqlite authorizer (read-only SQL scoped to one t_<name>)

For SDK/Engine, BEFORE ship. Surface: an **untrusted agent** sends arbitrary SQL via
`wheel query <table> "<SELECT…>"` → `POST /v1/cli/query` (and the UI's `POST /v1/tables/:id/query`).
The only control that matters is the rusqlite **authorizer** (a keyword/regex prefilter is bypassable and
must NOT be the sole gate). Design intent: allow read-only access to exactly one table `t_<name>` in
`main`, deny everything else. Every item below is "attacker SQL → the authorizer action it must trip →
expected DENY → the implementation bug it catches."

## Authorizer must be DEFAULT-DENY
The callback must return `Deny` for any `AuthAction` not explicitly allowed — not `Allow`. If it allow-lists
a few actions and falls through to `Allow`, every future/unlisted action (and the ones below) slips. First
assertion: a no-op action we don't expect (e.g. `SQLITE_TRANSACTION`, `SQLITE_SAVEPOINT`) is denied unless
deliberately allowed. Allowed set should be roughly: `SQLITE_SELECT`, `SQLITE_READ` (only when
`arg1==t_<name>` AND `db_name=="main"`), and a curated `SQLITE_FUNCTION` allowlist. Everything else Deny.

## SDK's four named questions
### 1. ATTACH
- `ATTACH DATABASE '/data/wheel.db' AS x; SELECT * FROM x.vault_values` — must trip `SQLITE_ATTACH` → DENY.
- `ATTACH DATABASE '/data/wheel.db' AS x; SELECT * FROM x.t_<name>` — same-file re-attach to escape the
  `db_name=="main"` scope (see §authorizer-context). DENY at `SQLITE_ATTACH`.
- `ATTACH ':memory:' AS m; CREATE TABLE m.x AS SELECT * FROM t_other` — DENY at ATTACH (and at the read).
- Expect: `SQLITE_ATTACH` (and `SQLITE_DETACH`) unconditionally denied. Also confirm `PRAGMA database_list`
  can't be used to enumerate attachable paths (covered in §2).

### 2. PRAGMA
- `PRAGMA table_info(t_other)` / `PRAGMA table_info(nodes)` — schema disclosure → `SQLITE_PRAGMA` DENY.
- `PRAGMA database_list` — file-path disclosure → DENY.
- `PRAGMA case_sensitive_like=…`, `PRAGMA query_only=OFF` (turn OFF read-only!), `PRAGMA writable_schema=ON`
  (then write via sqlite_master), `PRAGMA foreign_keys`, `PRAGMA journal_mode` — ALL `SQLITE_PRAGMA` → DENY.
  `writable_schema=ON` is the sharp one: it's a write-primitive to the schema. Deny SQLITE_PRAGMA wholesale.
- **Table-valued pragma functions bypass SQLITE_PRAGMA** — these are `SQLITE_READ` on a pragma vtable, not
  `SQLITE_PRAGMA`: `SELECT * FROM pragma_table_info('t_other')`, `SELECT * FROM pragma_database_list`,
  `SELECT * FROM pragma_table_list`. The scoping must reject reads whose `arg1` is a `pragma_*` /
  `sqlite_*` name, not only user tables. HIGH-value bypass — test explicitly.

### 3. CTEs / other-table reach
Each must trip `SQLITE_READ` with `arg1 == t_other` (≠ the allowed table) → DENY:
- JOIN: `SELECT * FROM t_self JOIN t_other USING(key)`
- Subquery in FROM: `SELECT * FROM (SELECT * FROM t_other)`
- Subquery in WHERE: `SELECT * FROM t_self WHERE key IN (SELECT key FROM t_other)`
- Correlated subquery in SELECT list: `SELECT (SELECT v FROM t_other WHERE t_other.key=t_self.key) FROM t_self`
- CTE: `WITH x AS (SELECT * FROM t_other) SELECT * FROM x`
- Recursive CTE reaching a table: `WITH RECURSIVE x AS (SELECT * FROM t_other …) …`
- UNION: `SELECT key FROM t_self UNION SELECT key FROM t_other`
- Qualified/aliased: `SELECT * FROM main.t_other`, `SELECT * FROM t_other AS t_self`, `SELECT * FROM "t_other"`
  (aliasing to the allowed name must NOT fool it — the authorizer sees the real `arg1`).
- **System catalogs are tables too**: `SELECT * FROM sqlite_master`, `sqlite_schema`, `sqlite_temp_master`,
  `SELECT * FROM t_self, sqlite_master`. Scoping must treat `sqlite_*` as "not the allowed table" → DENY.
  (sqlite_master leaks every other agent's `t_<name>` and the `vault_values`/`messages` schema.)

### 4. Does the authorizer hold under a subquery? — the meta-question
The bug class: an implementation that inspects the *parsed statement's top-level FROM* (or the CLI arg
`<table>`) to decide scope, instead of relying on the per-access `SQLITE_READ` callback. If so, every
subquery/CTE/JOIN above slips because only the outer table is checked. **Correct design: scope is enforced
in the authorizer callback on EVERY `SQLITE_READ`, not from the statement text or the CLI `<table>` arg.**
The CLI `<table>` arg selects which table is *allowed*; it must not be assumed to be the only one *accessed*.
Assert by: allowed=`t_self`, and confirm every t_other access above is denied even though the statement
"is a SELECT on t_self" at the top level.

## Beyond SDK's four
### Read-only actually enforced (writes)
- `INSERT INTO t_self …`, `UPDATE t_self …`, `DELETE FROM t_self`, `REPLACE INTO t_self …`,
  `INSERT INTO t_self SELECT * FROM t_self` — `SQLITE_INSERT/UPDATE/DELETE` → DENY (write, even to own table:
  `wheel query` is READ-only; writes go through `wheel write`).
- `DROP TABLE t_self`, `ALTER TABLE t_self …`, `CREATE TABLE x…`, `CREATE INDEX…` → DENY.
- **Trigger/view as stored write or read-escape**: `CREATE TEMP TRIGGER … BEGIN UPDATE t_other…; END`
  (`SQLITE_CREATE_TEMP_TRIGGER`) and `CREATE TEMP VIEW v AS SELECT * FROM t_other` then `SELECT * FROM v`
  → DENY at the DDL. (A view defers the read; verify the read of a view over t_other is ALSO denied — the
  authorizer fires `SQLITE_READ` on the underlying table with the view name in the trigger/view slot.)
- Belt-and-braces: open the query connection `OpenFlags::SQLITE_OPEN_READ_ONLY` and set `PRAGMA query_only`
  at open (from trusted code, before the authorizer hands control to the agent). Then even an authorizer gap
  can't write. Confirm both layers exist.

### Extensions / dangerous functions (`SQLITE_FUNCTION`)
- `SELECT load_extension('…')` — must be impossible: `SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION` OFF (rusqlite
  default) AND authorizer denies the function. Test it errors, not loads.
- `SELECT readfile('/data/wheel.db')`, `writefile('/x',…)`, `edit()` — only exist if the fileio ext is
  loaded; confirm the bundled build does NOT expose them (should error "no such function"). If present → DENY.
- Function allowlist: deny `SQLITE_FUNCTION` by default; allow only pure scalar/aggregate helpers you intend
  (`count,sum,min,max,length,substr,lower,upper,coalesce,json_extract`…). Test a denied function errors.

### DoS / resource
- Recursive CTE row-bomb: `WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r) SELECT * FROM r`
  — no table access, so the authorizer won't stop it. Needs a **statement timeout** (`progress_handler`
  returning nonzero to interrupt, or a watchdog calling `interrupt()`) AND a row cap applied DURING
  iteration, not after. The §"read ceiling 10,000 rows" must be enforced by LIMIT injection or a
  fetch-counter, not by materialising then truncating.
- Memory: `SELECT randomblob(1000000000)`, `SELECT zeroblob(1e9)`, `SELECT group_concat(x) FROM
  (…10^7 rows…)`, cartesian `SELECT count(*) FROM t_self a, t_self b, t_self c`. Needs
  `sqlite3_soft_heap_limit`/`hard_heap_limit` or a per-query memory cap, plus the timeout. Test each returns
  an error (interrupted/too big), not an OOM of the engine.

### Prefilter-bypass (if any keyword filter exists in addition to the authorizer)
- `SeLeCt`, leading `/* c */ SELECT`, `--\nSELECT`, stacked `SELECT 1; ATTACH…`, `SELECT`. If a regex
  "must start with SELECT / no ATTACH" gate exists, prove it's bypassable AND that the authorizer still
  catches the payload underneath. The finding if the prefilter is the ONLY gate.

### Stacked statements
- `SELECT 1; ATTACH …` / `SELECT 1; DROP TABLE t_self`. Confirm the engine compiles/executes exactly ONE
  statement (rusqlite `prepare` compiles only the first; ensure it's not `execute_batch`). If only the first
  runs, note trailing SQL is silently ignored (acceptable, but document).

### Authorizer-context correctness (the subtle bugs)
- **db_name check**: the `SQLITE_READ` callback gets `(action, arg1=table, arg2=column, db_name, …)`. Scoping
  MUST require `db_name=="main"` as well as `arg1==t_<name>`. Otherwise a `temp` table or an ATTACHed db with
  a table also named `t_<name>` is read. (ATTACH is denied, so this is belt-and-braces — but assert it.)
- **Case-insensitivity**: SQLite identifiers are case-insensitive; the allow-comparison and the reject-set
  must both be ASCII-case-insensitive. `SELECT * FROM T_OTHER`, `sqlite_MASTER`. A case-sensitive
  reject-set is the bug (attacker uses odd case to dodge a deny-list; the correct design is allow-list on the
  one table, case-insensitive, default-deny — which makes case irrelevant).
- **Column arg**: for some reads arg2 (column) is set and arg1 the table; ensure the check keys on the TABLE
  (arg1), and doesn't accidentally allow when arg1 is empty (which happens for some function/DEFAULT-eval
  contexts — default-deny handles it, but test `SELECT 1` still works and `SELECT` on a bare expression
  doesn't wrongly enable a table read).

## Delivery
Staged probe: `redteam/pocs/table-sql/t_query_authorizer.py` — runs the moment `POST /v1/cli/query`
exists; asserts each payload above returns denied/error (exit 3/4 or SQL error), and that a legitimate
`SELECT * FROM t_self` and scoped aggregate SUCCEED (so the authorizer isn't just "deny all"). PASS =
resisted. Boot harness mirrors `run_cli_campaign.sh` (agent A wired `read`→table `self`, another table
`other` A is NOT wired to; A's node token; send SQL via the CLI realm).
