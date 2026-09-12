# 053 — A third-party MCP node's tool results reach the model with no wrapping, no escaping, and no SSRF-class gate at all

- **Severity:** Medium (design-level; scoped, but the least-defended of the indirect-prompt-injection
  surfaces once defect #2 closes the others)
- **Type:** DESIGN gap, surfaced reviewing `docs/proposals/tool-mcp-output-escaping.md` (defect #2, §5:
  "explicitly out of scope").
- **Owner:** SDK/Engine
- **Status:** OPEN
- **Boundary:** TB5 (child ↔ MCP), same boundary as 005; this finding is about output TRUST, not RCE/SSRF
  on the node's own config, which 005 already covers.

## Claim
§3's wire matrix: `agent → mcp (read)`: "MCP server is attached to the agent's harness config at next
start." This is a **direct** connection — the harness (claude/codex) talks to that MCP server's process
or endpoint itself. Wheel does not sit between them, does not proxy the `tools/call` response, and
therefore cannot apply anything defect #2 builds: not the per-source escaping proposed for ctx/table,
not the non-mutating wrapper proposed for tool-node HTTP results and script output, not even 005's SSRF
allowlist (005 covers the `command`/`url` used to **start** the server; a **remote** `http`-transport MCP
server can itself proxy or fetch anything, same as an unconstrained tool node would, with no §3d gate at
all once attached).

So of every content source that reaches a model as "returned data" (the table in defect #2's proposal
§2), the attached MCP node is the only one with:
- no escaping/wrapping treatment, even after #2 ships (explicitly out of scope there, correctly — fixing
  it is a materially bigger change, per that proposal's §5), and
- no SSRF-class egress control on the *results* it returns (only on how the server itself was reached at
  attach time, and only for the `command`/`url` fields 005 already flags).

This is a strictly worse position than a `tool` node calling the same external endpoint: a `tool` node's
call is engine-mediated (§3d), goes through the SSRF policy, and — once #2 lands — has its result
wrapped. An MCP node pointed at the identical external service gets none of that, for no reason a board
author would find obvious from the config surface (both look like "a node that lets my agent call an
API").

## Impact
An operator who attaches an MCP node backed by, or proxying to, anything that returns attacker-influenced
content (a web-search MCP server, a ticket-tracker integration, anything reading third-party data) hands
the model raw, unmediated text with the full forged-`<AgentPrompt>` primitive finding 001 established as
the highest-leverage injection shape in this system — and, unlike the tool-node case, no amount of
implementing defect #2 closes it, because Wheel structurally cannot see the bytes.

## Why this is scoped Medium, not High
- It requires the operator (or a `may_place` agent, §3e) to have attached an MCP node backed by
  untrusted/external content in the first place — not a wire any agent can create for itself against an
  arbitrary target (mcp node creation is operator/API-only per 005's required invariants, assuming those
  landed — worth re-confirming as part of this finding's follow-up, not re-litigating 005 here).
- The harness (claude/codex) has its own baseline handling of tool-result content as "data," same as any
  MCP client — Wheel adding a wrapper is defense in depth, not the only thing standing between the model
  and a forged tag, unlike the stdin envelope where Wheel's escaping *is* the whole defense.

## Recommendation (not a full design — flagging for its own proposal, per #2's author's invitation)
1. Document the gap explicitly and plainly where an operator decides to attach an MCP node: the board UI
   / node-creation flow should say, in words, "results from this server reach the agent's model directly,
   unmediated — treat it like giving the agent a browser to an untrusted site," so the asymmetry with
   `tool` nodes (which get SSRF + wrapping) is a decision, not a surprise.
2. Actually closing it is the bigger lift #2's proposal correctly declines to fold in: proxying MCP
   `tools/call` responses through the engine the way tool-node calls already are. That's worth its own
   proposal (SDK), scoped against: does `stdio` transport (a local child process) even make sense to
   proxy the same way `http` transport would, or does this end up being two different fixes for the two
   transports in §3's `mcp` config.
3. Interim, cheaper mitigation worth considering in that follow-up proposal rather than here: `http`
   transport MCP servers could get the *SSRF* allowlist treatment (§3d(4), extended per 005's already-open
   ask) even before result-wrapping exists — closes half the gap (egress) without the bigger proxy
   rewrite.

## What would change my mind
If MCP node creation already requires the same `tool_allow_hosts`-style operator allowlist 005 asked for
*and* that allowlist is proven to also gate `http`-transport MCP `url`s (not just `command`), the SSRF
half of this is already closed and this finding narrows to "results still unwrapped" only — worth
re-checking against current code before this finding's fix is scoped, since 005 was a design review, not
confirmed-implemented.
