# Wheel shared projects — proposal (multiplayer M1)

SDK/multiplayer lane, 2026-09-12. Branch `sdk/shared-projects`.

## 0. The thing this is for

A second person — Souren — needs to see Morgan's board, on the live deployment at
`https://wheel.avo.so`, without using Morgan's login.

There is no sharing on this build at all. Projects are strictly owner-scoped, and the check is a
single SQL predicate; even the operator's `wht_` token gets a 404 on another user's project, because
the predicate does not care what kind of credential you hold. The two alternatives — hand over a
credential, or stand up a read-only mirror — were rejected in favour of building the access model
properly, once.

So the deliverable is concrete: **an admin can grant another account a tier on a project, and that
account then sees it in `GET /v1/projects` and can reach exactly what its tier allows.**

Two things follow from that framing and are worth stating before the design:

- **External pluggable auth (M0) is not a prerequisite and is not in this PR.** Both people will
  have local accounts on the same self-hosted deployment, so `AUTH_MODE=local` already suffices.
  M0 follows as its own change; membership keys off a Wheel principal, which local auth already
  provides.
- **This lands on a database that is already in use** — one user, one project, a real board. §4.2 is
  about that specifically, and it is the part of this document most worth attacking.

## 1. The thing being replaced

`ProjectScope` — the extractor a handler takes in order to name a project at all — resolves through
exactly one predicate:

```rust
// crates/wheel-api/src/auth/extractor.rs
    "SELECT ... FROM projects WHERE id = $1 AND owner_id = $2"
```

Its doc-comment calls it "the **only** function in the codebase that turns a project id into a
`Project`", and that is true, which is what makes this change tractable at all. Six further
`owner_id = $N` predicates ride on the same assumption (`routes/projects.rs`).

**This predicate is the entire authorization model.** Replacing it is a cross-tenant surface, so this
proposal carries its own threat model (§7) and expects a hostile review rather than a read-through.

## 2. The three tiers

Operator ruling, 2026-09-11. Exactly three, no fourth, and no tier with conditional powers.

| Tier | What it is |
|---|---|
| **admin** (root) | Everything. Board structure, members and invites, vault, project lifecycle, settings. |
| **prompter** | Manage context, and prompt agents: read the board, write ctx content, send messages, start/stop/restart agents. |
| **guest** | View only. Board, agent transcripts, logs, the events stream. Sends nothing. |

Ordered `guest < prompter < admin`; the whole check is `actual >= required`, so there is no table of
pairwise comparisons to get wrong. **The project's creator is always admin.**

### 2.1 What a guest can and cannot do

Stated explicitly, because it is the tier Souren gets first and the one whose boundary matters most.

**A guest can:** see the project and its status; read the board — nodes, wires, positions, agent
config; read any agent's transcript and logs; read table rows; read tool operation lists; see whether
an agent is authenticated; hold the events WebSocket and watch the board change live; see who else
is a member.

**A guest cannot:** send a message to an agent, interrupt or steer one, start/stop/restart anything,
write ctx content, write or query table rows, create/delete/rename/rewire any node, change agent
config, read or write any vault value — including the *list of key names* — attach or clear an
agent's LLM credential, invoke a tool, apply a board, change project settings or capabilities,
manage members or invites, or reach the `/v1/cli/*` realm at all.

The one thing a guest holds that is worth naming as a deliberate grant: **transcripts and logs**. An
agent's transcript contains everything it was told, which on a working board includes operator
instructions and injected context. Sharing a project with a guest shares that. It is the right
default for "see my board" and it is not a leak of *credentials* — vault values are redacted from
transcripts by the engine — but it is a disclosure, and an owner should know they are making it.

### 2.2 Boundary calls, as ruled

- A prompter **can start and stop agents**. Wheel's contract is that a message never starts a
  process, so a prompter who cannot start cannot prompt. `restart` goes with them: it is the
  composition of the two, and allowing both halves while refusing the whole is a rule with no
  content.
- **"Manage context" means what an agent reads, not the board's shape.** A prompter writes context
  content; a prompter does not create, delete, rename, reposition or rewire nodes, and does not
  change agent config — model, budget, harness.
