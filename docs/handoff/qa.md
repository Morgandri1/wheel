# QA handoff

## STATE (verifiable on origin/main)

Seven contract gates, all green, all static and sub-second — run `bash qa/check.sh`:
`qa:id-traceability` · `qa:testid-parity` · `qa:suite-isolation` · `qa:fake-steering` ·
`qa:route-parity` · `qa:env-allowlist` · `qa:ci-workflow-lint` · `infra:prune-probe-projects`.

Integration suites (`make test-int`, docker + fake harness): api-auth 19, api-ingress 16,
engine-cli 25, engine-mcp 14, engine-vault 21, engine-messages 24, engine-events 9,
engine-child-env 7, engine-auth-paste 15, engine-auth-routing 10, engine-validation 21,
wire-matrix (all 243 cells). E2E: chromium + local-auth + `packaged` (the real `npx wheel-web`).

TESTPLAN has ~570 named criteria; `qa:id-traceability` fails if a suite asserts an ID the plan
does not name, and it reads Playwright specs as well as Python.

## IN FLIGHT

- `WOW-table-survives-restart` is **RED by design** — it reproduces a real S1 (a table node
  whose sqlite table is gone answers `400 no such table` about a node the board still shows).
  Goes green when SDK re-ensures the table. Its companion asserts the rebuilt table accepts the
  node's **configured columns**; a table rebuilt from a default schema passes the first and
  fails the second, and would look like a fix.
- `WOW-toolchain-cargo-per-project` is **RED** — `CARGO_HOME=/data/cargo mode=755`. The PATH is
  correct on the docker backend; the **mode** is the violation. SDK has a fix described but not
  on main.
- `make check` is red on `wheeld` at 89.61% (0.39% under). API's.

## NEXT (in order, each with its gate)

1. Ingress, when SDK lands it — 29 `ING-*` IDs in §11c are written and will be red until it
   exists. Build against `ING-headers-subset` and `ING-bearer-not-in-transcript` first: the
   endpoint's bearer must reach neither the delivered headers **nor** the transcript. Then
   `ING-envelope-forge-from-stranger` — `MSG-envelope-forge` covers a body written by a WIRED
   agent, and ingress is where an anonymous stranger writes it into the same parser. If any
   operator-to-operator channel is ever exposed as an endpoint, that is the criterion standing
   between a passer-by and impersonating the operator.
2. Credential refresh — `AUTH-refresh-outlives-access-token` (the operator's acceptance: an
   agent alive past the ORIGINAL expiry, untouched), `AUTH-refresh-vault-updated`,
   `AUTH-durable-only-when-declared`, `AUTH-refresh-failure-is-visible`, `AUTH-refresh-not-in-log`.
3. `ENG-starts-without-shm` and `ENG-concurrent-readers` — no CI job covers either today.
4. `WOW-toolchain-cargo-distinct` / `-owned` — need TWO projects and a uid check; a
   single-project test cannot see a shared cache or a symlink into one.

## TRAPS (read this section first)

**Everything below cost me hours today. Each is a case where a green, or a red, meant something
other than what it looked like.**

1. **The shared cargo target-dir corrupts compiles, not just measurements.** All six worktrees
   share one `target-dir`. I filed an S1 against API for a compile break that did not exist: my
   worktree was one commit behind, another agent's newer `wheel-host` rlib was in the shared
   dir, and my build linked theirs against my source. I *had* suspected staleness and rebased —
   which rules out a stale CHECKOUT and says nothing about a stale ARTIFACT. Anything that
   shells out to cargo needs its own `CARGO_TARGET_DIR`. The coverage gate and the wheeld smoke
   now do.
2. **`docker` tags are mutable and six agents share the daemon.** A suite reported F015 unfixed
   with `/proc/<pid>/environ` evidence; the fix was already on main and the image had been
   rebuilt under me. Suites pin the tag to an image ID for the run (`pin_image`).
3. **`reuseExistingServer` will hand you the previous run's server.** I sabotaged the packaged
   suite to prove it could fail, and it PASSED — Playwright had reused a server started with the
   correct flag. A suite whose subject is *how the server was launched* may never reuse one, and
   a stale process on the port reproduces the vacuum by another route.
4. **An absence assertion needs a control, and a SKIPPED control is worse than none.** BUG-018:
   my control skipped and the assertion it guarded reported green against a child that held both
   secrets. Use `R.control()` / `R.gated()` in `wheel_client` — a gated assertion SKIPS when its
   control did not pass, rather than passing.
5. **A blanket `except` around a health loop turns any bug inside it into a timeout that blames
   the subject.** I lost several runs to "engine never became healthy" against an engine that
   answered 200 the instant I probed it by hand — a `NameError` in my own line, swallowed.
6. **`cmd | tail` reports tail's exit status.** A failed build read as `exit=0`. And my fix for
   it, `set -o pipefail`, is a bash-ism: the harness runs under dash. I fixed an honesty bug
   with a portability bug.
7. **A gate moved between jobs is a gate that can silently stop running.** `qa:image-contents`
   moved to the job that builds an image; `ci_workflow_lint` now asserts some job both builds it
   and invokes the gate.
8. **`grep` sees text, only a parser sees an import.** My verification for an import fix matched
   the `# noqa` comment it had landed in. `suite_isolation` checks imports by AST now.
9. **A test that reaches the same end state by a different route is testing something else.**
   SDK's first table-orphan test used delete-then-recreate, which always drops the table, so it
   passed with the bug restored. Only the type-change route reaches the adoption.
10. **Fixed ports let a leftover container impersonate a broken engine.** All suites use
    `free_port()`; `suite_isolation` enforces it.

## CONTRACT (rules I think are wrong, or worth stating)

- **No exemption without a machine-checkable expiry.** PM ruled this and it works: the
  `wheel-engine` coverage exemption is a ratchet with a floor that only rises, and the
  `RUSTUP_HOME` pending marker went red the hour the code landed. Keep it. An exemption whose
  expiry is prose expires when somebody remembers, which is never.
- **The F015 env allowlist is pinned with a reason per entry** (`qa/contract/env-allowlist.json`)
  because ADVERSARY asked to review changes and a review that depends on remembering is a habit,
  not a control. Hold the line I asked ADVERSARY to hold me to: a variable belongs there because
  a real agent needs it, never because a suite of mine does. I moved my fake harness to a config
  file rather than ask for `WHEEL_FAKE_*`.
- **`make check` exits non-zero and says INCONCLUSIVE when a gate could not run.** Do not
  "fix" that by letting it pass. "Could not check" must never read as "passed" — which is the
  single sentence the rest of this document is about.
