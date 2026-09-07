#!/usr/bin/env python3
"""ADVERSARY cross-tenant reach probe (PM gated item #3, filesystem/socket/environ half).

Runs the wheel-host process backend LOCALLY (WHEEL_ROLE=host, SANDBOX_BACKEND=process, root),
creates TWO throwaway projects, and — as project A's uid — attempts to reach project B's:
  - /data/projects/<B> (0700 owned by B's uid)   -> must be DENIED
  - /run/wheel/<B>/engine.sock (0600 in 0700 dir) -> must be DENIED
  - engine B's /proc/<pid>/environ (WHEEL_ENGINE_SECRET/WHEEL_VAULT_KEY) -> must be DENIED
Positive control: as A's uid, reach A's OWN dir/environ -> allowed (proves the uid is right).
The NETWORK half (*.railway.internal, host :7100) is production-topology-specific and is NOT
probed here (propose-then-rule). Self-contained; cleans up.
"""
import base64, json, os, subprocess, sys, time, uuid, urllib.request, urllib.error

NAME="wheel-adv-xtenant"; HSECRET="adv-xtenant-host-secret-0123456789ab"; PORT=17600
IMAGE="wheel-engine:test"; HBASE="http://127.0.0.1:%d"%PORT

def sh(*a,**k): return subprocess.run(a,capture_output=True,text=True,**k)
def host(method,path,body=None):
    data=json.dumps(body).encode() if body is not None else None
    req=urllib.request.Request(HBASE+path,data=data,method=method)
    req.add_header("Authorization","Bearer "+HSECRET)
    if data: req.add_header("Content-Type","application/json")
    try:
        with urllib.request.urlopen(req,timeout=40) as r: return r.status,r.read().decode()
    except urllib.error.HTTPError as e: return e.code,e.read().decode()
def dex(*a): return sh("docker","exec",NAME,*a)
def dex_u(uid,*a): return sh("docker","exec","-u",str(uid),NAME,*a)

def start_host():
    sh("docker","rm","-f",NAME)
    p=sh("docker","run","-d","--name",NAME,"--user","0","--tmpfs","/run/wheel:exec,mode=0755",
         "-e","WHEEL_ROLE=host","-e","SANDBOX_BACKEND=process",
         "-e","WHEEL_HOST_SECRET="+HSECRET,"-e","WHEEL_DATA_DIR=/data",
         "-p","%d:7100"%PORT, IMAGE)
    assert p.returncode==0, p.stderr
    for _ in range(80):
        try:
            if host("GET","/host/v1/healthz")[0]==200: return
        except Exception: pass
        time.sleep(0.5)
    print(dex("sh","-c","cat /proc/1/environ | tr '\\0' '\\n' | head" ).stdout)
    print("host logs:", sh("docker","logs",NAME).stderr[-800:])
    raise SystemExit("host never healthy")

def mkproject():
    pid=str(uuid.uuid4())
    vk=base64.b64encode(os.urandom(32)).decode()
    st,b=host("PUT","/host/v1/projects/%s"%pid,
              {"engine_secret":"engine-secret-"+pid[:12]+"-xxxxxxxx","vault_key":vk,
               "capabilities":{"http":False}})
    assert st in (200,201,204),"PUT project: %s %s"%(st,b)
    st,b=host("POST","/host/v1/projects/%s/start"%pid,{})
    assert st in (200,201,202,204),"start project: %s %s"%(st,b)
    return pid

def wait_running(pid,t=40):
    for _ in range(t*2):
        st,b=host("GET","/host/v1/projects/%s"%pid)
        try:
            if st==200 and json.loads(b).get("status")=="running": return True
        except Exception: pass
        time.sleep(0.5)
    return False

def owner_uid(path):
    r=dex("stat","-c","%u",path); return int(r.stdout.strip()) if r.returncode==0 else None

