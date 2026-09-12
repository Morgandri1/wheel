# Proposal: harness OAuth with real refresh (self-hosted Wheel)

Status: **implemented on `sdk/harness-oauth`; REQUIRES ADVERSARY review before merge** (ARCHITECTURE.md,
"Credential-distribution rule": this changes `save_to_vault`, the credential lookup, the vault export
and expiry). Author: SDK (harness-oauth lane). Date: 2026-09-11. Branch `sdk/harness-oauth`.

Closes docs/handoff/sdk.md NEXT #1 (P1, the operator re-authenticates every 8 h).

**Operator ruling, 2026-09-11: "we aren't going to do an API key worker, only oauth tokens and the
8 h oauth flow."** For self-hosted Wheel there is now no API-key fallback, so this is not a
convenience: it is the only way an agent runs at all on the VPS. Two consequences the design already
had to meet, now load-bearing rather than nice-to-have:

* a failed renewal has no second credential to fall back on, so it must WARN while the current token
  still works (§5.6) and then PARK rather than spend the queue against a dead token (§5.7);
* the whole flow must be headless — the VPS has no browser and is reached through an SSH tunnel — so
  it is `auth/begin` → the operator opens the URL on their own machine → `auth/complete` with the
  pasted code, and the engine never opens anything (§5.1).

Cloud (`wheel-host`) is unchanged and stays API-key only.

## 0. Policy this does not move

| Deployment | Default | Changed here? |
|---|---|---|
| Cloud, multi-tenant `wheel-host` | `WHEEL_HARNESS_AUTH=api-key-only` unless the project is in `WHEEL_HARNESS_AUTH_OAUTH_PROJECTS` (`crates/wheel-host/src/config.rs::harness_auth_for`, pinned by `an_empty_allowlist_is_api_key_only_for_everyone_fail_secure` and `tests/config_env.rs`) | **No.** Everything below is inert under `api-key-only`, and this change *adds* the route and vault gates that policy was missing (§6). |
| Self-hosted `wheeld` (laptop or owned VPS) | `oauth-token` (`crates/wheeld/src/embedded.rs`) | No. This is where refresh runs. |

Scope is the Linux credential store (a VPS, or Linux local `wheeld`). On macOS the CLI keeps its login in
the Keychain, not a file. The planned Desktop profile (docs/proposals/agent-grid-engine.md, PR #58) runs
claude with the user's real HOME, so the CLI's own keychain login and refresh just work there. Local
macOS `wheeld` *without* that profile is not covered by this change, and the doc says so rather than
pretending otherwise.

## 1. What is actually broken (traced in the code on `94dea07`)

1. **A per-node login already refreshes.** A paste-code login with no `save_to_vault` writes the CLI's
   whole store into the node's own `CLAUDE_CONFIG_DIR`, and the child runs with that directory. The CLI
   refreshes it in place. Option (c) below is in fact today's behaviour for that path.
2. **The vault copy is what dies at 8 h.** `auth.rs::find_access_token` keeps `accessToken` and
   `expiresAt` and drops `refreshToken`. `save_credential_to_vault` stores the bare access token as
   `CLAUDE_CODE_OAUTH_TOKEN` with its expiry. Every reader gets it as an env var, and an env token has
   no refresh. At expiry `lapsed_credential` sends every reader to `needs_auth`. The originating node
   also reads that vault (the save requires the wire), and the env var beats its own store, so even the
   node that *could* refresh stops.
3. **`StoredOauth::is_long_lived` promotes a missing field.** A store entry with an `sk-ant-oat` token and
   no `expiresAt` counts as durable. A field the engine failed to read is being treated as a promise.
4. **`api-key-only` is not enforced on every surface.** Only the spawn gate and the mid-run re-check
   exist, and both inspect the node's *own* directory. `auth/begin`, `auth/complete`, vault `PUT` and a
   vault-supplied `CLAUDE_CODE_OAUTH_TOKEN` at spawn are all ungated today. wheel-harness-auth.md
   enforcement points 1–3 were assigned to "API", and the wheeld proposal left that ownership question
   open. The routes are in SDK's crate, so this change closes them (§6).

## 2. What Claude Code 2.1.269 actually does

Read from the installed bundle's strings. Nothing was executed, and the operator's real `~/.claude` was
not touched (handoff rule). This is **not** verified against a live token exchange; §9 lists the
one-time live check.

- **Store.** `$CLAUDE_CONFIG_DIR/.credentials.json` holds
  `{"claudeAiOauth":{accessToken,refreshToken,expiresAt,scopes,subscriptionType,rateLimitTier}}`. The
  account identity is **not** in that file. It lives in `$CLAUDE_CONFIG_DIR/.claude.json` →
  `oauthAccount{accountUuid,organizationUuid,emailAddress}`.
- **In-process refresh.** It POSTs `{grant_type:"refresh_token",refresh_token,client_id,scope}`.
  `refresh_token: F = e` keeps the old refresh token when the response carries none, so the server *may*
  rotate and the client copes either way. The refresh is guarded by a lockfile
  (`tengu_oauth_token_refresh_lock_*`). That lock coordinates processes that share **one** config dir,
  and nothing else.
- **On `invalid_grant`** it marks the refresh token dead and tombstones the store on disk
  (`refreshToken:"", accessToken:"", expiresAt:0` — the record is emptied, not deleted). Precisely:
  this is reached from the CLI's **in-process refresh loop** (`grn`, called from the refresh error
  handler), which is exactly the path a CHILD takes under option (a) — so two children holding one
  rotating refresh token do not merely lose a race, the loser's store is emptied. It is NOT reached
  from `claude auth login`, the broker's own path, where there is nothing to wipe anyway: the broker
  renews in a throwaway directory.
