#!/usr/bin/env python3
"""MCP-* — the board as MCP tools over stdio (§3c #1).

Driven the way Claude drives it: `wheel mcp-serve` as a child process, line-delimited
JSON-RPC 2.0 on stdin/stdout. Not the HTTP route underneath it — the whole point of this
surface is that a model talks to it, and the CLI's framing is part of what can break.

The load-bearing one is MCP-advertised-has-handler. SDK's MCP server advertised `run` and
`ctx_clear` with no handler behind either; a tool that resolves to nothing teaches a model
the board is unreliable and it stops trying things that would have worked. Advertising is
a promise, and this is the test that the promise is kept.
"""
import json
import os
import subprocess
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, pin_image, run_suite, free_port  # noqa: E402

R = Results()
SKIP = 77
PORT = free_port(int(os.environ.get("WHEEL_ENGINE_MCP_PORT", "17431")))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-mcp-secret-at-least-16chars"
NAME = "qa-engine-mcp-%s" % uuid.uuid4().hex[:8]
IMAGE = os.environ.get("WHEEL_ENGINE_IMAGE", "wheel-engine:test")


def sh(*a, **kw):
    return subprocess.run(a, capture_output=True, text=True, **kw)


def http(method, path, body=None, token=SECRET):
    import urllib.error, urllib.request
    r = urllib.request.Request(BASE + path, method=method)
    r.add_header("Authorization", "Bearer " + token)
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
           "-p", "%d:7000" % PORT, IMAGE)
    if p.returncode != 0:
        return "could not start %s: %s" % (IMAGE, p.stderr.strip()[:200])
    for _ in range(90):
        try:
            if http("GET", "/healthz")[0] == 200:
                return None
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def node(name, typ, cfg, x=0):
    st, body = http("POST", "/v1/nodes", {"name": name, "type": typ,
                                          "position": {"x": x, "y": 0}, "config": cfg})
    return (body or {}).get("id"), st, body


def agent_cfg(prompt="M"):
    return {"harness": "claude", "system_prompt": prompt,
            "run_on_startup": False, "ephemeral_context": False}


def wait_token(node_id, timeout=60):
    for _ in range(int(timeout * 2)):
        if sh("docker", "exec", NAME, "test", "-s",
              "/data/run/%s/token" % node_id).returncode == 0:
            return True
        time.sleep(0.5)
    return False


class Mcp:
    """One `wheel mcp-serve` child, spoken to exactly as a harness speaks to it."""

    def __init__(self, node_id):
        self.p = subprocess.Popen(
            ["docker", "exec", "-i",
             "-e", "WHEEL_TOKEN_FILE=/data/run/%s/token" % node_id,
             "-e", "WHEEL_ENGINE_URL=http://127.0.0.1:7000",
             NAME, "wheel", "mcp-serve"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, bufsize=1)
        self.n = 0

    def died(self):
        """Why the child is gone, in its own words.

        The first version of this suite reported `initialize -> null` and then died on a
        broken pipe, which says the process left but not why -- I had to reproduce it by
        hand to find out, which is a diagnostic the test should have handed me.
        """
        if self.p.poll() is None:
            return ""
        try:
            err = (self.p.stderr.read() or "").strip()
        except Exception:
            err = ""
        return " | mcp-serve exited %s: %s" % (self.p.returncode, err[-300:] or "(silent)")

    def call(self, method, params=None, timeout=30):
        self.n += 1
        req = {"jsonrpc": "2.0", "id": self.n, "method": method}
        if params is not None:
            req["params"] = params
        if self.p.poll() is not None:
            return {"_dead": self.died()}
        try:
            self.p.stdin.write(json.dumps(req) + "\n")
            self.p.stdin.flush()
        except BrokenPipeError:
            return {"_dead": self.died()}
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.p.stdout.readline()
            if not line:
                return None
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                continue
            if msg.get("id") == self.n:
                return msg
        return None

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=10)
        except Exception:
            self.p.kill()