- A prompter **never writes vault values and never sees them.**
- A guest **sends nothing** — no messages, no interrupts, no steering, no endpoint invocation.
- **Only admin manages members and invites.** A prompter cannot invite and cannot raise their own
  tier.
- **`/p/` public ingress stays outside the tier system** on its capability gate (§5.3).

### 2.3 Two places the ruling meets the code awkwardly, reported rather than worked around

**(a) "Manage context" has no route today, so this slice adds one.**
`PATCH /v1/nodes/{id}` carries `name`, `position` *and* `config` in one body, so ctx content and agent
config arrive through the same door. Granting a prompter that route would mean authorising on the
request *body* — the API parsing node config to decide permissions, duplicating engine knowledge, and
deciding about one thing while the engine acts on another. That is the conditional power the ruling
forbids, and it is the confusion `extractor.rs` already refuses for `x-project-id`.

So the policy table stays a pure `(method, path)` map with no body inspection, and the engine gains
one narrow route: **`PUT /v1/nodes/{id}/content`**, body `{markdown}`, refused for any node type
without operator-editable content. Tier: prompter. Same idiom as the existing
`PUT /v1/vault/{id}/{key}`.

**(b) A prompter cannot write table rows in v1 — a missing surface, not a tier decision.**
The ruling says a prompter writes "ctx content and table rows". There is **no control-plane
table-row write route for anyone**: `GET /v1/tables/{id}/rows` reads, and `POST /v1/tables/{id}/query`
is explicitly read-only SQL. Rows are written only by agents through `/v1/cli/write`. Granting it
means designing `POST /v1/tables/{id}/rows` — structured, never SQL, because the SQL door is
read-only on purpose. **Deferred and named** rather than smuggled in, and flagged as the one part of
the prompter tier that does not ship complete.

**(c) A consequence of "project lifecycle is admin".** Agent start/stop is prompter; *project*
start/stop is admin. So a prompter arriving at a stopped sandbox cannot start it. That follows from
the ruling and is implemented as ruled — recorded here because it lands hardest in M4, where a
shared session is blocked for everyone by a stopped sandbox.

## 3. Schema — migration **0006**, both dialects

`0005_api_token_sessions.sql` already exists from the headless-first work, verified by listing the
directories rather than by reading a plan. Ours is **0006**.

```sql
CREATE TABLE project_members (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id    text NOT NULL,
    role       text NOT NULL CHECK (role IN ('admin', 'prompter', 'guest')),
    invited_by text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX project_members_user_idx ON project_members (user_id) WHERE revoked_at IS NULL;

CREATE TABLE project_invites (
    id uuid PRIMARY KEY, project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('admin','prompter','guest')),
    token_hash text NOT NULL UNIQUE, email text, created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz NOT NULL,
    max_uses integer NOT NULL DEFAULT 1 CHECK (max_uses > 0), uses integer NOT NULL DEFAULT 0,
    revoked_at timestamptz
);
```

`user_id` is `text`, matching `projects.owner_id` and `api_tokens.user_id`. SQLite gets the same
tables with `uuid → TEXT`, `timestamptz → TEXT` RFC3339, `integer → INTEGER`, per
`migrations_sqlite/0001_init.sql`'s stated translation rules.

Both statements are pure `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`. **No `ALTER`,
no `UPDATE`, no data migration, nothing that can fail against existing rows.** §4.2 is why.

## 4. `load_member` replaces `load_owned`

One query, one row, and authorisation stays a predicate in the `WHERE` clause rather than a
comparison after the fetch — the property `load_owned`'s doc-comment insists on, kept for its stated
reason: "row does not exist" and "row belongs to someone else" must remain one code path returning
one `NotFound`, so they cannot drift into an enumeration oracle.

```sql
SELECT p.id, p.owner_id, p.name, p.capabilities, p.status, p.created_at, p.updated_at,
       CASE WHEN p.owner_id = $2 THEN 'admin' ELSE m.role END AS role
  FROM projects p
  LEFT JOIN project_members m
    ON m.project_id = p.id AND m.user_id = $2 AND m.revoked_at IS NULL
 WHERE p.id = $1
   AND (p.owner_id = $2 OR m.role IS NOT NULL)
```

