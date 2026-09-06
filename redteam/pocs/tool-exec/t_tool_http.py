#!/usr/bin/env python3
"""Tool executor/importer e2e HTTP campaign. Owner: SDK/Engine. Confirms 022/023 live + SSRF-config +
fill precedence + cookie injection + ops projection, against the LIVE routes. RoE: no real outbound
hosts — every executor test uses curl/dry_run (build_request only) or a config-time-denied base_url;
the YAML-bomb import is bounded. Env from run_tool_http_campaign.sh: URL, ESEC, NAME (container)."""
import json, os, subprocess, sys, time, urllib.request, urllib.error
URL=os.environ["URL"]; ESEC=os.environ["ESEC"]; NAME=os.environ["NAME"]
SECRET="sk/live+abc=="            # vault value with non-unreserved chars
STATICV="ST/ATIC+val="            # static value likewise
findings=[]
def req(method, path, bearer, body=None):
    r=urllib.request.Request(URL+path, method=method); r.add_header("authorization",f"Bearer {bearer}")
    d=None
    if body is not None: d=json.dumps(body).encode(); r.add_header("content-type","application/json")
    try:
        with urllib.request.urlopen(r,d,timeout=30) as x: raw=x.read().decode(errors="replace"); return x.status,raw
    except urllib.error.HTTPError as e: return e.code, e.read().decode(errors="replace")
    except Exception as e: return None, repr(e)
def ck(label, ok, detail=""):
    print(("PASS " if ok else "FAIL ")+label+(f"  [{detail}]" if detail else ""))
    if not ok: findings.append(f"{label} — {detail}")

def mknode(cfg): return req("POST","/v1/nodes",ESEC,cfg)
def now(): return "2026-09-05T00:00:00Z"

