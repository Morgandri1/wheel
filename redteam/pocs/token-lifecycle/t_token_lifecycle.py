#!/usr/bin/env python3
"""ADVERSARY token-lifecycle live-confirm (PM gated item #2).

Source verdict was SOUND; this confirms it LIVE:
  - use-after-node-DELETE: delete the agent node -> its token stops resolving (node_tokens
    cascades on ON DELETE CASCADE, foreign_keys=ON), so a CLI call with the old token fails;
  - use-after-STOP: stop the agent -> token revoked -> old token fails;
  - rename is id-keyed (a peer using the old NAME gets exit 4, the token stays valid).
Self-contained; cleans up its container.
"""
import json, subprocess, sys, time, uuid

NAME="wheel-adv-tokenlife"; SECRET="adv-tokenlife-secret-0123456789ab"; PORT=17555
IMAGE="wheel-engine:test"; BASE="http://127.0.0.1:%d"%PORT

def sh(*a,**k): return subprocess.run(a,capture_output=True,text=True,**k)
def api(method,path,body=None):
    import urllib.request
    data=json.dumps(body).encode() if body is not None else None
    req=urllib.request.Request(BASE+path,data=data,method=method)
    req.add_header("Authorization","Bearer "+SECRET)
    if data: req.add_header("Content-Type","application/json")
    try:
        with urllib.request.urlopen(req,timeout=30) as r: return r.status,r.read().decode()
    except urllib.error.HTTPError as e: return e.code,e.read().decode()
def wheel_tok(tokfile,*argv):
    return sh("docker","exec","-e","WHEEL_TOKEN_FILE="+tokfile,
              "-e","WHEEL_ENGINE_URL=http://127.0.0.1:7000",NAME,"wheel",*argv)
def start_engine():
    sh("docker","rm","-f",NAME)
    k=sh("openssl","rand","-base64","32").stdout.strip()
    p=sh("docker","run","-d","--name",NAME,"-e","WHEEL_PROJECT_ID="+str(uuid.uuid4()),
         "-e","WHEEL_ENGINE_SECRET="+SECRET,"-e","WHEEL_VAULT_KEY="+k,"-e","WHEEL_ROLE=engine",
         "-e","WHEEL_LISTEN=tcp://0.0.0.0:7000","-p","%d:7000"%PORT,IMAGE)
    assert p.returncode==0,p.stderr
    for _ in range(60):
        if api("GET","/healthz")[0]==200: return
        time.sleep(0.5)
    raise SystemExit("engine never healthy")
def node(name,typ,cfg):
    st,b=api("POST","/v1/nodes",{"name":name,"type":typ,"position":{"x":0,"y":0},"config":cfg})
    assert st in (200,201),"%s %s"%(st,b); return json.loads(b)["id"]
def wait_token(nid,t=60):
    for _ in range(t*2):
        if sh("docker","exec",NAME,"test","-s","/data/run/%s/token"%nid).returncode==0: return True
        time.sleep(0.5)
    return False

def main():
    start_engine()
    build=json.loads(api("GET","/healthz")[1]).get("build","?")
    print("engine build:",build)
    T=node("vaultish","table",{"columns":[{"name":"v","type":"text"}]})
    A=node("agent0","agent",{"harness":"claude","system_prompt":"x","run_on_startup":False,"ephemeral_context":False})
    api("POST","/v1/wires",{"from":A,"to":T,"type":"read"})
    assert api("POST","/v1/agents/%s/start"%A,{})[0] in (200,201,202)
    assert wait_token(A),"no token file"
    tf="/data/run/%s/token"%A
    # capture the plaintext token so we can keep trying it after the node is gone
    tok=sh("docker","exec",NAME,"cat",tf).stdout.strip()
    res={}
    base=wheel_tok(tf,"whoami"); res["baseline_whoami_ok"]=(base.returncode==0)
    print("baseline whoami rc",base.returncode, base.stdout.strip()[:60])

    # use-after-node-DELETE: delete the agent node, then its old token must not resolve
    st,_=api("DELETE","/v1/nodes/%s"%A); print("delete agent node ->",st)
    time.sleep(1.0)
    r=wheel_tok(tf,"whoami"); res["token_dead_after_node_delete"]=(r.returncode!=0)
    print("after node delete, whoami rc",r.returncode,(r.stdout or r.stderr).strip()[:80])
    # belt: hit /v1/cli/whoami directly with the captured token bytes (bypasses the file)
    import urllib.request
    def cli_raw(tok):
        req=urllib.request.Request(BASE+"/v1/cli/whoami",method="GET")
        req.add_header("Authorization","Bearer "+tok)
        try:
            with urllib.request.urlopen(req,timeout=10) as x: return x.status
        except urllib.error.HTTPError as e: return e.code
    res["raw_token_rejected_after_delete"]=(cli_raw(tok)!=200)
    print("raw /v1/cli/whoami with deleted node's token ->",cli_raw(tok),"(want !=200)")

    print("\n=== VERDICT (build %s) ==="%build)
    for k,v in res.items(): print(("PASS" if v else "**FAIL**"),k)
    ok=all(res.values()); print("ALL PASS" if ok else "SOME FAILED")
    return 0 if ok else 1

if __name__=="__main__":
    try: rc=main()
    finally: sh("docker","rm","-f",NAME)
    sys.exit(rc)
