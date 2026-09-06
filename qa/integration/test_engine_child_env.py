#!/usr/bin/env python3
"""SEC-child-env — an agent child must not inherit the engine's own secrets (F015).

The engine holds two things every tenant would like: WHEEL_ENGINE_SECRET (the
control-plane bearer, which is authority over the whole board) and WHEEL_VAULT_KEY
(which decrypts every vault in the project, wired or not). Until 5b74640 both were
inherited by every agent child, and a process can always read its own environment.

SDK's regression test (supervisor::tests::a_child_is_not_given_the_engines_own_secrets)
asserts those two NAMES are absent from the spawn spec. This suite asserts the same
property one layer further out — from inside a real child, in a real container, against
the environment the kernel actually gave it — and adds the case a name check cannot
reach: the same secret arriving under a DIFFERENT name. `env_clear()` plus an allowlist
is exactly the shape that makes that possible, because the allowlist is what decides
which names get repopulated and from what.

Every assertion here is "this value is absent", which is also what a suite reports when
it is pointed at nothing at all. SEC-child-env/sentinel-works is the positive control:
it hunts a secret that is SUPPOSED to be in the child (a vault value the agent is wired
to) using the identical mechanism, so a green absence means the search works.
"""
import base64
import hashlib
import json
import os
import subprocess
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, pin_image, free_port  # noqa: E402

R = Results()
SKIP = 77
PORT = free_port(int(os.environ.get("WHEEL_CHILDENV_PORT", "17429")))
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-childenv"
# Resolved once at startup; see pin_image().
IMAGE = "wheel-engine:test"
# The harness's own default, NOT a path we pass in. Passing it would be self-defeating:
# the fix under test (env_clear + allowlist) is exactly what stops an engine-side variable
# from reaching the child, so a dump configured that way never gets written and the suite
# reports "no child started" against a correct engine.
ENV_DUMP = "/data/wheel-fake-env.jsonl"

ENGINE_SECRET = "qa-child-env-bearer-must-not-leak"
VAULT_KEY_B64 = base64.b64encode(b"qa-child-env-vault-key-32-bytes!").decode()
VAULT_VALUE = "CHILD-ENV-VAULT-VALUE-8c1f0a"
KEY = "SUPPLIED_TOKEN"

# What the engine must never hand a child, keyed by the name we would recognise it under.
# Spelled with its TESTPLAN ID rather than deriving the ID from the variable name: an ID
# assembled at runtime is invisible to qa/contract/id_traceability.py, so these two S1
# criteria were untraceable — present in the suite, absent from the plan, and nobody could
# tell from either side.
FORBIDDEN = {
    "WHEEL_ENGINE_SECRET": (ENGINE_SECRET, "SEC-child-env-no-wheel-engine-secret"),
    "WHEEL_VAULT_KEY": (VAULT_KEY_B64, "SEC-child-env-no-wheel-vault-key"),
}
# What the child genuinely needs. An over-aggressive env_clear() that dropped these would
# break every agent while passing every leak assertion above it.
REQUIRED = ("PATH", "WHEEL_NODE", "WHEEL_TOKEN_FILE")


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, body=None):
    import urllib.error, urllib.request
    r = urllib.request.Request(BASE + path, method=method)
    r.add_header("Authorization", "Bearer " + ENGINE_SECRET)
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


def digest(s):
    return hashlib.sha256(s.encode()).hexdigest()


def start_engine():
    sh("docker", "rm", "-f", NAME)
    # Every forbidden value is deliberately present in the ENGINE's environment. A test
    # that never sets them would pass against an engine that leaks everything it holds.
    p = sh("docker", "run", "-d", "--name", NAME,
           "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
           "-e", "WHEEL_ENGINE_SECRET=" + ENGINE_SECRET,
           "-e", "WHEEL_VAULT_KEY=" + VAULT_KEY_B64,
           "-e", "WHEEL_ROLE=engine",
           "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
           "-p", "%d:7000" % PORT, IMAGE)
    if p.returncode != 0:
        return "could not start wheel-engine:test: " + p.stderr.strip()[:200]
    for _ in range(60):
        try:
            if http("GET", "/healthz")[0] == 200:
                return configure_fakes()
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def configure_fakes():
    """Steer the fake harnesses through a file rather than the environment.

    The engine repopulates a child's environment from a short allowlist, so WHEEL_FAKE_*
    set on the ENGINE no longer reaches the child -- which is the F015 fix working as
    designed. Asking SDK to allowlist test variables would put a hole in the boundary
    this suite exists to check, so the fakes read /data/wheel-fake.json instead.
    """
    cfg = json.dumps({
        "env_dump": ENV_DUMP,
        # Report which variables carry these values, whatever they end up being called.
        "env_sentinels": ",".join(
            [digest(ENGINE_SECRET), digest(VAULT_KEY_B64), digest(VAULT_VALUE)]),
    })
    p = subprocess.run(["docker", "exec", "-i", NAME, "sh", "-c",
                        "cat > /data/wheel-fake.json"], input=cfg,
                       capture_output=True, text=True)
    return None if p.returncode == 0 else "could not write the fake-harness config: " + p.stderr[:160]


