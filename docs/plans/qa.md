# QA plan

Owner: QA. Living doc. Milestones track `docs/ARCHITECTURE.md` §6.

## Position

Everything I own that does not depend on product code is **done and on main**. The gates now sit
idle waiting for code, and activate themselves as it lands — no QA action needed to "turn them on".

| Deliverable | State |
|---|---|
| Fake harness + spec (`qa/harness/`) | **done** — SDK is building `wheel-engine:test` from it |
| `make check` + granular targets + CI | **done, green on main** |
| `docs/TESTPLAN.md` | **done** — 174 IDs + 192 generated wire-matrix cells |
| Wire matrix fixture + drift check | **done** — `qa/fixtures/wire_matrix.json` |
| Contract tests | blocked on `docs/schema/*.json` (SDK), `docs/PROTOCOL.md`, `docs/API.md` |
| Integration suite | blocked on `wheel-engine:test` + `infra/docker-compose.yml` |
| E2E | blocked on Web's board + `data-testid`s |

## Milestones

**M0 — done.** Plan, TESTPLAN, harness, `make check`, CI.

**M1 — vertical slice.** In dependency order, each written red-first so it goes green the moment
the feature lands:
1. Contract: JSON Schema validates fixtures for all 8 node types (`NODE-schema-roundtrip`).
2. Contract: PROTOCOL.md / API.md route parity, 404-vs-405 (`ENG-route-*`, `API-route-parity`).
3. Integration smoke for the slice: project → container → place agent+ctx → wire → auth → start →
   chat → log line → `wheel msg` between two agents.
4. The message path in depth — this is where I expect the bugs: `MSG-envelope-escape`,
   `MSG-envelope-forge`, `MSG-byte-exact`, `MSG-state-machine`, `MSG-inbox-reread`.
5. Playwright smoke (`E2E-*`).

**M2 — breadth.** All 192 wire-matrix cells asserted twice (API-side and engine-side) and a third
time at runtime through the CLI; all node types; ingress; ephemeral/injection; MCP tool surface.

**M3 — hardening.** `SANDBOX_BACKEND=process` re-run, soak/perf, red-team fix verification,
flakiness burn-down, `make check-strict` in CI.

## Design decisions

- **Python for integration/E2E**, not Rust. Fastest feedback, and it keeps the test suite honest:
  tests written in the same language and types as the engine tend to share its assumptions, which
  is exactly what a contract test must not do. The fake harness is Python for the same reason plus
  zero deps.
- **The wire matrix is generated, not hand-written.** `qa/tools/gen_wire_matrix.py` expands the
  contract into 192 cells; `make check` fails if the committed fixture drifts. A hand-maintained
  192-row table would be wrong within a day.
- **The suite is parameterised on `SANDBOX_BACKEND` from day one**, so the M3 process-backend run
  is a CI matrix flip rather than a rewrite.
- **`make check` skips loudly.** A gate that reports success while covering nothing is worse than
  no gate. `CHECK_STRICT=1` turns skips into failures once every area exists.
- **Assert what the child received, not what the engine logged.** Injection and delivery are
  asserted from the fake's first event and from `WHEEL_FAKE_TRANSCRIPT` — the engine's own logs
  are the thing under test and can't be the evidence.

## Risks

| Risk | Why it worries me | Mitigation |
|---|---|---|
| **Envelope escaping** (`MSG-envelope-escape/forge`) | An agent triggers it just by *talking about* the envelope format — and ours will, it's in their prompts. If escaping is wrong, agent A forges attribution and impersonates any node to agent B: privilege escalation straight through the wire matrix. | Highest-priority M1 test; vector handed to ADVERSARY. |
| Engine parses harness output exhaustively | Real CLI emits event types PROTOCOL.md won't list (`rate_limit_event`, `system/thinking_tokens`). Tidy fake + tidy fixtures = green tests, production falls over. | `ENG-log-unknown-event` / `ENG-log-garbage`; fake emits both on demand. SDK confirmed tolerance (A4) — still testing it. |
| Denials enforced in only one place | An API-only check passes while the engine is open to anything reaching it directly. | Every deny asserted twice, plus a third time at runtime via the CLI. |
| 404-vs-403 enumeration oracle | Easy to "fix" ownership by returning 403, which leaks which project ids exist. | `API-auth-owner-404`, `CLI-exit-nonexistent` — assert indistinguishability, not just refusal. |
| E2E flakiness | Flaky gates get ignored, then the gate is worthless. | Stable `data-testid`s only, no text scraping, no arbitrary sleeps; quarantine + fix, never retry-until-green. |

## Working agreements

- I never edit product code to make a test pass. I file a `BUG:` with the TESTPLAN ID.
- Test hooks (`data-testid`, dev tokens) are requested via PROPOSAL to the owner, through PM.
- After every merge to `main` I re-run `make check`; if red, PM + owner hear immediately with the
  commit.
