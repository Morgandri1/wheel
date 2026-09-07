# Web lane — state, 2026-09-07

Supersedes the stand-down brief, which predated everything below.

## On main (released, live to the operator)

`0.3.0` (`bf6acf8`). Endpoint panel truthfulness (four distinct probe states), the 043 composition
warning, the adopted bearer-auth UI, 45 kB off the board's first load, the auth-mode mismatch gate,
one shared wired-vault rule.

Deploy is **triggered, outcome unverified** — see "Why nobody can confirm a deploy" below.

## Held on `web/main`, merged nowhere (NOT live)

    917a6e9  wires leave by the side that faces the other node
    1b327e3  clear a budget or timeout with an explicit null
    f7b902d  workspaces, budget and idle timeout controls

All three gated green (`CHECK_ONLY=web`), 353 tests. Needs a merge onto a green main and a bump to
`0.4.0` — the bump IS the deploy; a merge alone changes nothing the operator can see.

## Dogfood findings (the reason to read this file)

**1. The generated client types were stale, silently.** `workspaces` exists in
`crates/wheel-core/src/node.rs:196` and in `docs/schema/`, but `web/src/lib/schema/generated.ts`
had ZERO occurrences of it. Building the workspaces control against those types would have been
impossible — and worse, casting past the error would have written a field the client believed did
not exist. `pnpm gen:types` fixes it. **Nothing regenerates this automatically**, so the client can
drift behind the contract indefinitely and only a person trying to use the missing field finds out.

**2. `undefined` cannot clear a field under merge-PATCH.** `JSON.stringify` DROPS undefined keys, so
`{...config, budget: undefined}` goes out with no `budget` key. Under replace semantics that clears
it; under merge, an absent key means "leave unchanged" — a user empties the spend cap, the UI says
saved, and the cap survives. Clearing now sends an explicit `null`, which means unset under BOTH
semantics. The test asserts the SERIALISED body, because an object-level assertion passes while the
wire form is wrong.

**3. Two config writes were safe only by accident.** `ctx-panel` sent `{markdown}` and `table-panel`
sent `{columns}`. Complete today because those config types have one field each; the day either
grows a second field, both silently delete it, and `Partial<Config>` type-checks the mistake. Both
read-modify-write now. Agent, tool and endpoint panels were already correct — **there was no live
silent-delete bug.**

**4. Why nobody can confirm a deploy.** No public route carries a version. `/healthz` gives the API's
build, nothing gives the web's. Every 0.3.0 change is behind the login wall — I diffed it: ZERO
public-route files changed — so no unauthenticated check can distinguish 0.2.1 from 0.3.0. I tried
four (landing chunks, the webpack chunk-id map, reconstructed lazy filenames, `/app`'s referenced
app-route chunks); the board's chunk hash is never exposed to a logged-out client.
**Fix worth doing: emit the version on a public route** (a meta tag, or `/version.json` from
`web/package.json` at build). Five lines, and it retires this whole class of question permanently.

**5. Agent state is not trustworthy, so the board under-reports.** Status never advances past
`starting` in production. `node-plate` gives `running || starting` the same live treatment (a
healthy agent spins forever), and the status bar counts only `running` and `parked` — so a stuck
agent appears in NEITHER count and nothing looks wrong. Engine-side; no UI work until the state is
true, per PM.

## Traps for whoever is next

- **A merge is not a deploy.** Vercel builds only when `web/package.json` version changes
  (`b99d1ca`). Merging web work to main changes nothing a user sees. Bump deliberately, changelog in
  the bump commit.
- **A triggered build is not a succeeded build.** The ignoreCommand firing means Vercel did not
  skip; it says nothing about the build passing.
- **wheel.dev is NOT the app.** It is a catch-all placeholder — `/`, `/app` and any nonsense path
  return the identical page. The app is `wheel-2708.vercel.app` (`web/DEPLOY.md:115`).
- **Check that a check can succeed before believing it.** Five times in one session an instrument
  lied rather than the subject: a manifest that is not what browsers download, a Chai matcher
  missing from the setup (`toBeInTheDocument` — jest-dom is NOT installed here), a stale local
  `.next` used as a control, a guessed URL that resolved, and a gate run against a mutated file.
- **A fast green gate is suspicious.** `web:test` in 4s where it normally takes 20 means stale
  state; re-run and count the tests.
