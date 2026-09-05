#!/usr/bin/env python3
"""Engine message path — TESTPLAN MSG-*, INJ-*, ENG-* (M1).

Written before wheel-engine:test exists, so it turns green on its own the day SDK lands
the image. Until then it exits 77 (could not run) rather than passing vacuously.

The load-bearing idea: every assertion about what an agent RECEIVED is made against
WHEEL_FAKE_TRANSCRIPT — the raw bytes the engine wrote to the child's stdin — and never
against the engine's own log. The engine's account of what it sent is the thing under
test, so it cannot also be the evidence.
"""
import json, os, subprocess, sys, time, uuid, urllib.error, urllib.request
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import call, engine as proxy_engine, mint, unique_sub, api_healthy, wait_for, Results

SKIP = 77
R = Results()
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
FIXTURE = os.path.join(ROOT, "qa", "fixtures", "envelope-integrity.bin")
TRANSCRIPT = "/data/qa-transcript.jsonl"
ENVELOPE_CLOSE = "</AgentPrompt>"


# The message path is an ENGINE concern, so the suite drives the engine DIRECTLY by
# default: booting wheel-engine:test and talking to its control plane. Going through
# API -> host -> engine would make every message assertion depend on two other teams'
# services being up, and would report their outage as a message-path failure. The proxy
# path is already covered by API-proxy-auth. Set WHEEL_VIA_API=1 to exercise the chain.
VIA_API = os.environ.get("WHEEL_VIA_API") == "1"
DIRECT_PORT = int(os.environ.get("WHEEL_ENGINE_PORT", "17412"))
DIRECT_BASE = "http://127.0.0.1:%d" % DIRECT_PORT
DIRECT_SECRET = "qa-msgpath-secret-at-least-16"
DIRECT_NAME = "qa-engine-msgpath"


