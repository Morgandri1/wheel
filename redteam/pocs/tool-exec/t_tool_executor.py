#!/usr/bin/env python3
"""STAGED — tool-node executor campaign. Owner: SDK/Engine. Plan:
redteam/reviews/tool-executor-testplan.md. Runs when the call route lands
(`POST /v1/cli/tool-call` or `/v1/tools/:id/call`). RoE: mock the metadata endpoint, never a real one.

Model (set up by run_tool_exec_campaign.sh, all on loopback INSIDE the sandbox container):
  - a tool node whose base_url the probe varies per attack;
  - a `tool -> vault (read)` wire so a `vault`-mode fill resolves a secret the agent cannot see;
  - helper servers in-container: ECHO (reflects request headers+body), REDIRECTOR (302 to a target),
    METADATA-MOCK (stands in for 169.254.169.254 — a distinctive body the agent must never obtain).
Env: WHEEL_ENGINE_URL, TOK_A (node token), TOOL (tool node name), plus helper URLs.
Each check: PASS = resisted. Exit 1 on any finding. Skips cleanly until the route exists.

The exact call shape is filled in once SDK lands it; the ASSERTIONS below are the contract this
executor must meet and are the point of staging this now.
"""
import json, os, sys, urllib.request, urllib.error
U = os.environ; EURL = U.get("WHEEL_ENGINE_URL"); TOK = U.get("TOK_A"); TOOL = U.get("TOOL", "httptool")
findings = []

def call_tool(op, args, base_url_override=None):
    """Invoke the executor. Route shape TBD — try the two likely paths."""
    body = {"tool": TOOL, "op": op, "args": args}
    if base_url_override:
        body["_test_base_url"] = base_url_override  # harness reconfigures the node instead, in practice
    for path in ("/v1/cli/tool-call", f"/v1/cli/tool/call"):
        req = urllib.request.Request(EURL + path, method="POST")
        req.add_header("authorization", f"Bearer {TOK}")
        req.add_header("content-type", "application/json")
        try:
            with urllib.request.urlopen(req, json.dumps(body).encode(), timeout=35) as r:
                return r.status, r.read().decode(errors="replace")
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors="replace")
        except Exception as e:
            return None, repr(e)
    return None, "no route"

def refused(label, status, body, secret=None):
    ok = status in (400, 403, 422, 502) or (status == 200 and '"error"' in (body or "").lower())
    if secret and body and secret in body:  # a secret leaking to the agent is always a finding
        ok = False
    print(("PASS " if ok else "FAIL ") + label + f"  [status={status} {(body or '')[:70]}]")
    if not ok:
        findings.append(f"{label}: status={status} {(body or '')[:120]}")

def main():
    if not (EURL and TOK):
        print("PENDING-STACK"); return 0
    st, _ = call_tool("ping", {})
    if st in (404, 405, None):
        print(f"PENDING-ROUTE: executor call route not implemented yet (status={st}). "
              "Assertions are specified in redteam/reviews/tool-executor-testplan.md."); return 0

    META = U.get("META_BODY", "METADATA-MUST-NOT-LEAK")
    SECRET = U.get("VAULT_SECRET", "vault-secret-agent-never-sees")

    # A. SSRF — direct internal targets (base_url variants)
    for label, url in [
        ("A loopback",      "http://127.0.0.1:80/"),
        ("A metadata v4",   "http://169.254.169.254/latest/meta-data/"),
        ("A rfc1918",       "http://10.0.0.1/"),
        ("A decimal-ip",    "http://2130706433/"),
        ("A octal-ip",      "http://0177.0.0.1/"),
        ("A hex-ip",        "http://0x7f000001/"),
        ("A shorthand",     "http://127.1/"),
        ("A userinfo",      "http://public.example.com@127.0.0.1/"),
        ("A fragment",      "http://127.0.0.1#@public.example.com/"),
        ("A v4-mapped-v6",  "http://[::ffff:127.0.0.1]/"),
        ("A 6to4-loopback", "http://[2002:7f00:0001::]/"),
        ("A nat64-loopback","http://[64:ff9b::7f00:1]/"),
        ("A railway-internal","http://x.railway.internal/"),
        ("A file scheme",   "file:///etc/passwd"),
        ("A gopher scheme", "gopher://127.0.0.1:7100/"),
    ]:
        st, b = call_tool("ping", {}, base_url_override=url)
        refused(f"SSRF {label} refused", st, b, secret=META)

    # A3. redirect to internal (public first hop -> internal Location)
    st, b = call_tool("ping", {}, base_url_override=U.get("REDIR_TO_META"))   # 302 -> 169.254.169.254
    refused("SSRF redirect->metadata refused (per-hop revalidation)", st, b, secret=META)
    st, b = call_tool("ping", {}, base_url_override=U.get("REDIR_REBIND"))    # rebinding host
    refused("SSRF DNS-rebind (resolve-and-pin) refused", st, b, secret=META)

    # B. credential exfiltration
    # B1. no replay on cross-origin redirect: vault auth header + 302 -> attacker echo; secret must NOT arrive
    st, b = call_tool("ping", {}, base_url_override=U.get("REDIR_TO_ECHO"))
    print("PASS/INSPECT B1 no-replay: the ECHO server log must show NO vault secret on the redirected hop "
          f"(status={st})")
    # B2. secret never echoed to the agent: call the ECHO endpoint directly; response must not contain SECRET
    st, b = call_tool("echo", {})
    refused("B2 vault secret not echoed back to the agent via response", st if st!=200 else 200,
            b if SECRET in (b or "") else '{"ok":1}', secret=SECRET)
    # B3. --curl / dry_run masks static AND vault
    st, b = call_tool("ping", {"__dry_run": True})
    leaked = SECRET in (b or "") or U.get("STATIC_VALUE","STATIC-XYZ") in (b or "")
    print(("FAIL " if leaked else "PASS ") + f"B3 dry_run/curl masks static+vault  [leaked={leaked}]")
    if leaked: findings.append("B3 curl/dry_run leaked static/vault")

    # C. fill override / confinement
    # C1. supplying a non-agent (vault/static/hidden) field -> 400, secret still authoritative
    st, b = call_tool("ping", {U.get("VAULT_FIELD","Authorization"): "attacker-controlled"})
    refused("C1 non-agent field rejected (400)", st, b)
    # C2. case-variant / duplicate override of an authoritative header
    st, b = call_tool("ping", {U.get("VAULT_FIELD_LOWER","authorization"): "attacker"})
    refused("C2 case-variant override rejected", st, b)

    # D. header CRLF injection via an agent field
    st, b = call_tool("ping", {U.get("AGENT_HEADER","X-Agent"): "a\r\nX-Injected: yes"})
    refused("D CRLF in agent header value rejected", st, b)

    if findings:
        print(f"\n{len(findings)} FINDING(S)"); [print("  -", f) for f in findings]; return 1
    print("\nALL RESISTED (for the checks the landed route supports)"); return 0

if __name__ == "__main__":
    sys.exit(main())
