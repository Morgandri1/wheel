# 061 — MCP tool dispatch skips the tier gate entirely: a Guest-tier member can start/stop/send to any agent

- **Severity:** Critical. Live, confirmed (source-verified end to end, not theoretical): a
  Guest-tier member of a shared project — a role whose own policy table names it "view only" —
  can use the MCP interface to `send`/`ask` (inject an arbitrary message into any agent, i.e.
  prompt injection with a legitimate credential) and `start`/`stop` (agent lifecycle control, a
  DoS vector against every other member's work) on any project they hold even guest access to.
  Discovered by API while auditing member-identifier surfaces for a membership-masking task;
  independently source-verified by ADVERSARY end to end (see "Verification" below) before this
  finding was filed. Reported by API directly to PM/ADVERSARY before touching any code, on their
  own initiative, specifically to avoid silently fixing a live authz hole as an undocumented side
  effect of an unrelated feature — that discipline is worth preserving through the fix.
- **Owner:** API (`crates/wheel-api/src/routes/mcp.rs`).
- **Status:** OPEN, reported, fix not yet started pending this triage.

## The bug, precisely
`mcp.rs::scope()` is the ONE authorization primitive every MCP tool handler calls:

```rust
async fn scope(state: &AppState, user: &AuthUser, args: &Value) -> Result<Uuid, ApiError> {
    let raw = string(args, "project")?;
    let id = Uuid::parse_str(raw.trim())...?;
    let scope = ProjectScope::for_target(state, user, id).await?;
    Ok(scope.project.id)
}
```

`ProjectScope::for_target` DOES correctly resolve the caller's tier (`load_member` returns
`(Project, Tier)`, same call every other membership check in the codebase uses). But `scope()`
discards `scope.tier` and returns only the bare project id — never calling
`scope.require(needed)`, the method `extractor.rs`'s own doc comment says "every project-scoped
handler calls." Every MCP tool handler (`"send" | "ask"`, `"start" | "stop"`, line 191/224 of
`mcp.rs`) then passes that bare id straight into `engine()`, which makes the actual engine call
using **the API's own internal `host_secret`** — the same bearer `routes/proxy.rs` uses to reach
the engine on the AUTHENTICATED PROXY's behalf, after `auth::policy`'s tier table has already
approved the request. MCP's path never goes through `routes/proxy.rs` or `auth::policy` at all;
it is a second, parallel way to reach the engine that happens to skip the one table that decides
who may do what.

## Verified independently (source, not relayed)
- `mcp.rs::scope()` (origin/dev, `crates/wheel-api/src/routes/mcp.rs`): confirmed — discards
  `.tier`, no `.require()` call anywhere in the function or at any call site.
- `auth::policy::RULES` (`crates/wheel-api/src/auth/policy.rs`): confirmed the paths MCP's
  `send`/`ask`/`start`/`stop` reach require `Tier::Prompter` (`v1/agents/{}/send`, `/start`,
  `/stop`, `/restart` all explicitly `Tier::Prompter`). `board` (`v1/board` GET) and `logs`
  (`v1/agents/{}/log` GET) correctly require only `Tier::Guest` — so MCP's `board`/`logs`/
  `projects` tools are NOT part of this bug; the exposure is specifically `send`/`ask`/`start`/
  `stop`.
- `agent_routes.rs` (engine side): confirmed `tier_from_headers`/`ActorTier` is used in exactly
  two places, both DATA REDACTION (`auth_status`'s `>= Admin` check at line ~1110,
  `board_routes.rs`'s `< Admin` redaction at line ~30) — never to refuse an action. The engine
  has no independent tier enforcement anywhere in `start`/`stop`/`send`/`restart`; it trusts the
  API proxy to have gated the request before it arrives. MCP's direct `engine()` call bypasses
  the only place that trust was supposed to be earned.
- MCP's own tool list (`mcp.rs::tools()`): confirmed bounded to `projects`, `board`, `send`,
  `ask`, `start`, `stop`, `logs` — nothing Admin-tier (no vault, no `tools/call`, no board-structure
  writes) is reachable through MCP at all, so the blast radius is Guest→Prompter, not Guest→Admin.
  Still Critical: Prompter-tier already grants message injection into a `bypassPermissions` agent
  (§2's whole threat model) and full agent lifecycle control.

## Impact
A Guest-tier member — a role the codebase's own policy.rs comments describe as "view only,"
presumably handed to a collaborator the operator specifically wants restricted — can, through MCP
alone, with no other capability:
1. **Inject an arbitrary message into any agent on the project** (`send`/`ask`), i.e. prompt an
   untrusted-by-design, `bypassPermissions` agent with content the operator never authorized that
   member to send — the same category of concern findings 031/035/043 raise for other unwarned
   agent-input channels, except this one requires no prompt-injection trick at all, just a
   legitimate (if low-trust) credential.
2. **Start or stop any agent** (`start`/`stop`) — a denial-of-service against every other member's
   work on a shared board, or an availability lever to hide activity (stop an agent mid-task before
   a higher-tier member notices).

## What would change my mind
If `Tier::Guest` were ever redefined to include prompting/lifecycle control (i.e. if `send`/
`start`/`stop` were reclassified as Guest-tier in `policy.rs` itself), this would not be a bug — it
would be the policy. `policy.rs`'s own doc comments are explicit that Guest is "view only" and
`send`/`start`/`stop`/`restart` are Prompter, so I found no basis for that reading; the mismatch is
between MCP's implementation and the policy table already written, not an ambiguous policy.

## Recommendation
Follow the same idiom `extractor.rs` already established for exactly this "can't forget" failure
mode (`AdminScope` as a type, so a handler's signature — not its body — declares the tier it
needs). Concretely: either (a) have `scope()` take a `needed: Tier` parameter and call
`.require(needed)` before returning, so every call site is forced to state what it needs, or (b)
change `scope()`'s return type to the full `ProjectScope` and require every call site to invoke
`.require()` explicitly — (a) is harder to forget at the next tool added, since a call site that
doesn't know its own required tier won't compile. `board`/`logs`/`projects` pass `Tier::Guest`
(no behavior change); `send`/`ask`/`start`/`stop` pass `Tier::Prompter`. Add a `tests/tiers.rs`-style
matrix test driving every MCP tool as Guest and asserting a 403 on the Prompter-gated ones,
mirroring the discipline that file already holds the REST routes to — this bug is exactly the kind
of gap that discipline exists to catch, and it did not exist for MCP's parallel path.

## Process note
Filed as its own finding rather than folded into the masking/info-disclosure task API was
originally assigned, per their own instinct on discovery — this is a different, more severe class
of bug (authorization bypass vs. information disclosure) and deserves its own tracked history, not
a side-effect fix buried in an unrelated PR's diff.
