#!/usr/bin/env python3
"""MCP-* — the built-in MCP server (§3c #1), driven as a model would drive it.

`wheel mcp-serve` speaks JSON-RPC 2.0 over stdio and is attached to every agent at spawn.
It is the PRIMARY agent interface, ahead of the shell, so a fault here is not cosmetic: it
is the surface an untrusted model uses to reach the board.

Everything below goes through the real binary over real stdin/stdout in the real image. A
unit test of the dispatcher cannot see the two failures that actually happened -- a tool
advertised with no handler behind it, and a handler registered by an edit that matched
nothing.
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
NAME = "qa-engine-mcp-%s" % uuid.uuid4().hex[:8]
PORT = free_port(int(os.environ.get("WHEEL_ENGINE_MCP_PORT", "17431")))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-mcp-secret-at-least-16chars"
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


def mcp(node_id, *requests, token_file=None):
    """Drive `wheel mcp-serve` over stdio and return the parsed responses.

    One process per call, exactly as a harness starts it, so nothing here can pass because
    of state a previous request left behind.
    """
    stdin = "".join(json.dumps(r) + "\n" for r in requests)
    tf = token_file or "/data/run/%s/token" % node_id
    p = subprocess.run(
        ["docker", "exec", "-i",
         "-e", "WHEEL_TOKEN_FILE=" + tf,
         "-e", "WHEEL_ENGINE_URL=http://127.0.0.1:7000",
         NAME, "wheel", "mcp-serve"],
        input=stdin, capture_output=True, text=True, timeout=60)
    out = []
    for line in p.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            out.append(json.loads(line))
        except ValueError:
            pass
    return p, out


def rpc(method, params=None, id_=1):
    r = {"jsonrpc": "2.0", "id": id_, "method": method}
    if params is not None:
        r["params"] = params
    return r


def tool_names(responses):
    for r in responses:
        tools = ((r.get("result") or {}).get("tools")) if isinstance(r, dict) else None
        if tools:
            return {t.get("name") for t in tools if isinstance(t, dict)}
    return set()


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


def main():
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    global IMAGE
    pinned = pin_image(IMAGE)
    if not pinned:
        print("%s not built — run `make engine-image-test`" % IMAGE)
        return SKIP
    IMAGE = pinned
    print("image %s" % pinned[:19])

    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        alice, st, _ = node("alice", "agent", agent_cfg(), x=0)
        notes, _, _ = node("notes", "ctx", {"markdown": "# notes\n\nMCP-CANARY-4b1a\n"}, x=200)
        locked, _, _ = node("locked", "ctx", {"markdown": "not for alice"}, x=400)
        if not R.check("MCP/setup", alice and notes and locked, "node creation -> %s" % st):
            return R.report("engine-mcp")

        http("POST", "/v1/agents/%s/start" % alice)
        if not R.check("MCP/token-file", wait_token(alice),
                       "no node token file, so every MCP call below would fail as transport"):
            return R.report("engine-mcp")

        # ---- the tool list is the model's whole view of the board --------------
        p, res = mcp(alice, rpc("initialize"), rpc("tools/list", id_=2))
        first = tool_names(res)
        if not R.check("MCP-tools-list", bool(first),
                       "tools/list returned nothing parseable: rc=%s stderr=%r"
                       % (p.returncode, p.stderr[:200])):
            return R.report("engine-mcp")

        # ---- every advertised tool must have something behind it ---------------
        #
        # The class SDK just hit: `run` and `ctx_clear` were ADVERTISED to the model with
        # no handler at all. A model does not experience that as an error it can route
        # around; it experiences the board as unreliable and stops trying things that
        # would have worked. So call each advertised tool with empty arguments and require
        # that the failure is a real one -- a denial or a bad-argument error -- never
        # "unknown tool" / "method not found".
        unhandled = []
        for name in sorted(first):
            _, out = mcp(alice, rpc("tools/call", {"name": name, "arguments": {}}, id_=7))
            blob = json.dumps(out).lower()
            if ("unknown tool" in blob or "not found" in blob or "unimplemented" in blob
                    or "no such tool" in blob or not out):
                unhandled.append(name)
        R.check("MCP-every-tool-has-handler", not unhandled,
                "advertised with nothing behind them: %s" % sorted(unhandled))

        # ---- the list must track CURRENT wires, with no restart ----------------
        #
        # The server fetches the tool list from the engine per request precisely so a wire
        # added while an agent is running is usable immediately. Asserted as a SET
        # DIFFERENCE, so a list that merely changed size cannot pass for the right change.
        http("POST", "/v1/wires", {"from": alice, "to": notes, "type": "read"})
        _, res2 = mcp(alice, rpc("initialize"), rpc("tools/list", id_=2))
        after = tool_names(res2)
        R.check("MCP-tools-list-follows-wires", after >= first,
                "the tool list SHRANK after adding a wire: lost %s" % sorted(first - after))
        gained_or_same = after != first or "read" in {n.split("__")[0] for n in after}
        R.check("MCP-tools-current-without-restart", gained_or_same,
                "adding a wire changed nothing in a freshly-started server; the list is "
                "not being fetched live (first=%s after=%s)" % (sorted(first), sorted(after)))

        # ---- the wire is still the capability, through MCP as through the CLI --
        _, out = mcp(alice, rpc("tools/call",
                                {"name": "read", "arguments": {"addr": "notes"}}, id_=9))
        wired_ok = R.check("MCP-read-wired", "MCP-CANARY-4b1a" in json.dumps(out),
                "a wired ctx could not be read through MCP: %s" % json.dumps(out)[:200])
        _, out = mcp(alice, rpc("tools/call",
                                {"name": "read", "arguments": {"addr": "locked"}}, id_=10))
        blob = json.dumps(out)
        R.check("MCP-read-unwired-denied", "not for alice" not in blob,
                "MCP read an UNWIRED ctx — the MCP path bypasses the wire matrix")
        # Gated on the WIRED read working. The first version of this suite passed
        # MCP-read-unwired-denied while sending the wrong argument name, so the content
        # was absent because the call malformed rather than because a wire was enforced —
        # a denial assertion that passes against a broken request proves nothing at all.
        R.check("MCP-read-unwired-denied/meaningful", wired_ok,
                "the denial above is not evidence: the WIRED read failed too, so this "
                "assertion would pass against a server that refuses everything")
        R.check("MCP-denial-is-explained", "wire" in blob.lower() or "denied" in blob.lower(),
                "the denial does not say it was a wire denial: %s" % blob[:200])

        # ---- the token is a FILE, never argv (§5b: argv is world-readable) -----
        ps = sh("docker", "exec", NAME, "sh", "-c",
                "for p in /proc/[0-9]*; do tr '\\0' ' ' < $p/cmdline 2>/dev/null; echo; done")
        R.check("MCP-token-not-in-argv",
                "WHEEL_TOKEN=" not in ps.stdout and "--token" not in ps.stdout,
                "a token appears in a command line, readable by every uid in the sandbox")

        # ---- a rotated token stops working ------------------------------------
        #
        # Tokens rotate on every agent start. If a stale one still worked, a token captured
        # once would be permanent, and rotation would be theatre.
        stale = sh("docker", "exec", NAME, "cat", "/data/run/%s/token" % alice).stdout.strip()
        sh("docker", "exec", NAME, "sh", "-c",
           "cp /data/run/%s/token /data/stale-token" % alice)
        http("POST", "/v1/agents/%s/stop" % alice)
        http("POST", "/v1/agents/%s/start" % alice)
        wait_token(alice)
        fresh = sh("docker", "exec", NAME, "cat", "/data/run/%s/token" % alice).stdout.strip()
        if not R.check("MCP-token-rotates", stale and fresh and stale != fresh,
                       "the node token did not change across a stop/start, so 'rotated' "
                       "cannot be tested and rotation is not happening"):
            return R.report("engine-mcp")
        _, out = mcp(alice, rpc("tools/call",
                                {"name": "read", "arguments": {"addr": "notes"}}, id_=11),
                     token_file="/data/stale-token")
        R.check("MCP-rotated-token-refused", "MCP-CANARY-4b1a" not in json.dumps(out),
                "a ROTATED token still reads through MCP: rotation is theatre")
    finally:
        sh("docker", "rm", "-f", NAME)

    return R.report("engine-mcp")


if __name__ == "__main__":
    sys.exit(run_suite(main, "engine-mcp", container=NAME))
