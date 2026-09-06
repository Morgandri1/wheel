#!/usr/bin/env python3
"""WOW — Wheel building Wheel (TESTPLAN WOW-*, contract M1.6, operator priority).

The acceptance test for the whole product, not for a component: an agent inside a sandbox,
handed a git token from a vault node, clones this repository and runs `cargo test -p
wheel-core` — the same gate its own authors run. If that works, the sandbox is a real
development environment and not a demo.

It needs a toolchain image (git + gh + cargo inside the sandbox) that `wheel-engine:test`
does not yet carry, so today every criterion below reports SKIP with that reason. It is
committed now, red-by-absence rather than absent, so that the day SDK lands the toolchain
image this suite turns green or red on its own instead of waiting for someone to remember
to write it.

Deliberately NOT hermetic in the usual sense: it clones over the network and compiles. It
is opt-in (`WHEEL_WOW=1`) and never runs in the default CI matrix — a 10-minute cargo build
inside a container does not belong in a gate developers run before every merge.
"""
import base64, json, os, subprocess, sys, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, run_suite, free_port

SKIP = 77
R = Results()
# Unique per RUN, not just per suite. A fixed name means any second runner — another
# agent's session on this shared host, a stale container from a killed run — is removed by
# our own `docker rm -f` and removes ours by theirs. This run lost its engine mid-clone
# exactly that way. Suite-level uniqueness (qa/contract/suite_isolation.py) is not enough
# when the same suite can be running twice.
NAME = "qa-engine-wow-%s" % uuid.uuid4().hex[:8]
PORT = free_port(int(os.environ.get("WHEEL_ENGINE_WOW_PORT", "17426")))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-wow-secret-at-least-16chars"
IMAGE = os.environ.get("WHEEL_ENGINE_IMAGE", "wheel-engine:test")
REPO = os.environ.get("WHEEL_WOW_REPO", "https://github.com/Morgandri1/wheel.git")

# What the sandbox must provide before this test means anything. Named individually so the
# skip reason says WHICH tool is missing rather than "unsupported".
TOOLCHAIN = ("git", "cargo", "gh")


def sh(*a, **kw):
    return subprocess.run(a, capture_output=True, text=True, **kw)


def missing_tools(image):
    out = []
    for t in TOOLCHAIN:
        p = sh("docker", "run", "--rm", "--entrypoint", "sh", image, "-c", "command -v " + t)
        if p.returncode != 0:
            out.append(t)
    return out