`ProjectScope` becomes `{ user, project, tier }` and still has no public constructor, so a handler
that acts on a project cannot be written without the check having run. The property `extractor.rs`
opens by describing is preserved and extended: from "is the owner" to "is a member, and which kind".

### 4.1 Why the creator is not a row in `project_members`

The role column admits all three tiers, so a second person *can* be made admin. But the creator's
admin comes from `projects.owner_id`, not from a row. A membership table that could *also* answer
"who owns this project" would be a second source of truth, free to disagree with the first — and a
disagreement about ownership is a cross-tenant bug.

The resolution is monotone and therefore cannot contradict itself:

    effective_tier(user) = Admin           if user == projects.owner_id
                         = member.role     otherwise
                         = <no access>     if neither

Being the creator only ever *adds* admin; it never subtracts. The one state that would look like a
disagreement — a member row for the creator — is refused at the API with a 409, so it never exists.

### 4.2 The live database, and why there is no backfill

**This is the requirement that would lock Morgan out of his own board if it were got wrong**, so it
is stated as a property rather than a procedure.

A backfill — "insert an `admin` row for every existing project's owner" — is the obvious answer and
the wrong one. Three reasons:

1. **It is the second source of truth again.** After a backfill, `owner_id` and a member row both
   claim the creator is admin. They agree on the day of the migration and nothing keeps them
   agreeing afterwards.
2. **It can fail, and it fails at the worst moment.** An `INSERT ... SELECT` against a live database
   is a write that can conflict, and a migration that half-applies leaves some projects reachable and
   others not.
3. **It only fixes the projects that exist when it runs.** Every project created afterwards needs the
   same row, from a different code path, and the two can drift.

Instead, **the creator's admin is derived, not stored.** The `CASE WHEN p.owner_id = $2` arm above
answers for every project that exists now, every project that existed before this migration, and
every project created after it, identically and without a row. A project with no `project_members`
rows at all — which is every project on the live deployment right now — still resolves its owner to
admin.

So migration 0006 creates two empty tables and touches no existing data. The safety property is not
"the backfill was correct", it is "**there is nothing to back-fill**".

This is testable, and is tested rather than asserted: `tiers.rs` inserts a project row directly, the
way the live database already holds one, with no member rows, and proves the owner still gets `admin`
and full access, while a non-member still gets 404. That test is the migration-safety check, and it
would go red if the derivation were ever replaced by a lookup.

`GET /v1/projects` becomes the union of created and joined projects, with the access rule in one
`WHERE` so the list and `load_member` cannot disagree about who can see what. The per-user quota
stays counted over `owner_id` only: a project you were invited to is not one you provisioned, and
charging a guest for it would let anyone exhaust anyone's quota by inviting them.

## 5. The policy table — default DENY

One table, consulted for every project-scoped route. **Anything not listed is refused.** Same idiom
as the wire matrix. Adding an engine route without adding a policy row makes it unreachable through
the API, which is the correct direction for a mistake to fall.

### 5.1 The API routes, each in exactly one tier

| # | Route | Requires |
|---|---|---|
| 1-2 | `GET /healthz`, `GET /v1/host/healthz` | public |
| 3-5 | `POST /v1/auth/signup`, `/login`, `/logout` | public |
| 6-11 | `/v1/auth/me`, `/password`, `/users`, `/tokens` × 3 | authenticated; no project, no tier |
| 12 | `POST /v1/projects` | authenticated; **creator becomes admin** |
| 13 | `GET /v1/projects` | authenticated; returns projects where tier ≥ guest |
| 14 | `POST /v1/projects/instantiate` | authenticated; creator becomes admin — **`HttpBoardClient`** |
| 15 | `GET /v1/projects/{id}` | **guest** |
| 16-17 | `PATCH`, `DELETE /v1/projects/{id}` | **admin** |
| 18-20 | `POST .../start`, `/stop`, `/restart` | **admin** (project lifecycle) |
| 21 | `POST .../ws-ticket` | **guest** |
| 22 | `POST .../board/apply` | **admin** — **`HttpBoardClient`** |
| 23 | `ANY .../engine/v1/events` | **guest** — ticket branch special-cased, §5.4 |
| 24 | `ANY .../engine/{*rest}` | per §5.2, default DENY |
| 25 | `ANY /p/{project_id}/{*rest}` | **outside the tier system**, §5.3 |

