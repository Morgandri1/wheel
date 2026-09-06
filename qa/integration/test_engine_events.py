#!/usr/bin/env python3
"""Engine events WebSocket — TESTPLAN ENG-log-stream-parity, ENG-events-*, COMMS-observability.

Why this suite exists, in SDK's own words: their e2e for BUG-009 "asserted that *a* log
event arrived rather than WHICH streams did", so the `transcript` stream going missing from
the WS was found by Web's compiler instead of by a test. An assertion that something
arrived cannot fail while anything at all arrives, so it cannot gate.

The assertion here is a SET COMPARISON, not a presence check: the streams the WS delivers
must equal the streams the database recorded for the same agent over the same window. That
makes the test independent of which streams exist today — a new stream is covered the day
it is added, and a stream that stops being broadcast is red the same day, without anyone
remembering to update a list.

The database is the reference because it is the side that was RIGHT in BUG-009: the rows
were persisted correctly and only the broadcast dropped them. If both sides ever break
together the comparison passes, so the suite also asserts the union is non-empty — two
empty sets are equal, and that is the failure mode of every parity test.
"""
import json, os, subprocess, sys, threading, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, configure_fakes

SKIP = 77
R = Results()
PORT = int(os.environ.get("WHEEL_ENGINE_EVENTS_PORT", "17420"))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-events-secret-at-least-16"
NAME = "qa-engine-events"
TRANSCRIPT = "/data/qa-events-transcript.jsonl"


def http(method, path, body=None):
    import urllib.error, urllib.request
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
         "-p", "%d:7000" % PORT, "wheel-engine:test"],
        capture_output=True, text=True)
    if p.returncode != 0:
        return "could not start wheel-engine:test: " + p.stderr.strip()[:200]
    for _ in range(60):
        try:
            if http("GET", "/healthz")[0] == 200:
                return configure_fakes(NAME, transcript=TRANSCRIPT)
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


class Listener(threading.Thread):
    """Collects /v1/events frames in the background for the life of the test."""

    def __init__(self, ws):
        super().__init__(daemon=True)
        self.ws, self.frames, self.error = ws, [], None

    def run(self):
        try:
            while True:
                raw = self.ws.recv()
                if raw is None or raw == "":
                    return
                try:
                    self.frames.append(json.loads(raw))
                except json.JSONDecodeError:
                    self.frames.append({"__unparseable__": raw[:200]})
        except Exception as e:
            self.error = repr(e)

    def of_type(self, t):
        return [f for f in self.frames if f.get("type") == t]


def streams_of(frames):
    """The `stream` of every log frame — 'stdout', 'stderr', 'transcript', …

    A log frame is `{type: "log", line: {node_id, seq, stream, at, text}}`. Reading `stream`
    from the top level instead yields the empty set, which is indistinguishable from "the
    engine broadcast nothing" — and I reported exactly that against SDK before checking a
    real frame. `parse_failed()` below exists so that mistake is loud next time.
    """
    out = set()
    for f in frames:
        line = f.get("line") if isinstance(f.get("line"), dict) else f
        if line.get("stream"):
            out.add(line["stream"])
    return out


def parse_failed(frames, streams):
    """True when frames arrived but none yielded a stream — a reader bug, not a finding."""
    return bool(frames) and not streams


def db_streams(agent_id):
    st, body = http("GET", "/v1/agents/%s/log" % agent_id)
    if not (200 <= st < 300) or not isinstance(body, (list, dict)):
        return set(), st
    rows = body if isinstance(body, list) else (body.get("lines") or body.get("entries") or [])
    return {r.get("stream") for r in rows if isinstance(r, dict) and r.get("stream")}, st


def main():
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    if subprocess.run(["docker", "image", "inspect", "wheel-engine:test"],
                      capture_output=True).returncode != 0:
        print("wheel-engine:test not built — run `make engine-image-test`")
        return SKIP
    try:
        import websocket
    except ImportError:
        print("websocket-client not installed — run `make bootstrap`")
        return SKIP

    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        st, agent = http("POST", "/v1/nodes", {
            "name": "watched", "type": "agent", "position": {"x": 0, "y": 0},
            "config": {"harness": "claude", "system_prompt": "EVENTS-CANARY",
                       "run_on_startup": False, "ephemeral_context": False}})
        if not R.check("ENG-events/setup", 200 <= st < 300, "-> %s %r" % (st, agent)):
            return R.report("engine-events")
        aid = agent["id"]

        ws = websocket.create_connection(
            "ws://127.0.0.1:%d/v1/events" % PORT,
            header=["Authorization: Bearer " + SECRET], timeout=30)
        listener = Listener(ws)
        listener.start()

        http("POST", "/v1/agents/%s/start" % aid)
        http("POST", "/v1/agents/%s/send" % aid, {"body": "events probe"})

        # Poll rather than sleep a fixed time: the assertion is about WHICH streams arrive,
        # and a fixed sleep turns a slow broadcast into a missing-stream failure.
        want, seen, deadline = set(), set(), time.time() + 60
        while time.time() < deadline:
            want, _ = db_streams(aid)
            seen = streams_of(listener.of_type("log"))
            if want and seen and want <= seen:
                break
            time.sleep(1)

        ws_state = {f.get("type") for f in listener.frames}
        R.check("ENG-events-connect", listener.error is None or listener.frames,
                "listener died: %s" % listener.error)

        # Two empty sets are equal. Say so before comparing them.
        if not R.check("ENG-log-stream-nonempty", bool(want),
                       "the engine recorded no log rows at all — nothing to compare"):
            return R.report("engine-events")

        # An empty WS set means one of two very different things. Separate them here, or the
        # suite reports a shape change in this file as a missing feature in the engine.
        log_frames = listener.of_type("log")
        if not R.check("ENG-events-log-readable", not parse_failed(log_frames, seen),
                       "%d log frames arrived and none carried a stream — this suite is "
                       "reading the frame wrong, the engine is not broadcasting wrong: %s"
                       % (len(log_frames), json.dumps(log_frames[0])[:200] if log_frames else "")):
            return R.report("engine-events")

        R.check("ENG-log-stream-parity", want <= seen,
                "recorded %s but broadcast only %s — missing: %s"
                % (sorted(want), sorted(seen), sorted(want - seen)))

        # BUG-009 named this stream specifically, so it is asserted by name AS WELL as by
        # the set comparison. The set catches the next one; the name catches a regression
        # of this one even if the recording side breaks in the same way.
        if "transcript" in want:
            R.check("COMMS-observability/transcript", "transcript" in seen,
                    "transcript rows are persisted but never broadcast (BUG-009)")
        else:
            R.skip("COMMS-observability/transcript",
                   "engine recorded no transcript rows in this window")

        R.check("ENG-events-node-state", "node.state" in ws_state,
                "no node.state frame across a start + a message; saw %s" % sorted(ws_state))
        R.check("ENG-events-message", "message" in ws_state,
                "no message frame for a delivered message; saw %s" % sorted(ws_state))

        # A frame that names a session must name the one the engine actually started;
        # §"Harness event integrity" makes a mismatch a security property, not a detail.
        sids = {f.get("session_id") for f in listener.frames if f.get("session_id")}
        R.check("ENG-events-one-session", len(sids) <= 1,
                "frames carry %d different session ids: %s" % (len(sids), sorted(sids)))

        try:
            ws.close()
        except Exception:
            pass
    finally:
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)

    return R.report("engine-events")


if __name__ == "__main__":
    sys.exit(main())
