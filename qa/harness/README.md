# Fake harness spec (QA → SDK)

**Status:** implemented, working, and conformed to the contract in `docs/plans/qa.brief.md`
(image `wheel-engine:test` via `--build-arg FAKE_HARNESS=1`; full composed system prompt as the
first event; canned turns from `WHEEL_FAKE_SCRIPT=<jsonl>`).

`qa/harness/fake-claude` and `qa/harness/fake-codex` are executable Python 3 scripts with no
third-party deps. **SDK: if you have not started yours yet, take these** — they are done and
verified against the real binary. If you have, tell me and I'll delete mine and write tests
against yours instead. Either way we should not ship two. Routed via PM.

## 1. Why

Integration/E2E tests must never make a real Anthropic/OpenAI call: they'd be non-deterministic,
slow, cost money, and fail in CI without credentials. The fake speaks the *exact same*
stream-JSON protocol as the real `claude` binary, so **the engine cannot tell the difference** —
which is the point: we test the engine's real parsing/supervision code path, not a mock of it.

`fake-claude`'s output shape was verified byte-for-byte against real `claude` 2.1.261
(`--print --output-format stream-json --verbose`) on 2026-09-05.

## 2. What SDK needs to do

Add a **test variant** of the image — same engine binary, different harness, gated on a build arg:

```dockerfile
ARG FAKE_HARNESS=0
COPY qa/harness/fake-claude qa/harness/fake-codex /opt/wheel-fake/
RUN if [ "$FAKE_HARNESS" = "1" ]; then \
      install -m 0755 /opt/wheel-fake/fake-claude /usr/local/bin/claude && \
      install -m 0755 /opt/wheel-fake/fake-codex  /usr/local/bin/codex  ; \
    fi && rm -rf /opt/wheel-fake
```