New: `GET/POST/DELETE .../members` (guest to read, **admin** to change),
`GET/POST/DELETE .../invites` (**admin** throughout), `POST /v1/invites/accept` (authenticated, no
tier — the caller is not a member yet, which is the point).

Routes 14 and 22 reach the engine through `HttpBoardClient` rather than through `routes/proxy.rs`,
and are called out because policy or actor work applied only at the proxy silently misses them.

### 5.2 Engine control-plane routes (the `{*rest}` of route 24)

| Path | Methods | Requires |
|---|---|---|
| `v1/engine`, `v1/board`, `v1/events` | GET | **guest** |
| `v1/agents/{}/log`, `.../inbox`, `.../inbox/{}` | GET | **guest** |
| `v1/agents/{}/auth` | GET | **guest** |
| `v1/tables/{}/rows`, `v1/tools/{}/ops` | GET | **guest** |
| `v1/nodes/{}/content` *(new, §2.3a)* | PUT | **prompter** |
| `v1/agents/{}/send\|start\|stop\|restart\|clear` | POST | **prompter** |
| `v1/tables/{}/query` | POST | **prompter** |
| `v1/nodes`, `v1/nodes/{}`, `v1/wires` | POST/PATCH/PUT/DELETE | **admin** |
| `v1/vault/{}`, `v1/vault/{}/{}` | GET/PUT/DELETE | **admin** |
| `v1/agents/{}/auth` | DELETE | **admin** |
| `v1/agents/{}/auth/begin\|complete` | POST | **admin** |
| `v1/tools/import`, `v1/tools/{}/import`, `v1/tools/{}/call` | POST | **admin** |
| **`v1/cli/**`** | any | **DENIED to every tier, admin included** |
| anything else | any | **DENIED** |

Five rows are doing real work:

- **Vault and agent-auth are admin.** That is where third-party credentials live, and per ADVERSARY
  037 a vault value is readable by every agent in the project. Sharing a project must not share the
  creator's Anthropic bill or their GitHub PAT. `GET /v1/vault/{id}` lists key *names* only, but a map
  of where the secrets are is still the vault.
- **`v1/tools/{}/call` is admin, not prompter.** Invoking a tool spends vault-filled credentials —
  credential *use*, the vault boundary wearing a different hat.
- **`v1/tables/{}/query` is prompter, though it is a read.** It is read-only SQL behind an authorizer
  whose function arm was allow-by-default as recently as ADVERSARY 044. `GET .../rows` gives a guest
  the same data through a door with no SQL in it, so the lowest-trust tier need not stand on that.
- **`v1/agents/{}/clear` is prompter.** Resetting an agent's session is context management, and a
  prompter can already stop it.
- **`v1/cli/**` is denied outright, admins included.** It is the node-token realm, deliberately nested
  outside the engine-secret layer. ADVERSARY 002 and the M0 plan review both flagged that the
  authenticated proxy forwards it with the host bearer. Nothing in the product calls it through the
  API — verified by grep — so denying it costs nothing and closes a surface flagged twice.

**Matching is on the decoded segments the upstream URL is built from**, never the raw suffix. Matching
one spelling and forwarding another is how an authorisation check comes to be about a different
request than the one that happens.

### 5.3 `/p/` ingress stays outside the tier system

Route 25 is unauthenticated by design — an endpoint node's whole purpose is that a sender with
nothing but a URL can reach it. It keeps exactly the gate it has: the project's `http` capability,
the rate limit, the body cap.

Stated in both directions, because this is where a tier system would be tempted to leak:

- **A tier is never a way into `/p/`.** Being an admin changes nothing about what an ingress hit does.
- **`/p/` is never a way around a tier.** A hit arrives as `type=endpoint`, never `type=user`, and
  carries no actor. A guest who cannot `POST .../agents/{id}/send` cannot get the same effect by
  calling the project's public URL: the message is attributed to the endpoint node and marked
  untrusted external input, which is a different thing from a member's prompt.

Enabling `http` is admin (route 16), which is the only place the two systems touch.

### 5.4 `engine_events`: the one route that needs special-casing

It builds its scope imperatively and has two doors, and its ticket door never produces an `AuthUser`:

- **Header branch** — `ProjectScope` extracted by hand; its result was previously discarded, and now
  yields the tier, checked against guest.
- **Ticket branch** — `ws_ticket::redeem` returns the user id of whoever minted the ticket, and the
  code **threw it away**. It is wired through, and the tier is resolved by `load_member` **at
  redemption, not at mint**.

Resolving at redemption is the lesson `api_token::verify` already records for minting chains: a
ticket minted a second before a revocation and redeemed a second after must not open a socket, and
only a check at use time sees that.

## 6. The `x-wheel-` forgery hole, and actor headers

`forward_http` sanitised with an **empty** forbidden-prefix list, so an authenticated tenant could set
arbitrary `x-wheel-*` headers and they reached the host and the engine unmodified — `wheel-host`
removes nine fixed names and the prefix is not among them. Only the public ingress stripped that
namespace.

**Not exploitable as shipped, and that is stated rather than overclaimed.** The engine reads
`x-wheel-client-ip` and `x-wheel-secret` only in its *public ingress* router, not the `/v1` control
plane the authenticated proxy reaches; `x-wheel-ingress` is read by nobody. So there is no privilege
gain today. It becomes identity forgery by any tenant the moment a trust marker exists in that
namespace — which is what this change adds. Filed as
`redteam/findings/052-authenticated-proxy-does-not-strip-x-wheel-namespace.md`.

Strip the namespace, then set: `x-wheel-actor-id`, `x-wheel-actor-tier`, `x-wheel-actor-credential`.
There are **three** places the API reaches an engine and they build headers three different ways —
`forward_http`, `bridge_websocket` (a *fresh* request, no client headers at all), and
`HttpBoardClient` (bypasses the proxy entirely). All three go through one function, so the strip and
the set cannot drift apart.

## 7. Threat model

House format (`redteam/THREAT-MODEL.md`), boundary **TB1/TB2**. This replaces the sole authorization
check in the system, so the table is the point of the document.

**Assets.** A6 (cross-tenant data) is the one at stake: another user's board, transcripts, table rows
and — if a tier boundary leaks — their vault and their LLM accounts (A5, A8).

**Actors.** AU (an authenticated other user) becomes the primary adversary, in a new shape: **a
legitimate member at a lower tier**, who is inside the boundary and merely wants more than they were
given.

| # | Attack | Outcome | Control |
|---|---|---|---|
| 1 | A guest or prompter calls an admin route directly | Full project takeover | `require(tier)` per handler; §10's matrix drives every route as every lower tier |
| 2 | A tenant forges `x-wheel-actor-tier: admin` | The engine believes it | §6 — namespace stripped then set, on all three paths; tested by forgery, not only by absence |
| 3 | A forged header *replaces* nothing and sits beside ours | `HeaderMap` holds both; the engine reads the wrong one | The test asserts the header **count is one** |
| 4 | An engine route with no policy row | Reachable by everyone | Default DENY; a path with no rule is refused |
| 5 | Path spelled one way for the check, another for the forward | Authorise A, act on B | Policy matches the **decoded segments** the upstream URL is built from |
| 6 | `v1/cli/*` through the proxy with the host bearer | Node-token realm reached with an unattributable actor | Denied to every tier including admin |
| 7 | A non-member probes for a project's existence | Enumeration oracle | Access is a `WHERE` predicate; 404 for non-members, 403 only after membership is proven |
| 8 | A ws-ticket minted before a revocation, redeemed after | A revoked member opens a socket | Membership resolved at **redemption** |
| 9 | A live socket survives revocation | A revoked member keeps watching | NOTIFY/LISTEN + a periodic re-check + an absolute lifetime cap (§8) |
| 10 | A downgraded admin keeps an admin-tier socket | Stale privilege | The re-check compares tier, not just presence |
| 11 | A stale low-tier invite link demotes a member | Denial of service by anyone holding an old link | Accepting never lowers an existing tier |
| 12 | An invite link is probed for existence | Which links exist | Unknown, expired, revoked and exhausted are one answer |
| 13 | An invite redeemed twice, racing | Two members from a single-use link | Use count consumed inside the redeeming `UPDATE` |
| 14 | An email-locked invite redeemed by claiming the address | Wrong account admitted | Checked against the account's *verified* address, never a request field |
| 15 | A prompter grants themselves admin | Escalation | Member management is admin; a prompter never reaches the handler |
| 16 | The creator is demoted or removed | Owner locked out of their own board | Refused 409; the creator's admin is derived from `owner_id` and has no row to change |
| 17 | A principal containing a quote or newline | Header injection; a forged envelope attribute | Rejected at the verification boundary, once, by charset |
| 18 | A role string in the database this build cannot parse | Rounded up into access | `Tier::parse` returns `None`, which is no access |
| 19 | A tier becomes a way into `/p/`, or `/p/` a way around a tier | Guest sends via the public URL | §5.3, asserted in both directions |
| 20 | Migration leaves existing projects memberless | **The live deployment's owner locked out** | §4.2 — nothing to back-fill; tested against a directly-inserted project row |

