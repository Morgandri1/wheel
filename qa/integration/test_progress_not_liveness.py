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

# A transitional status is a promise that something is happening. If it is still true a
# minute later, the promise is broken whatever the process is doing.
TRANSITIONAL = {"starting", "queued"}
RESOLVE_SECS = float(os.environ.get("WHEEL_PROGRESS_RESOLVE_SECS", "60"))
CONSUME_SECS = float(os.environ.get("WHEEL_PROGRESS_CONSUME_SECS", "90"))


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
            "how alive the process is (/healthz: %s). `starting` forever is how production "
            "sat with three undelivered messages while every dashboard looked fine."
            % (last, waited, "200" if healthz() else "down"))


def main():
    print(__doc__.strip().splitlines()[0])
    print("\nThis suite asserts PROGRESS, never liveness. It needs a running engine and a\n"
          "fresh wheel-engine:test image; wire it into the M2 ingress run alongside ING-*.\n"
          "Producers to walk: user-send, agent-msg, endpoint-ingress, script-msg.")
    # The four producers are wired up as ING-*/MSG-* land. Until then this reports
    # honestly that it has not run, rather than defining zero producers and passing.
    R.skip("PROGRESS-message-reaches-consumed",
           "no producer is wired up in this suite yet — a suite with zero cases is not a "
           "passing suite. Wire user-send first (it exists today), then endpoint-ingress "
           "when SDK lands the P0 fix, which is the path that actually broke.")
    R.skip("PROGRESS-transitional-status-resolves",
           "needs a live agent; lands with the same wiring")
    return R.report("progress-not-liveness")


if __name__ == "__main__":
    sys.exit(main())
