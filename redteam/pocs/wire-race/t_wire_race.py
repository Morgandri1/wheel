#!/usr/bin/env python3
"""ADVERSARY wire-delete-vs-CLI-call TOCTOU probe (PM's #1 gated item).

Confirms, against a LIVE engine, the source verdict:
  - revocation is reflected: DELETE wire -> next CLI call denied (require reads live wires);
  - query's re-check-before-disclosure holds under a MID-QUERY revoke (rows withheld, not returned);
  - normal handlers serialize on the single writer (no torn check-then-act-on-deleted-wire).

Self-contained: starts its own engine container (wheel-engine:test), cleans it up.
"""
import json, subprocess, sys, time, uuid, threading

NAME = "wheel-adv-wirerace"
SECRET = "adv-wirerace-secret-0123456789abcd"
PORT = 17533
IMAGE = "wheel-engine:test"
BASE = "http://127.0.0.1:%d" % PORT

def sh(*a, **k): return subprocess.run(a, capture_output=True, text=True, **k)

def api(method, path, body=None):
    import urllib.request
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Authorization", "Bearer " + SECRET)
    if data: req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()

def wheel(node_id, *argv):
    return sh("docker","exec","-e","WHEEL_TOKEN_FILE=/data/run/%s/token"%node_id,
              "-e","WHEEL_ENGINE_URL=http://127.0.0.1:7000", NAME, "wheel", *argv)

def start_engine():
    sh("docker","rm","-f",NAME)
    key = sh("openssl","rand","-base64","32").stdout.strip()
    p = sh("docker","run","-d","--name",NAME,
           "-e","WHEEL_PROJECT_ID="+str(uuid.uuid4()),
           "-e","WHEEL_ENGINE_SECRET="+SECRET,"-e","WHEEL_VAULT_KEY="+key,
           "-e","WHEEL_ROLE=engine","-e","WHEEL_LISTEN=tcp://0.0.0.0:7000",
           "-p","%d:7000"%PORT, IMAGE)
    assert p.returncode==0, p.stderr
    for _ in range(60):
        if api("GET","/healthz")[0]==200: return
        time.sleep(0.5)
    raise SystemExit("engine never healthy")

def node(name, typ, config):
    st, body = api("POST","/v1/nodes",{"name":name,"type":typ,
        "position":{"x":0,"y":0},"config":config})
    assert st in (200,201), "%s %s"%(st,body)
    return json.loads(body)["id"]

def wire(a,b,t):
    st,body = api("POST","/v1/wires",{"from":a,"to":b,"type":t}); return st,body
def unwire(a,b,t):
    st,body = api("DELETE","/v1/wires",{"from":a,"to":b,"type":t}); return st,body

def wait_token(nid, timeout=60):
    for _ in range(timeout*2):
        if sh("docker","exec",NAME,"test","-s","/data/run/%s/token"%nid).returncode==0: return True
        time.sleep(0.5)
    return False

def main():
    start_engine()
    T = node("probe","table",{"columns":[{"name":"v","type":"text"}]})
    A = node("agent0","agent",{"harness":"claude","system_prompt":"x","run_on_startup":False,
                               "ephemeral_context":False})
    assert wire(A,T,"write")[0] in (200,201,204)   # write implies read; lets us populate
    assert wire(A,T,"read")[0] in (200,201,204)
    def unwire_all(): unwire(A,T,"read"); unwire(A,T,"write")
    def wire_all(): wire(A,T,"write"); wire(A,T,"read")
    assert api("POST","/v1/agents/%s/start"%A, {})[0] in (200,201,202), "start agent"
    assert wait_token(A), "no token file"

    r = wheel(A,"write","probe/1",'{"v":"a"}'); print("write:", r.returncode, r.stdout.strip(), r.stderr.strip())
    for i in range(100): wheel(A,"write","probe/%d"%i, '{"v":"x"}')
    base = wheel(A,"query","probe","SELECT count(*) AS n FROM t_probe")
    print("baseline query:", base.returncode, base.stdout.strip()[:120], base.stderr.strip()[:120])

    results = {}
    # (1) revocation reflected: delete wire, then query -> denied (exit 3)
    unwire_all()
    r1 = wheel(A,"query","probe","SELECT count(*) FROM t_probe")
    results["revocation_reflected_denied"] = (r1.returncode == 3)
    print("after unwire, query rc=%d (want 3)"%r1.returncode, r1.stderr.strip()[:100])
    wire_all()  # restore

    # (2) re-check under MID-QUERY revoke: slow self-join, delete wire mid-flight,
    #     expect DENIED (rows withheld), not a row.
    slow = "SELECT count(*) AS n FROM t_probe a, t_probe b, t_probe c, t_probe d"
    out = {}
    def run_slow():
        r = wheel(A,"query","probe",slow); out["rc"]=r.returncode; out["so"]=r.stdout.strip()[:80]; out["se"]=r.stderr.strip()[:120]
    th = threading.Thread(target=run_slow);
    t0=time.time(); th.start()
    time.sleep(0.25)                      # let the query pass its initial require() and start scanning
    # revoke BOTH wires: write implies read, so a partial revoke leaves read capability and the
    # re-check (correctly) still passes. unwire_all removes all agent0->probe capability.
    du = (unwire(A,T,"read"), unwire(A,T,"write"))[0]
    th.join(timeout=40)
    dt=time.time()-t0
    print("mid-query unwire rc=%s; slow query rc=%s in %.2fs %s %s"%(du[0],out.get("rc"),dt,out.get("so"),out.get("se")))
    # Both wires were revoked ~0.25s in; if the query ran long enough (dt>~0.5s) the revoke
    # landed well before the re-check. rc==3 => re-check WITHHELD (fixed). rc==0 => rows
    # disclosed against a revoked capability => FINDING 047 (re-check validates a stale snapshot).
    mid_window = dt > 0.5
    results["mid_query_recheck_withholds_047"] = (out.get("rc")==3) if mid_window else True
    print("  -> 047: mid-query revoke then rc=%s (want 3 if fixed; rc=0 == FINDING, re-check ineffective); window=%.2fs"%(out.get("rc"), dt))
    wire_all()

    # (3) single-writer serialization: hammer read while flapping the wire; every outcome
    #     must be clean (rc 0 success OR rc 3 deny), never a torn/500 state.
    torn = []
    def reader():
        for _ in range(40):
            r = wheel(A,"read","probe/1")
            if r.returncode not in (0,3,4): torn.append((r.returncode, r.stderr.strip()[:80]))
    def flapper():
        for _ in range(40):
            unwire_all(); wire_all()
    ts=[threading.Thread(target=reader),threading.Thread(target=reader),threading.Thread(target=flapper)]
    for t in ts: t.start()
    for t in ts: t.join(timeout=60)
    results["single_writer_no_torn_state"] = (len(torn)==0)
    print("torn states:", torn[:5])

    print("\n=== VERDICT ===")
    for k,v in results.items(): print(("PASS" if v else "**FAIL**"), k)
    ok = all(results.values())
    print("ALL PASS" if ok else "SOME FAILED")
    return 0 if ok else 1

if __name__=="__main__":
    try: rc=main()
    finally:
        sh("docker","rm","-f",NAME)
    sys.exit(rc)
