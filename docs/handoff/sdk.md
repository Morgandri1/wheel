# SDK / Engine — handoff

## STATE (on origin/main, verifiable)

`wheel-core` (types, wire matrix, name/config validation, schema export), `wheel-engine`
(sqlite + migrations, board CRUD, wires, agent supervisor with idle parking and
one-process-per-node, message queue with the user priority lane, events WS, `/v1/cli/*` with
wire enforcement, vault AES-256-GCM, table nodes, tool nodes end to end, built-in MCP server),
`wheel-cli`, `docker/Dockerfile.host`, `docs/PROTOCOL.md`, `docs/schema/`.

Findings closed: F015 (children inherited the engine's env — now `env_clear` + a 9-entry
allowlist, structurally enforced by `child_command`), 018, 021–027. Last verified push before
this handoff: **`9476350`** on `origin/main`. 262 engine tests, clippy clean.

Landed after `d206b95`, all on `origin/main`:
`7a76a5d` per-project crate cache (QA BUG-021 / ADVERSARY 029) ·
`32a1ff9` a table node's sqlite table follows it through every change of shape ·
`0692797` table storage re-established on boot (W1).

## IN FLIGHT (this branch — read the commits, they carry the reasoning)

1. **CARGO_HOME** (QA BUG-021 / ADVERSARY 029) — done, `7a76a5d`. `data_dir/.cargo`, mode SET to 0700 each
   start then verified; refuses to start a child it cannot make private. The gate asserts the
   VALUE the child got and the MODE, not that the variable exists.
2. **Table orphan** — done, `32a1ff9`. Changing a table node's config to another type orphaned `t_<name>`;
   the next table node with that name inherited its rows AND columns. `board::update` now carries
   the table through every transition; `board::delete` propagates a failed drop instead of `.ok()`.
3. **W1, table nodes lose their table across a restore** — done, `0692797`. Engine boot calls
   `board::ensure_tables`, which runs `tables::ensure` for every table node.
   **`tables::create` is destructive** (it claims the name); **`tables::ensure` is the safe
   one**. W1's fix must call `ensure`. See TRAPS.
   Gates: `a_table_node_whose_table_vanished_gets_it_back_with_its_own_columns` (QA's two
   assertions: the read returns EMPTY *and* the rebuilt table accepts the node's CONFIGURED
   columns — a default schema would pass the first and fail the second) and
   `a_column_added_to_the_config_appears_on_the_next_boot` (which also asserts existing rows
   survive the column add). Both mutation-checked: remove the boot call and both fail.

**Two sessions of me fixed W1 in parallel** and neither knew. The version that shipped is
`board::ensure_tables` (wired into `lib.rs` boot, tested). The other is `ensure_node_tables` in
`crates/wheel-engine/src/db/mod.rs` — same logic, never called, and it was still UNCOMMITTED in
this worktree when I stopped. I did not revert it: it is not mine to revert, and reverting
another session's uncommitted file is how I destroyed work twice today. **Whoever picks this up:
delete the dead `ensure_node_tables` if it is still there.** Its tests are worth reading first —
one asserted row survival that mine did not, and I took that assertion.

## NEXT, in priority order

1. **CREDENTIAL REFRESH (P1, operator blocked — re-authenticating every 8 h).** Not started.
   PM's five steps are in `SDK-DEV-CTX` verbatim, 11:59:53Z; follow them, they are correctly traced.
   The shape: `auth.rs::find_access_token` keeps only `accessToken`; the `refreshToken`
   (`sk-ant-ort…`) and `expiresAt` sit in the same object and are discarded. Keep the whole
   object; seed each child's config dir with it so Claude Code refreshes itself; re-persist what
   it writes back **including to the vault it came from**, or every peer keeps the dead pair.
   `StoredOauth::is_long_lived` (auth.rs:180) must invert: durable only when the store SAYS so
   (setup-token / api-key kind), unknown otherwise — a missing expiry is a failure to read, not
   a promotion. Acceptance: an agent still running with no human touch past the original expiry,
   and a vault holding the refreshed pair. Ask QA for IDs first.
2. **ENDPOINT INGRESS (operator blocked — `/tg` 404s).** No `/ingress` route exists in the engine
   at all; the API and host already proxy to it. Build to QA's 26 `ING-*` IDs. PM's ruling on
   the shape is settled: 404 `{"code":"no_such_endpoint"}` unmatched, 405 with `Allow:` naming
   only that endpoint's own method, ack → `202 {"accepted":true,"endpoint":<id>}` and deliver the
   hit on the endpoint's `send` wires with envelope `type="endpoint"` (**never `user`**), script
   mode returns the script's stdout. The consumed `x-wheel-ingress` header and bearer must never
   reach an agent. Also `wheel endpoint test <endpoint> --body …`.
3. **W2, bare "engine error"** — `cli_routes::inbox` throws the cause away at the CLI boundary.
   The CLI must print the engine's message and a stable code; the sqlite text goes to the engine
   log with the request id. Gate: `CLI-error-has-a-cause` (QA says it is green for `wheel inbox`
   already — confirm it covers the general boundary, not just that one command).
