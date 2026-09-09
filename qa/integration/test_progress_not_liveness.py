#!/usr/bin/env python3

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""PROGRESS-* — a process being alive is not evidence that the work is happening.

PM, after the second instance in one evening:

  1. The escaper panic. A message body with '<' followed by a multi-byte char panicked
     the delivery task. Tokio unwinds the TASK, not the process, so `/healthz` answered
     200 the entire time and the container stayed `running` while no message was ever
     delivered again. Every healthcheck-shaped verdict that day was ambiguous because of
     it.
  2. Endpoint ingress enqueues the message and wakes the agent, but never pumps the
     queue. Production sat with three Telegram messages undelivered and the `pm` agent
     frozen in `starting`.

Same mistake twice, and it is not a coding mistake -- it is an ASSERTION mistake, which
makes it ours. Both systems were asked "are you up?", both truthfully said yes, and up was
read as working. So this file contains no liveness check at all. Every assertion here is
about a thing MOVING:

  - a message reaching a terminal state, not an agent reporting `running`;
  - a transitional status resolving, not merely existing.

WHY THE PRODUCERS ARE PARAMETRISED. Each of the two failures was in one producer's path
(delivery loop; ingress). A gate written against the producer that broke last time would
have missed the one that broke this time. There are four ways a message enters an agent's
queue and they share a drain but not an entry, so each is walked separately.

LIVENESS IS RECORDED, DELIBERATELY, AND NEVER ASSERTED ON. When progress fails, the report
prints whether the engine was still answering /healthz -- because "healthy and stuck" is the
signature of this whole class, and naming it in the failure text is what stops the next
person reaching for a restart.
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

PORT = free_port(int(os.environ.get("WHEEL_PROGRESS_PORT", "0")))
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-progress-%s" % uuid.uuid4().hex[:8]
SECRET = "qa-progress-secret-at-least-16chars"
IMAGE_TAG = os.environ.get("WHEEL_PROGRESS_IMAGE", "wheel-engine:test")
IMAGE = None
AGENT_ID = None

SKIP = 77
R = Results()

# A transitional status is a promise that something is in flight.
TRANSITIONAL = {"starting", "queued"}

# THE BOUND, AND WHY IT IS THIS NUMBER (ADVERSARY 041). Asserting that `starting`
# "eventually" resolves is exactly the test production would pass right now: a child can
# spawn cleanly and then block forever on an init line that never comes, and an unbounded
# wait simply waits. So the assertion has to be a DEADLINE, and the deadline has to be
# defensible in both directions:
#
#   Not shorter, or it fires on a merely slow start. The engine spawn contract gives the
#   ENGINE 10s to answer /healthz and 15s to shut down; an agent child is heavier than
#   that -- a harness cold start, config load and auth probe on a host running six agents.
#   60s is 6x the engine's health budget and 4x its shutdown budget, so a start that is
#   simply slow has room.
#
#   Not longer, because the number that matters is how long an operator stares at a board
#   before concluding it is broken. Tonight that was 45 minutes with three messages queued
#   and zero delivered. Any bound in minutes is indistinguishable from hung to the person
#   watching, which makes it useless as a gate however correct it looks.
#
# 60s is therefore the largest value that is still shorter than human patience, and the
# smallest that cannot fire on a healthy slow start.
RESOLVE_SECS = float(os.environ.get("WHEEL_PROGRESS_RESOLVE_SECS", "60"))
CONSUME_SECS = float(os.environ.get("WHEEL_PROGRESS_CONSUME_SECS", "90"))

# The four ways a message enters an agent's queue. They share a drain but not an entry,
# which is the whole reason this is a list: tonight's P0 was the ingress entry while the
# agent-to-agent entry worked perfectly, and a gate covering only the latter would have
# been green straight through it.
# (name, send, message_state, agent_status, agent_state, healthz)
PRODUCERS = []          # wired as each path lands

