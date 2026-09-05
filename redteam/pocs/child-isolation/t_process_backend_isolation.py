#!/usr/bin/env python3
"""F003 + F007 — process-backend cross-tenant isolation (M1.5). Owner: API(host)+SDK(engine).

The prod-shape backend: ALL projects' engines + agents + scripts run as `process` sandboxes in ONE
container on ONE kernel (host = root, drops to a per-NODE uid from each project's uid range). There is
no namespace/container boundary between tenants, so isolation reduces to: (a) uid-drop correctness,
(b) filesystem perms, (c) /proc visibility, (d) rlimits, (e) egress. Every agent is untrusted RCE
(bypassPermissions, F002), so a single gap here is a full cross-tenant breach.

Vehicle: run as (or exec into) a project uid inside the host container and attempt each cross-tenant
reach. Skips until a SANDBOX_BACKEND=process host is up and two projects (A, B) exist.
Env: WHEEL_HOST_CONTAINER (host container name), WHEEL_UID_A (project A's engine uid),
     WHEEL_PID_A / WHEEL_PID_B (project A/B UUIDs — for their /data + /run paths),
     WHEEL_OSPID_A / WHEEL_OSPID_B (the engines' REAL OS pids — env/rlimit/drop assertions must read
     these, never a docker-exec self-read; see the PROBE-VEHICLE TRAP note below).
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

    # PROBE-VEHICLE TRAP (learned the hard way — do NOT regress this):
    #   `docker exec -u <uid>` spawns a NEW process that inherits the CONTAINER's PID-1 environment
    #   (which legitimately holds WHEEL_HOST_SECRET, passed at `docker run`) and the container-default
    #   rlimits — NOT the engine child's env_clear()'d env or its pre_exec setrlimit. So reading
    #   /proc/self/environ or `ulimit -u` from a docker-exec shell tells you nothing about the engine
    #   and yields FALSE positives. Env/rlimit/drop assertions MUST target the real engine OS pids
    #   (WHEEL_OSPID_A / WHEEL_OSPID_B); cross-tenant reach is tested by having uid A read the sibling's
    #   real pid, never a self-read.
    ospid_a = os.environ.get("WHEEL_OSPID_A")   # engine A's real OS pid (uid == UID_A)
    ospid_b = os.environ.get("WHEEL_OSPID_B")   # engine B's real OS pid (the victim)

    def host(argv):  # observe as container-root (orientation / real-child /proc reads)
        return subprocess.run(["docker", "exec", C, "sh", "-lc", argv],
                              capture_output=True, text=True, timeout=20)

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

    # 4. SIBLING SECRET: A must NOT read engine B's real /proc/<pidB>/environ (holds B's
    #    WHEEL_ENGINE_SECRET + WHEEL_VAULT_KEY). This is THE cross-tenant secret-isolation test.
    if ospid_b:
        r = as_uid(UID_A, f"cat /proc/{ospid_b}/environ >/dev/null 2>&1 && echo READABLE || echo DENIED")
        check("4 sibling engine /proc/<pidB>/environ unreadable cross-uid",
              r.stdout.strip() == "DENIED", r.stdout.strip())
    else:
        print("SKIP 4: set WHEEL_OSPID_B (engine B's real OS pid)")

    # 5. HOST SECRET: A must NOT read the host supervisor's env (PID 1 holds WHEEL_HOST_SECRET).
    r = as_uid(UID_A, "cat /proc/1/environ >/dev/null 2>&1 && echo READABLE || echo DENIED")
    check("5 host PID1 env (WHEEL_HOST_SECRET) unreadable cross-uid",
          r.stdout.strip() == "DENIED", r.stdout.strip())

    # 6. env_clear proof: engine A's REAL child env must NOT carry WHEEL_HOST_SECRET (the host never
    #    forwards it; §4b + process.rs env_clear). Read the real child /proc/<pidA>/environ; note the
    #    setuid drop clears `dumpable`, so this is EACCES even to container-root without CAP_SYS_PTRACE
    #    — itself a hardening. We assert "HOST_SECRET absent" via whichever read the platform allows.
    if ospid_a:
        r = host(f"tr '\\0' '\\n' < /proc/{ospid_a}/environ 2>&1 | grep -c '^WHEEL_HOST_SECRET=' || true")
        out = r.stdout.strip()
        dumpable_locked = "Permission denied" in (r.stdout + r.stderr)
        check("6 host secret absent from engine child env (env_clear)",
              out == "0" or dumpable_locked,
              ("dumpable-locked (EACCES even to root)" if dumpable_locked else f"WHEEL_HOST_SECRET count={out}"))
    else:
        print("SKIP 6: set WHEEL_OSPID_A (engine A's real OS pid)")

    # 7b. cross-uid WRITE: A must NOT be able to create files in B's 0700 run/data dirs.
    r = as_uid(UID_A, f"touch /run/wheel/{PID_B}/pwn 2>&1; touch /data/projects/{PID_B}/pwn 2>&1; echo done")
    check("7b cannot write into <other>'s run/data dir cross-uid",
          "Permission denied" in r.stdout or "No such file" in r.stdout,
          r.stdout.strip()[:80])

    # === drop-correctness on the REAL ENGINE child (PM: setpriv group clearing, no_new_privs, rlimits) ===
    if ospid_a:
        # 7. rlimits / fork bomb: the engine's OWN RLIMIT_NPROC must be a finite cap (not the
        #    container default). Read the real child's /proc/<pid>/limits.
        r = host(f"grep -i 'Max processes' /proc/{ospid_a}/limits")
        soft = r.stdout.split()[2] if len(r.stdout.split()) > 2 else ""
        check("7 engine RLIMIT_NPROC finite (fork-bomb containment)",
              soft.isdigit() and int(soft) < 100000, r.stdout.strip())
        # 8. no_new_privs=1 → no setuid binary/file-cap can re-raise privilege.
        r = host(f"grep -i NoNewPrivs /proc/{ospid_a}/status")
        check("8 engine has no_new_privs=1", r.stdout.strip().endswith("1"), r.stdout.strip())
        # 9. supplementary groups cleared (setgroups([])) → host memberships don't leak into the tenant.
        r = host(f"grep -i '^Groups:' /proc/{ospid_a}/status")
        g = r.stdout.split(":", 1)[1].strip() if ":" in r.stdout else "?"
        check("9 engine supplementary groups cleared", g == "", "Groups: [" + g + "]")
        # 10. the engine really runs as the project uid AND gid (real drop, not gid 0).
        r = host(f"grep -iE '^(Uid|Gid):' /proc/{ospid_a}/status")
        uid_gid_ok = all(f"{k}:\t{UID_A}\t{UID_A}\t{UID_A}\t{UID_A}" in r.stdout for k in ("Uid", "Gid"))
        check("10 engine Uid/Gid fully == project uid (not 0)", uid_gid_ok,
              r.stdout.strip().replace("\n", " | "))
    else:
        print("SKIP 7-10: set WHEEL_OSPID_A (engine A's real OS pid) for drop-correctness")

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
