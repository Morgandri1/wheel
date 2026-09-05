# Wheel on Wheel — the bootstrap board

A project on the production deployment whose agents run continuously and develop Wheel itself. This file is the
board's specification; `infra/bootstrap-board.sh` creates it through the public API. Secrets are entered by the
operator through the vault inspector — never in this repo.

## Nodes

| name         | type  | config                                                                                   |
|--------------|-------|------------------------------------------------------------------------------------------|
| `contract`   | ctx   | `docs/ARCHITECTURE.md` verbatim                                                          |
| `workflow`   | ctx   | the working rules below (repo, branches, PRs, CI, comms)                                 |
| `secrets`    | vault | keys: `GITHUB_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN` (values set by the operator)             |
| `pm`         | agent | claude · run_on_startup · system prompt: PM brief (`docs/plans/pm.brief.md` if present)  |
| `sdk`        | agent | claude · run_on_startup · SDK/Engine brief                                               |
| `api`        | agent | claude · run_on_startup · API brief                                                      |
| `web`        | agent | claude · run_on_startup · Web brief                                                      |
| `qa`         | agent | claude · run_on_startup · QA brief                                                       |
| `adversary`  | agent | claude · run_on_startup · red-team brief                                                 |
| `reports`    | table | columns: `ts text, author text, kind text, body json` — the durable status log          |

## Wires

- `contract → <every agent>` **send** (injection): every agent carries the contract in its preamble.
- `workflow → <every agent>` **send** (injection).
- `<every agent> → secrets` **read**: `GITHUB_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` in the child env — the second is also the
  agent's Anthropic authentication (`mode:"env"`), so no UI login step.
- `pm → <every other agent>` **send** and `<every other agent> → pm` **send**: the PM is the hub; peers get direct
  `send` wires only where the contract's ownership table says they collaborate (sdk↔api, sdk↔web).
- `<every agent> → reports` **write**: every `STATUS:`/`DONE:`/`BUG:` is also a row (§3c #2: nothing is only a message).

## Working rules (the `workflow` ctx)

1. Repo: `github.com/Morgandri1/wheel`. `gh auth` uses `GITHUB_TOKEN` from the environment. Clone into `$HOME/wheel`
   (per-project data dir), one branch per task named `<role>/<slug>`, never push to `main`.
2. Every change is a PR with `make check` green locally first; GitHub CI is the merge gate. The PM merges.
3. Status goes to the PM with `wheel msg pm --file …` AND to the `reports` table with `wheel write reports/<ts>-<role> …`.
4. Budget: one turn, one task. Ephemeral context is off for developers (they keep repo state in context);
   the PM runs ephemeral to stay current with the board.
5. Secrets never leave the sandbox: don't print `GITHUB_TOKEN`/`CLAUDE_CODE_OAUTH_TOKEN`, don't commit `.env`.

## Sizing

Six agents with cargo builds ⇒ the Railway host wants ≥ 8 vCPU / 32 GB and a volume ≥ 40 GB (≈5 GB per project of
toolchain + target). Builds are serialised per project by the engine's rlimits; cross-project parallelism is the
host's problem (§5b).

## Bringing it up

```bash
export WHEEL_API=https://wheel-api-production.up.railway.app
export WHEEL_EMAIL=you@example.com WHEEL_PASSWORD='…'        # an account created via the web app or signup route
infra/bootstrap-board.sh                                        # idempotent: creates project + nodes + wires, prints the project id
# then in the web app: open the project → vault "secrets" → set GITHUB_TOKEN and CLAUDE_CODE_OAUTH_TOKEN → start the project
```
