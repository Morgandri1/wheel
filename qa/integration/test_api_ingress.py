#!/usr/bin/env python3
"""Public ingress — TESTPLAN ING-*.

Ingress is the only unauthenticated path into a project, so it is the one route where a
mistake is reachable by the whole internet. Two things are asserted hardest:

  ING-cap-off    with capability http disabled, the request must be refused at the API and
                 must not reach the container at all.
  ING-traversal  no encoding of `..` may climb out of /p/<id>/ into the control plane.

Traversal is tested through raw HTTP rather than urllib's normalising helpers, because a
client that helpfully normalises the path tests the client rather than the server.
"""
import http.client, os, sys, urllib.parse
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import call, mint, api_healthy, unique_sub, Results, API

R = Results()


def raw_request(path, method="GET", host_hdr=None):
    """Send `path` verbatim — no normalisation, no encoding help."""
    u = urllib.parse.urlparse(API)
    conn = http.client.HTTPConnection(u.hostname, u.port or 80, timeout=30)
    try:
        conn.putrequest(method, path, skip_host=False, skip_accept_encoding=True)
        conn.putheader("Accept", "*/*")
        conn.endheaders()
        r = conn.getresponse()
        return r.status, r.read(4096).decode(errors="replace")
    finally:
        conn.close()


def main():
    api_healthy()
    alice = mint(unique_sub("alice"))

    st, proj, _ = call("POST", "/v1/projects", alice, {"name": "qa-ingress"})
    if not R.check("ING-setup/create", st in (200, 201), "-> %s %r" % (st, proj)):
        return R.report("api-ingress")
    pid = proj["id"]
    caps = (proj or {}).get("capabilities") or {}
    R.check("ING-cap-default-off", caps.get("http") is False,
            "http capability defaults to %r — a public write path should be opt-in"
            % caps.get("http"))

    try:
        # ------------------------------------------------------------ capability off
        st, body, _ = call("GET", "/p/%s/anything" % pid)
        R.check("ING-cap-off", st == 403, "ingress with http disabled -> %s %r" % (st, body))

        st, _, _ = call("POST", "/p/%s/anything" % pid, body={"x": 1})
        R.check("ING-cap-off/post", st == 403, "POST ingress with http disabled -> %s" % st)

        # ------------------------------------------------------------ no auth needed
        # Ingress is public by design; assert a token is neither required nor honoured as
        # a way to bypass the capability gate.
        st, _, _ = call("GET", "/p/%s/anything" % pid, alice)
        R.check("ING-cap-off/owner-token", st == 403,
                "the owner's own token bypassed the capability gate -> %s" % st)

        # ------------------------------------------------------------ no enumeration
        st_ghost, body_ghost, _ = call("GET", "/p/00000000-0000-4000-8000-000000000000/x")
        R.check("ING-no-enumeration", st_ghost in (403, 404),
                "nonexistent project ingress -> %s" % st_ghost)

        # ------------------------------------------------------------ traversal (S1)
        pid_q = pid
        for label, path in (
            ("dotdot", "/p/%s/../v1/board" % pid_q),
            ("dotdot-deep", "/p/%s/../../v1/board" % pid_q),
            ("encoded", "/p/%s/%%2e%%2e/v1/board" % pid_q),
            ("double-encoded", "/p/%s/%%252e%%252e/v1/board" % pid_q),
            ("backslash", "/p/%s/..\\v1\\board" % pid_q),
            ("semicolon", "/p/%s/..;/v1/board" % pid_q),
            ("mixed", "/p/%s/.%%2e/v1/board" % pid_q),
        ):
            try:
                st, body = raw_request(path)
            except Exception as e:
                R.check("ING-traversal/%s" % label, False, "request error: %r" % e)
                continue
            # Reaching the control plane means a 200 with board-shaped JSON. Anything that
            # refuses, 404s, or redirects is acceptable; leaking the board is not.
            leaked = st == 200 and ('"nodes"' in body or '"project"' in body)
            R.check("ING-traversal/%s" % label, not leaked,
                    "path %s -> %s %s" % (path, st, body[:120]))

        # control-plane routes must never be reachable unauthenticated, however addressed
        st, body = raw_request("/v1/board")
        R.check("ING-control-plane-unreachable", st in (401, 403, 404),
                "/v1/board unauthenticated -> %s %s" % (st, body[:100]))

        # ------------------------------------------------------------ capability on
        st, patched, _ = call("PATCH", "/v1/projects/%s" % pid, alice,
                              {"capabilities": {"http": True}})
        if R.check("ING-cap-toggle", st == 200 and
                   ((patched or {}).get("capabilities") or {}).get("http") is True,
                   "enabling http -> %s %r" % (st, patched)):
            st, body, _ = call("GET", "/p/%s/anything" % pid)
            # The project isn't started, so anything except 403 is correct here: the point
            # is that the capability gate stopped rejecting and the request got further.
            R.check("ING-cap-on", st != 403,
                    "http enabled but ingress still 403 -> %s %r" % (st, body))
    finally:
        call("DELETE", "/v1/projects/%s" % pid, alice)

    return R.report("api-ingress")


if __name__ == "__main__":
    sys.exit(main())