Built as **`wheel-engine:test`** with `--build-arg FAKE_HARNESS=1`; `make engine-image-test`
(SDK's target) should produce it. With the default `FAKE_HARNESS=0` the production image is
byte-identical to today's and the fakes are **absent from the image entirely**.
Requirement: `python3` must be on PATH in the image — the architecture doc already lists python
in the container, so this should be free. If it isn't, tell me and I'll rewrite the fakes in
POSIX sh + a tiny JSON emitter.

Everything else — engine binary, entrypoint, user, paths — stays byte-identical to production.
**The fake must not be reachable in the production image.**

## 3. Invocation contract (what the engine may pass)

`fake-claude` accepts and honours:

| Flag | Behaviour |
|---|---|
| `-p`, `--print` | accepted |
| `--input-format text\|stream-json` | `stream-json` = read one JSON user turn per stdin line, loop until EOF |
| `--output-format text\|json\|stream-json` | `stream-json` = emit the event stream below |
| `--verbose` | accepted |
| `--model <m>` | echoed back in `system/init` and every `assistant` message |
| `--system-prompt <s>` / `--system-prompt-file <f>` | part of the composed prompt echoed in event 1 |
| `--append-system-prompt <s>` / `--append-system-prompt-file <f>` | appended to the composed prompt echoed in event 1 |
| `--resume <session-id>` | reuses that `session_id` instead of a fresh one |
| `--permission-mode <m>` | echoed in `system/init` |
| `--mcp-config <json-or-path>` | server names echoed in `system/init.mcp_servers` |
| `--version` | prints `2.1.261 (Claude Code)`, exit 0 |
| anything else | **tolerated and ignored** (with its value, if any) |

Unknown flags are deliberately non-fatal: the real CLI has ~80 of them and I don't want the fake
to break every time SDK adds one.

`fake-codex` mirrors `codex exec --json`.

## 4. Output contract (`--output-format stream-json`)

One JSON object per line, flushed immediately. Exactly the real event sequence:

1. `{"type":"system","subtype":"init", "session_id", "model", "cwd", "tools", "mcp_servers",
    "permissionMode", "claude_code_version", "system_prompt", ...}` — once, at startup.
    **`system_prompt` carries the full composed system prompt verbatim** (`--system-prompt` +
    `--append-system-prompt`, in that order). This is the primary hook for asserting that
    ctx-node injection and the agent's configured `system_prompt` actually reached the child.
    (`_fake_system_prompt` is kept as an alias for now; SDK, say if you'd rather I drop it.)
2. Per turn: `{"type":"assistant","message":{...Anthropic message...},"session_id","uuid","timestamp","request_id"}`
3. Per turn: `{"type":"result","subtype":"success"|"error_during_execution","is_error",
    "result":"<assistant text>","session_id","num_turns","duration_ms","total_cost_usd","usage",...}`

**The `result` event is the turn-complete signal** the engine keys `idle` / ephemeral-context-clear off.

### The envelope (SDK-confirmed, 2026-09-05)

The engine writes **exactly one line** of compact JSON to stdin per turn, newline-terminated,
always the content-block form:

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<ENVELOPE>"}]}}
```

where `<ENVELOPE>` is the JSON-escaped string:

```
<AgentPrompt id="<message uuid>" from="<from name>" type="<from type>">
<body>
</AgentPrompt>
```

(literal newline after the open tag and before the close tag; `id` is the message uuid — the same
id as the `messages` row and the `message` WS event). A literal `</AgentPrompt>` occurring inside
`<body>` is escaped by the engine — see `MSG-envelope-escape` in the TESTPLAN, which is the case
most likely to be got wrong, since it is the one an agent can trigger by simply *talking about*
the envelope format.

Spawn argv is likewise SDK-confirmed: `--print --input-format stream-json --output-format
stream-json --verbose --append-system-prompt <composed> [--model M] [--mcp-config P]
[--resume S] --permission-mode bypassPermissions`. The fake accepts all of it.

### The echo property (this is what makes injection testable)

Default reply text is:

```
[fake-claude] turn <N>: <the exact user text received>
```

So when QA asserts that ctx injection worked, we assert the agent's *log line* contains the ctx
markdown — proving the text really reached the child's prompt. Same for the `<AgentPrompt …>`
envelope above. If the engine mangles, drops, double-encodes or reorders anything on the way to
stdin, the test fails and points straight at the bug.

## 5. Test-side steering — `<<FAKE:...>>` directives

QA drives failure modes by embedding directives **in the message body**, so we never rebuild the
image or restart the container to test an error path. The fake strips them from its reply.

| Directive | Effect | Tests |
|---|---|---|
| `<<FAKE:REPLY=text>>` | reply exactly `text` instead of the echo | deterministic assertions |
| `<<FAKE:ERROR=msg>>` | `result.is_error=true`, `subtype=error_during_execution` | engine → agent `status=error` |
| `<<FAKE:EXIT=N>>` | exit with code N mid-stream | supervisor restart / `status=error` |
| `<<FAKE:CRASH>>` | SIGKILL self | supervisor survives a hard child death |
| `<<FAKE:SLEEP=S>>` | sleep S seconds before replying | timeouts, `status=running` vs `idle`, concurrent delivery |
| `<<FAKE:STDERR=msg>>` | write msg to stderr | engine captures stderr into the log |
| `<<FAKE:GARBAGE>>` | emit one **non-JSON** line on stdout | **engine must not crash or drop the stream** |
| `<<FAKE:NOISE>>` | emit `rate_limit_event` + `system/thinking_tokens` | **engine must ignore unknown event types** |
| `<<FAKE:TOOL=cmd>>` | emit a `tool_use` block + `tool_result` turn | engine renders tool calls in the log |

`GARBAGE` and `NOISE` are the two I care about most: the real CLI emits event types not in
`PROTOCOL.md` (I saw `rate_limit_event` and `system/thinking_tokens` from the real binary today),
and an engine that pattern-matches exhaustively on event type will fall over in production.

## 5b. Canned turns — `WHEEL_FAKE_SCRIPT=<file.jsonl>`

For scripted multi-turn scenarios. One JSON object per line = one turn, in order:

```jsonl
{"reply":"first answer"}
{"reply":"slow one","sleep":2}
{"is_error":true,"error":"rate limited"}
{"events":[{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}],"reply":"after noise"}
```

Keys: `reply`, `is_error`, `error`, `sleep`, `exit`, `stderr`, `events` (raw events emitted
verbatim before the assistant message). Directives still apply for anything the line omits.

**Running past the end of the script is a hard failure** (exit 3, message on stderr) rather than a
silent fallthrough — a test that sends more turns than it scripted is a broken test, and should
say so instead of quietly passing.

## 6. Env vars

| Var | Effect |
|---|---|
| `WHEEL_FAKE_SCRIPT` | JSONL of canned turns (§5b) |
| `WHEEL_FAKE_SESSION_ID` | pin the session id (deterministic assertions) |
| `WHEEL_FAKE_AUTH=needs_auth` | exit 1 with `Invalid API key · Please run /login` on stderr — drives the `needs_auth` state |
| `WHEEL_FAKE_STRICT_AUTH=1` | require `ANTHROPIC_API_KEY`/`CLAUDE_CODE_OAUTH_TOKEN` to be set, else `needs_auth` |
| `WHEEL_FAKE_TRANSCRIPT=/path` | append every raw stdin line to a file — QA reads it to assert **exactly** what the engine wrote to the child |

`WHEEL_FAKE_TRANSCRIPT` is the other half of injection testing: it captures the engine's stdin
framing verbatim, independent of what the fake chooses to reply.

## 7. Ephemeral context

The fake holds no cross-process state: a new process = a new `session_id` (unless `--resume` or
`WHEEL_FAKE_SESSION_ID`). So `ephemeral_context: true` is asserted as *"`session_id` in the log
changed after the turn completed, and the re-applied system prompt + ctx injection reappeared"*.

## 8. Asks for SDK

1. Build **`wheel-engine:test`** with the two files above, and a `make engine-image-test` target.
2. Confirm `python3` is on PATH in the image.
3. **Tell me the exact argv the engine uses to spawn `claude`**, and the exact bytes it writes to
   stdin per turn. I've guessed `--print --input-format stream-json --output-format stream-json
   --verbose`; if you diverge, the fake needs to know — it's cheap for me to adjust.
4. Confirm the engine treats `result` as turn-complete, and tolerates unknown event types
   (`<<FAKE:NOISE>>`) and non-JSON lines (`<<FAKE:GARBAGE>>`) without dying.
5. `fake-codex` is **provisional** (see Q-HARNESS-CODEX) — the real `codex exec --json` event
   names are unverified. Not on the M1 path; let's pin it together before M2.
6. **Ownership:** the brief says SDK is landing a fake harness. If that work hasn't started, take
   this one. If it has, I'll drop mine and test against yours — but please keep the four
   properties I actually need: composed system prompt in event 1, `WHEEL_FAKE_SCRIPT` canned
   turns, a way to force error/exit/slow turns, and a raw capture of what the engine wrote to
   stdin. Without that last one I can only test what the engine *believes* it sent.
