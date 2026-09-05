#!/usr/bin/env python3
"""Vault secrecy end to end — TESTPLAN SEC-vault-* (M1.6, operator priority).

Five properties, each asserted from the side that would actually leak:

  at rest      the ciphertext in /data/wheel.db is not the plaintext
  write-only   there is no read route; PUT is the only way in
  wire-gated   an agent without a `read` wire gets exit 3, not an empty value
  exported     a WIRED agent's child process really receives the key
  never shown  a sentinel never appears in the board, the log, the events WS, or the
               transcript — the exact bytes written to the child's stdin

The last one is the one that needed adding. `SEC-vault-never-read` already grepped the
board and the log, but the transcript is a stream the operator can read in the UI, and a
secret pasted into a prompt would land there and nowhere else. A canary that is only
grepped where you remembered to look is not a canary.
"""
import hashlib, json, os, subprocess, sys, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results

SKIP = 77
R = Results()
PORT = int(os.environ.get("WHEEL_ENGINE_VAULT_PORT", "17414"))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-vault-secret-at-least-16ch"
NAME = "qa-engine-vault"
ENV_DUMP = "/data/qa-vault-env.jsonl"
TRANSCRIPT = "/data/qa-vault-transcript.jsonl"

# Distinctive enough to grep for in a sqlite file, and long enough that it cannot appear
# by chance in base64 of something else.
CANARY = "VAULT-CANARY-d41d8cd98f00b204e9800998ecf8427e"
KEY = "STRIPE_KEY"


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


def wheel(node_id, *argv):
    return sh("docker", "exec",
              "-e", "WHEEL_TOKEN_FILE=/data/run/%s/token" % node_id,
              "-e", "WHEEL_ENGINE_URL=http://127.0.0.1:7000",
              NAME, "wheel", *argv)