- **The CLI's failure text carries no OAuth error code.** The token POST goes through axios, whose
  default `validateStatus` rejects anything >= 400, so what surfaces is axios's own error and the CLI
  prints `Login failed: Request failed with status code 400`. The `Token refresh failed: …` string in
  the CLI is unreachable for an HTTP failure, and the `invalid_grant`/`revoked` strings belong to the
  MCP OAuth code, not to this path. Anything keyed on those words is reading a dialect the CLI does
  not speak — and `CERT_REVOKED` from a TLS fault contains "revoked", so a substring match on it
  parks a board that only needed its network back.
- **`Login successful.` and exit 0 do not mean a credential was written.** The writer returns success
  without writing when `expiresAt` is NaN (`!e.expiresAt`, which an omitted `expires_in` produces) or
  when the scopes are not claude.ai ones, and the login path does not throw on either. The server has
  spent the refresh token regardless.
- **The writer clears before it writes.** `oV` deletes `claudeAiOauth` from the store ahead of the
  write, so a process death in that window leaves no credential at all. Harmless in a throwaway
  directory; a silent sign-out in a directory somebody depends on. This is why the staging and
  renewal directories are empty and disposable, and why that is load-bearing rather than tidy.
- **An env `CLAUDE_CODE_OAUTH_TOKEN`** is a bare access token that cannot be refreshed.
- **Headless refresh, CLI-native.** `CLAUDE_CODE_OAUTH_REFRESH_TOKEN=<ort>
  CLAUDE_CODE_OAUTH_SCOPES="<scopes>" claude auth login` makes the CLI exchange the refresh token with
  its **own** client id (`CLAUDE_CODE_OAUTH_CLIENT_ID` is only an override, and we never set it). It
  writes the store, prints `Login successful.` and exits 0. On failure it prints `Login failed: …` and
  exits 1. There is no browser and no model call.
- **The expired-login banners** are `Login expired · Please run /login` and
  `OAuth token revoked · Please run /login`. `ClaudeDriver::classify_startup_failure` already maps them
  to `needs_auth`.

## 3. ADVERSARY threat model

**Assets.**
- **R**, the refresh token: persistent account access until revoked. This is strictly more than today's
  8 h access token.
- **A**, the access token.
- **V**, vault integrity: which account every peer runs as.
- **B**, board availability.

**Actors.**
- **T1**, an agent: untrusted RCE with `bypassPermissions` and a shell (§2), prompt-injected or
  malicious. Today it runs as the *same uid* as every sibling and as the engine (F007/037).
- **T2**, a hostile dependency in a workspace. It is the same as T1.
- **T3**, the operator (trusted).
- **T4**, the token server's behaviour: rotation, revocation, lifetimes. It is not adversarial, but it is
  not under our control.

**The honest baseline.** Under one uid, T1 with arbitrary code can already read every node's creds dir
and `/proc/<engine>/environ` (`WHEEL_VAULT_KEY`), and so decrypt every vault (037). No file-level check
holds against a *fully* malicious T1 until per-node uids (M2/M3). The design is therefore judged on two
things:

