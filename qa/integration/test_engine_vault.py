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
from wheel_client import Results, pin_image, free_port

SKIP = 77
R = Results()
PORT = free_port(int(os.environ.get("WHEEL_ENGINE_VAULT_PORT", "17424")))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-vault-secret-at-least-16ch"
NAME = "qa-engine-vault"
IMAGE = "wheel-engine:test"   # replaced by its immutable ID at startup; see pin_image()
# Fixed by the harness, not passed in: the engine does not forward its own env to
# children (F015), so a path exported here would never reach the child.
ENV_DUMP = "/data/wheel-fake-env.jsonl"
TRANSCRIPT = "/data/qa-vault-transcript.jsonl"

# Distinctive enough to grep for in a sqlite file, and long enough that it cannot appear
# by chance in base64 of something else.
CANARY = "VAULT-CANARY-d41d8cd98f00b204e9800998ecf8427e"
# Written to a ctx node in the clear, as the positive control for the at-rest scan.
PLAIN_CANARY = "PLAINTEXT-CONTROL-59e6f1c0a7b34d28"
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


VAULT_KEY = None


def start_engine():
    global VAULT_KEY
    sh("docker", "rm", "-f", NAME)
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    VAULT_KEY = key
    p = sh("docker", "run", "-d", "--name", NAME,
           "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
           "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
           "-e", "WHEEL_VAULT_KEY=" + key,
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
    """Steer the fakes by file: the engine's allowlist (F015) drops WHEEL_FAKE_* on the
    way into a child, so setting them on the engine container steers nothing."""
    cfg = json.dumps({"env_dump": ENV_DUMP, "transcript": TRANSCRIPT,
                      "env_dump_keys": KEY})
    p = subprocess.run(["docker", "exec", "-i", NAME, "sh", "-c",
                        "cat > /data/wheel-fake.json"], input=cfg,
                       capture_output=True, text=True)
    return None if p.returncode == 0 else "could not write the fake config: " + p.stderr[:160]


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


def db_bytes():
    """Every byte sqlite is holding, not just the main file.

    The engine opens the database in WAL mode (db/mod.rs:40), so a row written seconds
    ago lives in `wheel.db-wal` and has not been checkpointed into `wheel.db` yet.
    Scanning only the main file made SEC-vault-at-rest pass because the value was not
    there to find — the strongest possible pass for the weakest possible reason. The
    positive control below is what caught it: the key NAME was missing from the same
    scan, and a search that cannot find a plaintext string proves nothing about a
    ciphertext one."""
    return b"".join(container_bytes("/data/wheel.db" + ext)
                    for ext in ("", "-wal", "-shm"))


def exec_in(*argv):
    p = subprocess.run(["docker", "exec", NAME, *argv], capture_output=True, text=True)
    return p.returncode, p.stdout


def wait_token(node_id, timeout=60):
    """The supervisor mints the node's token file when the agent starts; every `wheel`
    call below authenticates with it, so a CLI assertion made before it exists is
    measuring a missing file rather than a wire."""
    for _ in range(int(timeout * 2)):
        if subprocess.run(["docker", "exec", NAME, "test", "-s",
                           "/data/run/%s/token" % node_id],
                          capture_output=True).returncode == 0:
            return True
        time.sleep(0.5)
    return False


def db_bytes():
    """Every byte sqlite owns, not just the main file.

    The engine runs sqlite in WAL mode, so a freshly written row lives in `wheel.db-wal`
    until a checkpoint; on a short-lived test container `wheel.db` itself stays a 4096-byte
    header and NOTHING is ever in it. Scanning only that file made SEC-vault-at-rest pass
    unconditionally — it would have reported green against an engine that stored every
    secret in the clear. Scan the whole set, and let the positive control below prove the
    scan can still find something.
    """
    out = b""
    for suffix in ("", "-wal", "-shm"):
        out += container_bytes("/data/wheel.db" + suffix)
    return out


def wait_token(node_id, timeout=60):
    """The supervisor mints the node's token file when the agent starts; the CLI cannot
    authenticate before that exists. Without this wait a `wheel` call fails with rc=1
    (transport: no token file) which is NOT rc=3, so every wire-gating assertion below
    would report a spurious failure against the engine."""
    for _ in range(int(timeout * 2)):
        if sh("docker", "exec", NAME, "test", "-s",
              "/data/run/%s/token" % node_id).returncode == 0:
            return True
        time.sleep(0.5)
    return False


def main():
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    if subprocess.run(["docker", "image", "inspect", "wheel-engine:test"],
                      capture_output=True).returncode != 0:
        print("wheel-engine:test not built — run `make engine-image-test`")
        return SKIP
    global IMAGE
    pinned = pin_image()
    if pinned:
        IMAGE = pinned
        print("image wheel-engine:test = %s" % pinned[:19])

    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        vault, st1 = node("secrets", "vault", {"keys": [KEY]})
        # Stored deliberately in the clear: the at-rest scan must be able to FIND this.
        node("plain", "ctx", {"markdown": PLAIN_CANARY}, x=600)
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

        # Start the children only now. Vault keys are exported into a child's environment
        # AT SPAWN (§3 matrix, agent -> vault read), so an agent started before the value
        # was stored would legitimately have nothing — testing the ordering, not the export.
        http("POST", "/v1/agents/%s/start" % wired)
        http("POST", "/v1/agents/%s/start" % unwired)

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
        db = db_bytes()
        if not R.check("SEC-vault-at-rest/readable", bool(db), "could not read /data/wheel.db*"):
            return R.report("engine-vault")
        # Control FIRST: a value we deliberately stored in the clear must be findable by
        # this exact scan. If it is not, the scan is broken and the encryption verdict
        # below carries no information, so refuse to report one.
        R.control("SEC-vault-at-rest/grep-works", PLAIN_CANARY.encode() in db,
                  "a ctx markdown stored in the clear is not in %d scanned bytes — this scan "
                  "finds nothing, so any at-rest verdict would be vacuous" % len(db))
        R.gated("SEC-vault-at-rest", "SEC-vault-at-rest/grep-works",
                CANARY.encode() not in db,
                "the vault plaintext is in the sqlite files (%d bytes scanned)" % len(db))

        # ---- wire-gated read
        ok_w, ok_u = wait_token(wired), wait_token(unwired)
        if not R.check("SEC-vault/token-files", ok_w and ok_u,
                       "no node token file (wired=%s unwired=%s) — a `wheel` call would fail "
                       "with rc=1 for want of credentials, which is not the rc=3 denial these "
                       "assertions are about" % (ok_w, ok_u)):
            for tid in ("CLI-secret-get", "SEC-vault-wire-gated", "SEC-vault-wire-gated/no-value"):
                R.skip(tid, "no token file to authenticate with")
            return R.report("engine-vault")

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
        dump = container_bytes(ENV_DUMP).decode(errors="replace")
        recs = [json.loads(l) for l in dump.splitlines() if l.strip()]
        if not R.check("SEC-vault-env-scope/spawned", bool(recs),
                       "no env dump — neither child spawned, so nothing here is evidence"):
            return R.report("engine-vault")
        R.check("SEC-vault-env-scope/dump-clean", CANARY not in dump,
                "the env dump contains the plaintext — the harness itself is leaking")

        # F015 (a child must not inherit WHEEL_ENGINE_SECRET / WHEEL_VAULT_KEY) is
        # asserted in test_engine_child_env.py, which owns the SEC-child-env-* IDs and
        # carries its own positive control. Duplicating it here would mean two IDs for one
        # property, and two places to update when it changes.

        want_digest = hashlib.sha256(CANARY.encode()).hexdigest()
        with_key = [r for r in recs
                    if want_digest in set((r.get("env_digests") or {}).values())]
        R.check("SEC-vault-env-scope/wired", bool(with_key),
                "no spawned child received the stored value in its environment; "
                "%d dump record(s)" % len(recs))
        R.check("SEC-vault-env-scope/unwired", len(with_key) <= 1,
                "%d children hold the secret — an agent with no wire to the vault got it"
                % len(with_key))
        R.check("SEC-vault-env-scope/name",
                all(KEY in {n for n, d in (r.get("env_digests") or {}).items()
                            if d == want_digest} for r in with_key),
                "the secret reached a child under a name that is not %s" % KEY)
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
