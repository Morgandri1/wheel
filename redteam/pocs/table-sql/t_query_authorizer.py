#!/usr/bin/env python3
"""STAGED — `wheel query` rusqlite-authorizer campaign. Owner: SDK/Engine. Plan:
redteam/reviews/table-query-authorizer-testplan.md. Runs the moment POST /v1/cli/query exists.

Model: agent A has a READ wire to table `self` (t_self) and NO wire to table `other` (t_other).
A sends SQL via its node token on the CLI realm. Every attack payload must be DENIED (exit 3/4 or a
SQL error); the two legitimate queries must SUCCEED (proves the authorizer isn't just "deny all").

Env (set by run_query_campaign.sh):
  WHEEL_ENGINE_URL, TOK_A (A's node token), SELF (allowed table name), OTHER (forbidden table name)
PASS = resisted. Exit 1 if any FINDING.
"""
import json, os, sys, urllib.request, urllib.error

EURL = os.environ.get("WHEEL_ENGINE_URL")
TOK = os.environ.get("TOK_A")
SELF = os.environ.get("SELF", "selft")     # NODE name the caller is wired to (body.table)
OTHER = os.environ.get("OTHER", "othert")  # NODE name the caller is NOT wired to
ST = f"t_{SELF}"      # the sqlite table name used INSIDE the SQL
OT = f"t_{OTHER}"
findings = []

def q(sql, table=None):
    """Send one query on the CLI realm. `table` = the CLI <table> scope arg (defaults to SELF)."""
    body = {"table": table or SELF, "sql": sql}
    req = urllib.request.Request(EURL + "/v1/cli/query", method="POST")
    req.add_header("authorization", f"Bearer {TOK}")
    req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, json.dumps(body).encode(), timeout=20) as r:
            return r.status, r.read().decode(errors="replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")
    except Exception as e:
        return None, repr(e)

def denied(label, sql, table=None):
    st, b = q(sql, table)
    ok = st in (400, 403, 404, 422) or (st == 200 and '"error"' in b.lower())
    print(("PASS " if ok else "FAIL ") + label + f"  [status={st} {b[:70]}]")
    if not ok:
        findings.append(f"{label}: status={st} body={b[:120]}")

def check(label, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + label + (f"  [{detail}]" if detail else ""))
    if not ok:
        findings.append(f"{label}: {detail}")

def allowed(label, sql, table=None):
    st, b = q(sql, table)
    ok = st == 200 and '"error"' not in b.lower()
    print(("PASS " if ok else "FAIL ") + label + f"  [status={st} {b[:70]}]")
    if not ok:
        findings.append(f"{label} (legit query wrongly denied): status={st} body={b[:120]}")

def main():
    if not (EURL and TOK):
        print("PENDING-STACK: needs WHEEL_ENGINE_URL + TOK_A and POST /v1/cli/query to exist"); return 0
    # sanity — is the route live?
    st, _ = q(f"SELECT 1")
    if st in (404, 405, None):
        print(f"PENDING-ROUTE: /v1/cli/query not implemented yet (status={st})"); return 0

    # legitimate — must SUCCEED
    allowed("L1 scoped SELECT on own table", f"SELECT * FROM {ST} LIMIT 5")
    allowed("L2 scoped aggregate on own table", f"SELECT count(*) FROM {ST}")

    # 1. ATTACH
    denied("1a ATTACH main db", f"ATTACH DATABASE '/data/wheel.db' AS x; SELECT 1")
    denied("1b ATTACH memory", "ATTACH ':memory:' AS m; SELECT 1")

    # 2. PRAGMA (+ table-valued pragma bypass)
    denied("2a PRAGMA table_info(other)", f"PRAGMA table_info({OT})")
    denied("2b PRAGMA database_list", "PRAGMA database_list")
    denied("2c PRAGMA query_only=OFF", "PRAGMA query_only=OFF")
    denied("2d PRAGMA writable_schema=ON", "PRAGMA writable_schema=ON")
    denied("2e pragma_table_info() table-valued fn", f"SELECT * FROM pragma_table_info('{OT}')")
    denied("2f pragma_database_list table-valued fn", "SELECT * FROM pragma_database_list")

    # 3. other-table reach (each must trip SQLITE_READ on t_other)
    denied("3a JOIN other", f"SELECT * FROM {ST} JOIN {OT} USING(key)")
    denied("3b subquery FROM other", f"SELECT * FROM (SELECT * FROM {OT})")
    denied("3c subquery WHERE other", f"SELECT * FROM {ST} WHERE key IN (SELECT key FROM {OT})")
    denied("3d correlated subquery other", f"SELECT (SELECT 1 FROM {OT}) FROM {ST}")
    denied("3e CTE other", f"WITH x AS (SELECT * FROM {OT}) SELECT * FROM x")
    denied("3f UNION other", f"SELECT key FROM {ST} UNION SELECT key FROM {OT}")
    denied("3g qualified main.other", f"SELECT * FROM main.{OT}")
    denied("3h alias other AS self", f"SELECT * FROM {OT} AS {ST}")
    denied("3i sqlite_master", "SELECT * FROM sqlite_master")
    denied("3j sqlite_schema", "SELECT name FROM sqlite_schema")
    denied("3k self + sqlite_master", f"SELECT * FROM {ST}, sqlite_master")

    # writes (read-only enforcement) — even on own table
    denied("W1 INSERT own", f"INSERT INTO {ST}(key) VALUES('x')")
    denied("W2 UPDATE own", f"UPDATE {ST} SET key='x'")
    denied("W3 DELETE own", f"DELETE FROM {ST}")
    denied("W4 DROP own", f"DROP TABLE {ST}")
    denied("W5 CREATE TEMP TRIGGER", f"CREATE TEMP TRIGGER t AFTER INSERT ON {ST} BEGIN SELECT 1; END")
    denied("W6 CREATE TEMP VIEW over other", f"CREATE TEMP VIEW v AS SELECT * FROM {OT}")

    # extensions / dangerous functions
    denied("F1 load_extension", "SELECT load_extension('x')")
    denied("F2 readfile", "SELECT readfile('/data/wheel.db')")
    denied("F3 writefile", "SELECT writefile('/tmp/x','y')")

    # DoS — a recursive row-bomb must be BOUNDED: either an error, or a 200 capped at MAX_ROWS (10k).
    # A 200 with the fetch cap applied is the designed protection (bounded work), NOT a finding; an
    # unbounded/hung response would be. So PASS = error OR (200 AND rows<=MAX_ROWS).
    st,b = q("WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r) SELECT * FROM r")
    try: nrows = len(json.loads(b).get("rows", []))
    except Exception: nrows = -1
    bounded = (st in (400,403,422)) or (st==200 and 0 <= nrows <= 10000)
    check("D1 recursive CTE row-bomb is bounded (cap 10k / 5s timeout)", bounded, f"status={st} rows={nrows}")
    denied("D2 huge randomblob", "SELECT randomblob(1000000000)")

    # case-insensitivity of the reject set
    denied("C1 UPPERCASE other", f"SELECT * FROM {OT.upper()}")
    denied("C2 mixed sqlite_MASTER", "SELECT * FROM sqlite_MASTER")

    # prefilter bypass / stacked
    denied("P1 comment-led ATTACH", "/* x */ ATTACH ':memory:' AS m; SELECT 1")
    denied("P2 stacked drop after select", f"SELECT 1; DROP TABLE {ST}")

    if findings:
        print(f"\n{len(findings)} FINDING(S)")
        for f in findings: print("  -", f)
        return 1
    print("\nALL RESISTED")
    return 0

if __name__ == "__main__":
    sys.exit(main())