1. It must close the paths that need **no tampering at all**: an agent that runs `claude auth login` or
   `/login` into another account in its own HOME, which the CLI itself then writes.
2. It must be **structurally** correct on the day per-node uids land, not correct by a heuristic that
   the uid work would then have to trust.

**Threats** (each maps to a test in §8):

| # | Threat | Class |
|---|---|---|
| TH1 | Promotion: an agent-planted credential (another account, or garbage) is written back into the shared vault, and every peer then runs as, or bills, an agent-chosen account or bricks | S1 (the rule's own class) |
| TH2 | Rotation race: N holders of one rotating R. The first refresh wins, every other holder gets `invalid_grant`, and the CLI then wipes its store. The board bricks until a human re-authenticates, which is *worse* than the 8 h problem | availability |
| TH3 | R exfiltration: every child that holds R is a place R can be stolen from | confidentiality |
| TH4 | Stale clobber: a slow write-back lands after the operator re-authenticated and replaces the new lineage with the old one | integrity |
| TH5 | Silent loop: refresh fails → restart → fail … with no one told | availability, operator trust |
| TH6 | Policy bypass: a cloud (`api-key-only`) project obtains or runs an OAuth credential through a vault or a route | compliance |
| TH7 | R on a command line, in a log or in a transcript | confidentiality |
| TH8 | Resurrecting a dead pair: re-seeding a copy the server already rotated away | availability |

## 4. Options

### (a) Child self-refresh plus a validated write-back (the handoff's shape)

Seed every child's config dir with the whole object and let the CLI refresh in place. Afterwards,
harvest what it wrote and promote it to the vault if it looks like a refresh of the same lineage.

- **The gate has nothing firm to stand on.** The store file carries no identity, and the identity file
  (`.claude.json`) is written by the same agent-controlled process. "Newer expiry, changed access token"
  is true of *any* fresh login, including the agent's own `/login` into account B. The one strong
  property is *who held R*.
- **The race (TH2) is inherent.** Every child holds R, so every child is a refresher. The CLI's lock only
  covers one shared config dir, and per-node dirs are the isolation boundary, so they cannot share one.
  Serialising means an engine lease: one child gets R, the others get access-only files. Followers must
  still be restarted on rotation. When the lease-holder is idle near expiry the lease has to be moved,
  which means parking the holder, and that is a state machine. Losing the race wipes a store.
- **TH3.** At least the lease holder holds R.
- **Verdict:** workable, but the security gate is heuristic and the lease is where the bugs would live.

### (b) An engine-side broker that calls the token endpoint itself

It serialises cleanly, but Wheel would become an OAuth client presenting **Claude Code's client id**.
Anthropic scopes subscription OAuth to Claude Code (and Claude.ai). A third-party process presenting
Claude Code's client id to the token endpoint is exactly the use those terms exclude. **Plainly: not
allowed. Rejected.** It would also hard-code the CLI's token URL, scopes and client id into Wheel, which
is brittle on a surface Wheel does not own.

### (b′) A broker that delegates the exchange to the CLI's own headless refresh (**proposed**)

There is one refresher per credential source, serialised by an engine mutex. The exchange itself is
`claude auth login` with `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`: Claude Code refreshing its own token with its
own client, the same call it makes in-process. Wheel only decides *when*.

- **TH1 is closed structurally.** Children never receive R. They get `CLAUDE_CODE_OAUTH_TOKEN=<access>`,
  which is exactly today's env path. **Nothing in any agent-writable directory is ever read back into a
  vault.** The write-back source is an engine-created 0700 directory, made per refresh and deleted after,
  and it is validated anyway (§5.3) as defence in depth for the single-uid period.
- **TH2 is impossible by construction.** Each lineage has one holder. Concurrent spawns wait on the
  lineage mutex and re-check under it (double-checked), so N agents cost exactly one exchange.
- **TH3 shrinks.** Agents see access tokens, as today. R lives encrypted in the vault, and in the
  refresher child's environment for the ~1 s it runs. That environment is readable by the same uid, the
  same class as 037.
- **ToS posture** is unchanged from today's operator-accepted one (auth-model-tos-risk.md, FINAL): Claude
  Code performs every exchange; Wheel is not an OAuth client. It runs only where OAuth is already
  permitted (self-hosted, allowlisted).
- **Cost.** It depends on a CLI feature (§2). If that feature changes, refresh fails **closed**:
  `needs_auth` with the CLI's own words, never a silent loop.

### (c) One OAuth login per agent, with no sharing

It already works (§1.1). There is no promotion, no race and no broker. The cost is N browser round-trips,
**once** rather than every 8 h, and each node dir holds its own R, so an agent can exfiltrate only its
own. It stays supported unchanged, apart from the capture hardening in §5.1.

| | (a) self-refresh + gate | (b) engine client | **(b′) CLI-delegated broker** | (c) per-agent |
|---|---|---|---|---|
| Agent-writable source in the promotion path | **yes** (the gate is heuristic) | no | **no** | n/a (no promotion) |
| Rotation race | lease required | none | **none (single holder)** | none |
| R held by children | lease-holder | none | **none** | each its own |
| Uses Claude Code's client id from Wheel | no | **yes: not allowed** | no | no |
| Logins | 1 | 1 | **1** | N (once) |
| New moving parts | lease + harvest + gate | HTTP client | mutex + timer + CLI call + gate | none |

**Decision: (b′)**, for vault-shared logins, with (c) unchanged for per-node logins. Built and
merged into this branch; §11 records what implementation changed. The VPS case does not move the
choice — it sharpens it. With no API-key fallback, (a)'s rotation race is not a degraded mode but a
bricked board: the loser of a race has its store wiped by the CLI itself (§2), and on a VPS reached
through an SSH tunnel the recovery is a person with a browser. (b′) has one refresher per credential
source, so the race cannot happen. If PM rules (b′) out, the fallback is (a) with a lease (§7).

## 5. Design (b′)

### 5.1 Capture: the only way a session enters a vault

- `auth/begin` runs the paste-code login in an **engine-owned staging dir** (`<data>/oauth-staging/<uuid>`,
  0700), not in the node's HOME. No agent process ever has that directory as its HOME or cwd, so nothing
  planted in a node dir can be mistaken for what the login produced.
- On `auth/complete` with `save_to_vault`, the session is read **strictly**:
  - only `claudeAiOauth` from `<staging>/.credentials.json`;
  - it must be a regular file, not a symlink, and within a size cap;
  - identity comes from `<staging>/.claude.json` `oauthAccount`.

  If the session carries R and scopes, it is stored as vault key **`CLAUDE_OAUTH_SESSION`** =
  `{"claudeAiOauth":{…},"oauthAccount":{accountUuid,organizationUuid}}`, and the row's `expires_at`
  column records the access expiry. A legacy `CLAUDE_CODE_OAUTH_TOKEN` in the same vault is removed,
  because the session supersedes it and both present would be ambiguous for the harness. With no R, the
  old behaviour applies: the access token plus the ADVERSARY-021 `shared_expiry` gate.
- On `auth/complete` **without** `save_to_vault`, the staged store is installed into the node's own dir.
  That is option (c), now also free of the planted-file race.
- `PUT /v1/vault/:id/CLAUDE_OAUTH_SESSION` is **refused, always**. A session only ever comes from a login
  the engine itself ran.

### 5.2 Distribution: children get the access token only

- `vault::env_for_agent` exports `CLAUDE_CODE_OAUTH_TOKEN=<accessToken>`, derived from the session. The
  session JSON is never exported.
- `wheel secret get <vault>/CLAUDE_OAUTH_SESSION` is refused, so R never reaches an agent through the
  board.
- For ambiguity, the session **is** the `CLAUDE_CODE_OAUTH_TOKEN` slot. Two vaults supplying either one
  to one agent is refused at the wire, the PUT and the spawn, as today.

### 5.3 Refresh, and the write-back gate

`ensure_fresh(vault)` takes the per-vault mutex and re-reads the vault. If more than `lead` remains
(30 min in production) it returns. Otherwise:

1. Run `claude auth login` through `child_command`, so the env is cleared and the allowlist applies. Set
   `CLAUDE_CONFIG_DIR=HOME=<fresh 0700 dir>`, `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`,
   `CLAUDE_CODE_OAUTH_SCOPES`, `stdin=null`, a 60 s timeout and kill-on-drop. R goes in env, never in
   argv (TH7). The CLI's output is redacted of R and A before it is logged.
2. Read the candidate from `<dir>/.credentials.json` (the CLI's own store path, regular file, no symlink)
   and `<dir>/.claude.json`.
3. **Gate** (`auth::check_refresh`, pure). A candidate is accepted only if all of these hold:
   - an access token is present **and differs** from the previous one;
   - a refresh token is present;
   - `expiresAt` is present, **later** than the previous one, later than now, and no more than 400 days
     out;
   - the scopes are a **subset** of the previous ones (no escalation);
   - account and org **match** whenever both sides expose them.

   A missing expiry is a failure to read, never a promotion. Rejection names the check.
4. **Compare-and-swap:** write only if the vault still holds the R we refreshed from. If the operator
   re-authenticated mid-refresh, their lineage wins and ours is discarded (TH4).
5. The directory is deleted on every path.

### 5.3b The expiry ceiling: what Wheel records, not what the server claims

The refresh path asks for `expiresIn: 31536000` — one year — while the interactive login takes the
server's own value, about eight hours. **Nobody has observed which the server honours**, and if it is
the year, then after the first renewal every child would hold a year-long bearer token instead of an
eight-hour one: the same theft, two orders of magnitude more blast radius.

So the answer is not trusted. Wheel records at most `MAX_RECORDED_LIFETIME_MS` — **12 hours** —
whatever comes back, and reports it when the clamp fires. Twelve hours sits comfortably above the
~8 h an interactive login is understood to last (so a normal renewal is never disturbed) and far
below a year.

The clamp bounds **how long Wheel uses one token**, not how long that token is valid — only the
server can say that, and it cannot be changed from here. What it buys is that children are rotated on
Wheel's schedule rather than the server's, and that the discrepancy becomes a fact in the log instead
of an assumption in a proposal.

### 5.4 Scheduling (the engine stays at ~0 CPU)

- One sleeping timer per lineage fires at `expiresAt − lead`.
- On boot, timers are re-armed from `vault_values.expires_at`, which needs no decryption.
- A spawn calls `ensure_fresh` (the lazy path).
- Nothing polls.

### 5.5 Running children adopt the new token

Each child records the generation it was spawned with (the session's `expiresAt`).

- After a refresh, **idle** readers are recycled at once: kill, keep the session, `parked`, then
  `deliver`, which resumes them with the new env.
- **Busy** readers are recycled at the end of their turn instead of pumping the next message.
- A runtime auth failure on a **stale** generation recycles once.
- On the **current** generation it forces one refresh, and only if the token is within `lead` of expiry.
  Otherwise the agent goes to `needs_auth`. That bound is what keeps an account that refreshes fine but
  is refused for another reason from looping (TH5).

### 5.6 Surfacing expiry

- `GET /v1/agents/:id/auth` returns `mode: env` with `source` and `expires_at` (the access expiry), plus
  new fields:
  - `refreshable: true`;
  - `warning`, whenever the last refresh failed. The warning names the vault, the reason and the time
    at which agents will stop.
- **Before** hard failure: every running or parked reader gets a `node.state` event whose `last_error`
  carries that warning, with its status unchanged. A later successful refresh clears it.
- **At** hard failure: readers go to `needs_auth` through the spawn gate or the runtime classifier, and
  the in-flight message is requeued, never consumed.

### 5.7 When refresh fails

- **Permanent** failures (`invalid_grant`, `invalid_refresh_token`, `expired_refresh_token`, `revoked`)
  are not retried. The warning goes up, and readers reach `needs_auth` when the access token lapses.
- **Transient** failures are retried every 5 min, only while the access token is still valid. Every
  attempt is logged on the engine log.
- **No loop:** a `needs_auth` agent is never restarted by delivery. Recovery is either the operator's
  re-auth (`save_to_vault` now resumes *every* reader stuck in `needs_auth`, not only the one that
  signed in) or an explicit start.

## 6. `api-key-only` on every surface

Under `api-key-only`:

- `auth/begin` answers `403 policy_denied`.
- `auth/complete` refuses `code`, `setup_token` and an OAuth-shaped `api_key`.
- Vault `PUT` refuses an `sk-ant-oat…` value under **any** key, because an agent can export any key it
  can read.
- Spawn refuses a vault-supplied `CLAUDE_CODE_OAUTH_TOKEN` or any `sk-ant-oat…` vault value.
- The broker refuses to run.

The existing node-dir spawn gate and the 60 s re-check are unchanged. So is wheel-host's default.

## 7. Fallback if PM rules (b′) out: (a) with a lease

The lease is one holder per lineage, the only child seeded with R. Followers get access-only files.

- **Harvest** happens only from the holder, only from `$CLAUDE_CONFIG_DIR/.credentials.json`, and only
  through the same `check_refresh` gate plus "the holder was the one seeded with R".
- **Release** happens on park or exit. A holder that is idle within `lead` of expiry is parked so the
  lease can move.

Every other rule (CAS, surfacing, no-loop) carries over. The extra state is the lease table and its
release paths.

## 8. Tests (mutation-checked; each restores its bug and watches the test go red)

The tests use `qa/harness/fake-claude` extended with a **fake token store**: a JSON file standing in for
the token server, which mints access/refresh pairs with a short lifetime, rotates refresh tokens
(single-use), and validates each turn's access token. The fake also serves the CLI-native refresh
(`CLAUDE_CODE_OAUTH_REFRESH_TOKEN` + `auth login`) and the paste-code login. It never touches a real
`~/.claude`.

| Threat | Test |
|---|---|
| acceptance | an agent keeps answering past its token's original expiry with no human touch, and the vault ends up holding the refreshed pair |
| TH1 | an agent-planted credential of another account in its own dir (and in the HOME layout) is **not** promoted, whether by a refresh or by a `save_to_vault` login; a refresher output naming another account is refused and the vault is unchanged |
| TH1 | `check_refresh` refuses each failure shape (same token, older expiry, missing expiry, missing R, scope escalation, account/org change, absurd horizon) |
| TH2 | two agents on one credential both keep working across a rotation; N concurrent `ensure_fresh` calls cost exactly **one** exchange |
| TH4 | an operator re-auth during a refresh is not clobbered |
| TH5 | a failed refresh warns before expiry (a `node.state` event with `last_error`), then `needs_auth` with the message still queued, and **one** attempt rather than a loop |
| TH6 | `api-key-only` refuses OAuth-shaped credentials on begin, complete, vault PUT and spawn, and the broker never runs; wheel-host's default is still `api-key-only` |
| TH7 | the refresh token is never in argv and never in a log line |
| inversion | `is_long_lived` is true only when the route asserted durability; a store entry with no expiry is *unknown* |

## 9. What the operator does on the VPS

The board is reached through the SSH tunnel, so every step below is an API call to the engine (or the
same thing through wheeld's UI). Nothing opens a browser on the server.

1. **Once per host:** the VPS needs `claude` ≥ 2.1.269 on PATH (`claude --version`). That is the
   build whose headless refresh this depends on (§2).
2. **Create one vault** — say `anthropic` — and draw a `read` wire from every agent that should use
   the account. One vault per account (M1.6); agents wired to it share that login.
3. **Start the sign-in** on any ONE of those agents:
   `POST /v1/agents/<agent>/auth/begin` → `{mode: "paste_code", url, session}`.
4. **Open that URL on their own laptop**, sign in to Anthropic, and copy the code it shows.
5. **Finish it**, naming the vault:
   `POST /v1/agents/<agent>/auth/complete {"code": "<pasted>", "session": "<from begin>",
   "save_to_vault": "anthropic"}`.
   The response carries `vault.key = CLAUDE_OAUTH_SESSION`, `vault.refreshable = true` and the
   current token's `expires_at`.
6. **Read back what the server actually issued.** The sign-in writes a line to every reader agent's
   engine log (`GET /v1/agents/<agent>/log?stream=engine`, or the log pane):

   ```
   oauth sign-in on vault anthropic: login valid for 480 min, scopes [user:inference user:profile]
   ```

   and the same line, reading `oauth renewal …`, after each automatic renewal. If the server issued
   more than the ceiling it says so explicitly:

   ```
   oauth renewal on vault anthropic: login valid for 720 min, scopes [...]; CLAMPED: the server
   issued 525600 min, held to 720 min
   ```

   **This is the live check §2 could not do.** The first real sign-in and the first real renewal
   settle three things a disassembly cannot: the actual lifetime the server grants, the scopes it
   echoes back, and whether the one-year `expiresIn` is honoured. No token, refresh token, or
   anything derived from either appears in these lines.

7. **That is the last human step.** From then on the engine renews the login about 30 minutes before
   each expiry, and every agent wired to that vault picks the new token up at its next turn
   boundary. `GET /v1/agents/<agent>/auth` shows `refreshable: true`, an `expires_at` that moves
   forward on its own, and a `warning` if a renewal ever fails.
8. **If something is wrong**, the operator sees it BEFORE the board stops: the `warning` on
   `GET auth` and a `node.state` event carrying it, while agents keep running on the current token.
   Only when the token really lapses do the agents park `needs_auth` with their messages still
   queued — and signing in again (step 3-5) resumes every one of them.

What NOT to do: do not paste a laptop's own `~/.claude/.credentials.json` into the vault. That
laptop's Claude Code holds the same single-use refresh token and will rotate it, and whichever side
loses that race has its store wiped. `PUT /v1/vault/:id/CLAUDE_OAUTH_SESSION` is refused for exactly
this reason — a login only ever enters a vault through a sign-in the engine ran itself.

## 10. Residuals (tracked, not closed here)

- **Single uid.** An agent can read `WHEEL_VAULT_KEY` from `/proc` (037), and R from the refresher's env
  during its ~1 s run. Per-node uids (F007) close both.
- **CLI dependency.** Refresh fails closed if `CLAUDE_CODE_OAUTH_REFRESH_TOKEN` changes meaning.
- **Old tokens.** Whether an old access token dies at refresh is unknown. §5.5 recycles either way.
- **Refresh-token lifetime** (`refresh_token_expires_in`) is unknown. Proactive refresh keeps R in use.
- **macOS local without the Desktop profile** (§0).

## 11. As built (what implementation changed, and what it found)

Design unchanged; these are the corrections the work itself produced. Each has a mutation-checked
test.

1. **AgentGrid contract.** `GET /v1/engine` (PR #58) gains the feature id **`oauth_refresh`**: the
   engine renews a vault-held login itself, and `GET auth` reports `refreshable`, `expires_at` and
   `warning`. Both OAuth ids are absent on `api-key-only`, so a client feature-detects rather than
   guesses. Documented in PROTOCOL.md's discovery table.
2. **`api-key-only` was never enforced on the routes.** The policy doc claimed `auth/complete`
   refused OAuth credentials; no such check existed, and `auth/begin` would run a whole login whose
   product the spawn gate then refused. Now `auth/begin` answers `403 policy_denied` before any
   child exists, `auth/complete` refuses `code`/`setup_token`/an OAuth-shaped `api_key`, vault `PUT`
   refuses an `sk-ant-oat` value under any key, and spawn refuses what a VAULT supplies as well as
   what is in the node's own dir. `config.rs`'s doc now says what is true.
3. **"Idle" in the database is not "not busy".** `init` sets `idle` even when a turn is already in
   flight, so adoption judged by status recycled a child mid-turn. Busy is now the supervisor's own
   `in_flight`, per §3c#15.
4. **A recycled child's turn was stranded.** Recycling takes the slot, so `reap` will not settle the
   child it kills; the in-flight message sat `delivered` for ever. It is requeued by the recycle.
5. **`live_agents` counted agents that had ever started**, not agents holding a process — and
   `api::stalled_agents` reads it to tell a turn in progress from a wedge, so the one state the
   system cannot leave on its own was invisible to the healthcheck. Fixed, with a test.
6. **A renewal has to be pending for every login**, not only after a renewal or a restart: a spawn
   that finds the login still fresh arms the next one, or a board that never restarts never
   schedules its first.
7. **A failed renewal parks the agent.** Without it a warm child spends one turn per queued message
   being told the same thing; `pump_queue` also refuses to write to an agent in `needs_auth`.
8. **The schema drift test compares file NAMES, not contents**, so `docs/schema` was stale while the
   gate was green. Regenerated in this change; worth a QA ticket of its own.

### Round 2 (ADVERSARY review of the implementation)

9. **The dead-refresh classifier read a dialect the CLI does not speak.** It matched
   `invalid_grant`/`revoked` as substrings; the real CLI prints axios's `Request failed with status
   code 400` and no OAuth code at all, so a permanently dead token was classified transient and
   retried for ever — a silent loop on a live box — while `CERT_REVOKED` from a TLS fault was
   classified permanently dead. It now keys on **exit status plus the HTTP status parsed from the
   message**: 400–403 is a refused grant (permanent), 429/5xx is worth retrying, and an unreadable
   failure is retried a BOUNDED number of times and then stops driving itself. The QA fake was fixed
   FIRST, to emit what the real binary emits; the tests went red; only then was the classifier
   changed.
10. **A `stop` during a renewal did not stop the agent** — a regression introduced by the fix for the
    freeze below. `start` releases the slot across the renewal, so a stop took an empty slot, reported
    success, and the renewal then spawned a child anyway on a token minted after the operator stopped
    it. A stop epoch is captured before the renewal and re-checked when the slot is taken back.
11. **A healthcheck during a renewal froze the board.** `live_agents` awaited every slot while holding
    the map lock, and `start` held one slot across the CLI's 60 s budget; one `/healthz` then stalled
    every other agent. Handles are cloned, the map lock is released, and each slot is taken with
    `try_lock` (a slot in transition counts as live); the renewal happens outside the slot lock.
    Measured before the fix: an unrelated start waited 4.7 s.
12. **A renewal schedule that can compute "now" is a loop.** `expires - lead` is already in the past
    for any token shorter-lived than the lead, so the wait floored at zero and the engine renewed
    again immediately — ADVERSARY measured 40 unrequested exchanges in two seconds. Timers now have a
    floor, and a login too short-lived to schedule is not armed at all (it is renewed on demand, which
    is bounded by messages arriving) and says so.
13. **A tool node could put the login on the wire.** `resolve_vault_fills` had no session gate, so a
    tool wired to the vault could send the refresh token upstream as a parameter. It now refuses the
    key exactly as `wheel secret get` does.

### Round 3 (defect #8, PM-assigned; residuals closed)

14. **Every `check_refresh` rejection was classified permanent.** The server is not guaranteed to
    rotate the refresh token on every exchange (§2), so a rejection that is not itself
    security-relevant (a bad read, a stale expiry, the same access token echoed back) could in
    principle pass on a genuine retry with the SAME refresh token — treating it as unrecoverable
    was giving up sooner than the credential actually was. `RefreshRejected::is_permanent` now
    keys the two apart: `ScopeEscalation`, `ScopeLost`/`ScopeDropped` and `AccountChanged` are
    identity/grant-narrowing and stay permanent (a retry cannot fix a wrong account or a server
    that is handing back less than it was); everything else (`NoAccessToken`, `SameAccessToken`,
    `NoRefreshToken`, `NoExpiry`, `NotNewer`, `Implausible`) is `ambiguous` — bounded by the same
    attempt cap as a CLI failure nothing could read, rather than parked on the first bad read.
15. **The scope ratchet only defended `user:inference`.** `REQUIRED_SCOPE` refused a renewal that
    dropped the inference scope, but a server that quietly echoed back a narrower grant on any
    OTHER previously-held scope passed. `check_refresh` now refuses dropping any scope the prior
    login had (`RefreshRejected::ScopeDropped`), not only the one the CLI cannot run without.
16. **`ensure_fresh` enforced no minimum interval on its own.** The timer's own wait floors at
    `min_interval`, but a spawn and `recover_from_auth_failure` call `ensure_fresh` directly, with
    nothing between them and the lineage lock — several such callers arriving close together on
    one stale, still-failing generation serialised straight through into one real exchange per
    caller instead of one per floor interval. `ensure_fresh` now tracks the last attempt per vault
    and floors on it the same way the timer does.
17. **`arm_renewal`'s doc comment named `Broker::min_interval`** as the guard it applies, when the
    code beneath it (correctly) keys on `Broker::lead` — the exact distinction the comment two
    lines below it draws. Fixed to name the right field.

### Deferred to a follow-up, deliberately (they are real, and none blocks the board)

- `resume_readers_if_blocked` — the documented VPS recovery path — has no test of its own.
- A table in PROTOCOL.md swallowed the sentence after it, which now renders as a third cell.

### Still true, and still residual

* Local macOS **without** the Desktop profile stores the login in the Keychain, not in a file, so
  neither the capture nor the renewal here applies to it (§0). The bundle shows secure storage with
  a plaintext fallback and no env var that forces the file, so this is a boundary, not a fix — the
  Desktop profile (PR #58) is where local-macOS OAuth belongs.
* The single-uid residual (037) is unchanged: until per-node uids, an agent can read
  `WHEEL_VAULT_KEY` from `/proc` and decrypt the vault directly. The write-back path is
  structurally correct for the day that lands — nothing agent-writable is read back — which is the
  property this design was chosen for.
