# Workflow Builder — feature scope (operator directive, 2026-09-07)

Operator: "when someone creates a new project (or wants to talk to an agent about improving an existing
workflow), the agent can help the user assemble their workflow." The system prompt is `docs/BUILDER_PROMPT.md`
(rewritten at 5e42395). This scopes the FEATURE around it. Design details in [brackets] are the implementer's
call within the constraints; owners named per surface.

## The two entry points
1. **New project** — after create, the user lands in a builder conversation instead of an empty grid: "what
   do you want this workflow to do?" The builder proposes a board in plain language, iterates, and on
   confirmation the board is applied to the (empty) project.
2. **Improve existing** — from a populated board, a "talk to the builder" action opens the same conversation
   seeded with the CURRENT board. The builder proposes a diff (add/remove/rewire/reconfigure), explains it,
   and on confirmation applies the change.

## Backend shape — implementer's choice within these constraints (owner: SDK + API)
The builder is a Claude run with `BUILDER_PROMPT.md` as system prompt, the conversation as messages, and —
for "improve" — the current board JSON appended. [Whether it is a server-side harness call or a transient
engine-managed agent is SDK+API's call.] Hard constraints:
- It runs on the USER's auth (their claude credential), like every other agent — no shared/privileged run.
- It is conversational: plain-text turns until the user confirms, then exactly one board between
  `---START-WORKFLOW---` / `---END-WORKFLOW---` (the prompt's output contract).
- The new-project case has NO board yet, so the builder cannot be a node on the board it is creating — that
  is why a server-side/transient run is the natural fit for at least entry point 1.

## The apply step — the load-bearing part (owner: API)
Parse the emitted board JSON and realise it against the project:
- New project: for each node `POST /v1/nodes` (typed config), then for each wire `POST /v1/wires`.
- Improve: compute a DIFF against the current board and apply only the delta — create/delete nodes, create/
  delete wires, PATCH changed config (merge-PATCH is live, RFC-7386, so partial config is safe).
- **Every wire is validated against the default-DENY matrix before creation** (the engine already refuses
  illegal pairs; the apply step must surface a refusal as an error to the builder/user, not a silent drop).
- Apply is **all-or-nothing where possible**, or reports exactly what landed and what failed — never a
  half-applied board with no report (the success-shape invariant: do not report success for a partial apply).

## UI (owner: Web)
- New-project → builder conversation panel; existing-board → "improve with the builder" entry.
- Stream the builder's turns; render the proposed board as a preview (nodes + wires) BEFORE apply; an
  explicit "apply" confirm; show apply results (what landed / what a wire refusal rejected).
- Honest capability status surfaced from the prompt: if the builder proposes a codex agent or a script node,
  the UI notes it is not runnable yet (codex refused today; script execution landing).

## Acceptance (per standing rules)
- The apply step has tests: a legal board applies fully; an illegal wire is rejected with a named error, not
  dropped; an "improve" diff applies only the delta and does not clobber untouched config (merge-PATCH).
- ADVERSARY reviews the apply path — it creates nodes/wires from LLM-emitted JSON, so it is an untrusted-input
  surface: the board JSON must be validated (node types, config shapes, wire legality) before anything is
  created, and a malformed or hostile board is refused whole, never partially realised.
- QA >=90% on the new code. Web + API + engine each green their own gates.

## Sequencing
Not on the wake's critical path — different surface, no wheel-host dependency beyond the existing node/wire
API. Can proceed in parallel. The new-project entry point (1) is the higher-value half and the cleaner build
(empty board, no diff); "improve" (2) rides the same conversation + a diff. Suggest landing (1) first.
