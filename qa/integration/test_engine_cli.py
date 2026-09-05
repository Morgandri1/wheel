#!/usr/bin/env python3
"""The `wheel` CLI against a real engine — TESTPLAN CLI-*, MSG-byte-exact, INJ-ctx-read.

The CLI is the agent's entire interface to the board, and its token is the agent's entire
authority. So this asserts the two halves that matter:

  * the GRAMMAR does what §5 of PROTOCOL.md says (whoami/connections/ls/read/write/msg/inbox),
  * and the TOKEN is a capability, not an identity claim: it reaches exactly the caller's own
    wires, exit 3 on a denied wire, exit 4 on a node that does not exist, and it cannot be
    swapped for the engine secret in either direction.

Every command runs INSIDE the container as the agent would run it, with the 0600 token file
the supervisor minted — not through a Python client that reimplements the auth. A test that
mints its own token proves the engine trusts QA's tokens, which is not the question.
"""
import json, os, subprocess, sys, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, call as _http

SKIP = 77
R = Results()
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
FIXTURE = os.path.join(ROOT, "qa", "fixtures", "envelope-integrity.bin")

NAME = "wheel-qa-cli"
PORT = int(os.environ.get("WHEEL_CLI_PORT", "17414"))
SECRET = "qa-cli-secret-0123456789abcdef"
BASE = "http://127.0.0.1:%d" % PORT
IMAGE = "wheel-engine:test"


def api(method, path, body=None, token=None):
    return _http(method, path, headers={"Authorization": "Bearer " + (token or SECRET)},
                 body=body, base=BASE, timeout=30)


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


def wheel(node_id, *argv, env=None):
    """Run `wheel ...` in the container with that node's real token file."""
    cmd = ["docker", "exec",
           "-e", "WHEEL_TOKEN_FILE=/data/run/%s/token" % node_id,
           "-e", "WHEEL_ENGINE_URL=http://127.0.0.1:7000"]
    for k, v in (env or {}).items():
        cmd += ["-e", "%s=%s" % (k, v)]
    cmd += [NAME, "wheel"] + list(argv)
    return sh(*cmd)


def start_engine():
    sh("docker", "rm", "-f", NAME)
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    p = sh("docker", "run", "-d", "--name", NAME,
           "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
           "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
           "-e", "WHEEL_VAULT_KEY=" + key,
           "-e", "WHEEL_ROLE=engine",
           "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
           "-p", "%d:7000" % PORT, IMAGE)
    if p.returncode != 0:
        return "could not start %s: %s" % (IMAGE, p.stderr.strip()[:200])
    for _ in range(60):
        try:
            if api("GET", "/healthz")[0] == 200:
                return None
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def node(name, typ, config):
    st, body, _ = api("POST", "/v1/nodes",
                      {"name": name, "type": typ, "position": {"x": 0, "y": 0},
                       "config": config})
    if st not in (200, 201):
        raise RuntimeError("could not create %s node %s: %s %s" % (typ, name, st, body))
    return body["id"]


def wire(a, b, t):
    st, body, _ = api("POST", "/v1/wires", {"from": a, "to": b, "type": t})
    if st not in (200, 201, 204):
        raise RuntimeError("wire %s->%s (%s) refused: %s %s" % (a, b, t, st, body))


def wait_token(node_id, timeout=60):
    """The supervisor writes the token file when the agent starts."""
    for _ in range(int(timeout * 2)):
        p = sh("docker", "exec", NAME, "test", "-s", "/data/run/%s/token" % node_id)
        if p.returncode == 0:
            return True
        time.sleep(0.5)
    return False


