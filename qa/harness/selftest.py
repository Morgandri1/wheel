#!/usr/bin/env python3
"""Self-tests for the fake harness. Runs in `make check` — no docker, no network.

The fake harness is test infrastructure: if it lies, every integration test that
depends on it lies too. So it gets tested like production code.
"""
import json, os, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
CLAUDE = os.path.join(HERE, "fake-claude")
CODEX = os.path.join(HERE, "fake-codex")
FAILS = []

def run(args, stdin="", env=None, binary=None):
    e = dict(os.environ); e.update(env or {})
    p = subprocess.run([binary or CLAUDE] + args, input=stdin, capture_output=True, text=True, env=e, timeout=30)
    return p

def events(out):
    evs = []
    for ln in out.splitlines():
        ln = ln.strip()
        if not ln:
            continue
        try:
            evs.append(json.loads(ln))
        except json.JSONDecodeError:
            evs.append({"type": "__raw__", "raw": ln})
    return evs

def turn(text):
    return json.dumps({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}}) + "\n"

def check(name, cond, detail=""):
    if cond:
        print("  ok   %s" % name)
    else:
        print("  FAIL %s %s" % (name, detail))
        FAILS.append(name)

SJ = ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"]

def t_protocol_shape():
    p = run(SJ + ["--model", "m1"], turn("hello"))
    evs = events(p.stdout)
    check("exits 0", p.returncode == 0, p.stderr[:200])
    check("first event is system/init",
          evs and evs[0]["type"] == "system" and evs[0]["subtype"] == "init")
    check("init carries session_id + model", bool(evs[0].get("session_id")) and evs[0]["model"] == "m1")
    types = [e["type"] for e in evs]
    check("emits assistant then result", "assistant" in types and types[-1] == "result")
    res = evs[-1]
    for k in ("type", "subtype", "is_error", "result", "session_id", "num_turns", "duration_ms", "usage"):
        check("result has %s" % k, k in res)
    check("result not an error", res["is_error"] is False and res["subtype"] == "success")
    asst = [e for e in evs if e["type"] == "assistant"][0]
    check("assistant message shape",
          asst["message"]["role"] == "assistant" and asst["message"]["content"][0]["type"] == "text")
    check("session_id consistent across events",
          asst["session_id"] == res["session_id"] == evs[0]["session_id"])

def t_echo_property():
    """The property every injection test depends on."""
    secret = "the-sky-is-green-4f2a"
    env = '<AgentPrompt id="11111111-2222-4333-8444-555555555555" from="ctx" type="ctx">\n%s\n</AgentPrompt>' % secret
    p = run(SJ, turn(env))
    out = events(p.stdout)[-1]["result"]
    check("reply echoes user text", secret in out)
    check("reply preserves the AgentPrompt envelope", "<AgentPrompt" in out and "</AgentPrompt>" in out)

def t_envelope_roundtrip():
    """The engine escapes a literal </AgentPrompt> in a body; the fake must not mangle it.

    This is the transport half of MSG-envelope-escape: whatever bytes the engine writes, the
    transcript must show byte-for-byte, or the engine-side escaping test is measuring the fake.
    """
    nasty = 'body with </AgentPrompt> and "quotes" and \\backslash and \u00e9 unicode'
    env = '<AgentPrompt id="abc" from="a" type="agent">\n%s\n</AgentPrompt>' % nasty
    with tempfile.TemporaryDirectory() as d:
        tp = os.path.join(d, "t.jsonl")
        p = run(SJ, turn(env), env={"WHEEL_FAKE_TRANSCRIPT": tp})
        check("nasty envelope does not crash the fake", p.returncode == 0, p.stderr[:200])
        raw = open(tp).read()
        parsed = json.loads(raw.strip())
        got = parsed["message"]["content"][0]["text"]
        check("transcript round-trips the envelope byte-for-byte", got == env, repr(got[:120]))
        check("reply carries the nasty body", nasty.split()[0] in events(p.stdout)[-1]["result"])

