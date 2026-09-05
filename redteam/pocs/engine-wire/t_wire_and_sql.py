#!/usr/bin/env python3
"""003 Engine wire/token + SQL escapes. Owner: SDK. → THREAT-MODEL TB4.
Secure outcomes: (a) a node token used on a wire it lacks → exit-3/403. (b) `wheel query` cannot
ATTACH/PRAGMA/read sqlite_master/other t_ tables/readfile. Run inside a sandbox with WHEEL_TOKEN set
for a node with ONLY a read wire to one table."""
import os, sys; sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

SQL_ESCAPES = ["ATTACH DATABASE '/data/wheel.db' AS x", "PRAGMA database_list",
               "SELECT * FROM sqlite_master", "SELECT readfile('/data/wheel.db')",
               "SELECT * FROM t_other_nodes_table", "SELECT load_extension('x')"]

def run(_):
    # This probe runs against the engine CLI plane; requires a node token (WHEEL_TOKEN) + a table name.
    tok = os.environ.get("WHEEL_TOKEN"); tbl = os.environ.get("WHEEL_TABLE"); base = os.environ.get("WHEEL_ENGINE")
    if not (tok and tbl and base): return None
    for sql in SQL_ESCAPES:
        st, _, body = h.req("POST", "/v1/cli/query", token=tok, base=base,
                            headers={"content-type": "application/json"},
                            body='{"table":"%s","sql":"%s"}' % (tbl, sql.replace('"', '\\"')))
        if st == 200: return f"query allowed escape: {sql!r} (status 200, body {body[:80]!r})"
    return None

if __name__ == "__main__": h.finish(run)
