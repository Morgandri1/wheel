#!/usr/bin/env python3
"""Credential routing into the child — TESTPLAN AUTH-cred-* (§4, §5b).

SDK asked for this and named the gap precisely: their unit tests assert what the engine
MEANT to export, and they verified the rest by hand in a container. This asserts what the
child actually RECEIVED, from outside the engine, in CI.

Why it matters more than it looks: `claude setup-token` mints an `sk-ant-oat…` token that
must arrive as CLAUDE_CODE_OAUTH_TOKEN, and an `sk-ant-api…` key must arrive as
ANTHROPIC_API_KEY. Send either as the other and the request fails at the API looking
exactly like a bad credential — so the operator is told their perfectly good token is
wrong. Codex has the same trap in the other direction: CODEX_API_KEY authenticates,
OPENAI_API_KEY is noticed by `codex doctor` and does nothing.

Evidence is WHEEL_FAKE_ENV_DUMP: one record per spawn, written by the fake harness itself,
naming which credential variables were set and the sha256 (never the value) of each. The
engine's own log is not evidence here — the engine is the thing under test.
"""
import json, os, subprocess, sys, time, uuid, urllib.error, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results

SKIP = 77
R = Results()

PORT = int(os.environ.get("WHEEL_AUTH_ENGINE_PORT", "17422"))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-authroute-secret-at-least-16"
NAME = "qa-engine-authroute"
DUMP = "/data/qa-env.jsonl"

OAT = "sk-ant-oat01-" + "o" * 48
KEY = "sk-ant-api03-" + "k" * 48
CODEX_KEY = "sk-proj-" + "c" * 40


def sha(s):
    import hashlib
    return hashlib.sha256(s.encode()).hexdigest()


def req(method, path, body=None):
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
    subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)
    key = subprocess.run(["openssl", "rand", "-base64", "32"],
                         capture_output=True, text=True).stdout.strip()
    p = subprocess.run(
        ["docker", "run", "-d", "--name", NAME,
         "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
         "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
         "-e", "WHEEL_VAULT_KEY=" + key,
         "-e", "WHEEL_ROLE=engine",
         "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
         "-e", "WHEEL_FAKE_ENV_DUMP=" + DUMP,
         "-p", "%d:7000" % PORT, "wheel-engine:test"],
        capture_output=True, text=True)
    if p.returncode != 0:
        return "could not start wheel-engine:test: " + p.stderr.strip()[:200]
    for _ in range(60):
        try:
            if req("GET", "/healthz")[0] == 200:
                return None
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def read_dump():
    """Every spawn record the fake harness has written so far."""
    p = subprocess.run(["docker", "exec", NAME, "cat", DUMP], capture_output=True)
    if p.returncode != 0:
        return []
    out = []
    for line in p.stdout.decode("utf-8", errors="replace").splitlines():
        if line.strip():
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return out


def truncate_dump():
    subprocess.run(["docker", "exec", NAME, "sh", "-c", ": > " + DUMP], capture_output=True)


def place_agent(name, harness="claude"):
    st, node = req("POST", "/v1/nodes", {
        "name": name, "type": "agent", "position": {"x": 0.0, "y": 0.0},
        "config": {"harness": harness, "system_prompt": "cred routing probe",
                   "run_on_startup": False, "ephemeral_context": False},
    })
    return node.get("id") if st in (200, 201) and isinstance(node, dict) else None


def spawn_with(agent_id, token):
    """Store a credential, start the agent, and return the spawn records it produced."""
    truncate_dump()
    req("POST", "/v1/agents/%s/stop" % agent_id)
    st, body = req("POST", "/v1/agents/%s/auth/complete" % agent_id, {"api_key": token})
    if st not in (200, 201, 204):
        return None, "auth/complete -> %s %r" % (st, body)
    st, body = req("POST", "/v1/agents/%s/start" % agent_id)
    if st not in (200, 201, 202, 204):
        return None, "start -> %s %r" % (st, body)
    for _ in range(60):
        recs = read_dump()
        if recs:
            return recs, None
        time.sleep(0.5)
    return None, "the child never spawned (no env-dump record in 30s)"


