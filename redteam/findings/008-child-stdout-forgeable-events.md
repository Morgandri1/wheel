# 008 — Child stdout is attacker-controlled → forgeable harness events

- **Severity:** Medium
- **Owner:** SDK/Engine
- **Status:** OPEN — surfaced in the sdk.md plan review (pre-code).
- **Boundary:** TB5 (child ↔ supervisor stdout parser).

## Claim
The supervisor parses the child's stdout stream-JSON for control events (turn `result`, usage/budget,
`session_id`, `system/init`). Under bypassPermissions + same-uid (007), the agent can write bytes
directly into the CLI process's stdout fd (`/proc/<cli-pid>/fd/1`), injecting **forged** events the
supervisor trusts:
- Forge `result` → end a turn early / mark a message consumed without doing the work.
- Under-report `usage` → evade `budget.max_usd`/`max_turns` (cost-control bypass, §3e budgets).
- Forge `session_id` → poison `--resume` so a later resume attaches to an attacker-chosen session.

The plan's "permissive parse, log everything else verbatim" is correct for robustness but means the
supervisor cannot distinguish a genuine CLI event from an injected one on the same stream.

## Also (log injection, Low)
"Log verbatim" sends child-controlled bytes to the UI log sink: ANSI escapes / CR / fake event lines
can corrupt the operator's terminal or spoof UI log entries. Sanitize control/ANSI at the UI sink.

## Proposed action (to SDK)
- Treat budget accounting as **best-effort from stdout, hard-capped elsewhere**: enforce `max_turns` by
  the supervisor's own turn counter (not the child's usage event), and wall-clock/`max_usd` with a
  server-side estimate; never let a child's self-reported usage be the only budget gate.
- Bind `session_id` to what the harness returned at spawn/`system/init` and reject mid-stream changes.
- Sanitize/escape control chars before the log reaches the UI (cross-ref Web XSS/log-injection).
- Residual (same-uid fd injection) is bounded by 007's ruling; note it there.
PoC once engine exists: from the agent shell, write a forged `result`/`usage` line into the CLI fd and
observe the supervisor's turn/budget state.
