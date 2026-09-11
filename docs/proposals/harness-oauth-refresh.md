# Proposal: harness OAuth with real refresh (self-hosted Wheel)

Status: **draft for PM ruling. REQUIRES ADVERSARY review before merge** (ARCHITECTURE.md,
"Credential-distribution rule": this changes `save_to_vault`, the credential lookup, the vault export
and expiry). Author: SDK (harness-oauth lane). Date: 2026-09-11. Branch `sdk/harness-oauth`.

Closes docs/handoff/sdk.md NEXT #1 (P1, the operator re-authenticates every 8 h).

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
- **On `invalid_grant`** it marks the refresh token dead and clears the store on disk
  (`refreshToken:"", accessToken:"", expiresAt:0`). Two holders of one rotating refresh token do not
  merely lose a race: the loser's store is wiped.
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

**Recommendation:** (b′) for vault-shared sessions, and (c) unchanged for per-node logins. If PM rules
(b′) out, the fallback is (a) with a lease. The extra state is sketched in §7.

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

## 9. What the operator does on the VPS (after merge)

1. Upgrade `wheeld`, and make sure the host's `claude` is ≥ 2.1.269 (`claude --version`).
2. Create one vault (e.g. `anthropic-me`) and draw a `read` wire from every agent that should use the
   account.
3. **Sign in once** through the web Authenticate panel on any one of those agents, with "save to vault"
   = `anthropic-me`. That is the only human step. `GET auth` on any reader then shows
   `refreshable: true` and an `expires_at`, and that value moves forward on its own about 30 min before
   each expiry.
4. Do **not** paste a laptop's `~/.claude/.credentials.json` into Wheel. The laptop's own Claude Code would
   refresh the same refresh token and the two would race (TH2). The engine refuses a PUT of the session
   key for this reason.
5. `claude setup-token` into a vault (`auth/complete {setup_token}`) remains the alternative with no
   refresh machinery: a long-lived token, no broker. Either works; a login is simpler if the web panel
   is in use.
6. **One-time live check** (the part §2 could not prove offline): after step 3, wait for the first
   refresh, or restart `wheeld` with less than 30 min left, and confirm `expires_at` advanced and the
   agents kept answering. If it did not, the engine log line `oauth refresh failed` carries the CLI's
   own words.

## 10. Residuals (tracked, not closed here)

- **Single uid.** An agent can read `WHEEL_VAULT_KEY` from `/proc` (037), and R from the refresher's env
  during its ~1 s run. Per-node uids (F007) close both.
- **CLI dependency.** Refresh fails closed if `CLAUDE_CODE_OAUTH_REFRESH_TOKEN` changes meaning.
- **Old tokens.** Whether an old access token dies at refresh is unknown. §5.5 recycles either way.
- **Refresh-token lifetime** (`refresh_token_expires_in`) is unknown. Proactive refresh keeps R in use.
- **macOS local without the Desktop profile** (§0).