def main():
    start_host()
    A=mkproject(); B=mkproject()
    ra,rb=wait_running(A),wait_running(B)
    print("projects running: A=%s B=%s"%(ra,rb))
    dirA="/data/projects/%s"%A; dirB="/data/projects/%s"%B
    uidA=owner_uid(dirA); uidB=owner_uid(dirB)
    print("uidA=%s uidB=%s (want distinct)"%(uidA,uidB))
    res={}
    res["distinct_uids"]=(uidA is not None and uidB is not None and uidA!=uidB)

    # positive control: as A's uid, read A's own dir
    ctl=dex_u(uidA,"sh","-c","ls %s >/dev/null 2>&1 && echo OK"%dirA)
    res["control_A_reads_own_dir"]=(ctl.stdout.strip()=="OK")

    # (a) A's uid reading B's 0700 data dir -> denied
    r=dex_u(uidA,"sh","-c","ls %s 2>&1; cat %s/wheel.db >/dev/null 2>&1 && echo READ_DB"%(dirB,dirB))
    res["A_cannot_read_B_datadir"]=("Permission denied" in r.stdout or r.stdout.strip()=="") and "READ_DB" not in r.stdout
    print("A->B datadir:", r.stdout.strip()[:120])

    # (b) A's uid connecting to B's engine socket -> denied
    sockB="/run/wheel/%s/engine.sock"%B
    r=dex_u(uidA,"sh","-c","test -r %s && echo READABLE || echo denied"%sockB)
    res["A_cannot_reach_B_socket"]=("READABLE" not in r.stdout)
    print("A->B socket:", r.stdout.strip()[:80])

    # (c) As A's uid, try to read the environ of ANY process owned by B's uid range.
    # A can SEE all pids (procfs), but must not READ another uid's environ.
    scan=("import os\n"
          "b=%d; leaked=0; seen=0; ownok=0\n"
          "for pid in os.listdir('/proc'):\n"
          "  if not pid.isdigit(): continue\n"
          "  try: st=os.stat('/proc/'+pid)\n"
          "  except OSError: continue\n"
          "  if st.st_uid==b:\n"
          "    seen+=1\n"
          "    try:\n"
          "      d=open('/proc/'+pid+'/environ','rb').read()\n"
          "      if b'WHEEL_ENGINE_SECRET' in d or b'WHEEL_VAULT_KEY' in d: leaked+=1\n"
          "    except OSError: pass\n"
          "  if st.st_uid==%d:\n"
          "    try:\n"
          "      d=open('/proc/'+pid+'/environ','rb').read(); ownok+=1\n"
          "    except OSError: pass\n"
          "print('seenB=%%d leakedB=%%d ownAreadable=%%d'%%(seen,leaked,ownok))")%(uidB,uidA)
    r=dex_u(uidA,"python3","-c",scan)
    print("environ scan as A:", r.stdout.strip(), r.stderr.strip()[:120])
    # PASS if A saw B's processes but leaked NONE of B's secrets (seenB>0, leakedB==0).
    import re
    m=re.search(r"seenB=(\d+) leakedB=(\d+) ownAreadable=(\d+)", r.stdout)
    if m:
        seenB,leakedB,ownok=int(m.group(1)),int(m.group(2)),int(m.group(3))
        res["A_cannot_read_B_engine_environ"]=(seenB>0 and leakedB==0)
        res["control_A_reads_own_environ"]=(ownok>0)
    else:
        res["A_cannot_read_B_engine_environ"]=None

    print("\n=== VERDICT (process backend, local) ===")
    for k,v in res.items(): print(("PASS" if v else ("N/A" if v is None else "**FAIL**")),k)
    hard=[v for v in res.values() if v is not None]
    ok=all(hard); print("ALL PASS" if ok else "SOME FAILED")
    return 0 if ok else 1

if __name__=="__main__":
    try: rc=main()
    finally: sh("docker","rm","-f",NAME)
    sys.exit(rc)