def http(method, path, body=None):
    import urllib.error, urllib.request
    r = urllib.request.Request(BASE + path, method=method)
    r.add_header("Authorization", "Bearer " + SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        r.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(r, data, timeout=120) as resp:
            txt = resp.read().decode(errors="replace")
            return resp.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(txt)
        except Exception:
            return e.code, txt
    except urllib.error.URLError as e:
        # The engine went away mid-run. Report it as a value the caller can assert on; a
        # traceback here reads as "the test is broken" when the subject is what vanished.
        return 0, {"error": "engine unreachable: %s" % e.reason}


def node(name, typ, cfg, x=0):
    st, body = http("POST", "/v1/nodes", {"name": name, "type": typ,
                                          "position": {"x": x, "y": 0}, "config": cfg})
    return (body or {}).get("id"), st


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


def engine_postmortem():
    """Why the engine went away — asked while the container still exists.

    "Connection refused" has at least three causes here and they need different actions:
    the kernel OOM-killed it (this host runs six agents and a cargo build is not small),
    another session removed it, or it crashed. Guessing produced a bug report against the
    wrong person once already; the container knows, so ask it before cleanup does.
    """
    p = sh("docker", "inspect", "-f",
           "status={{.State.Status}} oom={{.State.OOMKilled}} exit={{.State.ExitCode}}", NAME)
    if p.returncode != 0:
        return "the container no longer exists — something outside this suite removed it"
    logs = sh("docker", "logs", "--tail", "5", NAME)
    return "%s; last log: %s" % (p.stdout.strip(),
                                 (logs.stdout + logs.stderr).strip()[-300:] or "(none)")


def turn(agent, command, wait=180):
    """Ask the agent to run a command, and return what came back.

    The command travels as a directive inside a normal user message, so it goes through
    the real delivery path: queued, framed in an <AgentPrompt> envelope, written to the
    child's stdin by the single writer, executed by the child, and reported back through
    the engine's log. Nothing here reaches into the container.
    """
    b64 = base64.b64encode(command.encode()).decode()
    http("POST", "/v1/agents/%s/send" % agent,
         {"body": "<<FAKE:SH_B64=%s>>" % b64})
    deadline = time.time() + wait
    seen = ""
    while time.time() < deadline:
        _, log = http("GET", "/v1/agents/%s/log" % agent)
        seen = json.dumps(log)
        if "engine unreachable" in seen:
            return seen
        if "exit=" in seen:
            return seen
        time.sleep(2)
    return seen


def main():
    if os.environ.get("WHEEL_WOW") != "1":
        print("wheel-on-wheel is opt-in: set WHEEL_WOW=1 (it clones and compiles; minutes, "
              "not seconds, so it is not in the default gate)")
        return SKIP
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    image = IMAGE
    if subprocess.run(["docker", "image", "inspect", image], capture_output=True).returncode != 0:
        print("%s not built — run `make engine-image-test`" % image)
        return SKIP

    absent = missing_tools(image)
    if absent:
        # The whole point of the test is that a real agent can do real work in the sandbox.
        # Without the toolchain there is nothing to measure, and a green here would mean
        # only that we successfully did nothing.
        for tid in ("WOW-clone", "WOW-vault-token", "WOW-cargo-test", "WOW-no-token-in-log"):
            R.skip(tid, "sandbox image lacks %s — needs SDK's toolchain image" % ", ".join(absent))
        return R.report("wheel-on-wheel")

    print("toolchain present (%s) — running the real thing" % ", ".join(TOOLCHAIN))

    token = os.environ.get("WHEEL_WOW_GH_TOKEN") or sh("gh", "auth", "token").stdout.strip()
    if not token:
        for tid in ("WOW-vault-token", "WOW-clone", "WOW-cargo-test", "WOW-no-token-in-log"):
            R.skip(tid, "no GitHub token: set WHEEL_WOW_GH_TOKEN or run `gh auth login`")
        return R.report("wheel-on-wheel")

    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        vault, _ = node("ghsecrets", "vault", {"keys": ["GH_TOKEN"]})
        agent, st = node("builder", "agent",
                         {"harness": "claude", "system_prompt": "You build Wheel.",
                          "run_on_startup": False, "ephemeral_context": False}, x=200)
        if not R.check("WOW/setup", agent and vault, "node creation -> %s" % st):
            return R.report("wheel-on-wheel")

        # The credential reaches the agent the way the product intends: a vault node, a
        # read wire, exported at spawn. No token is passed on a command line anywhere.
        http("POST", "/v1/wires", {"from": agent, "to": vault, "type": "read"})
        st, _ = http("PUT", "/v1/vault/%s/GH_TOKEN" % vault, {"value": token})
        if not R.check("WOW-vault-token", 200 <= st < 300, "vault write answered %s" % st):
            return R.report("wheel-on-wheel")

        http("POST", "/v1/agents/%s/start" % agent)
        time.sleep(3)

        # `git clone` reads GH_TOKEN out of the environment via the credential helper, so
        # the secret is never in argv (argv is world-readable across uids — contract §5b).
        clone = ("set -e; cd /data; rm -rf wow; "
                 "git config --global credential.helper "
                 "'!f(){ echo username=x-access-token; echo password=$GH_TOKEN; };f'; "
                 "git clone --depth 1 %s wow 2>&1 | tail -3; "
                 "test -f wow/Cargo.toml && echo CLONE_OK" % REPO)
        out = turn(agent, clone, wait=180)
        vanished = "engine unreachable" in out
        if vanished:
            # The engine died; that says nothing about whether an agent can clone.
            R.skip("WOW-clone", "the engine went away mid-run — %s" % engine_postmortem())
        else:
            R.check("WOW-clone", "CLONE_OK" in out,
                    "the agent could not clone with its vault token: %s" % out[-400:])

        if os.environ.get("WHEEL_WOW_SKIP_BUILD") == "1":
            # The clone leg proves the interesting half (a vault secret reaching an agent
            # and authenticating a real remote); `cargo test` proves the sandbox is a build
            # environment. On a host with six agents resident the build gets OOM-killed, so
            # it is separable — but only ever by SKIPPING it, never by assuming it.
            R.skip("WOW-cargo-test", "WHEEL_WOW_SKIP_BUILD=1")
        elif not vanished and "CLONE_OK" in out:
            build = ("cd /data/wow && cargo test -p wheel-core 2>&1 | tail -15")
            out = turn(agent, build, wait=1800)
            R.check("WOW-cargo-test", "test result: ok" in out,
                    "cargo test -p wheel-core did not pass inside the sandbox: %s"
                    % out[-600:])
        else:
            R.skip("WOW-cargo-test", "nothing was cloned, so there is nothing to build")

        # The token travelled through the engine; it must not have been written down.
        #
        # Gated on the log being READABLE. An unreachable engine returns an error body with
        # no token in it, and "the token is not in this error message" is not the claim.
        # This assertion reported green against a dead engine on its first run, which is
        # the exact shape of vacuous pass this suite exists to avoid.
        st, log = http("GET", "/v1/agents/%s/log" % agent)
        if st == 0:
            R.skip("WOW-no-token-in-log", "the agent log could not be read, so its absence "
                                          "from the log proves nothing")
        else:
            R.check("WOW-no-token-in-log", token not in json.dumps(log),
                    "the GitHub token is in the agent log")
    finally:
        sh("docker", "rm", "-f", NAME)

    return R.report("wheel-on-wheel")


def _cleanup():
    subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)


if __name__ == "__main__":
    sys.exit(run_suite(main, "wheel-on-wheel", _cleanup))
