# Fake harness spec (QA → SDK)

**Status:** implemented and working. `qa/harness/fake-claude` and `qa/harness/fake-codex` are
executable Python 3 scripts with no third-party deps. **SDK does not need to write anything** —
just bake them into a test image variant. This document is the contract between us.

## 1. Why

Integration/E2E tests must never make a real Anthropic/OpenAI call: they'd be non-deterministic,
slow, cost money, and fail in CI without credentials. The fake speaks the *exact same*
stream-JSON protocol as the real `claude` binary, so **the engine cannot tell the difference** —
which is the point: we test the engine's real parsing/supervision code path, not a mock of it.

`fake-claude`'s output shape was verified byte-for-byte against real `claude` 2.1.261
(`--print --output-format stream-json --verbose`) on 2026-09-05.

## 2. What SDK needs to do

Add a **test variant** of the image — same engine binary, different harness:

```dockerfile
# docker/Dockerfile.test  (or: ARG HARNESS=real in the main Dockerfile)
FROM wheel-engine:latest
COPY qa/harness/fake-claude /usr/local/bin/claude
COPY qa/harness/fake-codex  /usr/local/bin/codex
RUN chmod +x /usr/local/bin/claude /usr/local/bin/codex
```

Built as **`wheel-engine:test`**. `make engine-image-test` (SDK's target) should produce it.
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
| `--append-system-prompt <s>` / `--append-system-prompt-file <f>` | echoed in `system/init` as `_fake_system_prompt` |
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
    "permissionMode", "claude_code_version", "_fake_system_prompt", ...}` — once, at startup.
2. Per turn: `{"type":"assistant","message":{...Anthropic message...},"session_id","uuid","timestamp","request_id"}`
3. Per turn: `{"type":"result","subtype":"success"|"error_during_execution","is_error",
    "result":"<assistant text>","session_id","num_turns","duration_ms","total_cost_usd","usage",...}`

**The `result` event is the turn-complete signal** the engine keys `idle` / ephemeral-context-clear off.

### The echo property (this is what makes injection testable)

Default reply text is:

```
[fake-claude] turn <N>: <the exact user text received>
```

So when QA asserts that ctx injection worked, we assert the agent's *log line* contains the ctx
markdown — proving the text really reached the child's prompt. Same for the
`[wheel] message from <from_name> (<from_type>):` framing from §3 of the architecture doc.
If the engine mangles, drops, double-encodes or reorders anything on the way to stdin, the test
fails and points straight at the bug.

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

## 6. Env vars

| Var | Effect |
|---|---|
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