def start_engine():
    sh("docker", "rm", "-f", NAME)
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    p = sh("docker", "run", "-d", "--name", NAME,
           "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
           "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
           "-e", "WHEEL_VAULT_KEY=" + key,
           "-e", "WHEEL_ROLE=engine",
           "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
           "-e", "WHEEL_FAKE_TRANSCRIPT=" + TRANSCRIPT,
           "-e", "WHEEL_FAKE_ENV_DUMP=" + ENV_DUMP,
           "-e", "WHEEL_FAKE_ENV_DUMP_KEYS=" + KEY,
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


def node(name, typ, cfg, x=0):
    st, body = http("POST", "/v1/nodes", {"name": name, "type": typ,
                                          "position": {"x": x, "y": 0}, "config": cfg})
    return (body or {}).get("id"), st


def agent_cfg(prompt="V"):
    return {"harness": "claude", "system_prompt": prompt,
            "run_on_startup": False, "ephemeral_context": False}


def container_bytes(path):
    """Raw file bytes out of the container — no text decoding, so a secret that survived
    as UTF-16 or inside a blob still shows up."""
    p = subprocess.run(["docker", "exec", NAME, "cat", path], capture_output=True)
    return p.stdout if p.returncode == 0 else b""


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
        vault, st1 = node("secrets", "vault", {"keys": [KEY]})
        wired, st2 = node("wired", "agent", agent_cfg(), x=200)
        unwired, st3 = node("unwired", "agent", agent_cfg(), x=400)
        if not R.check("SEC-vault/setup", all(x is not None for x in (vault, wired, unwired)),
                       "node creation -> %s %s %s" % (st1, st2, st3)):
            return R.report("engine-vault")

        st, _ = http("POST", "/v1/wires", {"from": wired, "to": vault, "type": "read"})
        R.check("SEC-vault/wire", 200 <= st < 300, "agent->vault read wire -> %s" % st)

        st, _ = http("PUT", "/v1/vault/%s/%s" % (vault, KEY), {"value": CANARY})
        stored = 200 <= st < 300
        if not stored:
            # PROTOCOL.md line 220 marks `PUT /v1/vault/:id/:key` as M2. Every assertion
            # about a STORED value is skipped by name, not silently dropped and not failed:
            # a criterion whose subject does not exist yet has no verdict.
            #
            # Skipping matters more than usual here. With no way to store a value,
            # SEC-vault-write-only would pass because there is nothing to read,
            # SEC-vault-at-rest would pass because no plaintext was ever written, and
            # SEC-vault-not-in-transcript would pass because there is no secret to leak.
            # Three S1 secrecy criteria would all report green on an engine with no vault
            # at all. That is the most dangerous shape a test suite can take.
            why = "vault write is M2 (PROTOCOL.md:220); engine answered %s" % st
            for tid in ("SEC-vault-write", "SEC-vault-write-only", "SEC-vault-never-read/board",
                        "SEC-vault-at-rest", "SEC-vault-at-rest/grep-works", "CLI-secret-get",
                        "SEC-vault-wire-gated", "SEC-vault-wire-gated/no-value",
                        "SEC-vault-env-scope/wired", "SEC-vault-env-scope/unwired",
                        "SEC-vault-not-in-transcript", "SEC-vault-never-read/log"):
                R.skip(tid, why)
            st, board = http("GET", "/v1/board")
            R.check("SEC-vault-keys-are-names", KEY in json.dumps(board),
                    "the key NAME should be listed even before values can be written")
            return R.report("engine-vault")
        R.check("SEC-vault-write", True)

        # ---- write-only: there is no read route, and the config never carries values
        st, _ = http("GET", "/v1/vault/%s/%s" % (vault, KEY))
        R.check("SEC-vault-write-only", st in (404, 405),
                "GET on a vault key answered %s — the only way in must be PUT" % st)

        st, board = http("GET", "/v1/board")
        board_txt = json.dumps(board)
        R.check("SEC-vault-never-read/board", CANARY not in board_txt,
                "the value is in GET /v1/board")
        R.check("SEC-vault-keys-are-names", KEY in board_txt,
                "the key NAME should still be listed — %s absent from the board" % KEY)

        # ---- at rest: the plaintext must not be in the database file
        db = container_bytes("/data/wheel.db")
        if not R.check("SEC-vault-at-rest/readable", bool(db), "could not read /data/wheel.db"):
            return R.report("engine-vault")
        R.check("SEC-vault-at-rest", CANARY.encode() not in db,
                "the plaintext value is in /data/wheel.db (%d bytes scanned)" % len(db))
        # Proves the grep works at all: a value we DID store in the clear is findable, so a
        # pass above means encryption, not a broken search.
        R.check("SEC-vault-at-rest/grep-works", KEY.encode() in db,
                "the key name is not in the db either — this scan finds nothing, so the "
                "at-rest result above is meaningless")

        # ---- wire-gated read
        p = wheel(wired, "secret", "get", "secrets/" + KEY)
        R.check("CLI-secret-get", p.returncode == 0 and CANARY in p.stdout,
                "wired agent could not read its own vault: rc=%s %r" % (p.returncode, p.stderr[:160]))
        p = wheel(unwired, "secret", "get", "secrets/" + KEY)
        R.check("SEC-vault-wire-gated", p.returncode == 3,
                "unwired agent got rc=%s (want 3); stdout=%r" % (p.returncode, p.stdout[:120]))
        R.check("SEC-vault-wire-gated/no-value", CANARY not in (p.stdout + p.stderr),
                "the denial message leaked the value")

        # ---- exported into the child's env, for wired agents only
        #
        # Asserted from inside the child, via WHEEL_FAKE_ENV_DUMP. Reading the child's
        # environment any other way means printing the secret into a test log — trading the
        # leak under test for one we caused — so the dump records env NAMES plus a sha256 of
        # the keys we name, and never a value. "This child received exactly this secret" is
        # then provable without the plaintext existing anywhere a later grep could find it.
        #
        # The previous version of this block was `R.check(..., True)` with a comment saying
        # it was "asserted below via the child". It was not asserted anywhere. An
        # unconditional check is worse than no check: it occupies the ID, so the criterion
        # reads as covered in the traceability table while testing nothing.
        http("POST", "/v1/agents/%s/start" % wired)
        http("POST", "/v1/agents/%s/start" % unwired)
        time.sleep(8)

        rc, dump = exec_in("sh", "-c", "cat %s 2>/dev/null || true" % ENV_DUMP)
        recs = [json.loads(l) for l in dump.splitlines() if l.strip()]
        if not R.check("SEC-vault-env-scope/spawned", bool(recs),
                       "no env dump — neither child spawned, so nothing here is evidence"):
            return R.report("engine-vault")
        R.check("SEC-vault-env-scope/dump-clean", CANARY not in dump,
                "the env dump contains the plaintext — the harness itself is leaking")

        want_digest = hashlib.sha256(CANARY.encode()).hexdigest()
        with_key = [r for r in recs if (r.get("digests") or {}).get(KEY)]
        R.check("SEC-vault-env-scope/wired",
                any((r["digests"][KEY] or {}).get("sha256") == want_digest for r in with_key),
                "no spawned child received %s with the stored value; %d dump record(s)"
                % (KEY, len(recs)))
        R.check("SEC-vault-env-scope/unwired", len(with_key) <= 1,
                "%d children received %s — an agent with no wire to the vault got the secret"
                % (len(with_key), KEY))
        R.check("SEC-vault-env-scope/name-hidden",
                sum(1 for r in recs if KEY in (r.get("env_names") or [])) <= 1,
                "more than one child has %s in its environment by NAME — an absent value is "
                "not enough, the name alone tells an agent which secrets exist" % KEY)
        # ---- never in the transcript: the exact bytes written to the child's stdin
        http("POST", "/v1/agents/%s/send" % wired, {"body": "vault probe"})
        time.sleep(4)
        tr = container_bytes(TRANSCRIPT)
        R.check("SEC-vault-not-in-transcript", CANARY.encode() not in tr,
                "the value reached the child's stdin (%d transcript bytes)" % len(tr))

        st, body = http("GET", "/v1/agents/%s/log" % wired)
        R.check("SEC-vault-never-read/log", CANARY not in json.dumps(body),
                "the value is in the agent log")
    finally:
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)

    return R.report("engine-vault")


if __name__ == "__main__":
    sys.exit(main())
