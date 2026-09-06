# PM — plan

I run `ephemeral_context: true` (contract §0/M1.6): every turn starts with a clean slate, nothing
carried over except what's durable on the board (git, `reports`, messages). This file, `reports`,
and `docs/handoff/` are how a fresh PM turn reconstructs state instead of guessing.

## What actually happened before this file existed

`origin/main` already contains a fully-built M1 → M2 → most-of-M3 Wheel, produced by a **separate
local swarm** (operator's laptop, `/Users/metatron/wheel`). That swarm stood down on 2026-09-06
(`docs/handoff/README.md`) and handed off to *this* board — the "wheel-dev" cloud project described
in `docs/WHEEL-ON-WHEEL.md` — to take over continuous development. Only `web.md` and `adversary.md`
handoff briefs were written before stand-down; `sdk`, `api`, `qa` have no brief, only their original
M0 plans in `docs/plans/{sdk,api,qa}.md`, which predate everything that shipped since.

This board itself had done nothing yet: `reports` table had zero rows, and the only traffic was the
operator confirming `secrets/GITHUB_TOKEN` and `secrets/CLAUDE_CODE_OAUTH_TOKEN` were set. Every
agent here (including me) is message-driven with idle-parking (§3c #14) — by design, nothing runs
unless something sends it a message. Nobody had sent sdk/api/qa/web/adversary a message on *this*
board since bootstrap, so nothing ran. That's the whole explanation for "why did you stop": there
was no ongoing turn to stop — each ephemeral PM reply answered the question and ended, and ending a
turn is normal (idle-parking, not a fault), but no one had yet kicked off the next round of work
here. Fixing that is this commit.

## Kickoff (this session)

Dispatch one message each to sdk, api, web, qa, adversary:
- point at `docs/handoff/<role>.md` where it exists, else their `docs/plans/<role>.md` + "read
  `git log origin/main` for your owned paths since the plan predates most of it"
- one task per turn (workflow rule 4), branch `<role>/<slug>`, PR gated by CI, PM merges
- `STATUS:`/`DONE:`/`BLOCKED:` to pm **and** a `reports` row (workflow rule 3) — `reports` has never
  been written to; first write creates `t_reports`

## Open items for a future PM turn

- No `pm-ctx` content has ever been written (the injected block is empty) — decide what standing
  context belongs there vs. this file.
- `sdk`/`api`/`qa` are overdue a handoff brief the way `web`/`adversary` got one; ask each to write
  its own once its current task lands, so the next stand-down doesn't repeat this gap.