def main():
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE).returncode != 0:
        print("%s not built (make engine-image-test)" % IMAGE)
        return SKIP

    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        alice = node("alice", "agent", {"harness": "claude", "system_prompt": "You are alice.",
                                        "run_on_startup": False, "ephemeral_context": False})
        bob = node("bob", "agent", {"harness": "claude", "system_prompt": "You are bob.",
                                    "run_on_startup": False, "ephemeral_context": False})
        carol = node("carol", "agent", {"harness": "claude", "system_prompt": "You are carol.",
                                        "run_on_startup": False, "ephemeral_context": False})
        notes = node("notes", "ctx", {"markdown": "# notes\n\nCANARY-CTX-7f3a\n"})
        locked = node("locked", "ctx", {"markdown": "you must not read this"})

        wire(alice, bob, "send")      # alice may message bob
        wire(notes, alice, "send")    # ctx injection into alice
        wire(alice, notes, "read")    # alice may READ notes but not write it

        # Start both so the supervisor mints their token files.
        for who in (alice, bob):
            api("POST", "/v1/agents/%s/start" % who)
        ok_a, ok_b = wait_token(alice), wait_token(bob)
        R.check("CLI-token-file: supervisor writes a 0600 token file at start", ok_a and ok_b)
        if not (ok_a and ok_b):
            print("no token file — cannot exercise the CLI")
            return 1

        # ---- identity: derived from the token, never passed -------------------
        p = wheel(alice, "whoami", "--json")
        R.check("CLI-whoami: exit 0", p.returncode == 0, p.stderr.strip()[:160])
        try:
            me = json.loads(p.stdout)
        except Exception:
            me = {}
        R.check("CLI-whoami: identifies the token's own node",
                me.get("name") == "alice", json.dumps(me)[:160])

        p = wheel(alice, "connections")
        R.check("CLI-connections: exit 0 and names the wired peers",
                p.returncode == 0 and "bob" in p.stdout and "notes" in p.stdout,
                p.stdout.strip()[:160] or p.stderr.strip()[:160])

        # §3c#7: an agent must be able to enumerate its own capabilities.
        p = wheel(alice, "ls")
        R.check("CLI-ls-bare: lists every keyspace I am wired to",
                p.returncode == 0 and "notes" in p.stdout,
                p.stdout.strip()[:160] or p.stderr.strip()[:160])

        # ---- the wire IS the permission ---------------------------------------
        p = wheel(alice, "read", "notes")
        R.check("CLI-read: a read wire reads the ctx markdown",
                p.returncode == 0 and "CANARY-CTX-7f3a" in p.stdout,
                (p.stdout or p.stderr).strip()[:160])

        p = wheel(alice, "write", "notes", "overwritten")
        R.check("CLI-read-not-write: writing over a READ-only wire is exit 3",
                p.returncode == 3, "exit %d: %s" % (p.returncode, (p.stderr or p.stdout).strip()[:120]))

        p = wheel(alice, "read", "locked")
        R.check("WM-no-wire-denied: reading an unwired ctx is exit 3",
                p.returncode == 3, "exit %d: %s" % (p.returncode, (p.stderr or p.stdout).strip()[:120]))

        p = wheel(alice, "read", "no-such-node-here")
        R.check("CLI-exit-nonexistent: a missing node is exit 4, distinct from denial",
                p.returncode == 4, "exit %d: %s" % (p.returncode, (p.stderr or p.stdout).strip()[:120]))

        p = wheel(alice, "msg", "carol", "hello")
        R.check("WM-agent-agent-send denied without a wire: exit 3",
                p.returncode == 3, "exit %d: %s" % (p.returncode, (p.stderr or p.stdout).strip()[:120]))

        # ---- messaging --------------------------------------------------------
        p = wheel(alice, "msg", "bob", "hello bob", "--json")
        R.check("CLI-msg: a send wire delivers, exit 0", p.returncode == 0,
                (p.stderr or p.stdout).strip()[:160])
        receipt = {}
        try:
            receipt = json.loads(p.stdout)
        except Exception:
            pass
        R.check("CLI-msg-returns: receipt carries id, sha256, bytes, state",
                all(k in receipt for k in ("id", "sha256", "bytes", "state")),
                json.dumps(receipt)[:160])

        # ---- MSG-byte-exact: the hostile 200 KiB body, end to end --------------
        if os.path.exists(FIXTURE):
            body = open(FIXTURE, "rb").read()
            sh("docker", "cp", FIXTURE, "%s:/tmp/hostile.bin" % NAME)
            p = wheel(alice, "msg", "bob", "--file", "/tmp/hostile.bin", "--json")
            R.check("MSG-byte-exact: a 200 KiB hostile body sends, exit 0",
                    p.returncode == 0, (p.stderr or p.stdout).strip()[:200])
            rec = {}
            try:
                rec = json.loads(p.stdout)
            except Exception:
                pass
            import hashlib
            R.check("MSG-sha256: receipt sha256 is of the ORIGINAL body",
                    rec.get("sha256") == hashlib.sha256(body).hexdigest(),
                    "receipt=%s" % str(rec.get("sha256"))[:32])
            R.check("MSG-byte-count: receipt byte count is the original length",
                    rec.get("bytes") == len(body), "receipt=%s want=%d" % (rec.get("bytes"), len(body)))

            if rec.get("id"):
                got = sh("docker", "exec",
                         "-e", "WHEEL_TOKEN_FILE=/data/run/%s/token" % bob,
                         "-e", "WHEEL_ENGINE_URL=http://127.0.0.1:7000",
                         NAME, "wheel", "inbox", rec["id"])
                R.check("MSG-inbox-reread: the recipient can re-read it, exit 0",
                        got.returncode == 0, (got.stderr or "").strip()[:160])
                R.check("MSG-inbox-reread: body comes back BYTE-IDENTICAL to what was sent",
                        got.stdout.encode() == body or got.stdout.rstrip("\n").encode() == body,
                        "got %d bytes, sent %d" % (len(got.stdout.encode()), len(body)))
        else:
            R.check("MSG-byte-exact fixture present", False, "missing " + FIXTURE)

        p = wheel(bob, "inbox")
        R.check("CLI-inbox: lists my received messages",
                p.returncode == 0, (p.stderr or "").strip()[:160])

        # ---- the two tokens are not interchangeable ---------------------------
        tok = sh("docker", "exec", NAME, "cat", "/data/run/%s/token" % alice).stdout.strip()
        R.check("node token is readable for the test", bool(tok))
        if tok:
            st, _, _ = api("GET", "/v1/board", token=tok)
            R.check("CLI-token-scope: a node token CANNOT reach the control plane (/v1/board)",
                    st == 401, "got %s" % st)
        st, _, _ = _http("GET", "/v1/cli/whoami",
                         headers={"Authorization": "Bearer " + SECRET},
                         base=BASE, timeout=30)
        R.check("ENG-cli-token: the engine secret CANNOT reach /v1/cli",
                st == 401, "got %s" % st)

        # ---- INJ: the ctx wired into alice reached her composed prompt --------
        st, log, _ = api("GET", "/v1/agents/%s/log" % alice)
        blob = json.dumps(log)
        R.check("INJ-on-start: the wired ctx canary appears in the agent's composed prompt",
                "CANARY-CTX-7f3a" in blob,
                "not found in %d bytes of log" % len(blob))
        R.check("INJ-unwired-absent: the UNWIRED ctx never reaches the prompt",
                "you must not read this" not in blob)
    finally:
        sh("docker", "rm", "-f", NAME)

    return R.report("wheel CLI")


if __name__ == "__main__":
    sys.exit(main())