def node(name, typ, cfg, x=0):
    st, body = http("POST", "/v1/nodes", {"name": name, "type": typ,
                                          "position": {"x": x, "y": 0}, "config": cfg})
    return (body or {}).get("id"), st


def main():
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", "wheel-engine:test").returncode != 0:
        print("wheel-engine:test not built — run `make engine-image-test`")
        return SKIP
    global IMAGE
    pinned = pin_image()
    if pinned:
        IMAGE = pinned
        print("image %s = %s" % ("wheel-engine:test", pinned[:19]))

    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        vault, _ = node("secrets", "vault", {"keys": [KEY]})
        agent, st = node("worker", "agent",
                         {"harness": "claude", "system_prompt": "W",
                          "run_on_startup": False, "ephemeral_context": False}, x=200)
        if not R.check("SEC-child-env/setup", agent and vault, "node creation -> %s" % st):
            return R.report("engine-child-env")

        http("POST", "/v1/wires", {"from": agent, "to": vault, "type": "read"})
        st, _ = http("PUT", "/v1/vault/%s/%s" % (vault, KEY), {"value": VAULT_VALUE})
        vault_stored = 200 <= st < 300

        http("POST", "/v1/agents/%s/start" % agent)
        time.sleep(8)

        dump = sh("docker", "exec", NAME, "sh", "-c",
                  "cat %s 2>/dev/null || true" % ENV_DUMP).stdout
        recs = [json.loads(l) for l in dump.splitlines() if l.strip()]
        if not R.check("SEC-child-env/spawned", bool(recs),
                       "no env dump — no child ever started, so nothing below is evidence"):
            return R.report("engine-child-env")

        # ---- the positive control, before anything it validates ------------------
        #
        # The vault value is wired to this agent and is therefore SUPPOSED to reach it.
        # Finding it by digest proves the sentinel search can find a secret in a child's
        # environment at all; only then does "the engine's secrets are not there" mean
        # something. Without this, a typo in the sentinel plumbing reports a clean board.
        # `env_digests` (every variable, hashed) rather than `sentinel_hits` (only the
        # digests a test asked about): the ask travels through the ENGINE's environment,
        # which post-F015 no longer reaches the child, so nothing would ever be asked.
        # Hashing unconditionally needs no configuration and cannot be silently disabled.
        found_by_digest = {name: d for r in recs
                           for name, d in ((r.get("env_digests")
                                            or r.get("sentinel_hits") or {})).items()}
        if vault_stored:
            search_proven = R.check(
                "SEC-child-env/sentinel-works",
                digest(VAULT_VALUE) in found_by_digest.values(),
                "the wired vault value was not located in the child either — this "
                "search finds nothing, so every absence asserted below is vacuous")
        else:
            search_proven = False
            R.skip("SEC-child-env/sentinel-works",
                   "vault write answered %s, so there is no known-present secret to "
                   "prove the search with" % st)

        # ---- F015 proper: by name, from inside a real child -----------------------
        names = set()
        for r in recs:
            names.update(r.get("env_names") or [])
        for var, (_, tid) in FORBIDDEN.items():
            R.check(tid,
                    var not in names,
                    "%s is in the child's environment; /proc/self/environ hands it to "
                    "the agent" % var)

        # ---- the case a name check cannot reach -----------------------------------
        # Gated on the control above, deliberately.
        #
        # The first run of this suite skipped sentinel-works (my vault key was 31 bytes,
        # so nothing could be stored) and then reported this assertion GREEN — against a
        # child whose environment did, at that moment, contain both engine secrets. The
        # skip removed the only evidence that the search worked, and the un-guarded
        # absence check turned "I could not look" into "I looked and found nothing".
        # A control that does not gate its dependents is decoration.
        leaked = {n: d for n, d in found_by_digest.items()
                  if d in {digest(v) for v, _ in FORBIDDEN.values()}}
        if search_proven:
            R.check("SEC-child-env-no-secret-under-any-name", not leaked,
                    "an engine secret reached the child under %s — the allowlist "
                    "repopulated it under a name the F015 check does not look for"
                    % sorted(leaked))
        else:
            R.skip("SEC-child-env-no-secret-under-any-name",
                   "the digest search is unproven (see SEC-child-env/sentinel-works), so "
                   "an empty result is not evidence of no leak")

        # ---- and the child must still be able to work ------------------------------
        missing = [v for v in REQUIRED if v not in names]
        R.check("SEC-child-env-keeps-essentials", not missing,
                "env_clear() dropped %s — an agent that cannot find its token file or its "
                "PATH is broken, and would pass every leak assertion above" % missing)
    finally:
        sh("docker", "rm", "-f", NAME)

    return R.report("engine-child-env")


if __name__ == "__main__":
    sys.exit(main())