# PM's rule, and it is the reason this constant exists rather than a bare len() check:
# SKIP is honest today and dishonest tomorrow. A suite reporting SKIP with zero producers
# is a result nobody reads, and nothing forces the wiring, so it stays SKIP for weeks.
# The moment the FIRST producer is wired this must be bumped to 1, and from then on a
# producer count BELOW the floor is a FAILURE, not a skip. 1 -> 0 is a red build.
WIRED_FLOOR = 2


def poll(fn, want, timeout):
    """Return (ok, last_seen, waited). No sleep-then-assert: the whole point is elapsed
    time, so the time actually waited is reported rather than assumed."""
    t0 = time.time()
    last = None
    while time.time() - t0 < timeout:
        last = fn()
        if want(last):
            return True, last, time.time() - t0
        time.sleep(1.0)
    return False, last, time.time() - t0


def check_producer(name, send, message_state, agent_status, healthz):
    """One producer's path from 'accepted' to 'consumed'.

    `send` returns the message id the producer's entry point handed back, or None.
    """
    mid = send()
    if not R.control("PROGRESS/%s-accepted" % name, bool(mid),
                     "the producer did not accept the message at all, so there is nothing "
                     "to watch move and no claim to make about draining"):
        return

    ok, last, waited = poll(lambda: message_state(mid),
                            lambda s: s == "consumed", CONSUME_SECS)
    alive = healthz()
    status = agent_status()
    R.check("PROGRESS-message-reaches-consumed/%s" % name, ok,
            "a message accepted by %s never reached `consumed`: it sat in %r for %.0fs. "
            "The engine was %s and the agent reported %r that whole time -- which is the "
            "signature of this bug, not a defence against it. Enqueuing and waking the "
            "agent is not the same as pumping the queue."
            % (name, last, waited, "still answering /healthz" if alive else "not responding",
               status))


# States that are an ANSWER. A deadline that fires must leave the agent in one of these,
# with a reason -- not back in `starting`.
SETTLED = {"error", "stopped", "parked", "budget_exhausted", "needs_auth", "running", "idle"}


def check_deadline_outcome(agent_state, healthz, queued_count):
    """What the deadline DOES, not just that it fires — and WHEN it is allowed to fire.

    PM's first objection: a fix that satisfies `resolves within 60s` by killing and
    respawning the child every 60s PASSES a bare deadline assertion while being worse than
    the bug. So the outcome is asserted on three axes: the status afterwards is an ANSWER,
    there is a READABLE reason, and NO SECOND PROCESS was spawned (§3c #13). The third is
    what separates a real fix from a respawn loop, and it has to be counted over time --
    at any single instant a respawn loop looks exactly like an agent that is starting.

    PM's second objection, measured on production tonight and the reason this function
    takes `queued_count`: the pm agent sat in `starting` for 52 MINUTES emitting nothing
    and went to `idle` the instant a message arrived. It was waiting correctly, not wedged.
    A flat deadline would have killed a healthy agent, which is a worse gate than none --
    it would have manufactured exactly the restart-loop this file exists to forbid.

    So the predicate is NOT "a transitional status resolves within 60s". It is:

        an agent WITH WORK QUEUED does not stay transitional past the deadline.

    An empty queue means there is nothing to be late for, and the check declines to have
    an opinion rather than inventing a failure.
    """
    if queued_count() == 0:
        R.skip("PROGRESS-deadline-outcome",
               "the agent's queue is empty, so a transitional status is an agent WAITING, "
               "not an agent wedged — measured on production tonight, pm sat in `starting` "
               "52 minutes with nothing queued and went to `idle` the moment a message "
               "arrived. There is nothing to be late for, so this declines to judge.")
        return

    ok, _last, _waited = poll(agent_state,
                              lambda st: (st or {}).get("status") not in TRANSITIONAL,
                              RESOLVE_SECS)
    seen_pids = set()
    for _ in range(int(RESOLVE_SECS)):
        st = agent_state() or {}
        if st.get("pid"):
            seen_pids.add(st["pid"])
        time.sleep(1.0)
    final = agent_state() or {}

    R.check("PROGRESS-deadline-settles", ok and final.get("status") in SETTLED,
            "the agent had work QUEUED and is %r after %.0fs. A deadline that fires and "
            "returns the agent to a transitional state has not resolved anything -- it has "
            "made the hang periodic. The status afterwards must be an answer."
            % (final.get("status"), RESOLVE_SECS))

    # A reason is owed only when the deadline actually FIRED. An agent that resolved
    # normally has nothing to explain, and demanding last_error there is a false positive
    # -- which is exactly what this check did on its first run, against a healthy engine.
    if not ok:
        R.check("PROGRESS-deadline-reason-readable",
                bool((final.get("last_error") or "").strip()),
                "the agent stayed transitional past the deadline with work queued and set "
                "no `last_error`. An operator watching a board needs to know WHY; a status "
                "with no reason sends them to the logs to reconstruct it, which is the 45 "
                "minutes this class costs. last_error was %r." % final.get("last_error"))

    R.check("PROGRESS-deadline-spawns-no-second-process", len(seen_pids) <= 1,
            "%d distinct pids for one agent node across %.0fs: %s. This is the respawn "
            "loop -- the deadline fires, the child is replaced, the status leaves and "
            "re-enters `starting`, and a status-only assertion would call that a pass "
            "while the system burns a process a minute and still delivers nothing. "
            "Contract 3c #13: exactly one harness process per agent node, ever."
            % (len(seen_pids), RESOLVE_SECS, sorted(seen_pids)))


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
            txt = r.read().decode(errors="replace")
            return r.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        return e.code, None
    except Exception:
        return 0, None


