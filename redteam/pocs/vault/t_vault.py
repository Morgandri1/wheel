#!/usr/bin/env python3
"""VAULT campaign (M1.6) — env-delivered credentials. Owner: SDK/Engine.

Driven on the engine control plane. Two realms (as everywhere):
  /v1/*      → Authorization: Bearer <ENGINE_SECRET>   (host; vault PUT/GET-names/DELETE live here)
  /v1/cli/*  → Authorization: Bearer <node token>      (per-node; `wheel secret get` lives here)

PM's targets: encrypted at rest, write-only API (no value on /v1/board, keys=names only),
wire-gated read (unwired agent → exit 3 / no value), ambiguity refused at wire+PUT+spawn
(409 ambiguous_credential), auth mode env + source, CLI path with a rotated/deleted token.

Env (set by run_vault_campaign.sh):
  WHEEL_ENGINE_URL, WHEEL_ENGINE_SECRET
  V1, V2, V3            vault node ids   (V1 declares K1; V2 declares K1 → dup; V3 empty)
  A_ID, B_ID           agent node ids   (A wired→V1 read; B unwired)
  TOK_A, TOK_B         valid node tokens (post-start)
  SECRET1              the plaintext PUT into V1/K1 (must never leave via any read path)
  DB_PLAINTEXT_HITS    count of SECRET1 occurrences in /data/wheel.db (bash grep; 0 = encrypted)
Each check prints PASS (resisted) / FAIL (FINDING). Exit 1 if any FINDING.
"""
import json, os, sys, time, urllib.request, urllib.error

EURL = os.environ.get("WHEEL_ENGINE_URL")
ESEC = os.environ.get("WHEEL_ENGINE_SECRET")
V1, V2, V3 = os.environ.get("V1"), os.environ.get("V2"), os.environ.get("V3")
A_ID, B_ID = os.environ.get("A_ID"), os.environ.get("B_ID")
TOK_A, TOK_B = os.environ.get("TOK_A"), os.environ.get("TOK_B")
SECRET1 = os.environ.get("SECRET1", "")
findings = []

def call(method, path, bearer=None, body=None):
    req = urllib.request.Request((EURL or "") + path, method=method)
    if bearer:
        req.add_header("authorization", f"Bearer {bearer}")
    data = None
    if body is not None:
        data = json.dumps(body).encode(); req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=15) as r:
            return r.status, r.read().decode(errors="replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")
    except Exception as e:
        return None, repr(e)

def check(label, resisted, detail=""):
    print(("PASS " if resisted else "FAIL ") + label + (f"  [{detail}]" if detail else ""))
    if not resisted:
        findings.append(f"{label} — {detail}")

def main():
    if not (EURL and ESEC):
        print("PENDING-STACK: run_vault_campaign.sh sets the env"); return 0

    # === TARGET 1: encrypted at rest (live grep from the harness) ===
    hits = os.environ.get("DB_PLAINTEXT_HITS")
    if hits is not None:
        check("1 plaintext absent from /data/wheel.db (encrypted at rest)", hits.strip() == "0",
              f"grep hits={hits.strip()}")

    # === TARGET 2: write-only API — no value on /v1/board, keys=names only ===
    st, board = call("GET", "/v1/board", bearer=ESEC)
    leaked = SECRET1 and SECRET1 in board
    check("2a /v1/board carries no vault value", st == 200 and not leaked,
          f"status={st} leaked={bool(leaked)}")
    st, vk = call("GET", f"/v1/vault/{V1}", bearer=ESEC)
    try:
        j = json.loads(vk)
        names_only = ("K1" in j.get("keys", [])) and (SECRET1 not in vk) and ("value" not in vk)
    except Exception:
        names_only = False
    check("2b GET /v1/vault/:id returns key NAMES only", st == 200 and names_only, f"status={st} body={vk[:80]}")

    # === TARGET 3: wire-gated CLI read ===
    if TOK_A:
        st, b = call("GET", "/v1/cli/secret?addr=v1/K1", bearer=TOK_A)
        got = ""
        try: got = json.loads(b).get("value", "")
        except Exception: pass
        check("3a wired agent A reads its own vault value (intended allow)",
              st == 200 and got == SECRET1, f"status={st}")
        # A has NO wire to V2 → exit 3 (403), NOT an empty/other answer.
        st, b = call("GET", "/v1/cli/secret?addr=v2/K1", bearer=TOK_A)
        check("3b agent A denied on a vault it is NOT wired to (exit 3)", st in (403, 404),
              f"status={st} {b[:60]}")
    if TOK_B:
        st, b = call("GET", "/v1/cli/secret?addr=v1/K1", bearer=TOK_B)
        check("3c unwired agent B gets NOTHING from the CLI (exit 3)", st in (403, 404),
              f"status={st} {b[:60]}")
        # names-only endpoint is also wire-gated
        st, b = call("GET", "/v1/cli/secret/keys?node=v1", bearer=TOK_B)
        check("3d unwired agent B cannot even list key names", st in (403, 404), f"status={st}")

    # === TARGET 5: ambiguity refused at WIRE creation and PUT (409 ambiguous_credential) ===
    # Wire A→V2 (V2 declares K1, A already reads K1 from V1) → refused at the wire.
    st, b = call("POST", "/v1/wires", bearer=ESEC, body={"from": A_ID, "to": V2, "type": "read"})
    is_amb = st == 409 and ("ambiguous" in b.lower())
    check("5a ambiguity refused at WIRE creation (409 ambiguous_credential)", is_amb, f"status={st} {b[:80]}")
    # Wire A→V3 (empty) is fine; then PUT K1 into V3 → refused at the write.
    st, _ = call("POST", "/v1/wires", bearer=ESEC, body={"from": A_ID, "to": V3, "type": "read"})
    check("5b non-conflicting wire A→V3 allowed", st in (200, 201, 204), f"status={st}")
    st, b = call("PUT", f"/v1/vault/{V3}/K1", bearer=ESEC, body={"value": "conflict-value"})
    check("5c ambiguity refused at PUT (409 ambiguous_credential)", st == 409 and "ambiguous" in b.lower(),
          f"status={st} {b[:80]}")

    # === TARGET 6: auth mode env + source ===
    # V1 was given ANTHROPIC_API_KEY by the harness; A reads V1 → source should be V1's name.
    st, b = call("GET", f"/v1/agents/{A_ID}/auth", bearer=ESEC)
    src_ok = st == 200 and ("v1" in b) and (SECRET1 not in b)
    check("6 agent auth reports env source (vault name), never the value", src_ok, f"status={st} {b[:100]}")

    # === TARGET 7: rotated / deleted node token is refused on the CLI path ===
    if TOK_A and A_ID:
        call("POST", f"/v1/agents/{A_ID}/restart", bearer=ESEC); time.sleep(1.2)
        st, _ = call("GET", "/v1/cli/secret?addr=v1/K1", bearer=TOK_A)
        check("7a rotated (pre-restart) token cannot read a secret", st in (401, 403), f"status={st}")
        call("POST", f"/v1/agents/{A_ID}/stop", bearer=ESEC); time.sleep(0.5)
        st, _ = call("GET", "/v1/cli/secret?addr=v1/K1", bearer=TOK_A)
        check("7b stopped agent's token stays dead on the secret path", st in (401, 403), f"status={st}")

    if findings:
        print(f"\n{len(findings)} FINDING(S)"); return 1
    print("\nALL RESISTED (for the checks whose env was provided)"); return 0

if __name__ == "__main__":
    sys.exit(main())
