#!/usr/bin/env python3
"""ENG-panic-* — a bad row must not take the board down, and the escaper must not panic.

The board has been 502 since 14:34 on a panic. A panic in the delivery path is worse than a
wrong answer: it takes down every project on the host, and a stored row that triggers it
takes the engine down again on every restart, so the system cannot recover by being turned
off and on again.

Two properties, and the second is the one that matters:

  1. The envelope escaper never panics on any input. Byte-indexing a UTF-8 string is the
     classic way to panic here: an em dash is 3 bytes, an emoji 4, and a boundary landing
     mid-character is a slice on a non-char-boundary. Driven from outside by putting those
     characters at EVERY offset around the escaper's boundary rather than at one.

  2. AN ENGINE STILL STARTS WHEN A STORED MESSAGE CANNOT BE PARSED. Whatever the parser
     rejects, it must reject that ROW, not the boot. A project whose database contains one
     unparseable message must still serve; anything else is a permanent outage that
     survives every restart, which is exactly what a 502 since 14:34 looks like.
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
from wheel_client import Results, free_port, pin_image, configure_fakes  # noqa: E402

SKIP = 77
R = Results()
PORT = free_port(int(os.environ.get("WHEEL_PANIC_PORT", "17436")))
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-panic-%s" % uuid.uuid4().hex[:8]
SECRET = "qa-panic-secret-at-least-16c"
TRANSCRIPT = "/data/qa-panic-transcript.jsonl"

# The tag is a mutable pointer on a host where six agents build it; resolved to an
# immutable ID once, at the top, so every container in this run is the same build and the
# report can say which. `WHEEL_PANIC_IMAGE` points the whole suite at another build, which
# is how the mutation check runs it against an engine WITHOUT the fix.
IMAGE_TAG = os.environ.get("WHEEL_PANIC_IMAGE", "wheel-engine:test")
IMAGE = None

# One of each width, plus a combining sequence: 3-byte em dash, 4-byte emoji, 3-byte CJK,
# and a family emoji built from joiners, which is where naive "one char" logic also fails.
NASTY = ["—", "\U0001F600", "中文", "\U0001F468‍\U0001F469‍\U0001F467"]
CLOSE = "</AgentPrompt>"


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, body=None, timeout=20):
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
    except Exception as e:
        return 0, str(e)


def wait_transcript(needle, timeout):
    """True once `needle` appears in the child's stdin transcript.

    The transcript is what the engine actually wrote to the harness, so it is proof the
    delivery path ran -- not proof that an HTTP call was accepted.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        got = sh("docker", "exec", NAME, "sh", "-c", "cat %s 2>/dev/null" % TRANSCRIPT)
        if needle in got.stdout:
            return True
        time.sleep(1)
    return False


def start(name, port):
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    sh("docker", "run", "-d", "--name", name,
       "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
       "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
       "-e", "WHEEL_VAULT_KEY=" + key,
       "-e", "WHEEL_ROLE=engine",
       "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
       "-p", "%d:7000" % port, IMAGE or IMAGE_TAG)
    for _ in range(60):
        if http("GET", "/healthz")[0] == 200:
            return True
        time.sleep(0.5)
    return False


