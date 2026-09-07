#!/usr/bin/env python3
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
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results  # noqa: E402

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
PRODUCERS = []          # (name, send, ...) — wired as each path lands

# PM's rule, and it is the reason this constant exists rather than a bare len() check:
# SKIP is honest today and dishonest tomorrow. A suite reporting SKIP with zero producers
# is a result nobody reads, and nothing forces the wiring, so it stays SKIP for weeks.
# The moment the FIRST producer is wired this must be bumped to 1, and from then on a
# producer count BELOW the floor is a FAILURE, not a skip. 1 -> 0 is a red build.
WIRED_FLOOR = 0


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


def check_no_stuck_status(agent_status, healthz):
    ok, last, waited = poll(agent_status, lambda s: s not in TRANSITIONAL, RESOLVE_SECS)
    R.check("PROGRESS-transitional-status-resolves", ok,
            "the agent was still %r after %.0fs. A transitional status is a claim that "
            "something is in flight; if it never resolves, the claim is false no matter "
            "how alive the process is (/healthz: %s). ADVERSARY 041: a child can spawn "
            "cleanly and then block forever waiting on an init line that never comes, so "
            "the process is genuinely healthy and genuinely doing nothing. This is a "
            "DEADLINE, not an eventually — an unbounded wait is the test production "
            "passes while sitting at 45 minutes and three undelivered messages."
            % (last, waited, "200" if healthz() else "down"))


def main():
    print(__doc__.strip().splitlines()[0])

    # PM's rule: a producer count below the floor is a FAILURE. Once anything has been
    # wired, "zero producers" stops meaning "too early" and starts meaning "someone
    # deleted the coverage", and those must not report the same colour.
    if len(PRODUCERS) < WIRED_FLOOR:
        R.check("PROGRESS-producers-stay-wired", False,
                "this suite has %d producers wired but the floor is %d. A producer was "
                "REMOVED. The path it covered is exactly the kind that broke in "
                "production while a neighbouring path stayed green, so losing one is a "
                "red build, not a skip. Restore it, or lower WIRED_FLOOR deliberately "
                "and say why in the commit." % (len(PRODUCERS), WIRED_FLOOR))
        return R.report("progress-not-liveness")

    if not PRODUCERS:
        # SKIP HERE MEANS "NOT WIRED YET", NOT "NOT APPLICABLE". The distinction is the
        # point: not-applicable is a permanent, acceptable state; not-wired-yet is a debt
        # with an owner and a deadline. Spelled out in the ID so nobody has to infer it.
        R.skip("PROGRESS/not-wired-yet--no-producers-defined",
               "no producer is wired in this suite yet, so it has asserted NOTHING. This "
               "is a DEBT, not a not-applicable: the paths exist, the coverage does not. "
               "Wire user-send first (it works today), then endpoint-ingress the moment "
               "SDK's P0 lands, since that is the path that actually broke. Bump "
               "WIRED_FLOOR to 1 in the same commit — from then on, dropping back to "
               "zero is a failing build rather than this message.")
        return R.report("progress-not-liveness")

    for name, send, message_state, agent_status, healthz in PRODUCERS:
        check_producer(name, send, message_state, agent_status, healthz)
        check_no_stuck_status(agent_status, healthz)
    return R.report("progress-not-liveness")


if __name__ == "__main__":
    sys.exit(main())