def healthz():
    return http("GET", "/healthz")[0] == 200


def inbox_ids():
    st, r = http("GET", "/v1/agents/%s/inbox?limit=200" % AGENT_ID)
    return {m["id"] for m in ((r or {}).get("messages") or [])}


def message_state(mid):
    st, r = http("GET", "/v1/agents/%s/inbox?limit=200" % AGENT_ID)
    for m in ((r or {}).get("messages") or []):
        if m.get("id") == mid:
            return m.get("state")
    return None


def agent_state():
    st, board = http("GET", "/v1/board")
    for n in ((board or {}).get("nodes") or []):
        if n.get("id") == AGENT_ID:
            return n.get("state") or {}
    return {}


def queued_count():
    st, r = http("GET", "/v1/agents/%s/inbox?limit=200" % AGENT_ID)
    return sum(1 for m in ((r or {}).get("messages") or [])
               if m.get("state") == "queued")


def agent_status():
    return agent_state().get("status")


def new_message_from(action, timeout=30):
    """Run `action`, then return the id of the message it caused.

    Producers do not agree on what they hand back -- `send` returns a receipt, an ingress
    hit returns an HTTP response with no message id at all -- so the id is recovered by
    DIFFING the inbox instead. That keeps every producer on one code path, which is the
    point of parametrising them: the entry points differ, the assertion must not.
    """
    before = inbox_ids()
    action()
    t0 = time.time()
    while time.time() - t0 < timeout:
        new = inbox_ids() - before
        if new:
            return sorted(new)[0]
        time.sleep(0.5)
    return None


def start_engine():
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    sh("docker", "run", "-d", "--name", NAME,
       "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
       "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
       "-e", "WHEEL_VAULT_KEY=" + key,
       "-e", "WHEEL_ROLE=engine",
       "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
       "-p", "%d:7000" % PORT, IMAGE or IMAGE_TAG)
    for _ in range(60):
        if healthz():
            return True
        time.sleep(0.5)
    return False