def main():
    # --- config-time SSRF: a denied base_url must be REFUSED at node creation ---
    for label,burl in [("loopback","http://127.0.0.1/"),("metadata","http://169.254.169.254/"),
                        ("rfc1918","http://10.0.0.1/"),("decimal","http://2130706433/"),
                        ("railway-internal","http://x.railway.internal/"),("file","file:///etc/passwd")]:
        st,b=mknode({"name":f"bad{label.replace('-','')}","type":"tool","config":{
            "kind":"http","source":{"format":"manual","raw":"","imported_at":now()},
            "base_url":burl,"operations":[]}})
        ck(f"config SSRF {label} base_url refused", st in (400,422), f"status={st} {b[:50]}")

    # --- the good tool: vault(query) + static(query) + agent cookie + agent header ---
    op=lambda i,params:{"id":i,"method":"GET","path":"/data","enabled":True,"params":params}
    P=lambda n,loc,fill:{"name":n,"location":loc,"required":False,"fill":fill}
    cfg={"kind":"http","source":{"format":"manual","raw":"","imported_at":now()},
         "base_url":"https://api.example.com","operations":[
            op("q",[P("key","query",{"mode":"vault","vault_ref":"v/SECRETKEY"})]),
            op("st",[P("tok","query",{"mode":"static","value":STATICV})]),
            op("ck",[P("sid","cookie",{"mode":"agent"})]),
            op("hd",[P("X-Agent","header",{"mode":"agent"})]),
         ]}
    st,b=mknode({"name":"httptool","type":"tool","config":cfg}); ck("create tool node", st in (200,201), f"status={st} {b[:60]}")
    mknode({"name":"a","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":False,"ephemeral_context":False}})
    mknode({"name":"v","type":"vault","config":{"keys":["SECRETKEY"]}})
    board=json.loads(req("GET","/v1/board",ESEC)[1])["nodes"]
    nid=lambda n:next(x["id"] for x in board if x["name"]==n)
    aid,tid,vid=nid("a"),nid("httptool"),nid("v")
    for frm,to,t in [(aid,tid,"read"),(tid,vid,"read")]:
        req("POST","/v1/wires",ESEC,{"from":frm,"to":to,"type":t})
    req("PUT",f"/v1/vault/{vid}/SECRETKEY",ESEC,{"value":SECRET})
    req("POST",f"/v1/agents/{aid}/start",ESEC); time.sleep(2)
    TOK=subprocess.run(["docker","exec",NAME,"sh","-lc",f"cat /data/run/{aid}/token"],
                       capture_output=True,text=True).stdout.strip()
    ck("agent token extracted", len(TOK)>=32, f"len={len(TOK)}")

    # NOTE: the agent-side POST /v1/cli/tool handler exists (cli_routes::tool_call) but is NOT registered
    # in the cli Router (api/mod.rs) -> unmatched 404. So we exercise the SHARED run_operation via the
    # wired OPERATOR route /v1/tools/:id/call (engine secret). Same build_request/send/curl_for path.
    st_cli,_=req("POST","/v1/cli/tool",TOK,{"node":"httptool","op":"q","args":{}})
    ck("(wiring) agent /v1/cli/tool is reachable", st_cli not in (404,405,None),
       f"status={st_cli} — handler exists but route not registered => agents cannot call tools")
    def tool(op,args,dry=False): return req("POST",f"/v1/tools/{tid}/call",ESEC,{"op":op,"args":args,"dry_run":dry})

    # --- 022: curl masks vault+static, INCLUDING query placement (the finding) ---
    st,b=tool("q",{},dry=True)
    enc="sk%2Flive%2Babc%3D%3D"
    leaked = st==200 and (SECRET in b or enc in b) and "<redacted>" not in b.split(enc)[0][-40:] if enc in b else (SECRET in b)
    # simpler: the secret (raw or encoded) must NOT appear; mask should have redacted it
    vault_leak = st==200 and (SECRET in b or enc in b)
    ck("022 vault QUERY secret masked in curl", not vault_leak, f"status={st} curl={b[:110]}")
    st,b=tool("st",{},dry=True)
    senc="ST%2FATIC%2Bval%3D"
    static_leak = st==200 and (STATICV in b or senc in b)
    ck("022 static QUERY secret masked in curl", not static_leak, f"status={st} curl={b[:110]}")

    # --- fill precedence: agent cannot set a vault field ---
    st,b=tool("q",{"key":"attacker"})
    ck("fill precedence: agent naming vault field refused (400)", st in (400,403), f"status={st} {b[:60]}")
    st,b=tool("q",{"invented":"x"})
    ck("invented field refused (400)", st in (400,403), f"status={st} {b[:60]}")

    # --- cookie injection: agent cookie value '; k=v' ---
    st,b=tool("ck",{"sid":"x; admin=true; role=root"},dry=True)
    injected = st==200 and "admin=true" in b and "role=root" in b
    ck("cookie value injection ('; k=v' smuggled into Cookie)", not injected, f"status={st} curl={b[:120]}")

    # --- ops projection: only agent fields; vault_ref never exposed ---
    st,b=req("GET",f"/v1/tools/{tid}/ops",ESEC)
    ops_ok = st==200 and "v/SECRETKEY" not in b and "SECRETKEY" not in b
    ck("ops projection hides vault ref + non-agent fields", ops_ok, f"status={st} {b[:100]}")

    # --- 023 (bounded): YAML anchor bomb on import must be REFUSED (size/alias cap) ---
    def bomb(depth,width=8):
        L=['a0: &a0 "x"']
        for i in range(1,depth+1): L.append(f"a{i}: &a{i} ["+",".join([f"*a{i-1}"]*width)+"]")
        L.append(f"openapi: '3.0.0'\ntop: *a{depth}"); return "\n".join(L)
    t0=time.time(); st,b=req("POST","/v1/tools/import",ESEC,{"format":"openapi","raw":bomb(5)}); dt=time.time()-t0
    # A cap would reject a bomb fast (400 "too large"/"aliases"). If it PARSES it (expanding) then fails
    # "no operations", the DoS vector is live (no cap) -> finding.
    refused_by_cap = st in (400,413,422) and any(w in b.lower() for w in ["too large","alias","anchor","size","limit"])
    ck("023 YAML anchor bomb refused by a size/alias cap", refused_by_cap, f"status={st} {dt*1000:.0f}ms {b[:80]}")

    # --- runtime SSRF: base_url that PASSES the config string-check (decimal/octal/hex IP) must be
    #     refused at CALL time by resolve_and_check (resolve -> ip_is_denied), not connected. ---
    for label,burl in [("decimal","http://2130706433/"),("octal","http://0177.0.0.1/"),("hex","http://0x7f000001/")]:
        st,b=mknode({"name":f"rt{label}","type":"tool","config":{"kind":"http",
            "source":{"format":"manual","raw":"","imported_at":now()},"base_url":burl,
            "operations":[{"id":"s","method":"GET","path":"/","enabled":True,"params":[]}]}})
        if st in (200,201):
            rid=json.loads(b)["id"]
            stc,bc=req("POST",f"/v1/tools/{rid}/call",ESEC,{"op":"s","args":{},"dry_run":False})
            refused = stc in (400,403,502) and any(w in bc.lower() for w in ["reachable","private","loopback","internal","denied"])
            ck(f"runtime SSRF {label} base_url ({burl}) refused at send", refused, f"status={stc} {bc[:70]}")
        else:
            ck(f"runtime SSRF {label} base_url refused at CONFIG (even better)", st in (400,422), f"status={st}")

    if findings: print(f"\n{len(findings)} FINDING(S)"); [print("  -",f) for f in findings]; return 1
    print("\nALL RESISTED"); return 0

if __name__=="__main__": sys.exit(main())
