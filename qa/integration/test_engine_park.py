#!/usr/bin/env python3

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""ENG-park-* — parking is a COST claim, so it is verified by observation, not by status.

Four criteria have been in TESTPLAN since M1 and were asserted by nothing until now:
`ENG-park-idle`, `ENG-park-resume`, `ENG-park-no-loss`, `ENG-park-ephemeral`. That gap was
invisible because `qa:id-traceability` checks one direction only — every ASSERTED id must be
planned; a PLANNED id with no test is not flagged. 194 asserted against 678 planned.

WHY STATUS IS NOT THE ASSERTION. `status: parked` is the engine's own claim about itself.
The point of parking is that a 162 MB process stops existing — that is the saving, and it is
observable from outside the engine with `docker exec`. So this counts PROCESSES, and treats
the status as corroboration rather than evidence. An engine that set `parked` and kept the
child would pass a status check and save nothing.

AND RESUMING THE SAME SESSION IS THE OTHER HALF. Parking that silently loses context is worse
than not parking: the agent comes back, answers, and has forgotten. `session_id` before park
must equal `session_id` after resume. A fresh id is a wiped context wearing a resume's
clothes.
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
from wheel_client import Results, configure_fakes, free_port, pin_image  # noqa: E402

SKIP = 77
R = Results()
PORT = free_port(0)
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-park-%s" % uuid.uuid4().hex[:8]
SECRET = "qa-park-secret-at-least-16chars"
IMAGE_TAG = os.environ.get("WHEEL_PARK_IMAGE", "wheel-engine:park")
IMAGE = None
IDLE = int(os.environ.get("WHEEL_PARK_IDLE_SECS", "12"))


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, body=None, timeout=30):
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Authorization", "Bearer " + SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=timeout) as r:
            t = r.read().decode(errors="replace")
            return r.status, (json.loads(t) if t.strip() else None)
    except urllib.error.HTTPError as e:
        return e.code, None
    except Exception:
        return 0, None


def state_of(aid):
    st, b = http("GET", "/v1/board")
    for n in ((b or {}).get("nodes") or []):
        if n.get("id") == aid:
            return n.get("state") or {}
    return {}


def harness_procs():
    """Count harness processes INSIDE the container. This is the saving, measured."""
    p = sh("docker", "exec", NAME, "sh", "-c",
           "ps -eo args 2>/dev/null | grep -c '[c]laude' || true")
    try:
        return int((p.stdout or "0").strip().splitlines()[-1])
    except Exception:
        return -1


def make_agent(name, idle_secs):
    st, n = http("POST", "/v1/nodes",
                 {"name": name, "type": "agent", "position": {"x": 0, "y": 0},
                  "config": {"harness": "claude", "system_prompt": "park probe",
                             "run_on_startup": False, "ephemeral_context": False,
                             "idle_timeout_secs": idle_secs}})
    return n["id"] if (200 <= st < 300 and n) else None


def wait_for(fn, want, timeout):
    t0 = time.time()
    last = None
    while time.time() - t0 < timeout:
        last = fn()
        if want(last):
            return True, last, time.time() - t0
        time.sleep(0.5)
    return False, last, time.time() - t0


def inbox_state(aid, mid):
    st, r = http("GET", "/v1/agents/%s/inbox?limit=200" % aid)
    for m in ((r or {}).get("messages") or []):
        if m.get("id") == mid:
            return m.get("state")
    return None


def turn(aid, body):
    """Send one message and wait until THAT message is observed `consumed`.

    NOT `queued_messages == 0`: that is already true the instant the send is issued, so the
    poll returns before the message is even enqueued and the "turn" never happened. My first
    version did exactly that, which made session_id read as None before any turn had run and
    produced two red assertions that looked like a context-wipe bug in the engine. Same trap
    as PROGRESS-*: never wait on a condition that starts out satisfied — wait on a specific
    thing reaching a specific state.
    """
    before = set()
    st, r = http("GET", "/v1/agents/%s/inbox?limit=200" % aid)
    for m in ((r or {}).get("messages") or []):
        before.add(m["id"])
    http("POST", "/v1/agents/%s/send" % aid, {"body": body})
    mid = None
    t0 = time.time()
    while time.time() - t0 < 30 and mid is None:
        st, r = http("GET", "/v1/agents/%s/inbox?limit=200" % aid)
        new_ids = {m["id"] for m in ((r or {}).get("messages") or [])} - before
        if new_ids:
            mid = sorted(new_ids)[0]
        else:
            time.sleep(0.5)
    if mid is None:
        return False
    ok, _, _ = wait_for(lambda: inbox_state(aid, mid), lambda s: s == "consumed", 90)
    return ok


def start_engine():
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    sh("docker", "run", "-d", "--name", NAME,
       "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
       "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
       "-e", "WHEEL_VAULT_KEY=" + key,
       "-e", "WHEEL_ROLE=engine",
       "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
       "-e", "WHEEL_LOG=json",
       "-p", "%d:7000" % PORT, IMAGE or IMAGE_TAG)
    for _ in range(80):
        if http("GET", "/healthz")[0] == 200:
            return True
        time.sleep(0.5)
    return False


