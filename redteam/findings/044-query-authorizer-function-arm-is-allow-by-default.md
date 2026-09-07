# 044 — `wheel query` authorizer's Function arm is allow-by-default; safety lives in Cargo build flags, not the authorizer

- **Severity:** Low (defense-in-depth; NO current escape — verified). Owner: SDK/Engine. Boundary TB4
  (agent → table via `wheel query`). Found by auditing the read-only SQL escape hatch end to end.
- **Status:** the `wheel query` path is otherwise SOUND (see "What holds"); this is the one deviation from its
  own stated invariant, and it is latent, not live, on the current build.

## What holds (the audit's main result — a clean bill on a high-value surface)
`wheel query <table> "<SELECT…>"` (agent→table) is well defended, verified in source:
- **Wire-gated to the target table.** `cli_routes::query` calls `me.require(&conn, &body.table, WireType::Read)`
  (cli_routes.rs:471) — an agent can only query a table NODE it has a `read` wire to — then resolves the SQL
  target from the VALIDATED node name (`tables::table_name(&node.name)` → `t_<name>`, cli_routes.rs:474), never
  from raw agent input. So an agent cannot name an arbitrary `t_othernode`.
- **SQL confined to that one table.** `tables::query` (tables.rs:408) opens a `SQLITE_OPEN_READ_ONLY`
  connection, sets an authorizer that DENIES by default and allows `Read` only when
  `table_name.eq_ignore_ascii_case(allowed)`, rejects non-`readonly()` statements, blocks multi-statement,
  denies `load_extension`, and bounds it with a deadline + row/byte/value caps. Cross-table reads
  (UNION/CTE/subquery), `sqlite_master`, ATTACH, PRAGMA, and writes are all denied. `readfile`/`writefile`
  aren't built-ins and can't be loaded. **No escape found.**

## The one deviation (this finding)
`tables.rs:500` states the invariant: "Default DENY (§3). Anything not named here is refused, so a sqlite
version that adds an action does not quietly widen this." Every arm honours it — EXCEPT `Function`
(tables.rs:517-523), which is **allow-by-default**: it allows EVERY function and denies only `load_extension`.
So the deny-by-default guarantee does not extend to functions: any scalar/aggregate function the sqlite build
happens to register is callable inside the query.

Why it is Low, not higher (verified, not assumed): the bundled build registers no dangerous functions. rusqlite
= `{ version = "0.32", features = ["bundled", "hooks", "limits"] }` — no `functions`/`vtab`/`csvtab`/`series`
feature, no `fileio` extension (so no `readfile`/`writefile`), extension loading off by default (and
`load_extension` denied anyway), and the amalgamation ships with `SQLITE_ENABLE_FTS3_TOKENIZER` OFF (so the
2-arg `fts3_tokenizer` pointer RCE, CVE-2019-8457, is not reachable). FTS3/5/JSON1/RTREE are on but expose no
function that reads files or escapes the read-only authorized box.

The risk is latent and lives in Cargo.toml, not in the authorizer: the day someone adds a rusqlite feature that
registers file/exec-capable functions (`functions`+a custom scalar, `fileio`/`vtab`, a loadable module), or
flips an FTS tokenizer flag, the allow-by-default arm exposes it with no code change to `authorize` and no
review prompt — exactly the "quiet widening" the line-500 comment is written to prevent, in the one place it
does not apply.

## Fix (SDK) — make the Function arm deny-by-default like the rest
Allowlist the function families the query hatch actually needs (aggregates, string/date/math, json, the benign
FTS aux funcs) and DENY everything else — or, minimally, extend the deny set beyond `load_extension` to the
known-dangerous names (`readfile`, `writefile`, `edit`, `fts3_tokenizer`, any app-registered function that
should not be reachable from tenant SQL). Add a test that a made-up function name is DENIED, so the invariant is
enforced by a test, not by the current build's function registry.

## Note
This is the kind of gap that is invisible until a dependency feature flips. Filed so a future `rusqlite`
feature change triggers review rather than silently widening the one authorizer arm that is not deny-by-default.