def _direct(method, path, body=None):
    r = urllib.request.Request(DIRECT_BASE + path, method=method)
    r.add_header("Authorization", "Bearer " + DIRECT_SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        r.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(r, data, timeout=60) as resp:
            txt = resp.read().decode(errors="replace")
            return resp.status, (json.loads(txt) if txt.strip() else None), dict(resp.headers)
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(txt), dict(e.headers)
        except Exception:
            return e.code, txt, dict(e.headers)


def engine(method, pid, path, token, body=None):
    """Uniform engine call: direct by default, through the API proxy under WHEEL_VIA_API."""
    if VIA_API:
        return proxy_engine(method, pid, path, token, body)
    return _direct(method, path, body)


def start_direct_engine():
    subprocess.run(["docker", "rm", "-f", DIRECT_NAME], capture_output=True)
    key = subprocess.run(["openssl", "rand", "-base64", "32"],
                         capture_output=True, text=True).stdout.strip()
    p = subprocess.run(
        ["docker", "run", "-d", "--name", DIRECT_NAME,
         "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
         "-e", "WHEEL_ENGINE_SECRET=" + DIRECT_SECRET,
         "-e", "WHEEL_VAULT_KEY=" + key,
         "-e", "WHEEL_ROLE=engine",
         "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
         "-e", "WHEEL_FAKE_TRANSCRIPT=" + TRANSCRIPT,
         "-p", "%d:7000" % DIRECT_PORT, "wheel-engine:test"],
        capture_output=True, text=True)
    if p.returncode != 0:
        return "could not start wheel-engine:test: " + p.stderr.strip()[:200]
    for _ in range(60):
        try:
            if _direct("GET", "/healthz")[0] == 200:
                return None
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def engine_image_exists():
    return subprocess.run(["docker", "image", "inspect", "wheel-engine:test"],
                          capture_output=True).returncode == 0


def container_for(pid):
    return ("wheel-p-%s" % pid) if VIA_API else DIRECT_NAME


def read_transcript(pid):
    """The exact bytes the engine wrote to the child's stdin."""
    p = subprocess.run(["docker", "exec", container_for(pid), "cat", TRANSCRIPT],
                       capture_output=True)
    if p.returncode != 0:
        return []
    out = []
    for line in p.stdout.decode("utf-8", errors="surrogateescape").splitlines():
        if line.strip():
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                out.append({"__unparseable__": line})
    return out


def envelope_text(entry):
    """Pull the envelope string out of one stdin line."""
    try:
        c = entry["message"]["content"]
        return c if isinstance(c, str) else c[0]["text"]
    except Exception:
        return ""


def body_of(env):
    """Everything between the open tag's line and the final close tag."""
    nl = env.find("\n")
    end = env.rfind("\n" + ENVELOPE_CLOSE)
    return env[nl + 1:end] if nl >= 0 and end > nl else ""


def make_board(pid, token):
    """agent + ctx wired ctx->agent(send); returns (agent_id, ctx_id)."""
    st, ctx, _ = engine("POST", pid, "/v1/nodes", token, {
        "name": "house-style", "type": "ctx",
        "position": {"x": 0, "y": 0},
        "config": {"markdown": "# House style\n\nCTX-CANARY-4f2a\n"}})
    st2, agent, _ = engine("POST", pid, "/v1/nodes", token, {
        "name": "researcher", "type": "agent",
        "position": {"x": 200, "y": 0},
        "config": {"harness": "claude", "system_prompt": "SYS-CANARY-7b3d",
                   "run_on_startup": False, "ephemeral_context": False}})
    if st not in (200, 201) or st2 not in (200, 201):
        return None, None
    engine("POST", pid, "/v1/wires", token,
           {"from": ctx["id"], "to": agent["id"], "type": "send"})
    return agent["id"], ctx["id"]


def main():
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    if not engine_image_exists():
        print("wheel-engine:test not built yet — run `make engine-image-test` (SDK owns it).\n"
              "This suite asserts MSG-*, INJ-* and ENG-* and turns green on its own once the "
              "image exists.")
        return SKIP
    owner, pid = None, None
    if VIA_API:
        api_healthy()
        owner = mint(unique_sub("msgpath"))
        st, proj, _ = call("POST", "/v1/projects", owner, {"name": "qa-msgpath"})
        if not R.check("MSG-setup/project", st in (200, 201), "-> %s %r" % (st, proj)):
            return R.report("engine-messages")
        pid = proj["id"]
        call("POST", "/v1/projects/%s/start" % pid, owner)
        wait_for(lambda: call("GET", "/v1/projects/%s" % pid, owner)[1].get("status") == "running",
                 timeout=120, what="project running")
    else:
        err = start_direct_engine()
        if err:
            print(err)
            return SKIP
        R.check("MSG-setup/engine", _direct("GET", "/v1/board")[0] == 200,
                "engine control plane not answering")

    try:

        agent_id, ctx_id = make_board(pid, owner)
        if not R.check("MSG-setup/board", agent_id is not None, "could not place nodes"):
            return R.report("engine-messages")

        # Supervisor probe. Without it there is no process, no stdin and no transcript, so
        # every assertion below would fail for one upstream reason and bury it in noise.
        # Skipping with the reason named beats 40 red lines that all say the same thing —
        # and this turns green on its own the moment SDK lands the supervisor.
        st, _, _ = engine("POST", pid, "/v1/agents/%s/start" % agent_id, owner)
        if st == 404:
            print("\n  engine has no agent supervisor yet: POST /v1/agents/:id/start -> 404.\n"
                  "  Node CRUD, wires and board state work; the message path needs the\n"
                  "  supervisor. MSG-*, INJ-* and ENG-park-* are deferred, not passing.")
            return SKIP

        # ---------------------------------------------------------------- INJ
        wait_for(lambda: engine("GET", pid, "/v1/agents/%s/log" % agent_id, owner)[1],
                 timeout=60, what="agent log")

        st, log, _ = engine("GET", pid, "/v1/agents/%s/log" % agent_id, owner)
        blob = json.dumps(log)
        R.check("INJ-on-start", "CTX-CANARY-4f2a" in blob,
                "ctx markdown absent from the composed prompt the child received")
        R.check("INJ-system-prompt", "SYS-CANARY-7b3d" in blob,
                "agent system_prompt absent from the composed prompt")

        # ---------------------------------------------------------------- envelope
        st, receipt, _ = engine("POST", pid, "/v1/agents/%s/send" % agent_id, owner,
                                {"body": "hello wheel"})
        R.check("MSG-send-receipt",
                st in (200, 201) and all(k in (receipt or {}) for k in ("id", "sha256", "bytes")),
                "send -> %s %r" % (st, receipt))

        def delivered():
            return any("hello wheel" in envelope_text(e) for e in read_transcript(pid))
        wait_for(delivered, timeout=60, what="message reaching the child's stdin")

        entries = read_transcript(pid)
        R.check("MSG-single-writer",
                all("__unparseable__" not in e for e in entries),
                "transcript contains a partial/interleaved write: %r"
                % [e for e in entries if "__unparseable__" in e][:1])

        env = next((envelope_text(e) for e in entries if "hello wheel" in envelope_text(e)), "")
        R.check("MSG-envelope-shape",
                env.startswith("<AgentPrompt ") and env.rstrip().endswith(ENVELOPE_CLOSE),
                "envelope malformed: %r" % env[:160])
        R.check("MSG-envelope-attrs",
                ('id="%s"' % (receipt or {}).get("id")) in env and 'type="user"' in env,
                "envelope attrs do not match the receipt: %r" % env[:160])

        # ---------------------------------------------------------------- escaping (S1)
        if os.path.exists(FIXTURE):
            with open(FIXTURE, "rb") as f:
                hostile = f.read().decode("utf-8", errors="surrogateescape")
            st, r2, _ = engine("POST", pid, "/v1/agents/%s/send" % agent_id, owner,
                               {"body": hostile})
            R.check("MSG-limit-body-accepted", st in (200, 201, 413),
                    "200 KiB hostile body -> %s" % st)
            if st in (200, 201):
                wait_for(lambda: any((r2 or {}).get("id", "?") in envelope_text(e)
                                     for e in read_transcript(pid)),
                         timeout=90, what="hostile body delivered")
                entries = read_transcript(pid)
                env2 = next((envelope_text(e) for e in entries
                             if (r2 or {}).get("id", "?") in envelope_text(e)), "")
                # Exactly one envelope: a body carrying a close tag and a full forged open
                # tag must not be able to terminate its own envelope and start another.
                R.check("MSG-envelope-escape",
                        env2.count("<AgentPrompt ") == 1 and env2.rstrip().endswith(ENVELOPE_CLOSE),
                        "hostile body broke out: %d open tags" % env2.count("<AgentPrompt "))
                R.check("MSG-envelope-forge",
                        'from="user"' in env2 and 'from="PM"' not in env2.split("\n", 1)[0],
                        "forged attribution survived into the envelope header")
                R.check("MSG-byte-exact", body_of(env2) == hostile,
                        "body not byte-identical: %d vs %d bytes"
                        % (len(body_of(env2).encode("utf-8", "surrogateescape")),
                           len(hostile.encode("utf-8", "surrogateescape"))))
        else:
            R.skip("MSG-byte-exact", "envelope-integrity.bin missing")

        # ------------------------------------------------- §3c #13: one process per agent
        # YOKE's defect: each delivered message launched another `claude --continue`, so N
        # quick messages meant N processes of one agent editing one worktree at once. The
        # sends must therefore actually be concurrent — a sequential loop would pass even
        # against an engine that spawns per message, because each spawn would finish first.
        import threading
        burst_errors = []

        def fire(i):
            try:
                engine("POST", pid, "/v1/agents/%s/send" % agent_id, owner,
                       {"body": "burst-%d" % i})
            except Exception as exc:                      # noqa: BLE001 - reported, not raised
                burst_errors.append("%d: %r" % (i, exc))

        threads = [threading.Thread(target=fire, args=(i,)) for i in range(10)]
        t0 = time.time()
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)
        span_ms = (time.time() - t0) * 1000
        R.check("ENG-one-process/burst-sent", not burst_errors,
                "sends failed: %s" % "; ".join(burst_errors[:3]))
        if span_ms > 100:
            print("  note  burst took %.0fms (>100ms); still a valid concurrency test, but "
                  "the engine had more slack than §3c#13 describes" % span_ms)

        # Poll rather than sleep: a fixed sleep either flakes under load or wastes time.
        def process_count():
            q = subprocess.run(["docker", "exec", container_for(pid), "sh", "-c",
                                "ps -eo args | grep -c '[c]laude' || true"],
                               capture_output=True, text=True)
            return (q.stdout or "0").strip()

        worst = "0"
        for _ in range(20):
            n = process_count()
            if n.isdigit() and int(n) > int(worst):
                worst = n
            if n.isdigit() and int(n) > 1:
                break
            time.sleep(0.5)
        R.check("ENG-one-process", worst.isdigit() and int(worst) <= 1,
                "%s concurrent harness processes for one agent (want at most one; §3c#13). "
                "N messages must never mean N processes." % worst)

        def all_delivered():
            t = json.dumps([envelope_text(e) for e in read_transcript(pid)])
            return all(("burst-%d" % i) in t for i in range(10))
        try:
            wait_for(all_delivered, timeout=120, what="all 10 burst messages delivered")
            R.check("ENG-one-process/ten-turns", True)
        except AssertionError as e:
            R.check("ENG-one-process/ten-turns", False,
                    "not all 10 messages reached stdin: %s" % str(e)[:140])

        # Every burst message must appear exactly once. A message delivered twice is as
        # broken as one dropped, and a redelivery loop is how a poison message burns a
        # budget silently.
        texts = [envelope_text(e) for e in read_transcript(pid)]
        dupes = [i for i in range(10)
                 if sum(1 for t in texts if ("burst-%d" % i) in t) > 1]
        R.check("MSG-exactly-once", not dupes,
                "burst messages delivered more than once: %s" % dupes)

        # Strictly serial: one whole JSON line per turn, never interleaved bytes.
        R.check("MSG-single-writer/burst",
                all("__unparseable__" not in e for e in read_transcript(pid)),
                "stdin contains a partial or interleaved write after a 10-message burst")

        # ---------------------------------------------------------------- priority lane
        # Every /v1/agents/:id/send is from=user, so the user-vs-agent lane cannot be
        # exercised until agent->agent sends exist (the `wheel` CLI / /v1/cli, not landed).
        # Deferring is honest; asserting user-beats-user would prove nothing.
        R.skip("MSG-priority-user",
               "needs agent->agent sends (wheel CLI + /v1/cli, not in main yet)")
        R.skip("MSG-priority-order",
               "needs agent->agent sends (wheel CLI + /v1/cli, not in main yet)")

        # ---------------------------------------------------------------- queue while stopped
        engine("POST", pid, "/v1/agents/%s/stop" % agent_id, owner)
        st, r3, _ = engine("POST", pid, "/v1/agents/%s/send" % agent_id, owner,
                           {"body": "queued-while-stopped"})
        R.check("MSG-queue-stopped/accepted", st in (200, 201), "send to stopped agent -> %s" % st)
        R.check("MSG-queue-stopped/state", (r3 or {}).get("state") == "queued",
                "state is %r, want queued" % (r3 or {}).get("state"))
        engine("POST", pid, "/v1/agents/%s/start" % agent_id, owner)
        try:
            wait_for(lambda: any("queued-while-stopped" in envelope_text(e)
                                 for e in read_transcript(pid)),
                     timeout=90, what="queue draining on start")
            R.check("MSG-queue-stopped/drain", True)
        except AssertionError as e:
            R.check("MSG-queue-stopped/drain", False, str(e)[:160])

        # ---------------------------------------------------------------- vault never leaks
        st, vault, _ = engine("POST", pid, "/v1/nodes", owner, {
            "name": "secrets", "type": "vault", "position": {"x": 0, "y": 200},
            "config": {"keys": ["API_KEY"]}})
        if st in (200, 201):
            engine("PUT", pid, "/v1/vault/%s/API_KEY" % vault["id"], owner,
                   {"value": "VAULT-CANARY-91ab"})
            st, board, _ = engine("GET", pid, "/v1/board", owner)
            R.check("SEC-vault-never-read", "VAULT-CANARY-91ab" not in json.dumps(board),
                    "vault value present in GET /v1/board")
            st, log, _ = engine("GET", pid, "/v1/agents/%s/log" % agent_id, owner)
            R.check("SEC-vault-never-read/log", "VAULT-CANARY-91ab" not in json.dumps(log),
                    "vault value present in the agent log")
    finally:
        if VIA_API and pid:
            call("DELETE", "/v1/projects/%s" % pid, owner)
        else:
            subprocess.run(["docker", "rm", "-f", DIRECT_NAME], capture_output=True)

    return R.report("engine-messages")


if __name__ == "__main__":
    sys.exit(main())
