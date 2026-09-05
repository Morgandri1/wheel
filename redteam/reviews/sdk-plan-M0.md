# Plan review — docs/plans/sdk.md (ADVERSARY, §0b)

**Verdict: strong, security-first plan. No blocker to M1.** One High design finding (007), one Medium
(008), plus four must-verify items and two endorsements. Nothing here should stop M1 code; items are
"prove it in the impl / test".

## New findings this review surfaced
- **007 (High):** one uid per project ⇒ same-uid agents can steal each other's node tokens/creds via
  /proc → per-node wire scoping and §3e attenuation are not runtime-enforced within a project. Likely a
  documented-residual (PM ruling), not a code bug. See findings/007.
- **008 (Medium):** supervisor trusts child stdout for turn/budget/session; same-uid /proc/fd1 write
  lets the agent forge those events (budget evasion, session hijack, log injection). See findings/008.

## Must-verify in the implementation (High if missed)
1. **user-SQL authorizer must be PER-CALL table-scoped, not just read-only.** A single read-only
   connection still sees every `t_*` and `sqlite_master`. `wheel query t_mine "SELECT * FROM t_other"`
   must be denied by the authorizer for THIS call's allowed table only; also deny ATTACH, PRAGMA,
   load_extension, readfile/writefile, and sqlite_master. (Cross-node table read = wire bypass.)
2. **No prompt/secret/paste-code/message-body in argv** (PM's 003 ruling; /proc/pid/cmdline is
   cross-uid readable). `Harness::argv` must pass system prompt + preamble + ctx via a file in the
   node's 0700 dir; the claude adapter must not use `--append-system-prompt <text>`. Verify with a
   test asserting argv contains no secret/prompt bytes.
3. **Token rotation must INVALIDATE the old token at start**, not just mint a new one; and the built-in
   MCP server + any child script must receive the new token or fail closed. Confirm old token → 401.
4. **Auth paste_code path must not be a second stdin writer** (§3c#12). "write the code to the pending
   child's stdin" has to go through the single-writer actor or a pre-delivery lifecycle state, never a
   side channel that can race a queued message.

## Endorsements (keep as-is)
- **Open Q1 script→tool: keep DENIED.** Least privilege; opening it widens the SSRF/egress surface to
  the script sandbox. Don't open without a concrete need.
- **Open Q2 two method enums: keep separate.** Sharing HttpMethod would silently widen the endpoint
  contract to PATCH/HEAD/OPTIONS. Also: engine must reject endpoint methods outside GET/POST/PUT/DELETE.
- Single capability choke point (`caps/`), deny-as-events, one writer, spawn mutex, permissive parser
  with `Unknown` as a normal variant, droppable WS subscribers — all correct and directly answer §3c.

## Confirmed good
- PROTOCOL.md 001 escaping (`</AgentPrompt>`→`<\/AgentPrompt>`, case-insensitive on decoded bytes,
  engine-set attrs, inbox reads pre-escape original from sqlite) resists my full fixture set
  (redteam/pocs/envelope-forgery). No un-escape ambiguity since storage is pre-escape. 
