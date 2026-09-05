# §0b adversarial review — docs/plans/sdk.md (M0)

Reviewer: ADVERSARY. Verdict: **strong, security-first plan; approved to proceed with 2 new findings
and 3 must-verify items.** None block M1 start; all must be settled before the relevant M1/M2 code merges.

## New findings (own files)
- **007 (High)** intra-project same-uid defeats per-node token/creds isolation → any injected agent
  assumes every sibling node's wires; §3e attenuation not runtime-enforced within a project. Needs a PM
  residual-risk ruling (accept+document, or per-node uid).
- **008 (Medium)** child stdout is attacker-controlled (same-uid fd injection) → forgeable `result`/
  `usage`/`session_id`; enforce budget & session server-side, don't trust child-reported usage.

## Must-verify before the code lands (High if missed)
1. **User-SQL authorizer must be PER-CALL table-scoped**, not just globally read-only. One read-only
   connection still sees every `t_*` and `sqlite_master`; the authorizer callback must reject any table
   other than the exact `t_<node>` for THIS call, reject `sqlite_master`, ATTACH, PRAGMA, `load_extension`,
   `readfile`/`writefile`, and non-SELECT. Else `wheel query mytable "SELECT * FROM t_other"` reads a
   node I have no wire to (cross-node data leak, bypassing the matrix).
2. **argv carries no prompt/secret/paste-code/message bytes** (PM 003 ruling — /proc/<pid>/cmdline is
   world/cross-uid readable). `Harness::argv` must pass system prompt + preamble + ctx via file, the auth
   paste-code and message bodies via stdin — never as argv. Confirm the claude adapter uses a file, not
   `--append-system-prompt <text>`.
3. **Auth paste-code path must go through the single-writer actor** (§3c#12). The plan says auth "writes
   to the pending child's stdin" — that must be the SAME actor that owns stdin, in a pre-delivery
   lifecycle state, not a second writer racing a queued message.

## Endorsements (keep as-is)
- Open Q1 `script → tool`: **CORRECTION** — the contract §3 matrix already lists `script → tool` as
  `read` ("same as agent"), and QA BUG-004 files SDK's deny as a divergence (26 allowed triples, not 24).
  My least-privilege preference for deny is a *PROPOSAL for PM*, not grounds to diverge from the contract:
  implement the contract (allow) unless PM changes it. (Retracts my earlier "keep DENIED".)
- Open Q2 two method enums (`ToolMethod` vs `HttpMethod`): **keep separate** — sharing widens the
  endpoint GET/POST/PUT/DELETE contract to PATCH/HEAD/OPTIONS. Also validate endpoint config rejects
  non-{GET,POST,PUT,DELETE}.
- Token rotation every start: good — confirm the OLD token is **invalidated** at rotation (fail-closed
  for in-flight children), not merely superseded.
- Single caps choke point, read-only SQL conn, droppable WS subscribers, permissive `Unknown` parser:
  all correct.

## Cross-refs
001 (envelope, now incl. escaping the OPENING tag + preamble authority note), 002 (untrusted RCE),
003 (uid/egress), 004 (tool SSRF resolve-and-pin), 005 (shared MCP+CLI authz fn).
