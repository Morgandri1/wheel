# Proposal: `wheel-host`'s operator allowlist for `WHEEL_HARNESS_AUTH` (wow-agent-brief task 4)

Status: **draft, sent for ADVERSARY review before code.**
Author: API. Date: 2026-09-10. Answers `docs/wow-agent-brief.md` task 4, sitting on top of the
already-accepted mechanism in `docs/proposals/wheel-harness-auth.md` (task 2, ADVERSARY-cleared,
engine spawn gate merged in `872ac31`).

## What already exists (read before reviewing anything new)

`wheel-harness-auth.md` designed and shipped the *mechanism*: `WHEEL_HARNESS_AUTH` is an engine-process
env var, `wheeld`/`wheel-host` set it, a project's own owner can never read or set it through any API
route. Two values, `oauth-token` (default, unrestricted) and `api-key-only` (OAuth-shaped credentials
refused on every surface, enforced authoritatively at spawn + a periodic re-check while running —
`crates/wheel-engine/src/supervisor/mod.rs`, `872ac31`).

`crates/wheeld/src/embedded.rs` (the self-hosted single-binary path) already hardcodes
`harness_auth: HarnessAuthPolicy::default()` (i.e. `oauth-token`) for every project it embeds — also
landed in `872ac31`. So **task 4's "self-hosted keeps today's unrestricted default" half is done.**
Nothing in this proposal touches `wheeld`.

**What is not done: `wheel-host` (`crates/wheel-host/src/sandbox/{docker,process}.rs`) does not set
`WHEEL_HARNESS_AUTH` in the spawned engine's env at all today** — grep confirms neither backend
references it. Every engine `wheel-host` starts today gets the engine's own default (`oauth-token`,
unrestricted), regardless of which project it is. That is the actual gap task 4 asks me to close: make
`wheel-host` itself enforce `api-key-only` by default, with a real, non-client-settable exception for
the operator's own project.

## Design

### The allowlist is host config, not a database value

Per `wheel-harness-auth.md`'s own open question ("leaning config... but this is API's call since
`wheel-host` is API-owned"): **config.** A new `wheel-host` env var,
`WHEEL_HARNESS_AUTH_OAUTH_PROJECTS` — comma-separated project UUIDs, parsed once at `wheel-host` boot
into `Config::oauth_allowed_projects: Vec<Uuid>`.

Why not Postgres (where `projects` already lives, and API already has a `capabilities` JSON column
precedent): the whole point of this control, stated in `wheel-harness-auth.md`'s own design ("a knob a
project's own owner could set would not be a policy") is that **nothing reachable through the project
API can influence it.** A database column is reachable by definition — anything that can run SQL
against the same table `PATCH /v1/projects/:id` writes is one migration/bug away from becoming
settable. An env var on the one process (`wheel-host`) that holds the deploy's actual secrets already
(`WHEEL_HOST_SECRET`) has no such surface: changing it requires redeploying `wheel-host` itself, which
only whoever controls that Railway service can do. Same trust tier as the thing it's gating.

### Default: deny (fail-secure, not fail-open)

Unset or empty `WHEEL_HARNESS_AUTH_OAUTH_PROJECTS` → the allowlist is empty → **every** project gets
`api-key-only`, including a project that happens to share the operator's id if the var were somehow
unset in production. This is the opposite of `WHEEL_HARNESS_AUTH` itself, which defaults *permissive*
(`oauth-token`) so upgrading the engine binary never silently changes an existing self-hosted board's
behaviour (`wheeld` has no such concern — it never reads this var, `wheeld` is not the "cloud" this
gates). `wheel-host` is unambiguously the cloud side, so its default must be the restrictive one:
a missing config value here should never be the reason an unintended board gets the OAuth-eligible
path. The operator's own board keeps working only because the allowlist is deliberately configured
with its project id on the actual deployment — a decision to make, not a default to fall into.

### Boot-time validation, not silent drop