def t_system_prompt_first_event():
    p = run(SJ + ["--system-prompt", "SYS-A", "--append-system-prompt", "CTX-B"], turn("hi"))
    ev = events(p.stdout)[0]
    check("composed system prompt in event 1",
          ev.get("system_prompt") == "SYS-A\nCTX-B", repr(ev.get("system_prompt")))

def t_multi_turn():
    p = run(SJ, turn("one") + turn("two"))
    res = [e for e in events(p.stdout) if e["type"] == "result"]
    check("one result per turn", len(res) == 2, str(len(res)))
    check("turn counter advances", res[0]["num_turns"] == 1 and res[1]["num_turns"] == 2)
    check("session_id stable across turns", res[0]["session_id"] == res[1]["session_id"])

def t_session_ids():
    a = events(run(SJ, turn("x")).stdout)[-1]["session_id"]
    b = events(run(SJ, turn("x")).stdout)[-1]["session_id"]
    check("fresh process => new session (ephemeral-context hook)", a != b)
    c = events(run(SJ + ["--resume", "fixed-123"], turn("x")).stdout)[-1]["session_id"]
    check("--resume pins session", c == "fixed-123")
    d = events(run(SJ, turn("x"), env={"WHEEL_FAKE_SESSION_ID": "env-9"}).stdout)[-1]["session_id"]
    check("WHEEL_FAKE_SESSION_ID pins session", d == "env-9")

def t_directives():
    r = events(run(SJ, turn("x <<FAKE:REPLY=exact>>")).stdout)[-1]
    check("REPLY overrides", r["result"] == "exact", r["result"])
    r = events(run(SJ, turn("x <<FAKE:ERROR=boom>>")).stdout)[-1]
    check("ERROR sets is_error", r["is_error"] is True and r["result"] == "boom")
    check("ERROR subtype", r["subtype"] == "error_during_execution")
    p = run(SJ, turn("x <<FAKE:EXIT=7>>"))
    check("EXIT sets exit code", p.returncode == 7, str(p.returncode))
    p = run(SJ, turn("x <<FAKE:STDERR=oops>>"))
    check("STDERR reaches stderr", "oops" in p.stderr)
    evs = events(run(SJ, turn("x <<FAKE:GARBAGE>>")).stdout)
    check("GARBAGE emits a non-JSON line", any(e["type"] == "__raw__" for e in evs))
    check("GARBAGE still completes the turn", evs[-1]["type"] == "result")
    evs = events(run(SJ, turn("x <<FAKE:NOISE>>")).stdout)
    check("NOISE emits unknown event types",
          any(e["type"] == "rate_limit_event" for e in evs)
          and any(e.get("subtype") == "thinking_tokens" for e in evs))
    evs = events(run(SJ, turn("x <<FAKE:TOOL=ls>>")).stdout)
    blocks = [b for e in evs if e["type"] == "assistant" for b in e["message"]["content"]]
    check("TOOL emits tool_use", any(b["type"] == "tool_use" for b in blocks))
    check("TOOL emits tool_result",
          any(b.get("type") == "tool_result" for e in evs if e["type"] == "user"
              for b in e["message"]["content"]))
    check("directives stripped from reply",
          "<<FAKE:" not in events(run(SJ, turn("visible <<FAKE:NOISE>>")).stdout)[-1]["result"])

def t_auth():
    p = run(SJ, turn("x"), env={"WHEEL_FAKE_AUTH": "needs_auth"})
    check("needs_auth exits non-zero", p.returncode != 0)
    check("needs_auth message on stderr", "login" in p.stderr.lower(), p.stderr[:120])
    p = run(SJ, turn("x"), env={"WHEEL_FAKE_STRICT_AUTH": "1", "ANTHROPIC_API_KEY": ""})
    check("strict auth without creds => needs_auth", p.returncode != 0)
    p = run(SJ, turn("x"), env={"WHEEL_FAKE_STRICT_AUTH": "1", "ANTHROPIC_API_KEY": "sk-test"})
    check("strict auth with creds => ok", p.returncode == 0)

