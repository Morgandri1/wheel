# 062 — Guest tier can read the full rendered transcript: system prompt, ctx injections, every operator instruction

- **Severity:** Medium-High. Not a new vulnerability class — it's the existing `Tier::Guest`
  grant on `GET /v1/agents/{}/log` (`policy.rs`), which correctly and deliberately includes
  `LogStream::Transcript` (§3c's own reasoning: "a separate subscription would double the
  reconnect and cursor logic for no gain"). The gap is that nobody has weighed what "view only"
  actually hands a Guest once the multiplayer tier model's own STATED philosophy is applied to
  transcript content specifically, the same way it already was for vault key names (finding,
  closed) and member emails (#118, closed). Discovered as a direct corollary while reviewing #129
  (on_behalf_of masking) — that PR correctly scopes ITSELF to a narrower field and explicitly
  flags this as "a separate question about policy.rs's tier table," per its own doc comment.
  Filing that separate question now rather than letting a correct scoping decision quietly stand
  in for "already handled."
- **Owner:** Whoever owns the tier model's threat surface — API (`policy.rs`'s own table) and/or
  SDK (what the engine actually puts in a transcript line).
- **Status:** OPEN.

## The gap, precisely
`policy.rs`'s "guest: view only" block includes `GET /v1/agents/{}/log` at `Tier::Guest` — correct
for STDOUT (what the agent said) and ENGINE lines (spawn/exit/session-clear), which are the
"view only" content a Guest's role is meant to describe. But the SAME route, on the SAME stream,
also carries `LogStream::Transcript` — "the exact bytes the engine wrote to the child's stdin"
(`event.rs`) — which is not "what happened," it is "everything the agent was ever told," per §3's
own preamble-composition spec:
- the node's own `system_prompt` (operator-authored, potentially containing internal workflow
  detail, credential *locations* even if not credential values, references to other systems);
- every `ctx→agent (send)` injected block — full markdown from every wired ctx node, which per §3
  is exactly the surface `finding 043`'s whole amplification chain worries about (a ctx node
  agents can write to, injected wholesale into a prompt);
- every operator/agent/endpoint/script message the agent has ever been handed, rendered inside
  `<AgentPrompt>` envelopes, including whatever those messages actually said.

A Guest — the tier `policy.rs`'s own comments repeatedly describe as deliberately excluded from
credential-adjacent and structural surfaces (vault: admin-only; tool `call`: admin-only, "credential
*use*... the vault boundary wearing a different hat"; auth begin/complete: admin-only) — can read
ALL of this in full, unmasked, today, for any agent on any project they hold guest access to. The
`on_behalf_of` masking #129 adds is a genuinely narrow field inside a stream whose CONTENTS are
already far more exposed than that one field.

## Why this wasn't caught as part of #129 or #118
Both of those PRs correctly scoped themselves to ONE field's identity-disclosure risk
(`on_behalf_of`, `Member.email`) on surfaces where masking a field is the right fix. Transcript
content is not one field to mask — it's the reasoning about WHETHER Guest should reach the stream
at all that's missing, which is a `policy.rs` table decision, not a masking-function decision. Two
correct, narrow fixes in a row can read like "the tier-exposure question is being handled" without
anyone having actually made this particular call.

## What would change my mind
If Guest-tier access was deliberately extended to transcript content with this exact tradeoff
already weighed (e.g., a design doc reasoning that a Guest is trusted with prompt content but not
credentials, and that's an intentional, considered line) — this becomes a documentation gap, not a
finding: state the reasoning in `policy.rs` or `PROTOCOL.md` next to the rule, the same way the CLI
realm's admin-including refusal already explains itself in-place. I did not find any such reasoning
anywhere I looked (policy.rs's comments, PROTOCOL.md, the contract) — the "guest: view only" block's
existing comments only ever reason about board/log/table/tool-ops read access in the ordinary sense,
never about transcript content specifically.

## Recommendation (not decided here, per #129's own restraint)
Options, roughly in order of how much they change:
- Split `LogStream::Transcript` onto its own tier requirement, separate from `Stdout`/`Engine` on
  the same route — the route already carries a `stream` field per line, so this is a policy-table
  and route-handler filtering change, not a new endpoint.
- Or explicitly rule that Guest transcript access is intended, and document why, closing this as a
  documentation fix.
- Or (bigger, but closes the class rather than one field): move the whole "which fields need
  masking" question upstream of the mask-per-field pattern #118/#129 both use, since a growing list
  of individually-masked fields is a sign the actual boundary should be "what may Guest see of an
  agent's own inputs," not "which of an agent's input fields happen to look like an email so far."