def tool_names(listing):
    return sorted(t.get("name") for t in ((listing or {}).get("result") or {}).get("tools", []))


def main():
    global IMAGE
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE).returncode != 0:
        print("%s not built — run `make engine-image-test`" % IMAGE)
        return SKIP

    pinned = pin_image()
    if pinned:
        IMAGE = pinned
        print("image wheel-engine:test = %s" % pinned[:19])

    err = start_engine()
    if err:
        print(err)
        return SKIP

    # Does the image under test actually contain the feature under test?
    #
    # The first run of this suite reported MCP-initialize FAILED against an image whose
    # `wheel` predated the MCP merge. That is a red that blames the product for the age of
    # a build artifact, and it is the same shape as testing an image another agent replaced
    # under me. A missing feature in a stale image is a gate that CANNOT RUN, not a gate
    # that failed.
    probe = sh("docker", "exec", NAME, "wheel", "mcp-serve", "--help")
    combined = (probe.stdout or "") + (probe.stderr or "")
    if "unknown command" in combined:
        print("this %s predates `wheel mcp-serve` (%s) — run `make engine-image-test`"
              % (IMAGE[:19], combined.strip()[:80]))
        return SKIP

    alice, st, _ = node("alice", "agent", agent_cfg(), x=0)
    notes, _, _ = node("notes", "ctx", {"markdown": "# notes"}, x=200)
    later, _, _ = node("later", "ctx", {"markdown": "# later"}, x=400)
    if not R.check("MCP/setup", alice and notes and later, "node creation -> %s" % st):
        return R.report("engine-mcp")

    http("POST", "/v1/wires", {"from": alice, "to": notes, "type": "read"})
    http("POST", "/v1/agents/%s/start" % alice)
    if not R.check("MCP/token-file", wait_token(alice),
                   "no node token file; every call below would be a transport error"):
        return R.report("engine-mcp")

    m = Mcp(alice)
    try:
        init = m.call("initialize", {})
        if not R.check("MCP-initialize",
                       bool(init) and "result" in (init or {}) and
                       "protocolVersion" in ((init or {}).get("result") or {}),
                       "initialize -> %s%s" % (json.dumps(init)[:200], m.died())):
            # Nothing below can mean anything if the server never came up, and pressing on
            # produces a broken pipe rather than a verdict.
            return R.report("engine-mcp")

        listing = m.call("tools/list", {})
        names = tool_names(listing)
        R.check("MCP-tools-list", bool(names), "tools/list returned nothing: %s"
                % json.dumps(listing)[:200])

        # ---- the list follows the wires, with no restart -------------------------
        #
        # ORDER MATTERS, and it bit me. This runs BEFORE the call-everything sweep below,
        # because that sweep invokes every advertised tool -- including ctx_clear, which
        # rotates the agent's session. Run after it, this check failed with "unknown or
        # expired node token" and read as "the MCP server ignores new wires", which is a
        # bug report I would have sent to SDK about my own test ordering. Read-only checks
        # first; anything with side effects last.
        st_w, _ = http("POST", "/v1/wires", {"from": alice, "to": later, "type": "read"})
        after_list = m.call("tools/list", {})
        after_txt = json.dumps(after_list)
        R.check("MCP-tools-live-wires", "later" in after_txt,
                "the same long-lived mcp-serve process did not see a wire added after it "
                "started (wire POST -> %s). A model would have to be restarted to notice a "
                "node it was just granted. Listing was: %s" % (st_w, after_txt[:400]))
        R.check("MCP-tools-live-wires/still-has-old", "notes" in after_txt,
                "adding a wire dropped the previously reachable node from the listing: %s"
                % after_txt[:400])

        # ---- the promise is kept: everything advertised resolves to a handler ----
        #
        # -32602 "unknown tool" is the exact failure SDK shipped with run/ctx_clear. A
        # wire denial or a bad argument is a TOOL error (isError) and is FINE here: the
        # claim is that the tool EXISTS, not that this call succeeds.
        orphans = []
        for name in names:
            resp = m.call("tools/call", {"name": name, "arguments": {}})
            code = (((resp or {}).get("error") or {}).get("code"))
            if code == -32602:
                orphans.append(name)
        R.check("MCP-advertised-has-handler", not orphans,
                "advertised with no handler behind them: %s — a tool that resolves to "
                "nothing teaches a model the board is unreliable" % orphans)

        # Positive control. If `route_for` ever answered every name, the check above would
        # pass against a server that advertises anything at all.
        bogus = m.call("tools/call", {"name": "definitely_not_a_tool_xyz", "arguments": {}})
        R.check("MCP-unknown-tool-is-refused",
                (((bogus or {}).get("error") or {}).get("code")) == -32602,
                "a made-up tool name was NOT refused with -32602, so the check above "
                "cannot detect an orphan either: %s" % json.dumps(bogus)[:200])

        # ---- the token is a FILE, never argv (§5b: argv is world-readable) -------
        ps = sh("docker", "exec", NAME, "sh", "-c",
                "for p in /proc/[0-9]*; do tr '\\0' ' ' < $p/cmdline 2>/dev/null; echo; done")
        token = sh("docker", "exec", NAME, "cat", "/data/run/%s/token" % alice).stdout.strip()
        R.check("MCP-token-not-in-argv",
                bool(token) and token not in ps.stdout,
                "the node token appears in a process command line; argv is readable by "
                "every uid on the box")
    finally:
        m.close()

    # ---- a rotated token is refused ---------------------------------------------
    #
    # Tokens rotate on every agent start (§4). A long-lived mcp-serve holding the old one
    # must stop working, or a stopped agent's credential outlives the agent.
    stale = Mcp(alice)
    try:
        stale.call("initialize", {})
        old_token = sh("docker", "exec", NAME, "cat",
                       "/data/run/%s/token" % alice).stdout.strip()
        http("POST", "/v1/agents/%s/restart" % alice)
        time.sleep(6)
        new_token = sh("docker", "exec", NAME, "cat",
                       "/data/run/%s/token" % alice).stdout.strip()
        if not R.check("MCP-token-rotates", bool(new_token) and new_token != old_token,
                       "the token did not change across a restart, so 'a rotated token is "
                       "refused' has nothing to test"):
            return R.report("engine-mcp")
        # The stale child still holds the old file contents in its own process.
        resp = stale.call("tools/list", {})
        txt = json.dumps(resp)
        R.check("MCP-rotated-token-refused",
                resp is None or "error" in (resp or {}) or "401" in txt or "unauthor" in txt.lower(),
                "a client holding the PREVIOUS token still got a tool listing: %s"
                % txt[:200])
    finally:
        stale.close()

    # ---- PM ruling: a table node's name may not contain '-' ----------------------
    bad_id, bad_st, bad_body = node("table-1", "table",
                                    {"columns": [{"name": "v", "type": "text"}]}, x=600)
    msg = json.dumps(bad_body)
    R.check("WM-table-name-hyphen", bad_st >= 400,
            "a table node named 'table-1' was ACCEPTED (%s); PM ruled it must be refused"
            % bad_st)
    R.check("WM-table-name-hyphen/explains", "-" in msg and ("_" in msg or "identifier" in msg),
            "refused without naming the fix: %s" % msg[:200])
    # No silent rename: nothing named table_1 may have appeared instead.
    st_board, board = http("GET", "/v1/board")
    R.check("WM-table-name-hyphen/no-silent-rename",
            "table_1" not in json.dumps(board),
            "the engine renamed 'table-1' to 'table_1' instead of refusing it — a node "
            "whose address is not what the user typed is unaddressable by the agent that "
            "was told about it")

    return R.report("engine-mcp")


def _cleanup():
    subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)


if __name__ == "__main__":
    sys.exit(run_suite(main, "engine-mcp", _cleanup, container=NAME))