def main():
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE_TAG).returncode != 0:
        print("%s not built — run `make engine-image-test`" % IMAGE_TAG)
        return SKIP
    global IMAGE
    IMAGE = pin_image(IMAGE_TAG)
    print("image: %s -> %s" % (IMAGE_TAG, (IMAGE or "?")[:19]))

    try:
        if not R.control("ENG-panic/engine-up", start(NAME, PORT),
                         "the engine did not start, so nothing below is evidence"):
            return R.report("engine-panics")

        st, agent = http("POST", "/v1/nodes",
                         {"name": "victim", "type": "agent", "position": {"x": 0, "y": 0},
                          "config": {"harness": "claude", "system_prompt": "V",
                                     "run_on_startup": False, "ephemeral_context": False}})
        aid = (agent or {}).get("id")
        if not R.control("ENG-panic/setup", bool(aid), "node create -> %s" % st):
            return R.report("engine-panics")

        # ---- 1. the escaper, driven where it actually runs ---------------------------
        #
        # REWRITTEN after a mutation check. The first version of this section POSTed bodies
        # to /send and asserted the engine stayed up. It passed against an engine built
        # from the exact pre-fix source (455b753^) -- the code that took the board down --
        # and it passed for two independent reasons, both mine:
        #
        #   a. It never started the agent. `escape_envelope_body` runs at DELIVERY
        #      (supervisor/mod.rs:545, via `Message::envelope`), not at send. A message to
        #      a stopped agent queues, and the escaper is never called at all.
        #   b. Even delivered, the bodies could not have panicked. The shape is `<` with a
        #      multi-byte character straddling `name_at + TAG.len()`; mine put the
        #      multi-byte character BEFORE the `<` or after a complete "</AgentPrompt>",
        #      where every offset is a boundary.
        #
        # So: the fake harness writes a transcript, the agent runs, and a delivered benign
        # body is the CONTROL that the escaper executes at all. Then the offsets are walked.
        if err := configure_fakes(NAME, transcript=TRANSCRIPT):
            R.skip("ENG-escaper-never-panics", err)
            return R.report("engine-panics")
        http("POST", "/v1/agents/%s/start" % aid)

        canary = "escaper-control-canary"
        http("POST", "/v1/agents/%s/send" % aid, {"body": canary})
        if not R.control("ENG-panic/escaper-runs", wait_transcript(canary, 90),
                         "no delivered message reached the child's stdin, so "
                         "`Message::envelope` -- and the escaper inside it -- never ran. "
                         "Nothing below this line would be evidence: an engine that stays "
                         "up because a code path is never executed is not an engine that "
                         "survives the code path"):
            return R.report("engine-panics")

        # `<` (and `</`) with k ASCII bytes then a multi-byte character, for every k that
        # can put a continuation byte on either end of the 11-byte tag slice. k up to 14
        # walks the character fully across and out the far side.
        cases = []
        for ch in NASTY:
            for k in range(0, 15):
                for lead in ("<", "</"):
                    cases.append(("%s%s%s%s" % (lead, "a" * k, ch, "z" * 14),
                                  "%r at k=%d after %r" % (ch, k, lead)))
        # PM pulled these bytes out of the production `nodes` row by hand. Carried verbatim
        # so the fixture is anchored to the thing that actually happened, not to my
        # reconstruction of it.
        cases.append(("- `BLOCKED: <what> \u2014 <what you need> \u2014 <what you\u2019re doing>`",
                      "PM's production line, verbatim"))

        # A panic in a tokio task unwinds THAT task, so the symptom is not always a dead
        # process: it can be a 5xx, or a process that answers /healthz perfectly while the
        # delivery loop it needs is gone. Each signal is recorded separately, because
        # "the engine went down" and "the engine stopped delivering" are different claims
        # and only one of them is going to be true.
        survived, detail = True, ""
        for body, what in cases:
            st, _ = http("POST", "/v1/agents/%s/send" % aid, {"body": body})
            health = http("GET", "/healthz")[0]
            if st == 0 or st >= 500 or health != 200:
                running = sh("docker", "inspect", "-f", "{{.State.Running}}", NAME).stdout.strip()
                probe = "escaper-liveness-%s" % uuid.uuid4().hex[:6]
                http("POST", "/v1/agents/%s/send" % aid, {"body": probe})
                delivering = wait_transcript(probe, 25)
                survived = False
                detail = ("%s: send -> %s, /healthz -> %s, container running=%s, "
                          "a later benign message %s. This is the outage shape: a message "
                          "body a human would type, sliced at a byte that is not a "
                          "character boundary. Engine said: %s"
                          % (what, st, health, running,
                             "was still delivered" if delivering else "was NOT delivered",
                             sh("sh", "-c",
                                "docker logs --tail 4 %s 2>&1" % NAME).stdout[-260:]))
                break
        R.gated("ENG-escaper-never-panics", "ENG-panic/escaper-runs", survived, detail)

        # Distinct property, distinct ID: the process is still there. It can be true while
        # the one above is false, and on its own it means very little -- which is the point
        # of keeping them apart rather than reporting one verdict for both.
        R.gated("ENG-escaper-engine-survives", "ENG-panic/escaper-runs",
                http("GET", "/healthz")[0] == 200,
                "the engine stopped serving after the escaper inputs — it panicked on one "
                "of them and took the board with it")

        # And the board is only actually alive if it still DELIVERS. A process answering
        # /healthz with a dead delivery loop is the failure that reads as healthy.
        after = "escaper-still-delivering-%s" % uuid.uuid4().hex[:6]
        http("POST", "/v1/agents/%s/send" % aid, {"body": after})
        R.gated("ENG-delivery-survives-escaper", "ENG-panic/escaper-runs",
                wait_transcript(after, 60),
                "the engine answers /healthz but no longer delivers messages: the panic "
                "took the delivery loop, not the process. Every board on the host looks up "
                "and is doing nothing")

        # ---- 2. THE ONE THAT MATTERS: an unparseable row must not stop the boot -------
        #
        # Written straight into sqlite, because the point is a row the ENGINE did not
        # create and would never accept — the state a bad migration, a partial write or an
        # older version leaves behind.
        # Invalid UTF-8 built in SQL, not in a Python string literal. My first attempt
        # passed a lone surrogate and a NUL through `docker exec python3 -c`, which the
        # shell and the encoder mangled between them — the control caught it, which is the
        # only reason this is a corrected fixture rather than a false green.
        script = (
            "import sqlite3, uuid\n"
            "c = sqlite3.connect('/data/wheel.db')\n"
            "c.execute(\"INSERT INTO messages (id,from_kind,from_id,to_id,body,sha256,"
            "bytes,state,is_error,created_at) VALUES (?,?,?,?,CAST(x'FFFE003C2F41' AS TEXT),"
            "?,?,?,?,?)\", (str(uuid.uuid4()), 'user', None, %r, 'not-a-sha', 6, 'queued', "
            "0, 'not-a-timestamp'))\n"
            "c.commit()\n"
            "print('inserted')\n" % aid)
        bad = sh("docker", "exec", "-i", NAME, "python3", "-c", script)
        if not R.control("ENG-panic/bad-row-inserted", "inserted" in bad.stdout,
                         "could not write an unparseable row, so a green below would only "
                         "mean the engine restarted normally: %s"
                         % (bad.stderr or "")[-200:]):
            return R.report("engine-panics")

        # THREE reboots, not one. The failure this guards against is a row that poisons
        # every boot — "it came back once" and "it comes back" are different claims, and a
        # single restart can pass by coincidence while the second dies. An engine that
        # recovers once and then dies is indistinguishable, from one restart, from one that
        # recovered; the operator only learns the difference at 3am.
        back = True
        reboots = 0
        for attempt in range(1, 4):
            sh("docker", "restart", NAME)
            came_up = False
            for _ in range(60):
                if http("GET", "/healthz")[0] == 200:
                    came_up = True
                    break
                time.sleep(0.5)
            if not came_up:
                back = False
                break
            reboots = attempt
        logs = sh("sh", "-c", "docker logs --tail 6 %s 2>&1" % NAME).stdout[-300:]
        R.gated("ENG-starts-with-unparseable-message", "ENG-panic/bad-row-inserted", back,
                "the engine did not come back after a restart with ONE unparseable message "
                "row — it survived %d reboot(s) and then did not. The parser must reject "
                "the ROW, not the boot: a row it cannot read takes the board down on EVERY "
                "restart, so the system cannot recover by being turned off and on again. "
                "Engine said: %s" % (reboots, logs))
        # ---- ordinary prose in a ctx node --------------------------------------------
        #
        # STATUS, stated up front because it changes what this proves: these three checks
        # pass against the PRE-FIX engine too (wheel-engine built from 455b753^, the code
        # that was live during the outage). They are a regression guard, NOT the
        # reproduction. The reproduction is section 1.
        #
        # PM pulled these bytes from the production `nodes` table and read them as the
        # cause. The bytes are certainly there. But the escaper has exactly one production
        # call site -- `Message::envelope`, supervisor/mod.rs:545 -- and it runs on message
        # BODIES. A ctx node's markdown reaches the child through supervisor/prompt.rs,
        # which does no slicing and no escaping at all. So the same sentence kills the
        # engine when it arrives as a message and does nothing when it sits in a ctx node,
        # which is what these checks measure and what section 1 measures.
        #
        # PM's contract text is also sent as messages constantly, and "BLOCKED: <what> —
        # <what you need>" is in the message template itself. The likeliest story is that
        # the bytes were found in the node and did their damage in the message. Kept
        # anyway: ctx markdown is replayed at every start, so if anything downstream ever
        # does start slicing it, this is where that shows up.
        #
        # Pulled from the production database by PM after this suite's first fixture could
        # not reproduce the outage. It is not a message row and it is not malformed: it is
        # the `nodes` table, a ctx node's markdown, perfectly valid UTF-8. The engine WROTE
        # it happily and could not read it back.
        #
        # The shape is a '<' followed within TAG.len() bytes by a multi-byte character:
        # name_at lands after the '<', name_at + TAG.len() lands INSIDE the three bytes of
        # an em dash, and slicing there panics. The bytes below are PM's own contract text,
        # and the other two production hits were a table of CLI examples — `wheel write
        # <ctx> "…"`. This is a sentence anyone would write.
        #
        # It is board CONTENT, so it is replayed at EVERY start. That is why the board
        # stayed down for 56 minutes across however many reboots: restarting reloads the
        # poison. A message row can at least be drained; a ctx node is read again forever.
        poison = "- `BLOCKED: <what> \u2014 <what you need> \u2014 <what you\u2019re doing>`\n"
        st, ctx = http("POST", "/v1/nodes",
                       {"name": "housestyle", "type": "ctx", "position": {"x": 9, "y": 0},
                        "config": {"markdown": poison}})
        cid = (ctx or {}).get("id")
        if not R.control("ENG-panic/poison-ctx-stored", 200 <= st < 300 and bool(cid),
                         "the engine refused the ctx markdown (%s), so it never reached the "
                         "board and nothing below is evidence" % st):
            return R.report("engine-panics")

        # Wired ctx -> agent is the injection path: this markdown is prepended to the
        # child's prompt at every start, which is where the escaper reads it.
        http("POST", "/v1/wires", {"from": cid, "to": aid, "type": "send"})

        survived_reboots = 0
        alive = True
        for attempt in range(1, 4):
            sh("docker", "restart", NAME)
            up = False
            for _ in range(60):
                if http("GET", "/healthz")[0] == 200:
                    up = True
                    break
                time.sleep(0.5)
            if not up:
                alive = False
                break
            survived_reboots = attempt

        logs2 = sh("sh", "-c", "docker logs --tail 8 %s 2>&1" % NAME).stdout[-320:]
        R.gated("ENG-starts-with-poison-ctx", "ENG-panic/poison-ctx-stored", alive,
                "the engine did not come back with an ordinary sentence in a ctx node — "
                "'<what> — <what you need>'. It survived %d reboot(s). This is board "
                "CONTENT, replayed at every start, so it cannot be drained the way a "
                "message can: the board stays down until somebody edits the database. "
                "Engine said: %s" % (survived_reboots, logs2))

        # And the injection path itself, which is where the slice actually happens.
        if alive:
            http("POST", "/v1/agents/%s/start" % aid)
            time.sleep(5)
            R.gated("ENG-injects-poison-ctx-without-panic", "ENG-panic/poison-ctx-stored",
                    http("GET", "/healthz")[0] == 200,
                    "the engine died while composing a prompt containing the ctx markdown — "
                    "the escaper sliced inside a multi-byte character. Engine said: %s"
                    % sh("sh", "-c", "docker logs --tail 6 %s 2>&1" % NAME).stdout[-300:])

    finally:
        sh("docker", "rm", "-f", NAME)

    return R.report("engine-panics")


if __name__ == "__main__":
    sys.exit(main())
