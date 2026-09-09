#!/usr/bin/env python3

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""ADVERSARY stale-resume run-verification (cbc6b4a) — the false-positive-session-wipe attack.

Verifies against a LIVE engine built from cbc6b4a:
  CASE 1: a resumed agent KILLED PRE-INIT by stop KEEPS its session (reap's run_id guard holds).
  CASE 2: a resumed agent that EXITS PRE-INIT on its own has its session CLEARED (intended).
A fixed session is first established by a normal start (init). Mounts a controllable fake claude
over the image's. Self-contained; cleans up.
"""
import json, os, subprocess, sys, time, uuid, urllib.request, urllib.error

NAME="wheel-adv-staleresume"; SECRET="adv-staleresume-secret-0123456789ab"; PORT=17622
IMAGE="wheel-engine:test"; BASE="http://127.0.0.1:%d"%PORT
FAKE=os.path.join(os.path.dirname(os.path.abspath(__file__)),"fakeclaude.sh")

def sh(*a,**k): return subprocess.run(a,capture_output=True,text=True,**k)
def api(method,path,body=None):
    data=json.dumps(body).encode() if body is not None else None
    req=urllib.request.Request(BASE+path,data=data,method=method)
    req.add_header("Authorization","Bearer "+SECRET)
    if data: req.add_header("Content-Type","application/json")
    try:
        with urllib.request.urlopen(req,timeout=30) as r: return r.status,r.read().decode()
    except urllib.error.HTTPError as e: return e.code,e.read().decode()

def start_engine():
    sh("docker","rm","-f",NAME)
    os.chmod(FAKE,0o755)
    k=sh("openssl","rand","-base64","32").stdout.strip()
    p=sh("docker","run","-d","--name",NAME,
         "-v","%s:/usr/local/bin/claude:ro"%FAKE,
         "-e","WHEEL_PROJECT_ID="+str(uuid.uuid4()),"-e","WHEEL_ENGINE_SECRET="+SECRET,
         "-e","WHEEL_VAULT_KEY="+k,"-e","WHEEL_ROLE=engine",
         "-e","WHEEL_LISTEN=tcp://0.0.0.0:7000","-p","%d:7000"%PORT,IMAGE)
    assert p.returncode==0,p.stderr
    for _ in range(60):
        if api("GET","/healthz")[0]==200: return
        time.sleep(0.5)
    print("logs:", sh("docker","logs",NAME).stderr[-500:]); raise SystemExit("engine never healthy")

def setmode(m): sh("docker","exec",NAME,"sh","-c","echo %s > /tmp/fakemode; chmod 644 /tmp/fakemode"%m)
def session_of(aid):
    st,b=api("GET","/v1/board")
    if st!=200: return None,("board %s"%st)
    for n in json.loads(b).get("nodes",[]):
        if n.get("id")==aid: return (n.get("state") or {}).get("session_id"), (n.get("state") or {}).get("status")
    return None,"not found"
def status_of(aid): return session_of(aid)[1]
def wait_status(aid,want,t=30):
    for _ in range(t*2):
        if status_of(aid)==want: return True
        time.sleep(0.5)
    return False

def main():
    start_engine()
    st,b=api("POST","/v1/nodes",{"name":"a0","type":"agent","position":{"x":0,"y":0},
        "config":{"harness":"claude","system_prompt":"x","run_on_startup":False,"idle_timeout_secs":0}})
    assert st in (200,201),b; A=json.loads(b)["id"]
    setmode("init")
    res={}
    # establish a session: normal start -> fake inits -> session_id set
    assert api("POST","/v1/agents/%s/start"%A,{})[0] in (200,201,202)
    wait_status(A,"idle") or wait_status(A,"running")
    sess0,stat0=session_of(A)
    print("after first start: status=%s session=%s"%(stat0,sess0))
    res["session_established"]=(sess0=="SESS-POC-1")
    # stop keeps the session
    api("POST","/v1/agents/%s/stop"%A,{}); time.sleep(1)
    sess1,_=session_of(A); print("after stop: session=%s (kept?)"%sess1)
    res["stop_keeps_session"]=(sess1=="SESS-POC-1")

    # CASE 1: resume, fake sleeps (never init); stop PRE-INIT -> session must be KEPT
    setmode("sleep")
    assert api("POST","/v1/agents/%s/start"%A,{})[0] in (200,201,202)
    time.sleep(2)  # spawned, in Starting, fake sleeping (never inits)
    print("case1 pre-stop status=%s (expect starting)"%status_of(A))
    api("POST","/v1/agents/%s/stop"%A,{}); time.sleep(2)
    sessC1,statC1=session_of(A); print("CASE1 after stop-pre-init: status=%s session=%s (want SESS-POC-1)"%(statC1,sessC1))
    res["case1_killed_pre_init_KEEPS_session"]=(sessC1=="SESS-POC-1")

    # CASE 2: resume, fake exits immediately (never init, on its own) -> session CLEARED
    setmode("exit")
    assert api("POST","/v1/agents/%s/start"%A,{})[0] in (200,201,202)
    time.sleep(4)  # child exits pre-init; reap runs -> clear_session
    sessC2,statC2=session_of(A); print("CASE2 after exit-pre-init: status=%s session=%s (want None/empty)"%(statC2,sessC2))
    res["case2_exits_pre_init_CLEARS_session"]=(not sessC2)

    print("\n=== VERDICT (stale-resume, cbc6b4a) ===")
    build=json.loads(api("GET","/healthz")[1]).get("build","?"); print("engine build:",build)
    for k,v in res.items(): print(("PASS" if v else "**FAIL**"),k)
    ok=all(res.values()); print("ALL PASS — no false-positive wipe of a good session" if ok else "SOME FAILED")
    return 0 if ok else 1

if __name__=="__main__":
    try: rc=main()
    finally: sh("docker","rm","-f",NAME)
    sys.exit(rc)