def main():
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    if subprocess.run(["docker", "image", "inspect", "wheel-engine:test"],
                      capture_output=True).returncode != 0:
        print("wheel-engine:test not built — run `make engine-image-test`")
        return SKIP
    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        # The fake must actually be the `claude` on PATH, or every assertion below is
        # measuring nothing. SDK now spawns harness.program() rather than a literal
        # "claude", so PATH order is the whole contract.
        p = subprocess.run(["docker", "exec", NAME, "sh", "-c",
                            "head -1 \"$(command -v claude)\" || true"],
                           capture_output=True, text=True)
        if "python" not in p.stdout:
            print("the image's `claude` is not the fake harness — wrong image variant")
            return SKIP

        agent_id = place_agent("cred-probe")
        if not R.check("AUTH-cred-setup", agent_id is not None, "could not place an agent node"):
            return R.report("engine-auth-routing")

        recs, err = spawn_with(agent_id, OAT)
        if err:
            print("could not drive the auth+start path: " + err)
            return SKIP
        r = recs[-1]
        R.check("AUTH-cred-oat-var",
                r["credential_vars_set"] == ["CLAUDE_CODE_OAUTH_TOKEN"],
                "an sk-ant-oat token must arrive as CLAUDE_CODE_OAUTH_TOKEN only, got %s"
                % r["credential_vars_set"])
        R.check("AUTH-cred-oat-value",
                r["credentials"].get("CLAUDE_CODE_OAUTH_TOKEN", {}).get("sha256") == sha(OAT),
                "the token that arrived is not the token that was stored")
        R.check("AUTH-cred-oat-not-as-key",
                "ANTHROPIC_API_KEY" not in r["credentials"],
                "the setup-token was ALSO exported as ANTHROPIC_API_KEY, which the API rejects")

        recs, err = spawn_with(agent_id, KEY)
        if err:
            R.check("AUTH-cred-key-spawn", False, err)
        else:
            r = recs[-1]
            R.check("AUTH-cred-key-var",
                    r["credential_vars_set"] == ["ANTHROPIC_API_KEY"],
                    "an sk-ant-api key must arrive as ANTHROPIC_API_KEY only, got %s"
                    % r["credential_vars_set"])
            R.check("AUTH-cred-key-value",
                    r["credentials"].get("ANTHROPIC_API_KEY", {}).get("sha256") == sha(KEY),
                    "the key that arrived is not the key that was stored")
            R.check("AUTH-cred-no-stale",
                    r["credentials"].get("CLAUDE_CODE_OAUTH_TOKEN") is None,
                    "the previous spawn's oauth token is still exported — replacing a "
                    "credential must not leave the old variable set")

        # §5b: argv is world-readable across uids, so a credential must never be on it.
        R.check("SEC-no-secret-in-argv",
                not any(OAT in a or KEY in a for a in r.get("argv", [])),
                "a credential appears in the child's argv: %r" % (r.get("argv"),))
        R.check("AUTH-cred-config-dir",
                bool(r.get("config", {}).get("CLAUDE_CONFIG_DIR")),
                "CLAUDE_CONFIG_DIR is unset — per-node credential isolation depends on it")

        codex_id = place_agent("cred-probe-codex", harness="codex")
        if codex_id is None:
            R.check("AUTH-cred-codex-var", False, "could not place a codex agent node")
        else:
            recs, err = spawn_with(codex_id, CODEX_KEY)
            if err:
                R.check("AUTH-cred-codex-var", False, err)
            else:
                r = recs[-1]
                R.check("AUTH-cred-codex-var",
                        r["credential_vars_set"] == ["CODEX_API_KEY"],
                        "codex authenticates with CODEX_API_KEY; OPENAI_API_KEY is noticed by "
                        "`codex doctor` and authenticates nothing. Got %s"
                        % r["credential_vars_set"])

        return R.report("engine-auth-routing")
    finally:
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)


if __name__ == "__main__":
    sys.exit(main())