def t_transcript():
    with tempfile.TemporaryDirectory() as d:
        tp = os.path.join(d, "t.jsonl")
        run(SJ, turn("captured-me"), env={"WHEEL_FAKE_TRANSCRIPT": tp})
        body = open(tp).read()
        check("transcript captures raw stdin", "captured-me" in body)
        check("transcript is one line per turn", len(body.strip().splitlines()) == 1)

def t_script_replay():
    with tempfile.TemporaryDirectory() as d:
        sp = os.path.join(d, "s.jsonl")
        with open(sp, "w") as f:
            f.write('{"reply":"one"}\n')
            f.write('# a comment line\n')
            f.write('{"is_error":true,"error":"nope"}\n')
            f.write('{"events":[{"type":"rate_limit_event"}],"reply":"three"}\n')
        p = run(SJ, turn("a") + turn("b") + turn("c"), env={"WHEEL_FAKE_SCRIPT": sp})
        evs = events(p.stdout)
        res = [e for e in evs if e["type"] == "result"]
        check("script turn 1", res[0]["result"] == "one")
        check("script turn 2 errors", res[1]["is_error"] is True and res[1]["result"] == "nope")
        check("script turn 3", res[2]["result"] == "three")
        check("script raw events emitted", any(e["type"] == "rate_limit_event" for e in evs))
        p = run(SJ, turn("a") * 5, env={"WHEEL_FAKE_SCRIPT": sp})
        check("overrunning script fails loudly", p.returncode != 0 and "exhausted" in p.stderr)
        p = run(SJ, turn("a"), env={"WHEEL_FAKE_SCRIPT": os.path.join(d, "missing.jsonl")})
        check("missing script fails loudly", p.returncode != 0)

def t_robustness():
    p = run(SJ, "not json at all\n")
    check("malformed stdin rejected", p.returncode != 0)
    p = run(SJ, "\n\n" + turn("ok") + "\n")
    check("blank stdin lines tolerated", p.returncode == 0)
    p = run(SJ + ["--some-future-flag", "v", "--another"], turn("ok"))
    check("unknown flags tolerated", p.returncode == 0, p.stderr[:120])
    p = run(["--version"])
    check("--version works", p.returncode == 0 and "Claude Code" in p.stdout)
    p = subprocess.run("%s %s < /dev/null | head -1" % (CLAUDE, " ".join(SJ)),
                       shell=True, capture_output=True, text=True, timeout=30)
    check("no traceback when stdout closes early", "Traceback" not in p.stderr, p.stderr[:200])

def t_text_mode():
    p = run(["-p", "hello there"])
    check("text mode plain output", p.returncode == 0 and "hello there" in p.stdout)
    check("text mode emits no JSON", not p.stdout.strip().startswith("{"))

def t_codex():
    p = run(["exec", "--json"], '{"message":"hi codex"}\n', binary=CODEX)
    evs = events(p.stdout)
    types = [e["type"] for e in evs]
    check("codex thread.started first", types and types[0] == "thread.started")
    check("codex completes turn", "turn.completed" in types)
    check("codex echoes", any(e.get("item", {}).get("text", "").endswith("hi codex") for e in evs))
    p = run(["exec", "--json", "direct prompt"], "", binary=CODEX)
    check("codex positional prompt", "direct prompt" in p.stdout)

def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("t_")]
    for t in tests:
        print("%s:" % t.__name__[2:])
        try:
            t()
        except Exception as e:
            print("  FAIL %s raised %r" % (t.__name__, e))
            FAILS.append(t.__name__)
    print()
    if FAILS:
        print("fake-harness selftest: %d FAILED -> %s" % (len(FAILS), ", ".join(FAILS)))
        return 1
    print("fake-harness selftest: all passed")
    return 0

if __name__ == "__main__":
    sys.exit(main())