Same discipline `crates/wheel-engine/src/config.rs`'s `harness_auth()` already established for
`WHEEL_HARNESS_AUTH` itself ("an unrecognised value is a boot failure, not a silent default" —
`an_unrecognised_harness_auth_value_is_a_boot_failure_not_a_silent_default`): a malformed entry in
`WHEEL_HARNESS_AUTH_OAUTH_PROJECTS` (not a UUID) fails `wheel-host`'s boot with the offending token
named, rather than silently dropping it from the list (which would look identical to "the operator
successfully exempted this project" right up until it doesn't).

### Where it's applied: both `wheel-host` backends, not the `docker` backend specifically

`wheel-harness-auth.md` says "`wheel-host`... for every engine it spawns" — not "the `process` backend
specifically." `docker` and `process` are both `wheel-host` (§ARCHITECTURE: "one binary above the
`Sandbox` trait, neither backend visible to the API"); the distinction that matters for this policy is
*which host is running*, not which backend it happens to use. A contributor's own `docker compose up`
against `infra/docker-compose.yml` is still `wheel-host`, so it inherits the restrictive default too —
consistent with task 4's actual framing ("cloud is API-key-only"; the docker-compose path in this repo
is a dev/production-topology-testing tool, not the recommended self-host path, which is `wheeld`). A
contributor who wants OAuth mode against the docker-compose stack sets the same env var locally, same
as anyone else.

Concretely, one new field on `wheel_host::Config`:

```rust
pub oauth_allowed_projects: Vec<Uuid>,

pub fn harness_auth_for(&self, id: &Uuid) -> &'static str {
    if self.oauth_allowed_projects.contains(id) { "oauth-token" } else { "api-key-only" }
}
```

and one new line in each backend's engine-env construction:
`docker.rs`'s container `env` vec and `process.rs`'s `engine_env()` — both already build a
`Vec` of `(K, V)`/`K=V` strings per project id, so this is an additive entry alongside
`WHEEL_PROJECT_ID`, not a new code path.

### What this does NOT touch

- `crates/wheel-engine` — already correct (spawn gate, mid-flow re-check, `Config::from_env` already
  reads `WHEEL_HARNESS_AUTH`). No engine change.
- `crates/wheeld` — already correct (never sets the var, gets the engine's permissive default).
- The three enforcement points `wheel-harness-auth.md`'s Owners section calls "API's" (`auth/begin`
  offer-suppression, `auth/complete` rejection, vault `PUT` rejection) — **open question below,** not
  assumed to be in this proposal's scope.

## Open question for SDK (asked directly, not guessed)

`wheel-harness-auth.md`'s Owners section assigns points 1 and 3 (`auth/begin`/`auth/complete`,
vault `PUT`) to "API... both in routes API owns per the ownership table" — but those routes are
implemented in `crates/wheel-engine/src/api/{agent_routes,vault_routes}.rs`, which is SDK's crate per
`docs/ARCHITECTURE.md` §4 ("Engine control plane... SDK owns, host+API proxy, Web consumes"). `wheel-api`
only proxies this path (`ANY /v1/projects/:id/engine/*`) verbatim; it does not parse or gate engine
request bodies today, and doing so here would mean the proxy stops being a proxy for exactly one route
family. I read this as either (a) a slip in that doc — SDK meant "these are enforced in wheel-engine,
which the API surfaces unchanged," and mechanically they're SDK's, or (b) a genuine ask for `wheel-api`
to add its own pre-check in the proxy layer as defense in depth. Asking SDK directly rather than
implementing a guess in either crate. This proposal's own scope (the `wheel-host` allowlist) does not
depend on the answer either way.

## What ships in this PR if cleared

1. `wheel_host::Config::oauth_allowed_projects` + `harness_auth_for`, boot validation, tests (parse
   empty/one/many/malformed; `harness_auth_for` for a listed vs. unlisted id).
2. `docker.rs` and `process.rs` set `WHEEL_HARNESS_AUTH` from it in the spawned engine's env; a test
   per backend asserting the exact value lands in the child's env for both cases.
3. `docs/API.md` / `infra/railway/README.md`: document `WHEEL_HARNESS_AUTH_OAUTH_PROJECTS`, and that
   production sets it to the operator's own project id (an ops action on the actual deployment, not a
   value committed to this repo).
4. README's self-hosting section gets `wheeld` promoted to the primary path per task 4's positioning
   directive (doc-only, no security edge — not blocked on this review, may land separately/first).

## Non-goals

- Does not change the operator's own board's behaviour (`wheeld` still runs it, unaffected either way
  per M1.6/M1.7 — task 4 explicitly says so).
- Does not resolve the SDK open question above; flagged, not blocking this PR's own scope.