def build_board():
    """agent + endpoint wired endpoint->agent(send). Returns (agent_id, endpoint_path)."""
    st, agent = http("POST", "/v1/nodes",
                     {"name": "worker", "type": "agent", "position": {"x": 0, "y": 0},
                      "config": {"harness": "claude", "system_prompt": "progress gate",
                                 "run_on_startup": False, "ephemeral_context": False}})
    if not (200 <= st < 300 and agent):
        return None, None
    path = "/hook"
    st2, ep = http("POST", "/v1/nodes",
                   {"name": "hook", "type": "endpoint", "position": {"x": 200, "y": 0},
                    "config": {"method": "POST", "path": path, "response_mode": "ack",
                               "auth": {"mode": "none"}}})
    if 200 <= st2 < 300 and ep:
        http("POST", "/v1/wires", {"from": ep["id"], "to": agent["id"], "type": "send"})
    return agent["id"], (path if (200 <= st2 < 300 and ep) else None)


def ingress_hit(path, body):
    req = urllib.request.Request(BASE + "/ingress" + path, method="POST",
                                 data=json.dumps(body).encode())
    req.add_header("content-type", "application/json")
    try:
        urllib.request.urlopen(req, timeout=30).read()
    except Exception:
        pass


def main():
    global IMAGE, AGENT_ID
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE_TAG).returncode != 0:
        print("%s not built — run `make engine-image-test`" % IMAGE_TAG)
        return SKIP
    IMAGE = pin_image(IMAGE_TAG)
    print("image: %s -> %s" % (IMAGE_TAG, (IMAGE or "?")[:19]))

    try:
        if not R.control("PROGRESS/engine-up", start_engine(),
                         "the engine never started, so nothing below is evidence"):
            return R.report("progress-not-liveness")
        if err := configure_fakes(NAME, transcript="/data/qa-progress-transcript.jsonl"):
            R.skip("PROGRESS/fakes", err)
            return R.report("progress-not-liveness")

        AGENT_ID, ep_path = build_board()
        if not R.control("PROGRESS/board-built", bool(AGENT_ID),
                         "could not create the agent node, so no producer has a target"):
            return R.report("progress-not-liveness")
        http("POST", "/v1/agents/%s/start" % AGENT_ID)

        producers = [("user-send",
                      lambda: http("POST", "/v1/agents/%s/send" % AGENT_ID,
                                   {"body": "progress-probe-user"}))]
        if ep_path:
            producers.append(("endpoint-ingress",
                              lambda: ingress_hit(ep_path, {"probe": "progress"})))
        else:
            # Named, not silently dropped: a producer that could not be built is missing
            # coverage, and PM's rule is that missing coverage must not look like a pass.
            R.skip("PROGRESS-message-reaches-consumed/endpoint-ingress",
                   "the endpoint node or its wire could not be created, so the ingress "
                   "path -- the one that broke in production -- was NOT exercised")

        if len(producers) < WIRED_FLOOR:
            R.check("PROGRESS-producers-stay-wired", False,
                    "%d producers wired, floor is %d. A producer was REMOVED; the path it "
                    "covered is exactly the kind that broke while a neighbour stayed "
                    "green." % (len(producers), WIRED_FLOOR))
            return R.report("progress-not-liveness")

        for name, action in producers:
            mid = new_message_from(action)
            if not R.control("PROGRESS/%s-accepted" % name, bool(mid),
                             "the producer did not create a message at all, so there is "
                             "nothing to watch move and no claim to make about draining"):
                continue
            ok, last, waited = poll(lambda: message_state(mid),
                                    lambda st: st == "consumed", CONSUME_SECS)
            alive, status = healthz(), agent_status()
            R.check("PROGRESS-message-reaches-consumed/%s" % name, ok,
                    "a message accepted by %s never reached `consumed`: it sat in %r for "
                    "%.0fs. The engine was %s and the agent reported %r that whole time "
                    "-- which is the signature of this bug, not a defence against it. "
                    "Enqueuing and waking the agent is not the same as pumping the queue; "
                    "the P0 was ingress calling start() instead of deliver()."
                    % (name, last, waited,
                       "still answering /healthz" if alive else "not responding", status))

        # Status assertions are about the AGENT, not a producer, so they run once.
        check_deadline_outcome(agent_state, healthz, queued_count)
        return R.report("progress-not-liveness")
    finally:
        sh("docker", "rm", "-f", NAME)


if __name__ == "__main__":
    sys.exit(main())