Residual, named not closed: an agent holding `WHEEL_ENGINE_SECRET` (ADVERSARY 037, confirmed by run)
can call the control plane as the host and set `x-wheel-actor-id` to anything. That is forged
attribution and it closes with per-node uids. An agent with a stolen *node token* reaches the CLI
plane, where the header is ignored, so that failure is **missing** attribution rather than forged.

## 8. Revocation and live connections

Revoking a member must end access that is *already open*. A member watching `/v1/events` holds a
socket that no future authorisation check ever runs against.

1. **NOTIFY/LISTEN** on Postgres closes a matching bridge within milliseconds, across replicas. On
   SQLite the same trait is an in-process broadcast — complete there, because `wheeld` is one process.
2. **A periodic re-check** re-runs `load_member`; gone *or downgraded* closes the socket. This is what
   works when the notification is missed — a dropped listener, a row changed by hand. NOTIFY makes
   revocation fast; this makes it certain.
3. **Lifetime caps**, which are also ADVERSARY 011's first three recommendations: a per-project
   concurrent-bridge cap (per replica, and `docs/API.md` says so), a keepalive with a pong deadline,
   and an absolute lifetime cap.

011's second half — the authenticated HTTP proxy has no rate limit — is **not** closed here. Named,
not silently inherited.

## 9. Invites

`POST /v1/projects/{id}/invites` (admin) mints `wi_` + 32 random bytes, storing only its SHA-256 — the
`wht_` pattern reused rather than reinvented, for the reason given there. `POST /v1/invites/accept`
redeems it into a `project_members` row for the calling principal.

Bounded by construction: `expires_at` (7 days default), `max_uses` (1 default), and an optional
`email` lock checked against the verified account address. Accepting is idempotent and never lowers an
existing tier.

Membership can also be granted directly by user id, which is the path that delivers §0 without anyone
copying a link.

## 10. What a lower tier attempts and is refused — the test matrix

Every cell is a test, and every test is mutation-checked. A negative authorisation test that cannot
fail looks exactly like one that passes, which is why this is explicit rather than "coverage".

**Honest path:** for every engine path, every tier below it gets 403 *and the request never reaches
the engine*; for every admin API route, guest and prompter get 403; a non-member gets 404 everywhere;
a revoked member stops being a member; `v1/cli/*` is refused to all three.

**Forged path**, tested as its own axis because §6 is exactly this hole: a guest sending
`x-wheel-actor-tier: admin` is still refused; on a route they *are* allowed, the value that reaches
the engine is the server's, present exactly once; the whole `x-wheel-` namespace is stripped
including names we do not set; and the same forgery is proven not to survive on the WebSocket
handshake or through `HttpBoardClient`.

**Migration safety:** a project row inserted directly, with no member rows — the shape the live
database holds today — still resolves its owner to admin with full access, and still 404s for
everyone else.

## 11. What this does not do

- No external pluggable auth (M0) — its own PR, and not a prerequisite for any of the above.
- No presence, shared sessions or co-editing (M3–M5).
- **No structured table-row writes, so the prompter tier ships incomplete** (§2.3b).
- No ownership transfer; the creator is permanent (§4.1).
- No project-scoped API tokens — a `wht_` token is account-wide, so handing one to another tool grants
  every project that account can reach, not just the shared one. The recommended next slice.
- No rate limit on the authenticated proxy (§8).
- No closure of forged attribution by an agent holding the engine secret (§7 residual).
