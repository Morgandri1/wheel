# 008 — Child stdout is attacker-controlled: forgeable harness events

- **Severity:** Medium (budget evasion, turn/state spoofing; same-uid vector). Intra-project.
- **Owner:** SDK/Engine (supervisor / harness parser).
- **Status:** OPEN — design review of docs/plans/sdk.md ("stdout parsing is permissive; log verbatim").
- **Boundary:** TB5.

## Claim
The supervisor derives control decisions (turn-complete/`result`, budget usage accounting,
`session_id` capture, state transitions) from parsing the child's stdout stream-JSON. Under
`bypassPermissions` + same-uid (finding 007), the agent can write bytes directly into the harness
CLI's stdout pipe via `/proc/<cli-pid>/fd/1`, forging events the supervisor trusts:
- forge a `result` to end a turn early or mask `is_error`;
- under-report token/usage to **evade the budget stop** (§3e budget);
- inject a false `session_id` to interfere with `--resume` (attach/hijack a session on next start);
- emit ANSI/newline/fake-event lines that, logged "verbatim", corrupt the operator UI (log injection).

Note: normal tool-output from the agent's bash tool is captured as tool-result, NOT injected into fd1;
the vector that matters is the direct `/proc/<pid>/fd/1` write, which same-uid allows.

## Required mitigations (each → a test)
1. **Sanitize the log sink:** strip/encode ANSI + C0 control chars before the UI; never let child
   text be rendered as a control event in the UI.
2. **Budget must not rely solely on child-reported usage** for the stop decision, or the residual
   (agent can under-report within its own project) must be explicitly accepted — an agent evading its
   own budget only harms its owner, but document it.
3. **Treat `session_id`/state from stdout as untrusted hints:** validate shape; resume only to a
   session id the engine itself recorded at spawn, not one first seen mid-stream.
4. Cross-ref 007: closing the same-uid /proc/fd vector needs the uid model decision.

## Ask
To SDK: confirm which supervisor decisions are stdout-derived; adopt (1)-(3). PoC (fd1 injection of a
forged `result`/usage) lands with the M1 stack. Low urgency vs 001/003/007 but cheap to harden now.