def main():
    global IMAGE
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE_TAG).returncode != 0:
        print("%s not built" % IMAGE_TAG)
        return SKIP
    IMAGE = pin_image(IMAGE_TAG) if IMAGE_TAG == "wheel-engine:test" else \
        sh("docker", "image", "inspect", "--format", "{{.Id}}", IMAGE_TAG).stdout.strip()
    print("image: %s -> %s" % (IMAGE_TAG, (IMAGE or "?")[:19]))

    try:
        if not R.control("ENG-park/engine-up", start_engine(),
                         "the engine never started; nothing below is evidence"):
            return R.report("engine-park")
        if err := configure_fakes(NAME, transcript="/data/park.jsonl",
                                  env_dump="/data/park-spawns.jsonl"):
            R.skip("ENG-park/fakes", err)
            return R.report("engine-park")

        aid = make_agent("parker", IDLE)
        if not R.control("ENG-park/agent-created", bool(aid),
                         "could not create an agent with idle_timeout_secs — if the engine "
                         "rejects that config key, nothing below is about parking"):
            return R.report("engine-park")

        http("POST", "/v1/agents/%s/start" % aid)
        if not R.control("ENG-park/turn-completes", turn(aid, "hello-1"),
                         "the agent never completed a turn, so it never became idle and "
                         "the park timer was never armed"):
            return R.report("engine-park")

        before = state_of(aid)
        sid_before = before.get("session_id")
        procs_running = harness_procs()
        R.control("ENG-park/process-observable", procs_running >= 1,
                  "no harness process is visible inside the container while the agent is "
                  "%r, so 'the process was released' cannot be measured — a later count of "
                  "0 would prove nothing. Saw %d." % (before.get("status"), procs_running))

        # ENG-park-idle — the SAVING, measured as a process count, not a status string.
        ok, st, waited = wait_for(lambda: state_of(aid).get("status"),
                                  lambda s: s == "parked", IDLE + 45)
        procs_parked = harness_procs()
        R.check("ENG-park-idle", ok and procs_parked == 0,
                "after %.0fs (idle_timeout_secs=%d) status is %r and %d harness process(es) "
                "are still running inside the container. Parking is a COST claim: the status "
                "is the engine's word for it, the process count is the saving."
                % (waited, IDLE, st, procs_parked))

        R.gated("ENG-park-keeps-session", "ENG-park/turn-completes",
                bool(sid_before) and state_of(aid).get("session_id") == sid_before,
                "session_id changed at park (%r -> %r). Parking must keep the session; "
                "dropping it turns a resume into a silent context wipe."
                % (sid_before, state_of(aid).get("session_id")))

        # ENG-park-resume + ENG-park-no-loss — one message, delivered once, same session.
        delivered = turn(aid, "hello-after-park")
        after = state_of(aid)
        R.check("ENG-park-no-loss", delivered,
                "a message sent to a PARKED agent was not consumed within the timeout; "
                "state %r" % after)
        # THE DECISIVE ONE (PM). A matching session_id column proves the engine remembered a
        # string. It does NOT prove the child was told to resume that session — the engine
        # could store the id and spawn a fresh context, and the board would look identical.
        # The mechanism is `--resume <id>` on the child's argv, and the fake records every
        # spawn's argv, so it is observable.
        #
        # WHAT THIS STILL CANNOT PROVE, stated because it is the limit of a fake harness:
        # that the RESUMED CONTEXT actually contains the earlier turn. The fake has no
        # memory to retain. Asking the agent something only answerable from a pre-park turn
        # needs a real harness, and that is a live-mode check, not this suite.
        spawns = []
        raw = sh("docker", "exec", NAME, "cat", "/data/park-spawns.jsonl").stdout or ""
        for ln in raw.splitlines():
            try:
                spawns.append(json.loads(ln))
            except ValueError:
                pass
        resume_argvs = [r.get("argv", []) for r in spawns
                        if "--resume" in (r.get("argv") or [])]
        resumed_ids = [a[a.index("--resume") + 1] for a in resume_argvs
                       if a.index("--resume") + 1 < len(a)]
        R.check("ENG-park-resume-passes-session-to-child",
                bool(sid_before) and sid_before in resumed_ids,
                "the child was spawned %d time(s); the ones carrying --resume passed %r, and "
                "the pre-park session was %r. A session_id that matches in the DATABASE while "
                "the child is started WITHOUT --resume is a fresh context wearing a resume's "
                "clothes — the board looks right and the agent has forgotten."
                % (len(spawns), resumed_ids, sid_before))

        R.check("ENG-park-resume", after.get("session_id") == sid_before,
                "resumed on session %r but parked on %r. A different id means the context "
                "was rebuilt, not resumed — the agent came back having forgotten, which is "
                "worse than never parking." % (after.get("session_id"), sid_before))

        # idle_timeout_secs == 0 -> never park. PM asked for this path explicitly.
        hot = make_agent("stays-hot", 0)
        if hot:
            http("POST", "/v1/agents/%s/start" % hot)
            turn(hot, "keep-me-hot")
            never, hst, _ = wait_for(lambda: state_of(hot).get("status"),
                                     lambda s: s == "parked", IDLE + 20)
            R.check("ENG-park-zero-never-parks", not never,
                    "idle_timeout_secs=0 means never park, but the agent reached %r" % hst)
        else:
            R.check("ENG-park-zero-never-parks", False,
                    "could not create an agent with idle_timeout_secs=0")

        return R.report("engine-park")
    finally:
        sh("docker", "rm", "-f", NAME)


if __name__ == "__main__":
    sys.exit(main())
