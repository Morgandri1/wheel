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

<!-- Further tasks appended as the operator provides them. -->
