# Web — handoff

Owner of `web/` (wheel.dev landing + `/app` board). Stack is fixed in the contract; nothing here
overrides it.

## STATE — done and provable on origin/main

| What | Merge | Verify |
|---|---|---|
| Auth panel never paints an unread state | `9a81ca9` (`0dd01ca`) | `pnpm vitest run src/components/inspector/auth-flow.test.tsx` (19) · `qa/e2e` `tests/auth-first-paint.spec.ts` (2) |
| Endpoint panel truthfulness + web **0.2.0** | `f477666` (`2ec1782`) | `pnpm vitest run src/lib/endpoint-probe.test.ts src/components/inspector/endpoint-panel.test.tsx` (23) |
| A 404 the engine *wrote* is not excused as a missing feature, web **0.2.1** | `ea6f2ab` (`36b5b21`) | same suite; `probeVerdict` table |

0.2.1 is the release on `origin/main` and includes the `ea6f2ab` 404-wording fix — it is no longer
sitting unreleased (a past version of this doc said it was; that was stale, not current truth).
Vercel builds only on a `web/package.json` version bump; verify with `git diff HEAD^ HEAD --
package.json | grep -q '^+.*"version"'` rather than assuming.

Earlier and still live: `npx wheel-web` (standalone package, runtime API resolution proven by
building against :8787 and running with `--api :9911`), CSP with per-request nonce, tool panel
against the live engine, local email/password auth.

## IN FLIGHT

