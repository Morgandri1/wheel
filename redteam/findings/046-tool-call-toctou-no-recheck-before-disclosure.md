# 046 — `tool_call` is a SECOND lock-releasing handler and lacks `query`'s re-check: a revoked tool wire still completes and discloses a call

- **Severity:** Low (capability-boundary TOCTOU; narrow window — a revoke must land during the specific call —
  and tool nodes are M2). Raised because it is on the capability boundary, which PM named as the class to be
  least wrong about, and because it is a side-effecting path. Owner: SDK/Engine. Boundary TB4 (agent → tool).
- **Status:** CONFIRMED by source (origin/main). Found by probing SDK's own TOCTOU description — SDK invited it
  ("if it doesn't match, I want to know"). It does not match: SDK's "every cli handler EXCEPT query holds the
  lock across check AND action" omits `tool_call`, which releases the lock like `query` but does NOT re-check.

## The invariant SDK stated, and where it breaks
SDK: every cli handler except `query` holds the single-writer lock across the capability check AND the action,
so a concurrent `DELETE /v1/wires` cannot interpose; `query` is the deliberate exception, and its window is
closed by **re-checking the wire before the rows are DISCLOSED** (cli_routes.rs — a second `me.require(..Read)`
after `tables::query`, before `Ok(Json({rows}))`). Verified: `query`'s re-check is present and correct.

**`tool_call` (cli_routes.rs:650) is a second exception SDK's description omits.** It scopes its lock in a
block (657) and RELEASES it at 673 — necessarily, because the action is a ≤30s external HTTP request that must
not stall delivery, the exact reason `query` releases. Then:
```
crate::api::tool_routes::run_operation(&s, &node, &cfg, &body.op, &body.args, body.curl)
    .await
    .map(Json)          // response disclosed directly — NO re-check
```
`run_operation` (tool_routes.rs:350) takes `s, node, cfg, op_id, args, dry_run` — **not** the caller identity —
so it cannot re-check the agent's wire (its internal `WireType::Read` at :437 is the tool→vault FILL check, a
different edge). So the agent's `read` wire to the tool node is checked ONCE (658, before the lock release) and
never again before the result is handed back.

## Impact
An operator revokes an agent's `read` wire to a tool node to stop it using that tool (e.g. a tool that calls a
sensitive API with a vault-resolved credential, §3d). If the agent has a `wheel tool call` in flight when the
revoke lands, the check at 658 already passed, `run_operation` performs the external HTTP request (with the
tool's vault secrets) and the response is DISCLOSED to the now-deauthorized agent. Two dimensions:
- **Disclosure (avoidable):** unlike `query`, the result is returned even though the capability no longer holds
  at disclosure time. This is the part `query` closes and `tool_call` does not.
- **Side-effect (inherent, narrower):** the external request has already gone out and cannot be un-sent, so for
  a side-effecting tool the revoke cannot prevent the in-flight action — only its disclosure. Worth stating so
  "revoke" is understood as "stops the result coming back," not "guarantees the action never happened."
Narrow window (revoke concurrent with a call) and M2, hence Low — but it is the capability boundary, and it is
side-effecting, which is why it is worth closing rather than accepting.

## Fix (SDK) — give `tool_call` parity with `query`
After `run_operation` returns and before disclosing, re-acquire the writer lock and re-run
`me.require(&conn, &body.node, WireType::Read)`; on failure, deny and WITHHOLD the response (exactly as `query`
does). That closes the disclosure window. Document the residual (the external call may already have fired) in
the same place, so the guarantee is precise: revoke withholds the result and prevents future calls, but does
not cancel one already in flight. Add a test mirroring `query`'s: a wire revoked between check and disclosure
yields a denial, not the tool's response.

## Note
`query`'s re-check is the RIGHT pattern and it is correct; this finding is only that `tool_call` — the other
lock-releasing handler — did not get it. `wheel run <script>` (the third potential slow action) is not yet a
wired cli handler on origin/main, so it is not in scope today; when it lands it needs the same re-check, being
the same shape (slow action → lock released → result disclosed).
