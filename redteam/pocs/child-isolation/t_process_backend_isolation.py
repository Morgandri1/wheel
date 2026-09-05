#!/usr/bin/env python3
"""F003 + F007 — process-backend cross-tenant isolation (M1.5). Owner: API(host)+SDK(engine).

The prod-shape backend: ALL projects' engines + agents + scripts run as `process` sandboxes in ONE
container on ONE kernel (host = root, drops to a per-NODE uid from each project's uid range). There is
no namespace/container boundary between tenants, so isolation reduces to: (a) uid-drop correctness,
(b) filesystem perms, (c) /proc visibility, (d) rlimits, (e) egress. Every agent is untrusted RCE
(bypassPermissions, F002), so a single gap here is a full cross-tenant breach.

Vehicle: run as (or exec into) a project uid inside the host container and attempt each cross-tenant
reach. Skips until a SANDBOX_BACKEND=process host is up and two projects (A, B) exist.
Env: WHEEL_HOST_CONTAINER (the host container name), WHEEL_UID_A, WHEEL_PID_A, WHEEL_PID_B,
     WHEEL_HOST_SECRET_CANARY (a sentinel value the host holds, to grep for in sandbox env).
Set them from the host boot, then run. Each check prints PASS(isolated)/FAIL(FINDING).
"""
import os, subprocess, sys

C = os.environ.get("WHEEL_HOST_CONTAINER")
UID_A = os.environ.get("WHEEL_UID_A")           # a node uid in project A
PID_A = os.environ.get("WHEEL_PID_A")            # project A id
PID_B = os.environ.get("WHEEL_PID_B")            # project B id (the victim)
findings = []

def as_uid(uid, argv):
    """Run argv inside the host container AS project uid `uid` (setpriv drops real+eff+saved + groups)."""
    return subprocess.run(
        ["docker", "exec", "-u", str(uid), C, "sh", "-lc", argv],
        capture_output=True, text=True, timeout=30)

def check(label, isolated, detail=""):
    print(("PASS " if isolated else "FAIL ") + label + ((f"  [{detail}]") if detail else ""))
    if not isolated:
        findings.append(label + (f" — {detail}" if detail else ""))

def run():
    if not (C and UID_A and PID_A and PID_B):
        print("PENDING-PROCESS-BACKEND: set WHEEL_HOST_CONTAINER/WHEEL_UID_A/WHEEL_PID_A/WHEEL_PID_B")
        return 0

    # 1. uid-drop correctness: a node's own id must equal its assigned uid; no ambient root.
    r = as_uid(UID_A, "id -u; id -G")
    check("1 uid-drop: child runs as its node uid, not root",
          r.stdout.strip().splitlines()[0:1] == [str(UID_A)] and " 0" not in (" " + r.stdout),
          r.stdout.strip().replace("\n", " "))

    # 2. cross-uid filesystem: A must NOT read B's data dir (mode 0700 to B's uid).
    r = as_uid(UID_A, f"cat /data/projects/{PID_B}/wheel.db 2>&1 | head -c1 | xxd | head -1; ls -la /data/projects/{PID_B} 2>&1 | head -3")
    check("2 /data/projects/<other> unreadable cross-uid",
          "Permission denied" in r.stdout or "No such file" in r.stdout,
          r.stdout.strip()[:80])

    # 3. cross-uid engine socket: A must NOT reach B's engine control socket.
    r = as_uid(UID_A, f"test -S /run/wheel/{PID_B}/engine.sock && (printf 'GET /v1/board HTTP/1.0\\r\\n\\r\\n' | nc -U /run/wheel/{PID_B}/engine.sock 2>&1 | head -c40) || echo NOSOCK-OR-DENIED")
    check("3 /run/wheel/<other>/engine.sock unreachable cross-uid",
          "Permission denied" in r.stdout or "NOSOCK-OR-DENIED" in r.stdout or "denied" in r.stdout.lower(),
          r.stdout.strip()[:80])

    # 4. /proc/<pid>/environ of another tenant must be unreadable (hidepid or uid perms).
    r = as_uid(UID_A, "for p in /proc/[0-9]*/environ; do cat $p 2>/dev/null; done | tr '\\0' '\\n' | grep -iE 'WHEEL_ENGINE_SECRET|WHEEL_VAULT_KEY|WHEEL_TOKEN|WHEEL_HOST_SECRET' | grep -v $$ | head")
    leaked = r.stdout.strip()
    check("4 no sibling/host secret via /proc/<pid>/environ", leaked == "", leaked[:80])

    # 5. host secret must be absent from A's OWN environment too.
    r = as_uid(UID_A, "cat /proc/self/environ | tr '\\0' '\\n' | grep -iE 'WHEEL_HOST_SECRET' | head")
    check("5 host secret absent from sandbox env", r.stdout.strip() == "", r.stdout.strip()[:80])

    # 6. WHEEL_TOKEN must be a 0600 file owned by the node uid, NOT in env (F007 ruling).
    r = as_uid(UID_A, "cat /proc/self/environ | tr '\\0' '\\n' | grep -c '^WHEEL_TOKEN='")
    check("6 WHEEL_TOKEN not in env (delivered via 0600 file)", r.stdout.strip() == "0", "env WHEEL_TOKEN count=" + r.stdout.strip())

    # 7. rlimits / fork bomb: a project must not exhaust the shared host (nproc cap).
    r = as_uid(UID_A, "ulimit -u")
    check("7 per-uid nproc rlimit set (fork-bomb containment)",
          r.stdout.strip().isdigit() and int(r.stdout.strip()) < 100000,
          "ulimit -u=" + r.stdout.strip())

    # NOTE (deployed only, not local): reach of *.railway.internal (postgres, host :7100) from a
    # sandbox — run on the Railway host, never locally, and only against our own infra:
    #   as_uid(UID_A, "getent hosts wheel-host.railway.internal; nc -z -w2 wheel-host.railway.internal 7100")
    # Expected: DENIED by egress policy. Left commented so this never dials infra from a laptop.

    if findings:
        print(f"\n{len(findings)} FINDINGS (cross-tenant reach):")
        for f in findings:
            print("  - " + f)
        return 1
    print("\nall isolation checks passed (process backend holds)")
    return 0

if __name__ == "__main__":
    sys.exit(run())