4. **028 declared-key ambiguity** — PM ruled: presence = a STORED non-empty value, everywhere.
   Declared-only keys never make "authenticated", never mask `needs_auth`, never count in
   `find_ambiguity`. Declared overlap becomes a create-time WARNING, not a 409. Today an operator
   who declared `CLAUDE_CODE_OAUTH_TOKEN` and left it empty gets a false 409 when wiring the
   second vault that actually holds the token — which is the multi-account setup we shipped for.
   Gates: QA `1b56a3d`. ADVERSARY confirms their `run_declared_empty.sh` §5 is the acceptance
   test and is **currently RED**: a declared-but-empty `CLAUDE_CODE_OAUTH_TOKEN` in vault A still
   409-blocks wiring vault B that holds the real value. `find_ambiguity` must treat a
   declared-but-unfilled key as non-blocking, the way presence already became stored-based.
5. **Child reaping** — was session 2's in-flight work (`signal_group`, `cmd.process_group(0)`,
   `GRACE`), uncommitted in `wheel-wt/sdk`, which **does not compile** (borrow error at lib.rs:92).
   Salvage or rewrite; do not assume it is a starting point.
6. **Loopback-at-create**, then the rest of M2 (scripts, chest, endpoint wires, MCP attach).

## TRAPS — read this section twice

- **`tables::create` is destructive now.** It drops and rebuilds. PM's staged W1 text says "call
  `tables::create` at boot, it is already `CREATE TABLE IF NOT EXISTS`" — that was true this
  morning and is false now. Implemented that way, **every restart wipes every table**. Use
  `tables::ensure`. I split them and documented both for exactly this reason.
- **`git checkout <file>` on an uncommitted file destroys it, and I did it again today** — while
  restoring a deliberate mutation, on a file holding forty uncommitted lines. Second time this
  session. `git stash` the mutation, or copy the file aside first (`cp x /tmp/x.bak`) and restore
  with `cp`, never `git checkout`.
- **A test that passes is not a test that works.** My first table-orphan test used
  delete-then-recreate and PASSED with the bug deliberately restored — delete always dropped the
  table, so that route never reached the adoption. I only found the real route (change the node's
  TYPE) by putting the bug back and watching the test stay green. **Restore the bug and watch your
  test fail, every time.** QA warned about this exact class an hour before I walked into it.
- **`git add -A` published another session's work under my commit message** (`58a333c`). YOKE runs
  several sessions of one agent at once. Name every file you commit. Check
  `git status --short` in a worktree before touching it, and make your own if it is dirty.
- **The shared worktree is not yours — and nor is your own.** `wheel-wt/sdk` is broken by another
  session's in-flight work (borrow error at lib.rs:92). I made `wheel-wt/sdk-1` from `origin/main`
  to escape it, and a second session then edited *that* too: I found `board::ensure_tables` and its
  tests in my tree, written by someone else, building on my uncommitted helpers. Five collisions in
  one day. Diff before every commit and read what you are about to sign your name to — if a hunk
  is not yours, keep it and say so rather than reverting or claiming it.
- **I read an empty grep as confirmation** (finding 025): three CLI handlers were registered by an
  edit that matched nothing, and I reported them shipped. Grep for the *route*, then add a test
  that walks the router. A negative search result is evidence of nothing.
- **Test flakes were my own load.** Two "timeouts" were four cargo suites competing on this host.
  Do not raise a timeout budget before you have checked what else is running. `until()` reports
  elapsed time now so this is visible.
- **Environment is process-global in tests.** A test proving secrets do not reach children leaked
  them to every sibling test in the process. Use `EnvGuard`.
- **`--append-system-prompt <text>` is forbidden**, and so is any secret on a command line: argv is
  world-readable across uids. Prompt and preamble go in a file in the node's 0700 config dir.
- **The operator's real credentials are in `~/.claude` and `~/.codex` on this host.** Never run a
  command that could refresh, rotate or overwrite them. Test against a temp config dir.
- **Never infer `needs_auth` from an exit code.** `bypassPermissions` as root exits identically to
  "not logged in". Use stderr or a probe.

## CONTRACT — where I think a rule is wrong

1. **"Comments sparingly" is being applied to the wrong axis.** The rule cost real safety today: the
   comment beside the CARGO_HOME code said the right thing while the code sat one level too high,
   and nothing caught it because the comment was doing the work a *test* should. My commits and
   doc-comments now carry the reasoning and I would keep them. Refactor prose into names, yes —
   but a `why` that a successor would otherwise re-derive from a production incident is not clutter.
2. **The messaging discipline is inverted for defects.** "Batch messages, PM is for rulings" is
   right for coordination and wrong for findings: I sat on the table-orphan S2 for twenty minutes
   because it was not a milestone `DONE:`. `qa/BUGS.md` and `redteam/findings/` are the system of
   record (§3c #15) — a defect should go there *first* and be mentioned second, and no agent
   should weigh whether a bug clears the messaging bar.
3. **One worktree per session is a workaround for a YOKE defect we are reproducing in Wheel.** §3c
   #13 fixes it for agents (one process per node, messages queue) but the *human-facing* half is
   missing: nothing stops two sessions of one agent editing one checkout. Wheel should own the
   working copy — one workspace per agent node, leased — or we ship the bug that cost this team
   four collisions today.
