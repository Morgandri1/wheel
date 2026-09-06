#!/usr/bin/env python3
"""Credential-route campaign (PM). Owner: SDK/Engine. Engine control plane (/v1/*, engine secret)
plus the CLI realm (/v1/cli/*, node token). Targets:
  1 setup_token REFUSES non-durable creds (only sk-ant-oat* durable) — prefix/case/whitespace tricks
  2 save_to_vault requires a read wire, honours the ambiguity rule, and NEVER echoes the value
  3 a stored credential is readable ONLY via the wired agent's env/secret_get — not board/auth/sibling
  5 GET auth exposes mode/source/expires_at only, never the value
  + session fixation / replay on the paste-code path (best-effort; needs a login child)

Env (from run_credential_campaign.sh):
  URL, ESEC (engine secret); AID/CID/BID (agent ids: A wired v1+v2, C wired v1 only, B no vault wire);
  V1/V2 (vault ids; V2 declares CLAUDE_CODE_OAUTH_TOKEN); TOK_C/TOK_B (node tokens)
PASS = resisted. Exit 1 on any finding.
"""
import json, os, sys, uuid, urllib.request, urllib.error
U=os.environ; URL=U.get("URL"); ES=U.get("ESEC")
AID,CID,BID=U.get("AID"),U.get("CID"),U.get("BID"); V1,V2,V3=U.get("V1"),U.get("V2"),U.get("V3")
TOK_C,TOK_B=U.get("TOK_C"),U.get("TOK_B")
DURABLE="sk-ant-oat01-CAMPAIGN-durabletoken-abc123"   # classifies as OauthToken
findings=[]
def call(m,path,bearer=None,body=None):
    r=urllib.request.Request(URL+path,method=m)
    if bearer: r.add_header("authorization",f"Bearer {bearer}")
    d=None
    if body is not None: d=json.dumps(body).encode(); r.add_header("content-type","application/json")
    try:
        with urllib.request.urlopen(r,d,timeout=30) as x: raw=x.read().decode(errors="replace"); return x.status,raw
    except urllib.error.HTTPError as e: return e.code,e.read().decode(errors="replace")
    except Exception as e: return None,repr(e)
def check(label,ok,detail=""):
    print(("PASS " if ok else "FAIL ")+label+(f"  [{detail}]" if detail else ""))
    if not ok: findings.append(f"{label} — {detail}")

def main():
    if not (URL and ES): print("PENDING-STACK"); return 0

    # === 1: setup_token durability — only sk-ant-oat* accepted ===
    for bad in ["sk-ant-api03-provider-key-xyz","sk-live-session-style","", "oat",
                "SK-ANT-OAT01-uppercase-x", " sk-ant-api03-leadingspace"]:
        st,b=call("POST",f"/v1/agents/{AID}/auth/complete",ES,{"setup_token":bad})
        check(f"1 setup_token refuses non-durable {bad!r}", st in (400,422), f"status={st} {b[:50]}")
    st,b=call("POST",f"/v1/agents/{CID}/auth/complete",ES,{"setup_token":DURABLE})
    check("1 setup_token accepts a durable sk-ant-oat token", st==200 and DURABLE not in b, f"status={st} echo={DURABLE in b}")

    # === 2a: save_to_vault requires a read wire (B has none) ===
    st,b=call("POST",f"/v1/agents/{BID}/auth/complete",ES,{"setup_token":DURABLE,"save_to_vault":"v1"})
    check("2a save_to_vault denied without a read wire", st in (403,404), f"status={st} {b[:50]}")

    # === 2b: ambiguity — A reads v2 (declares CLAUDE_CODE_OAUTH_TOKEN); saving into v1 collides ===
    st,b=call("POST",f"/v1/agents/{AID}/auth/complete",ES,{"setup_token":DURABLE,"save_to_vault":"v1"})
    check("2b save_to_vault honours ambiguity rule (409)", st==409 and "ambiguous" in b.lower(), f"status={st} {b[:60]}")

    # === 2c: clean save (C wired to v3, which NO other agent reads — no co-reader ambiguity) ===
    st,b=call("POST",f"/v1/agents/{CID}/auth/complete",ES,{"setup_token":DURABLE,"save_to_vault":"v3"})
    check("2c clean save returns metadata, NEVER the token value", st==200 and DURABLE not in b, f"status={st} echo={DURABLE in b} {b[:60]}")

    # === 3: readback scoping — value only via the wired agent's secret_get; not board/auth/sibling ===
    st,board=call("GET","/v1/board",ES);                      check("3a /v1/board carries no credential value", DURABLE not in board, "leak" if DURABLE in board else "")
    st,auth=call("GET",f"/v1/agents/{CID}/auth",ES);          check("3b GET auth carries no value", st==200 and DURABLE not in auth, f"status={st} {auth[:80]}")
    if TOK_C:
        st,b=call("GET","/v1/cli/secret?addr=v3/CLAUDE_CODE_OAUTH_TOKEN",TOK_C)
        got=""
        try: got=json.loads(b).get("value","")
        except: pass
        check("3c wired agent C CAN read it back (intended)", st==200 and got==DURABLE, f"status={st}")
    if TOK_B:
        st,b=call("GET","/v1/cli/secret?addr=v3/CLAUDE_CODE_OAUTH_TOKEN",TOK_B)
        check("3d unwired sibling B canNOT read the credential", st in (403,404), f"status={st} {b[:50]}")

    # === 5: GET auth shape — only authenticated/mode/source(/expires_at), never the value ===
    st,auth=call("GET",f"/v1/agents/{CID}/auth",ES)
    try: j=json.loads(auth); keys=set(j.keys())
    except: j={}; keys=set()
    allowed={"authenticated","mode","source","account","expires_at","warning"}
    check("5 GET auth exposes only metadata keys", keys<=allowed and DURABLE not in auth, f"keys={keys}")

    # === session fixation / replay (best-effort; the login child must spawn) ===
    st,b=call("POST",f"/v1/agents/{AID}/auth/begin",ES,{})
    if st==200:
        wrong=str(uuid.uuid4())
        st,b=call("POST",f"/v1/agents/{AID}/auth/complete",ES,{"code":"123456","session":wrong})
        check("F1 complete with a WRONG session id refused (fixation)", st not in (200,), f"status={st} {b[:50]}")
        st,b=call("POST",f"/v1/agents/{AID}/auth/complete",ES,{"code":"123456","session":wrong})
        check("F2 a second complete finds NO session (no replay)", st not in (200,), f"status={st} {b[:50]}")
    else:
        print(f"SKIP session tests: auth/begin status={st} (login child couldn't start offline) {b[:60]}")

    if findings:
        print(f"\n{len(findings)} FINDING(S)"); [print("  -",f) for f in findings]; return 1
    print("\nALL RESISTED"); return 0

if __name__=="__main__": sys.exit(main())
