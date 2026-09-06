# Web — handoff

Owner of `web/` (wheel.dev landing + `/app` board). Stack is fixed in the contract; nothing here
overrides it.

## STATE — done and provable on origin/main

| What | Merge | Verify |
|---|---|---|
| Auth panel never paints an unread state | `9a81ca9` (`0dd01ca`) | `pnpm vitest run src/components/inspector/auth-flow.test.tsx` (19) · `qa/e2e` `tests/auth-first-paint.spec.ts` (2) |
| Endpoint panel truthfulness + web **0.2.0** | `f477666` (`2ec1782`) | `pnpm vitest run src/lib/endpoint-probe.test.ts src/components/inspector/endpoint-panel.test.tsx` (23) |
| A 404 the engine *wrote* is not excused as a missing feature | `ea6f2ab` (`e83ac87`) | same suite; `probeVerdict` table |

0.2.0 is the deployed release: auth first-paint + endpoint panel + Test button. Vercel built it —
I simulated the `ignoreCommand` against the real merge rather than assuming (`git diff HEAD^ HEAD
-- package.json | grep -q '^+.*"version"'` → exit 1 = build).

Earlier and still live: `npx wheel-web` (standalone package, runtime API resolution proven by
building against :8787 and running with `--api :9911`), CSP with per-request nonce, tool panel
against the live engine, local email/password auth.

## IN FLIGHT

Nothing. Worktree `/Users/metatron/wheel-wt/web` is clean, `web/main` is merged and pushed.

**One thing is merged but NOT released:** `ea6f2ab` has no version bump, so production still runs
0.2.0 and shows the old 404 wording. Harmless today — the corrected wording only triggers on a 404
*with a body*, which needs ingress to exist. It ships with the next bump. Do not forget it.

## NEXT — priority order

1. **Re-probe the endpoint Test button against a real board** once API's CORS/preflight merge and
   SDK's ingress land (API owes a hash; they confirmed `Access-Control-Allow-Origin: *`, no
   credentials, on `/p/<project>/<path>`). When ingress lands, the "bodiless 404 → ingress is not
   built yet" branch becomes unreachable by construction and an E2E hitting a real endpoint is
   owed. Ask QA for the ID; do not invent one (see TRAPS).
2. **Bump `web/package.json` to ship `ea6f2ab`**, bundled with whatever else is ready. One minor
   bump per bundle, changelog in the bump commit.
3. **Endpoint `auth: {mode:"bearer", vault_ref}` UI** — M2, not built. Needs the endpoint→vault
   read wire in the picker.
4. **Coverage include list** (see CONTRACT) — needs a PM ruling before changing.

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

## CONTRACT — where I think a rule is wrong

1. **§0b rule 3 says "≥ 90 % test coverage per crate and per package."** For web that is not what
   is enforced. `web/vitest.config.ts` measures 90% across **nine hand-picked files**; everything
   else — every component, `runtime-config.ts`, `api.ts` — is outside the gate entirely. The
   scoping is defensible (those files encode rules; components are covered by Playwright), but the
   contract's words and the gate's behaviour disagree, and a successor reading only the contract
   will believe the package is covered. Either narrow the contract to say "the modules that encode
   rules, listed in vitest.config.ts", or change the include to `src/lib/**` with explicit
   exclusions. It needs a ruling, not a quiet edit — I left it alone.
2. **The release rule has no owner for "when".** Vercel now builds only on a `web/package.json`
   version bump, which correctly stopped 127 no-op deploys. But a fix merged without a bump is
   invisible in production for an unbounded time, and nothing schedules the next bump. `ea6f2ab` is
   sitting on main right now in exactly that state. This is fine for cosmetics and *not* fine for a
   security fix. Suggest: any merge touching auth, CSP, or a secret path bumps the patch version in
   the same commit, and the rule says so.
