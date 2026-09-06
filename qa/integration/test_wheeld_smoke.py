#!/usr/bin/env python3
"""WHEELD-* — one binary, nothing installed (M1.7).

The claim `wheeld` makes is that a person downloads ONE executable, runs it, and has a
working Wheel: no Postgres, no docker daemon, no toolchain. Every other suite here proves
pieces of the system with a stack already standing; this one proves the promise a new user
actually meets, and it is the only suite whose failure means "the thing we ship does not
start".

So it deliberately provides nothing. No DATABASE_URL, no compose, no engine image: a temp
data dir and the binary. If wheeld quietly needs something, this is where that shows up --
and the environment is scrubbed of WHEEL_*/DATABASE_URL precisely so a variable left in my
shell cannot supply it for the user.
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, free_port  # noqa: E402

SKIP = 77
R = Results()
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
PORT = free_port(int(os.environ.get("WHEEL_WHEELD_PORT", "18080")))
BASE = "http://127.0.0.1:%d" % PORT
EMAIL = "wheeld-smoke@example.test"
PASSWORD = "correct-horse-battery-staple"


def http(method, path, body=None, token=None, timeout=30):
    req = urllib.request.Request(BASE + path, method=method)
    if token:
        req.add_header("x-auth-token", token)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=timeout) as r:
            txt = r.read().decode(errors="replace")
            return r.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(txt)
        except Exception:
            return e.code, txt
    except Exception as e:
        return 0, str(e)


def clean_env(data_dir):
    """The environment a NEW USER has, not the one this repo's shell has.

    Every WHEEL_* and DATABASE_URL is removed. If wheeld needs one, the point of this suite
    is to find that out -- and a variable sitting in my shell would supply it silently and
    turn the one honest test of the promise into a test of my laptop.
    """
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("WHEEL_") and k not in ("DATABASE_URL", "BIND_ADDR")}
    env["WHEEL_DATA_DIR"] = data_dir          # the one flag a user would pass
    return env


def main():
    binary = shutil.which("wheeld") or os.path.join(ROOT, "target", "debug", "wheeld")
    if not os.path.exists(binary):
        build = subprocess.run(["cargo", "build", "-p", "wheeld"], cwd=ROOT,
                               capture_output=True, text=True)
        if build.returncode != 0:
            print("could not build wheeld: %s" % build.stderr[-400:])
            return SKIP
        binary = os.path.join(ROOT, "target", "debug", "wheeld")
    if not os.path.exists(binary):
        print("wheeld binary not found after build")
        return SKIP

    data_dir = tempfile.mkdtemp(prefix="wheeld-smoke-")
    proc = subprocess.Popen([binary, "--data-dir", data_dir, "--bind", "127.0.0.1:%d" % PORT],
                            env=clean_env(data_dir), stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True)
    try:
        up = False
        for _ in range(120):
            if proc.poll() is not None:
                break
            if http("GET", "/healthz")[0] == 200:
                up = True
                break
            time.sleep(0.5)

        if not up:
            out = ""
            if proc.poll() is not None:
                out = (proc.stdout.read() or "")[-600:]
            R.check("WHEELD-starts", False,
                    "wheeld did not serve /healthz within 60s%s"
                    % (" — it exited: %s" % out if out else ""))
            return R.report("wheeld-smoke")
        R.check("WHEELD-starts", True)

        # Nothing was installed for this. If it works, it works with sqlite in the data dir.
        R.check("WHEELD-sqlite-store",
                any(n.endswith(".db") for n in os.listdir(data_dir)),
                "no sqlite file in %s — where did it put the board?" % data_dir)

        st, body = http("POST", "/v1/auth/signup", {"email": EMAIL, "password": PASSWORD})
        token = (body or {}).get("token") if isinstance(body, dict) else None
        if not R.check("WHEELD-signup", 200 <= st < 300 and bool(token),
                       "signup -> %s %s" % (st, str(body)[:160])):
            return R.report("wheeld-smoke")

        st, proj = http("POST", "/v1/projects", {"name": "smoke"}, token=token)
        pid = (proj or {}).get("id") if isinstance(proj, dict) else None
        R.check("WHEELD-project", 200 <= st < 300 and bool(pid),
                "create project -> %s %s" % (st, str(proj)[:160]))

        # The whole point of one binary: the board is reachable without a second service.
        if pid:
            st, board = http("GET", "/v1/projects/%s/engine/v1/board" % pid, token=token)
            R.check("WHEELD-engine-reachable", st == 200,
                    "the per-project engine did not answer through the API in the same "
                    "process: %s %s" % (st, str(board)[:160]))
    finally:
        # SIGTERM, not kill: a daemon a person runs in a terminal must stop when they press
        # ctrl-c, and must not leave the sqlite store wedged for the next start.
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=20)
            R.check("WHEELD-sigterm", proc.returncode in (0, -signal.SIGTERM, 143),
                    "wheeld exited %s on SIGTERM" % proc.returncode)
        except subprocess.TimeoutExpired:
            proc.kill()
            R.check("WHEELD-sigterm", False, "wheeld ignored SIGTERM for 20s and was killed")
        shutil.rmtree(data_dir, ignore_errors=True)

    return R.report("wheeld-smoke")


if __name__ == "__main__":
    sys.exit(main())