**PR #3 — endpoint bearer-auth UI**, branch `web/endpoint-bearer-auth-ui`, commit `a676f51`, not
yet merged. `EndpointConfig.auth` (`{mode:"bearer", vault_ref}`) existed in the schema/engine
contract with no UI; added the picker, gated so bearer is only selectable once the endpoint holds
a `read` wire to a vault (the wire matrix already had `endpoint → vault (read)`, so no
wire-matrix change was needed) and the vault-key datalist only offers keys from wired vaults,
mirroring `tool-panel.tsx`'s fill-mode picker. A saved config that is `bearer` whose vault wire was
since removed shows a warning instead of silently dropping the field. `pnpm typecheck` / `lint` /
`test` (284, 14 new/changed) all green. PM reviewed the design as correct; merge is held only on
`make check`/integration being red on `main` itself from BUG-022 (journal-mode fast-path — SDK's,
already filed S1, not this PR's diff). SDK's fix is PR #12, open, not yet merged by PM — merge #3
as soon as #12 lands and CI reruns green. Nothing more needed from web on either.

Also open, both docs-only: **PR #10** closes out the coverage-scope CONTRACT item PM ruled on.
**PR #13** documents the ingress error-shape gap found while re-probing NEXT#1 (see NEXT).

## NEXT — priority order

1. **Ingress landed (SDK, `340f318`) — re-probed at the source level, real board E2E still owed.**
   `crates/wheel-engine/src/api/ingress.rs` exists and is mounted at `/ingress` (confirmed by
   grep + reading it, not inferred). The "bodiless 404 → ingress is not built yet" branch in
   `probeVerdict` is now unreachable in practice: the engine's fallback handler always answers
   404 with a JSON body, never bodiless. Found one real gap while checking: `ingress.rs`'s `err()`
   helper emits a bare `{"code":"no_such_endpoint"}`, not the `{"error":{"code":...,"message":...}}`
   envelope every other engine route uses (`wheel_core::ErrorBody`) and that `errorCode()` requires
   by design — so a real "no endpoint here" 404 shows the generic bodied-404 message instead of the
   specific one already written for it. Not misleading, just less specific. Reported to SDK; PR #13
   documents today's real (bare-shape) behavior in a test rather than the intended one, to flip once
   their fix lands. Still owed: an actual browser E2E clicking Test against a live running board —
   ask QA for the ID; do not invent one (see TRAPS).
2. Nothing else queued. Once PR #3 lands, re-check this list against `origin/main` rather than
   trusting it verbatim (see TRAPS #13).

**Standing obligation from the coverage ruling below:** any new `src/lib` module whose wrong
branch would be silently wrong (permission/wire checks, auth, security headers, state machines,
anything §3c calls out) gets added to `vitest.config.ts`'s coverage `include` on the same PR that
introduces it. Not a NEXT item — a check to run on every future PR that touches `src/lib`.

## TRAPS — every one of these I walked into today

1. **An exit code is not evidence.** A Playwright run reported exit 0 with *zero bytes* of output
   (piping to `tail` swallowed it; a backgrounded `&` detached it from the harness). I re-ran to a
   file instead of believing it. Earlier the same shape cost more: a stale `next-server` held :3000
   and my readiness check only asked "is something listening", so I got a **false PASS on CSP**.
   Assert the BUILD_ID you just built is the one being served.
2. **A test that has never failed proves nothing.** Every assertion here was mutation-verified:
   reintroduce the bug, watch the test go red, restore, watch it go green. For the auth fix that
   meant restoring `?? false` and confirming 3 unit cases *and* the E2E turned red. Do this by
   default; it is cheap and it is the only thing that makes "green" mean anything.
3. **A flash is invisible to an assertion that runs after it.** The operator's bug was a one-frame
   render. Polling for it cannot work. `auth-first-paint.spec.ts` installs a MutationObserver via
   `addInitScript`, records every mount/unmount of the form, slows `/auth` 2s with `page.route`,
   and requires the recorded list to be **empty**. Reuse that shape for anything transient.
4. **Rebase BEFORE diagnosing a red gate.** `make check` went red on `rust:clippy`/`rust:test` and
   I nearly reported a broken build to SDK. My worktree was 49 commits stale; main already had the
   fix. Cost: nothing, because I checked `git show main:<file>` first. Always do that first.
5. **`??` is not `||`.** Bit me twice: `ingress_base_url` arrives as an **empty string** before the
   project starts (so `??` produced a "URL" of just `/hook`), and `WHEEL_API_URL=` with nothing
   after it is an ordinary empty string. For anything that can be empty-but-present, use
   truthiness.
6. **Never paint a state you have not read.** The whole auth bug. `data?.x ?? false` collapses "not
   loaded" into "false". Pending is its own state — it deserves its own branch and its own
   placeholder. The same bug already existed once as `E2E-local-session-gate` (loading ≠ anon); it
   will happen a third time somewhere else.
7. **`qa/contract/testid_parity.py` only sees literal `data-testid="..."`.** A computed one
   (`data-testid={x ? "a" : "b"}`) is invisible, and so is any id passed as a `testId=` *prop*
   (e.g. `CopyField`). The gate was right both times; I made the testids literal and dropped the
   registration for the prop-based one rather than loosening the regex. Do not weaken it.
8. **`web:coverage` is scoped to an explicit include list in `vitest.config.ts`.** A new module is
   invisible to the 90% gate until someone adds it. I only noticed because my coverage numbers were
   **byte-identical** before and after adding an 84-line file. If a number does not move when it
   should, that is the finding.
9. **Playwright collects spec files at start.** I edited a spec mid-run and could not tell which
   version had passed. Re-run rather than reason about it.
10. **Read another agent's constraint literally.** API confirmed my error code and then explained
    that *only a bodiless 404* becomes 501. That sentence contained a delayed bug in code I had
    already shipped: once ingress lands, a real "no endpoint at this path" would have rendered as
    "it does not mean your path is wrong". A plain "yes" would not have surfaced it. Read the
    boundary, not just the answer.
11. **Prefer branches that retire themselves.** The "ingress is not built yet" wording becomes
    unreachable the moment API's fix lands, rather than needing someone to remember it. QA's
    `pending` marker in `env-allowlist.json` expired the same way, by breaking. Copy the pattern.
12. **Background work does not survive a session restart.** Long gates died twice. `CHECK_ONLY=web`
    / `CHECK_ONLY=qa` run in seconds; the rust gates take ~28 min under a contended cargo lock and
    are not yours to re-prove for a web-only diff — say so instead of implying green (PM ruled this
    correct).
13. **This file is a snapshot, not truth.** A prior version of this doc said `ea6f2ab` was merged
    but unreleased — `git log -- web/package.json` showed it had already shipped as 0.2.1
    (`36b5b21`). It said NEXT#1 just needed "API's CORS/preflight merge and SDK's ingress" without
    saying whether either had actually landed — a grep of `crates/wheel-engine/src` (no
    `/ingress/*` route) settled it in one command, and both API and SDK independently confirmed the
    same thing minutes later. Before acting on anything in STATE or NEXT, check it against
    `git log`/grep on the actual paths named — a stale handoff read as current truth wastes a whole
    agent (or worse, a whole turn) that a two-minute check would have caught.

## CONTRACT — where I think a rule is wrong

~~1. §0b rule 3's "≥90% per crate and per package" vs. web's actual 9-file-scoped gate.~~
**Resolved 2026-09-06 (`db1416f`):** PM ruled for narrowing the contract wording (my
recommendation) rather than widening the gate. §0b/3 in `docs/ARCHITECTURE.md` now says web's bar
is scoped to the `src/lib` rule-modules enumerated in `vitest.config.ts`'s include list, and that
the list is a standing obligation — see NEXT.

1. **The release rule has no owner for "when".** Vercel now builds only on a `web/package.json`
   version bump, which correctly stopped 127 no-op deploys. But a fix merged without a bump is
   invisible in production for an unbounded time, and nothing schedules the next bump — `ea6f2ab`
   sat on main in exactly that state for a while before 0.2.1 finally bumped it. This is fine for
   cosmetics and *not* fine for a security fix. Suggest: any merge touching auth, CSP, or a secret
   path bumps the patch version in the same commit, and the rule says so. Still unresolved.
