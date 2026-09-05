#!/usr/bin/env python3
"""API auth, ownership and tenancy — TESTPLAN API-*.

The heart of this file is API-auth-owner-404: a valid token for someone else's project
must be INDISTINGUISHABLE from a project that does not exist. Not "refused" —
indistinguishable. A 403 is a correct-looking refusal that still answers "does project
<uuid> exist?", which turns any id leak into an enumeration oracle. So these assertions
compare status, body AND the shape of the response between the two cases, rather than
just checking that both were rejected.
"""
import sys, os, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import call, mint, api_healthy, unique_sub, Results, ISSUER

R = Results()


def main():
    api_healthy()

    alice = mint(unique_sub("alice"))
    mallory = mint(unique_sub("mallory"))

    # ---------------------------------------------------------------- unauthenticated
    st, _, _ = call("GET", "/v1/projects")
    R.check("API-auth-missing", st == 401, "no token -> %s (want 401)" % st)

    st, _, _ = call("GET", "/v1/projects", "not-a-jwt")
    R.check("API-auth-invalid/garbage", st == 401, "garbage token -> %s" % st)

    st, _, _ = call("GET", "/v1/projects", mint(unique_sub("u"), exp_delta=-10))
    R.check("API-auth-invalid/expired", st == 401, "expired -> %s" % st)

    st, _, _ = call("GET", "/v1/projects", mint(unique_sub("u"), nbf_delta=3600))
    R.check("API-auth-invalid/nbf", st == 401, "not-yet-valid -> %s" % st)

    st, _, _ = call("GET", "/v1/projects", mint(unique_sub("u"), iss="https://evil.example"))
    R.check("API-auth-invalid/issuer", st == 401, "wrong issuer -> %s" % st)

    st, _, _ = call("GET", "/v1/projects", mint(unique_sub("u"), secret=b"wrong-secret-entirely"))
    R.check("API-auth-wrong-key", st == 401, "wrong signing key -> %s" % st)

    # alg:none — the classic. Signature stripped, alg swapped.
    import base64, json, time
    def b64u(b):
        return base64.urlsafe_b64encode(b).rstrip(b"=").decode()
    now = int(time.time())
    hdr = b64u(json.dumps({"alg": "none", "typ": "JWT"}).encode())
    pay = b64u(json.dumps({"sub": unique_sub("alice"), "iss": ISSUER,
                           "exp": now + 3600, "nbf": now - 60}).encode())
    st, _, _ = call("GET", "/v1/projects", "%s.%s." % (hdr, pay))
    R.check("API-auth-alg-none", st == 401, "alg:none accepted -> %s" % st)

    # ---------------------------------------------------------------- ownership
    st, proj, _ = call("POST", "/v1/projects", alice, {"name": "qa-auth-board"})
    if not R.check("API-project-create", st in (200, 201), "-> %s %r" % (st, proj)):
        return R.report("api-auth")
    pid = proj["id"]

    st_own, body_own, _ = call("GET", "/v1/projects/%s" % pid, alice)
    R.check("API-project-get-own", st_own == 200, "owner GET -> %s" % st_own)

    st_other, body_other, _ = call("GET", "/v1/projects/%s" % pid, mallory)
    ghost = str(uuid.uuid4())
    st_ghost, body_ghost, _ = call("GET", "/v1/projects/%s" % ghost, mallory)

    R.check("API-auth-owner-404", st_other == 404,
            "another user's project -> %s (want 404, NOT 403)" % st_other)
    R.check("API-auth-owner-404/indistinguishable",
            st_other == st_ghost and body_other == body_ghost,
            "other=%s/%r vs nonexistent=%s/%r — these must be identical or project ids "
            "are enumerable" % (st_other, body_other, st_ghost, body_ghost))

    # An INVALID token against someone else's project must 401, not 404: proves the JWT is
    # verified BEFORE the project is loaded (API-auth-order).
    st, _, _ = call("GET", "/v1/projects/%s" % pid, "not-a-jwt")
    R.check("API-auth-order", st == 401,
            "invalid token on a real project -> %s (401 proves verify-then-load)" % st)

    # Ownership must gate mutation and lifecycle too, not just reads.
    for verb, path, tid in (
        ("PATCH", "/v1/projects/%s" % pid, "API-auth-owner-404/patch"),
        ("DELETE", "/v1/projects/%s" % pid, "API-auth-owner-404/delete"),
        ("POST", "/v1/projects/%s/start" % pid, "API-auth-owner-404/start"),
        ("POST", "/v1/projects/%s/stop" % pid, "API-auth-owner-404/stop"),
    ):
        body = {"name": "hijacked"} if verb == "PATCH" else None
        st, _, _ = call(verb, path, mallory, body)
        R.check(tid, st == 404, "%s %s as non-owner -> %s" % (verb, path, st))

    # And the engine proxy, which is the interesting one: it must not forward at all.
    st, _, _ = call("GET", "/v1/projects/%s/engine/v1/board" % pid, mallory)
    R.check("API-proxy-auth", st == 404, "non-owner engine proxy -> %s" % st)

    st, _, _ = call("GET", "/v1/projects/%s/engine/v1/board" % pid)
    R.check("API-proxy-auth/unauth", st == 401, "unauthenticated engine proxy -> %s" % st)

    # ---------------------------------------------------------------- tenancy listing
    st, mine, _ = call("GET", "/v1/projects", alice)
    R.check("API-project-list", st == 200 and any(p["id"] == pid for p in (mine or [])),
            "alice's list -> %s" % st)
    st, theirs, _ = call("GET", "/v1/projects", mallory)
    R.check("API-tenancy-list", st == 200 and not any(p["id"] == pid for p in (theirs or [])),
            "mallory can see alice's project in her list")

    st, _, _ = call("DELETE", "/v1/projects/%s" % pid, alice)
    R.check("API-project-delete", st in (200, 204), "owner delete -> %s" % st)

    # §5: delete "stops + removes container + volume". An orphaned sandbox per deleted
    # project is a slow resource leak that looks like nothing until the host runs out of
    # memory — which it did today, with nine leaked containers on a 16 GB box.
    import subprocess, time
    if subprocess.run(["docker", "info"], capture_output=True).returncode == 0:
        name = "wheel-p-%s" % pid
        gone = False
        for _ in range(20):
            q = subprocess.run(["docker", "ps", "-aq", "--filter", "name=" + name],
                               capture_output=True, text=True)
            if not q.stdout.strip():
                gone = True
                break
            time.sleep(1)
        R.check("API-project-delete-reaps", gone,
                "container %s still exists after the project was deleted" % name)
        if not gone:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True)
    else:
        R.skip("API-project-delete-reaps", "docker not available")

    return R.report("api-auth")


if __name__ == "__main__":
    sys.exit(main())
