#!/usr/bin/env python3
"""ENG-panic-* — a bad row must not take the board down, and the escaper must not panic.

The board has been 502 since 14:34 on a panic. A panic in the delivery path is worse than a
wrong answer: it takes down every project on the host, and a stored row that triggers it
takes the engine down again on every restart, so the system cannot recover by being turned
off and on again.

Two properties, and the second is the one that matters:

  1. The envelope escaper never panics on any input. Byte-indexing a UTF-8 string is the
     classic way to panic here: an em dash is 3 bytes, an emoji 4, and a boundary landing
     mid-character is a slice on a non-char-boundary. Driven from outside by putting those
     characters at EVERY offset around the escaper's boundary rather than at one.

  2. AN ENGINE STILL STARTS WHEN A STORED MESSAGE CANNOT BE PARSED. Whatever the parser
     rejects, it must reject that ROW, not the boot. A project whose database contains one
     unparseable message must still serve; anything else is a permanent outage that
     survives every restart, which is exactly what a 502 since 14:34 looks like.
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, free_port  # noqa: E402

SKIP = 77
R = Results()
PORT = free_port(int(os.environ.get("WHEEL_PANIC_PORT", "17436")))
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-panic-%s" % uuid.uuid4().hex[:8]
SECRET = "qa-panic-secret-at-least-16c"

# One of each width, plus a combining sequence: 3-byte em dash, 4-byte emoji, 3-byte CJK,
# and a family emoji built from joiners, which is where naive "one char" logic also fails.
NASTY = ["—", "\U0001F600", "中文", "\U0001F468‍\U0001F469‍\U0001F467"]
CLOSE = "</AgentPrompt>"


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, body=None, timeout=20):
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Authorization", "Bearer " + SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=timeout) as r:
            txt = r.read().decode(errors="replace")
            return r.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        return e.code, None
    except Exception as e:
        return 0, str(e)


def start(name, port):
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    sh("docker", "run", "-d", "--name", name,
       "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
       "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
       "-e", "WHEEL_VAULT_KEY=" + key,
       "-e", "WHEEL_ROLE=engine",
       "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
       "-p", "%d:7000" % port, "wheel-engine:test")
    for _ in range(60):
        if http("GET", "/healthz")[0] == 200:
            return True
        time.sleep(0.5)
    return False


def main():
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", "wheel-engine:test").returncode != 0:
        print("wheel-engine:test not built — run `make engine-image-test`")
        return SKIP

    try:
        if not R.control("ENG-panic/engine-up", start(NAME, PORT),
                         "the engine did not start, so nothing below is evidence"):
            return R.report("engine-panics")

        st, agent = http("POST", "/v1/nodes",
                         {"name": "victim", "type": "agent", "position": {"x": 0, "y": 0},
                          "config": {"harness": "claude", "system_prompt": "V",
                                     "run_on_startup": False, "ephemeral_context": False}})
        aid = (agent or {}).get("id")
        if not R.control("ENG-panic/setup", bool(aid), "node create -> %s" % st):
            return R.report("engine-panics")

        # ---- 1. the escaper, with the boundary landing at every offset ----------------
        #
        # A fixed body exercises ONE alignment. The panic shape is a boundary landing
        # mid-character, so the character has to be walked across the boundary: pad by 0..N
        # bytes so every offset is hit, for each width.
        survived = True
        detail = ""
        for ch in NASTY:
            for pad in range(0, 8):
                body = ("a" * pad) + ch + CLOSE + ch + ("b" * pad)
                st, _ = http("POST", "/v1/agents/%s/send" % aid, {"body": body})
                if st == 0 or st >= 500:
                    survived = False
                    detail = ("body with %r at offset %d answered %s — a 5xx or a dropped "
                              "connection here is the panic" % (ch, pad, st))
                    break
            if not survived:
                break
        R.gated("ENG-escaper-never-panics", "ENG-panic/engine-up", survived, detail)

        # Still serving afterwards: a panic can take the process down AFTER answering.
        R.gated("ENG-escaper-engine-survives", "ENG-panic/engine-up",
                http("GET", "/healthz")[0] == 200,
                "the engine stopped serving after the escaper inputs — it panicked on one "
                "of them and took the board with it")

        # ---- 2. THE ONE THAT MATTERS: an unparseable row must not stop the boot -------
        #
        # Written straight into sqlite, because the point is a row the ENGINE did not
        # create and would never accept — the state a bad migration, a partial write or an
        # older version leaves behind.
        # Invalid UTF-8 built in SQL, not in a Python string literal. My first attempt
        # passed a lone surrogate and a NUL through `docker exec python3 -c`, which the
        # shell and the encoder mangled between them — the control caught it, which is the
        # only reason this is a corrected fixture rather than a false green.
        script = (
            "import sqlite3, uuid\n"
            "c = sqlite3.connect('/data/wheel.db')\n"
            "c.execute(\"INSERT INTO messages (id,from_kind,from_id,to_id,body,sha256,"
            "bytes,state,is_error,created_at) VALUES (?,?,?,?,CAST(x'FFFE003C2F41' AS TEXT),"
            "?,?,?,?,?)\", (str(uuid.uuid4()), 'user', None, %r, 'not-a-sha', 6, 'queued', "
            "0, 'not-a-timestamp'))\n"
            "c.commit()\n"
            "print('inserted')\n" % aid)
        bad = sh("docker", "exec", "-i", NAME, "python3", "-c", script)
        if not R.control("ENG-panic/bad-row-inserted", "inserted" in bad.stdout,
                         "could not write an unparseable row, so a green below would only "
                         "mean the engine restarted normally: %s"
                         % (bad.stderr or "")[-200:]):
            return R.report("engine-panics")

        sh("docker", "restart", NAME)
        back = False
        for _ in range(60):
            if http("GET", "/healthz")[0] == 200:
                back = True
                break
            time.sleep(0.5)
        logs = sh("sh", "-c", "docker logs --tail 6 %s 2>&1" % NAME).stdout[-300:]
        R.gated("ENG-starts-with-unparseable-message", "ENG-panic/bad-row-inserted", back,
                "the engine did not come back after a restart with ONE unparseable message "
                "row. The parser must reject the ROW, not the boot: a row it cannot read "
                "takes the board down on every restart, so the system cannot recover by "
                "being turned off and on again. Engine said: %s" % logs)
    finally:
        sh("docker", "rm", "-f", NAME)

    return R.report("engine-panics")


if __name__ == "__main__":
    sys.exit(main())
