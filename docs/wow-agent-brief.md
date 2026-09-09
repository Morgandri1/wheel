# Wheel-on-wheel agent brief

Tasks for the cloud-board agents to work on once the board is developing Wheel on its own. Each is a goal, not a
design — the agents own the design, the plan, and the adversarial review + QA that the contract requires. Nothing
here is started until the board is broadened; this is a staged backlog, PM-curated.

Working rules still apply (`docs/WHEEL-ON-WHEEL.md`): work in `$WHEEL_WORKSPACE`, one branch per task, PR with CI
green, PM merges, every implementation passes adversarial review + QA at ≥90% coverage.

## 1. Portals — inter-workflow communication

**Goal:** let one project's board communicate with another's. Today every wire is intra-project; a *portal* is the
node (or wire class) that carries a `send`/`read` across the project boundary, so a workflow can hand work to, or
read a result from, a different workflow.

A portal has an explicit **direction**: an **out** portal exposes something from this project to a peer, an **in**
portal receives from a peer. The two are distinct nodes/ends and pair up — an out on one side connects to an in on
the other. Direction is part of the access decision: an `in` portal is the only thing a peer may target, and it
governs what the peer may do once through it.

**Non-negotiable constraints (design around these, don't relax them):**
- A portal must not bypass the project-ownership auth boundary. Cross-project access is a grant between two
  projects, explicit and revocable — never ambient. This is exactly the cross-tenant surface ADVERSARY has flagged
  repeatedly; treat an unauthorised cross-project read/send as the primary thing to make impossible.
- The wire matrix still governs what a portal may do on each side (a portal into an agent is a `send`; into a
  table/ctx/chest is `read`/`write` per the existing rules). A portal does not invent new capabilities, it extends
  existing ones across a boundary.
- Poison-in-content still applies: anything crossing a portal that becomes a message body must pass the same
  envelope escaping (the 034/036 sink), or a hostile project could crash a peer.

**Likely owners:** SDK (engine: cross-project routing + access enforcement), API (the grant/handshake between
projects), Web (the portal node + wiring UI). PM curates the split so the halves don't overlap.

**Open design questions for the agents to answer in a proposal first:** is a portal a new node type or a wire
attribute? how is the peer project addressed (id vs a capability token)? is delivery at-least-once or
effectively-once, and does it survive a deploy-drain? what does the grant lifecycle look like (who creates, who
accepts, who revokes)?

## 2. Selectable harness authentication (wheeld env var)

**Goal:** a wheeld environment variable that selects *how* the harness (claude/codex) authenticates, so the
deployment can run on a ToS-compliant method instead of the current one. Today the harness is authenticated with a
personal OAuth token (`CLAUDE_CODE_OAUTH_TOKEN`, injected `mode:"env"`), which is technically a ToS break; the
operator wants a switch to an approved path (a real Anthropic API key / Console credential) without ripping out the
existing mode.

**Shape:** an env var (name the agents' to propose, e.g. `WHEEL_HARNESS_AUTH`) whose value chooses the auth method
per project/agent — at minimum `oauth-token` (today's behaviour, kept for the operator's own board) and an API-key
mode that passes an Anthropic API key to the harness the supported way. This env var is the *mechanism* task 4's
policy runs on. The default and the migration path both matter: existing boards must keep working, and the
compliant mode must be a one-line switch.

**Constraints:** the credential never lands on disk or in a URL (same discipline as GITHUB_TOKEN via askpass — see
finding 036); the vault stays the store; no secret in logs. See `docs/proposals/auth-model-tos-risk.md` for the
risk analysis this closes.

**Open design questions:** does the switch live per-project (vault key) or per-agent (node config)? how does an
API-key mode reach claude/codex — env var the CLI reads, or a config the harness expects? does codex need its own
mode? what happens on a board that has only the OAuth token set (graceful default vs hard error)?

**Likely owners:** SDK (engine: env plumbing + harness spawn), with API if the selector is surfaced through the
project API.

## 3. Board templates on the website

**Goal:** the website offers ready-made workflow *templates* a user can instantiate as their own wheel-hosted
project — pick a template, get a project pre-populated with its nodes and wires, then edit. Turns the empty grid
into a starting point.

**Source (operator's requirement):** templates are files dropped into `public/workflow_templates` — the operator
updates the catalogue by adding/editing files there, no CMS or DB. The site reads that directory and lists what it
finds. Keep it that simple: a new file appears in the gallery, a removed file disappears.

**Shape:** each file is a validated board spec (nodes + wires + default config, secrets left as vault placeholders
the user fills). Instantiating it = create a project, then create its nodes/wires through the same validated create
path the app already uses (matrix-checked at creation), never a raw import. This is the sibling of the
workflow-builder apply step (`docs/proposals/apply-step-constraints.md`) — reuse that validation, don't fork it. A
malformed file in the directory must fail loudly at load/validate, never instantiate a broken board.

**Constraints:** a template carries no live secrets, only vault key *names*; instantiation runs entirely through
the public project API under the user's own auth (no privileged shortcut); a template that references a capability
the target can't grant is rejected at instantiation, not silently dropped.

**Likely owners:** Web (template gallery + instantiate flow on the site), API (instantiate-template → validated
project/node/wire creation). SDK only if a template needs an engine primitive that doesn't exist yet.

## 4. Self-hosted wheeld as first-class; cloud is API-key-only

**Policy (operator direction):** self-hosted `wheeld` is now a first-class way to run Wheel, not a second-class
alternative to the cloud. And cloud boards authenticate the harness with an **API key only** — for everyone except
the operator, whose personal board keeps the OAuth-token mode. This is the positioning/default layer on top of
task 2's mechanism.

**What this means for the work:**
- `wheeld` (the one-binary self-hosted mode) gets the docs, defaults, and onboarding of a primary product path —
  someone running Wheel on their own machine is a supported first-class user, not an afterthought.
- On cloud, the default and the enforced mode is API-key auth (task 2's `oauth-token` mode becomes operator-only).
  New cloud boards must not be creatable on the OAuth path except for the operator's account.
- Nothing here changes the operator's own board (project 6906cadb): it stays on the OAuth token as the explicit
  exception. So this task does not block or alter the current wheel-on-wheel push.

**Likely owners:** SDK (wheeld first-class + enforce cloud API-key default), API (gate cloud-board creation to the
API-key path except the operator), Web (self-hosting onboarding on the site). Proposal-first — this is a
positioning decision with a security edge (the operator-exception must be a real allowlist, not a client flag).

## 5. Board-scoped helper agent (global read/write + workflow-building tools)

**Goal:** a special-purpose agent a user can drop on any board that already has blanket read/write across the
whole project and a toolset aimed at helping the user build and edit their workflow — no manual wiring per node.

**Shape:** this is not a normal `agent` node with hand-wired connections; it needs *every* node on the project,
including ones added after it's placed, auto-covered by read (and write where the type allows it — table/chest/ctx)
without the user drawing N wires. Its tool surface should cover the operations a human would otherwise do through
the UI: place/update/remove nodes, wire/grant, import a tool spec, inspect the board — i.e. it's an in-band driver
for the same validated creation/wiring path the API already enforces (§3e `place`/`grant`, `POST /v1/nodes`,
`POST /v1/wires`), not a new bypass.

**Constraints:** this inverts the default-deny wire matrix (§3, "anything not listed is rejected") for one agent
type, so the blast radius of that agent being turned hostile or misprompted is the whole board — the untrusted-RCE
principle (§2) says the containment has to hold anyway; a "global" grant must still be a real, visible set of
wires/capabilities the UI can show and the user can revoke, not an invisible bypass flag. Vault values stay
write-only and out of this agent's reach the same as any other agent unless explicitly wired.

**Open design questions for the agents to answer in a proposal first:** is "global read/write" a project-level flag
on the agent's config, or does the engine auto-generate real wires to every node (and keep them live as nodes are
added/removed)? does write-access extend to other agents (i.e. can it `start`/`stop`/`update` peer agents per the
agent→agent `write` = manage cell), or is it capped to data nodes? is this one board-provided node type (e.g.
`agent.kind: "helper"`) or a template (task 3) that happens to pre-wire everything? how is it kept from being the
single most dangerous node on every board it's placed on (confirmation on placement, a distinct badge in the UI,
rate limits on its own place/grant calls)?

**Likely owners:** SDK (engine: auto-wire semantics + the manage-capable tool surface), Web (placement flow,
"global access" badge/warning, revoke UI), API only if placement needs a new endpoint. Proposal-first given the
security edge.

## 6. Agent-visible token/usage awareness (avoid silent limit failures)

**Goal:** agents should be able to see their own token/usage consumption as they work, so they can slow down or
wrap up gracefully as they approach a limit instead of quietly failing (or burning budget) when they hit one.

**Shape:** the harness protocol already emits usage data on result/turn events (input/output tokens, and Anthropic
API responses carry rate-limit headers) — that data currently lands in engine logs but isn't surfaced back to the
agent itself. Give agents a way to check it: at minimum an MCP tool (alongside the §3c #1 built-in server, e.g.
`usage`) returning current turn/session token counts and, where known, how close the agent is to its configured
`budget` (§3 `budget: {max_turns?, max_usd?}`) or to the provider's own rate limit. Consider also a lightweight
proactive nudge — e.g. injected into the next turn's context once a threshold is crossed — rather than requiring
every agent to poll.

**Constraints:** this reads from data the engine already has (harness usage events, budget config) — no new
external calls per turn. Must not leak cross-project or cross-tenant usage; scoped strictly to the querying agent's
own session/project. Should degrade gracefully when the harness doesn't report usage for a given turn.

**Open design questions for the agents to answer in a proposal first:** poll (`wheel usage` / MCP tool) vs push
(context injection at a threshold) vs both? what's the threshold and is it configurable per agent or fixed
(e.g. 80% of `max_usd`)? does this piggyback on `budget_exhausted` (§3 agent status) as the hard stop, with usage
visibility as the soft warning before it? does codex expose enough usage data to match claude's, or does this ship
claude-first with a documented gap?

**Likely owners:** SDK (engine: capture + expose usage from harness events, MCP tool, threshold injection). Web
only if usage should also render in the agent inspector (likely yes, but secondary to the agent-visible part).

<!-- Further tasks appended as the operator provides them. -->
