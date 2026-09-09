# Proposal: `WHEEL_HARNESS_AUTH` — a deployment-level policy gate on credential kind

Status: **proposed, not started.** Doc only — no engine code in this change. Requires a PM ruling to
merge; ADVERSARY review before implementation (this is a security-relevant gate, and per §2 the design
must assume the agent will try to route around it, not just fail to notice it).
Author: SDK. Date: 2026-09-09. Answers `docs/wow-agent-brief.md` task 2, informed by the risk this task
exists to close (`docs/proposals/auth-model-tos-risk.md`).

## What already exists (read before designing anything new)

Task 2's brief describes the goal as "an env var that selects *how* the harness authenticates... an
API-key mode that passes an Anthropic API key to the harness the supported way." That mechanism is
**already built** — this proposal is not that. Grounding, from the current engine
(`crates/wheel-engine/src/auth.rs`, `vault.rs`, `supervisor/mod.rs`):

A node's credential can come from **three surfaces today**, all already routed correctly regardless of
kind:

1. **Per-node stored token** (`auth.rs::TOKEN_FILE`, `wheel-token`, 0600 in the node's own config dir).
   Set via `POST /v1/agents/:id/auth/complete` with either `setup_token` (a `sk-ant-oat…` long-lived
   Claude credential) or `api_key`. `classify_token` reads the *value*, not a caller-supplied flag, to
   tell them apart (only the `sk-ant-oat` prefix is treated as OAuth; everything else — including
   gateway/proxy keys with no Anthropic prefix — is an API key). `token_env` then picks the right
   variable: `CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` / `CODEX_API_KEY` (never `OPENAI_API_KEY` —
   codex does not read it).
2. **The harness's own native login store** — `claude auth login` (paste-code) writes
   `.credentials.json` into the node's `CLAUDE_CONFIG_DIR`; `codex login` (device-code) writes
   `auth.json` into `CODEX_HOME`. `has_stored_credentials` checks for these directly; there is no
   `wheel-token` file involved at all on this path.
3. **A wired vault**, exported as env vars at spawn (`vault::env_for_agent`), taking precedence over
   surface 1 when both are present ("a vault-supplied credential wins over a pasted one" — the vault is
   what the operator can see and change).

So: **"how does API-key mode reach claude/codex" is already answered — env var, already wired, already
correct for both harnesses.** Nothing here needs new plumbing to make an API key work. What's actually
missing is a *policy gate*: something that can refuse an OAuth-shaped credential on a deployment that
must not use one, without touching any of the three surfaces above for the API-key path, which must
keep working exactly as it does today.

## The actual problem: surface 2 is not API-mediated

Surfaces 1 and 3 are both created through engine API routes (`auth/complete`, vault `PUT`) — an
enforcement point that runs there sees the value before it is ever stored. Surface 2 is not: `claude
auth login` and `claude setup-token` are commands the harness process itself can run, and an agent is
**untrusted code running as a normal shell inside its own sandbox** (contract §2: nothing relies on the
agent restraining itself). Nothing stops an agent — or the paste-code flow the operator's own `auth/
begin` starts — from producing a native `.credentials.json` that no API route ever inspected. A gate
that only checks `auth/complete` and vault `PUT` would be real but incomplete: nothing here
lets an agent self-provision its way past a project meant to be API-key-only unless the spawn path
itself is where the policy is enforced.

**Consequence for the design:** the API-level checks (below) are worth having — they fail fast and give
a clear error instead of a credential silently sitting on disk — but the gate that actually holds is
**at spawn**, inspecting whatever the resolved credential turns out to be, the same way every other
capability in this engine is enforced engine-side rather than trusted to the agent (contract §2's
general rule, applied here).

## Design

### Where the switch lives: deployment env, not vault key, not node config

Per-project (vault key) and per-agent (node config) were the two options the brief asked to choose
between. Neither is right, for the same reason: **the thing being gated is which credential kind is
*allowed to exist at all* on this deployment**, and both a vault key and a node config field are things
the project's own owner (or an agent with a `write` wire to another agent, i.e. "manage") can set. A
policy that a resource's own owner can flip is not a policy — task 4 already says this in different
words ("the operator-exception must be a real allowlist, not a client flag").

**Proposal:** `WHEEL_HARNESS_AUTH` is an env var on the **engine process**, set once per project at
spawn time by whatever starts the engine — `wheeld` for self-hosted, `wheel-host` for cloud — the same
tier `WHEEL_ENGINE_SECRET` and `WHEEL_VAULT_KEY` already live at (§4 spawn contract). It is never
readable or settable through the project API, so no project owner — including the operator's own,
running through the same code path as everyone else — can self-escalate it.

Values: `oauth-token` (today's behaviour: either kind may be stored/used, unrestricted) and
`api-key-only` (OAuth-shaped credentials on any of the three surfaces are refused). Two values, not
three — brief suggested "at minimum oauth-token and an API-key mode" and nothing here needs a third.

**Default, unset:** `oauth-token` (today's behaviour). This directly satisfies "a board with only the
OAuth token set must keep working" — no existing self-hosted install or the operator's own board
changes behaviour by upgrading the engine binary. The restrictive mode is something the **cloud**
deployment opts into explicitly, not a new default silently applied everywhere.

**The operator exception (task 4):** `wheel-host` sets `WHEEL_HARNESS_AUTH=api-key-only` for every
engine it spawns *except* the operator's own project id, which it starts with `oauth-token` (or leaves
unset). This lives in `wheel-host`'s own spawn logic — a fixed project-id comparison, API-owned,
invisible to and unreachable from inside any sandbox. `wheeld` (self-hosted) does not set it at all by
default, matching task 4's "self-hosted is first-class, cloud is the restricted one": a person running
Wheel on their own machine, on their own subscription, is not the case this gate exists for.

### Enforcement points

1. **`POST /v1/agents/:id/auth/begin`** — when `api-key-only`, the response never offers `paste_code`
   or `device_code` for Claude (Claude's `setup_token` path is also an OAuth credential by value, so
   `auth/complete` catches it too, but not offering the flow at all is the better UX: no half-finished
   login to clean up). `api_key` remains offered unconditionally. Codex: see below, it needs its own
   handling.
2. **`POST /v1/agents/:id/auth/complete`** — reject if `classify_token` (already exists, already
   correct) resolves the submitted value to `CredentialKind::OauthToken`, or if the request used
   `setup_token` at all under `api-key-only`. Same error shape as the existing "not this harness"
   rejections in this file, naming the policy so the operator isn't left guessing.
3. **`PUT /v1/vault/:id/:key`** — reject a value that `classify_token` would call OAuth-shaped (the
   `sk-ant-oat` prefix is Claude-specific and unambiguous regardless of which agent eventually reads the
   key), under `api-key-only`.
4. **Spawn** (`supervisor/mod.rs`, right before composing the child's env) — the actual gate, per the
   reasoning above. Before starting the process: if `api-key-only` and either (a) `credential_env`
   resolves to `CLAUDE_CODE_OAUTH_TOKEN`, or (b) `has_stored_credentials` finds a native
   `.credentials.json`/`auth.json` whose content is OAuth-shaped (Claude: prefix check on whatever
   `oauth_token_from_store` would return, cheap and already-written code; Codex: see below) — refuse
   to spawn, agent goes to `error` (not `needs_auth`, which implies "log in and it'll work" — this is
   "this credential kind is not allowed here, use api_key instead"), with an error naming the policy.
   This is the ONE point that cannot be bypassed by an agent using its own shell, because it inspects
   the credential the harness is actually about to run with, not a value the API was asked to store.

### Codex needs different handling, not its own mode

Per `auth.rs`'s own comment, codex has no long-lived-token env var — its OAuth is a *session* written
to `auth.json` by `codex login`, not a bearer string `classify_token` can inspect. So "is this an OAuth
credential" for codex is not a value-prefix check, it's **"does `auth.json` exist as a login session at
all, as opposed to `CODEX_API_KEY` being set."** Concretely: under `api-key-only`, `auth/begin` never
offers Codex's `device_code` flow, and spawn (point 4) refuses if `CODEX_API_KEY` is absent from the
resolved env AND a codex login session exists on disk. This is the same policy as Claude's, expressed
against Codex's actual credential shape — not a second, differently-named mode. (If `CODEX_AUTH_JSON`
per-vault credential injection — mentioned as a later recognised key in the M1.6 contract section —
ships before this does, it needs the identical spawn-time check: a session written from a vaulted value
is still a session, not an API key.)

### Credential discipline (brief's requirement, already the codebase's own standard)

Nothing above requires printing, logging, or transmitting a credential's value anywhere it doesn't
already flow. `classify_token` is a prefix check on a value the engine already holds in memory at the
point it's being stored or resolved; the spawn-time check re-uses `oauth_token_from_store`, which
already exists for the vault-copy feature and already avoids writing the token anywhere but the
in-memory `StoredOauth`. The rejection error messages must name *why* (policy, not "invalid
credential") without echoing the value — matching the existing `auth_complete` error style, which
already does this for its other rejections.

## Open questions for reviewers (not resolved by this proposal)

- Exact wording of the `auth/begin`/`auth/complete` rejection body — API's call, since API owns those
  routes' response shape.
- Whether `api-key-only` should also block an agent from running `claude setup-token`/`claude auth
  login` *interactively inside its own turn* (i.e. detect and kill mid-flow) versus only refusing at the
  next spawn — this proposal takes the latter (simpler, still closes the gap, costs one restart cycle
  instead of zero) but a reviewer may want the former for a tighter window.
- Whether the project-id allowlist (task 4's operator exception) belongs in `wheel-host` config or as a
  literal constant — leaning config (an env var listing allowed project ids), so it isn't a code change
  to add a second exception later, but this is API's call since `wheel-host` is API-owned.

## Owners

SDK: `classify_token`/spawn-time check extensions in `wheel-engine` (points 2 and 4 above are already
adjacent to code SDK owns). API: `auth/begin` offer-suppression and vault `PUT` rejection (points 1 and
3, both in routes API owns per the ownership table), plus the `wheel-host` project-id allowlist for
task 4. Both halves are additive to existing routes, not new ones — no schema/wire-matrix change.
