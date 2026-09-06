#!/usr/bin/env python3
"""The paste-code OAuth flow end to end — TESTPLAN AUTH-paste-*.

`POST /v1/agents/:id/auth/begin` spawns the real harness's `auth login --claudeai`,
scrapes the authorize URL off its stdout and holds the child open until the user pastes
the code back. Three things can go wrong and only one of them is about the URL:

  the URL      must survive the CLI's surrounding prose intact (state + PKCE challenge),
               because a truncated URL fails only later, in the browser, as "invalid state"
  the child    every begin spawns a process that waits on stdin forever; an abandoned
               login must not leave one behind per retry
  the code     a wrong code must not authenticate, and must not half-authenticate

The 15-minute TTL reap is NOT asserted here: `LoginSessions.ttl` is settable only from
Rust (oauth.rs:68), so covering it end to end would mean a 15-minute test. SDK's
`an_abandoned_login_is_collected_when_its_ttl_runs_out` covers the timer at unit level.
What IS covered here is the other reaping path, which is the one a user actually hits:
`begin` kills the previous in-flight login for the node, so retrying a login cannot leak
a process per attempt.
"""
import json, os, subprocess, sys, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results

SKIP = 77
R = Results()
PORT = int(os.environ.get("WHEEL_ENGINE_AUTH_PORT", "17425"))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-auth-secret-at-least-16chars"
NAME = "qa-engine-auth-paste"
CODE = "fake-auth-code"


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, body=None):
    import urllib.error, urllib.request
    r = urllib.request.Request(BASE + path, method=method)
    r.add_header("Authorization", "Bearer " + SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        r.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(r, data, timeout=60) as resp:
            txt = resp.read().decode(errors="replace")
            return resp.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(txt)
        except Exception:
            return e.code, txt


def start_engine():
    sh("docker", "rm", "-f", NAME)
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    p = sh("docker", "run", "-d", "--name", NAME,
           "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
           "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
           "-e", "WHEEL_VAULT_KEY=" + key,
           "-e", "WHEEL_ROLE=engine",
           "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
           "-e", "WHEEL_FAKE_LOGIN_CODE=" + CODE,
           "-p", "%d:7000" % PORT, "wheel-engine:test")
    if p.returncode != 0:
        return "could not start wheel-engine:test: " + p.stderr.strip()[:200]
    for _ in range(60):
        try:
            if http("GET", "/healthz")[0] == 200:
                return None
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def login_children():
    """Every `auth login` child alive in the sandbox right now."""
    p = sh("docker", "exec", NAME, "ps", "-eo", "pid,args")
    return [l for l in p.stdout.splitlines() if "auth" in l and "login" in l and "ps -eo" not in l]


def main():
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", "wheel-engine:test").returncode != 0:
        print("wheel-engine:test not built — run `make engine-image-test`")
        return SKIP
    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        st, body = http("POST", "/v1/nodes",
                        {"name": "loginner", "type": "agent", "position": {"x": 0, "y": 0},
                         "config": {"harness": "claude", "system_prompt": "L",
                                    "run_on_startup": False, "ephemeral_context": False}})
        agent = (body or {}).get("id")
        if not R.check("AUTH-paste/setup", agent is not None, "node create -> %s %s" % (st, body)):
            return R.report("engine-auth-paste")

        st, b1 = http("POST", "/v1/agents/%s/auth/begin" % agent)
        if not R.check("AUTH-paste-begin", 200 <= st < 300,
                       "auth/begin answered %s: %s" % (st, str(b1)[:200])):
            return R.report("engine-auth-paste")

        R.check("AUTH-paste-mode", (b1 or {}).get("mode") == "paste_code",
                "claude must be paste_code (contract §4); got %r" % (b1 or {}).get("mode"))

        url = (b1 or {}).get("url") or ""
        R.check("AUTH-paste-url", url.startswith("https://"),
                "auth/begin returned no https url: %r" % url[:160])
        # The prose around the URL is the trap: a scanner that grabs the whole line, or
        # stops at the first space, yields a URL that only fails later in the browser.
        R.check("AUTH-paste-url-intact",
                "state=" in url and "code_challenge=" in url and " " not in url
                and "\n" not in url,
                "the authorize url lost its state/PKCE or picked up prose: %r" % url[:200])

        kids = login_children()
        R.check("AUTH-paste-child", len(kids) == 1,
                "expected exactly one `auth login` child after begin, saw %d" % len(kids))

        # A user who gives up and clicks "sign in" again must not cost a process.
        st, b2 = http("POST", "/v1/agents/%s/auth/begin" % agent)
        R.check("AUTH-paste-begin/retry", 200 <= st < 300, "second begin answered %s" % st)
        R.check("AUTH-paste-url-fresh", (b2 or {}).get("url") not in (None, "", url),
                "the retry replayed the first url — an abandoned login's state was reused")
        time.sleep(1)
        kids = login_children()
        R.check("AUTH-paste-supersede", len(kids) == 1,
                "an abandoned login leaked: %d `auth login` children after two begins"
                % len(kids))

        # ---- a wrong code must not authenticate, and must not half-authenticate
        st, _ = http("POST", "/v1/agents/%s/auth/complete" % agent, {"code": "not-the-code"})
        R.check("AUTH-paste-wrong-code", st >= 400,
                "a bogus paste code was accepted (%s)" % st)
        st, auth = http("GET", "/v1/agents/%s/auth" % agent)
        R.check("AUTH-paste-wrong-code/state", not (auth or {}).get("authenticated"),
                "the agent reports authenticated after a REJECTED code: %s" % str(auth)[:160])

        # ---- and the real one must
        st, b3 = http("POST", "/v1/agents/%s/auth/begin" % agent)
        st, _ = http("POST", "/v1/agents/%s/auth/complete" % agent, {"code": CODE})
        ok = 200 <= st < 300
        R.check("AUTH-paste-complete", ok, "the correct code was refused: %s" % st)
        if ok:
            st, auth = http("GET", "/v1/agents/%s/auth" % agent)
            R.check("AUTH-paste-complete/state", (auth or {}).get("authenticated") is True,
                    "auth/complete succeeded but GET auth still says %s" % str(auth)[:160])
            time.sleep(1)
            R.check("AUTH-paste-reaped", len(login_children()) == 0,
                    "the login child outlived a COMPLETED login")
        else:
            for tid in ("AUTH-paste-complete/state", "AUTH-paste-reaped"):
                R.skip(tid, "the login never completed, so there is nothing to check")

        # The code and the token are credentials; neither belongs in the agent log.
        st, log = http("GET", "/v1/agents/%s/log" % agent)
        R.check("AUTH-paste-no-code-in-log", CODE not in json.dumps(log),
                "the pasted code is in the agent log")
    finally:
        sh("docker", "rm", "-f", NAME)

    return R.report("engine-auth-paste")


if __name__ == "__main__":
    sys.exit(main())
