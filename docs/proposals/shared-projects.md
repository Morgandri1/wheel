# Wheel shared projects — proposal (multiplayer M1)

SDK/multiplayer lane, 2026-09-11. Branch `sdk/multiplayer-identity`. Companion:
`docs/proposals/external-auth.md` (M0), which defines the principal this proposal keys off.

This is the smallest shippable slice of multiplayer: a Wheel project can have members in one of three
access tiers, every route lands in exactly one tier, and everything a member does is attributed to
them. Presence (M3), shared agent sessions (M4) and co-editing (M5) build on this and are not in it.

## 1. The thing being replaced

`ProjectScope` — the extractor a handler takes in order to name a project at all — resolves through
exactly one predicate:

```rust
// crates/wheel-api/src/auth/extractor.rs:181
    let row: Option<ProjectRow> = crate::db_fetch_optional!(
        &state.db,
        "SELECT ... FROM projects WHERE id = $1 AND owner_id = $2",
        id, owner_id
    )?;
```

Its doc-comment calls it "the **only** function in the codebase that turns a project id into a
`Project`", and that is true, which is what makes this change tractable. Six further `owner_id = $N`
predicates ride on the same assumption (`routes/projects.rs:63, 88, 159, 216/222, 262, 395`).

Because this predicate *is* the tenant boundary, changing it is a cross-tenant surface and it gets the
adversary pass rather than a review.

## 2. The three tiers

Operator ruling, 2026-09-11. Exactly three, no fourth, and no tier with conditional powers.

| Tier | What it is |
|---|---|
| **admin** (root) | Everything. Board structure, members and invites, vault writes, project lifecycle, tokens, settings. |
| **prompter** | Manage context, and prompt agents. Read the board, write context, send messages to agents, start and stop them. |
| **guest** | View only. Sees the board, agent transcripts and logs. Sends nothing. |

Ordered `guest < prompter < admin`, and the policy check is `actual >= required`. **The project
creator is admin of their own project.**

### 2.1 Boundary calls, as ruled

- A prompter **can start and stop an agent**. Wheel's contract is that a message never starts a
  process, so a prompter who cannot start cannot prompt. `restart` goes with them: it is the
  composition of the two, and allowing both halves while refusing the whole would be a rule with no
  content.
- **"Manage context" means what an agent reads, not the board's shape.** A prompter writes context
  content; a prompter does not create, delete, rename, reposition or rewire nodes, and does not change
  agent config — model, budget, harness. Creating or deleting a ctx node is structure, so it is admin.
- A prompter **never writes vault values and never sees them.**
- A guest **sends nothing** — no messages, no interrupts, no steering, no endpoint invocation.
- **Only admin manages members and invites.** A prompter cannot invite and cannot raise their own tier.
- **`/p/` public ingress is outside the tier system entirely** (§5.3).

### 2.2 Two places the ruling meets the code awkwardly, reported rather than worked around

**(a) "Manage context" has no route today, so this slice adds one.**
`PATCH /v1/nodes/{id}` carries `name`, `position` *and* `config` in one body
(`board_routes.rs:86-91`), so ctx content and agent config arrive through the same door. Granting a
prompter that route would mean authorising on the body's contents — the API parsing node config to
decide permissions, duplicating engine knowledge, and authorising against one thing while acting on
another. That is precisely the conditional power the ruling forbids, and it is the shape
`extractor.rs` already refuses for `x-project-id`.

So the policy table stays a pure `(method, path) → tier` map with no body inspection, and the engine
gains one narrow route: **`PUT /v1/nodes/{id}/content`**, body `{markdown}`, refused by the engine for
any node type that has no operator-editable content. Tier: prompter. This mirrors the engine's
existing narrow-route idiom (`PUT /v1/vault/{id}/{key}`) and `table_routes.rs`'s stated principle that
"the operator gets exactly the same box an agent does" — the storage path already exists behind
`/v1/cli/write`; this is the operator-plane door to it, minus the wire check.

**(b) A prompter cannot write table rows in v1, and this is a missing surface, not a tier decision.**
The ruling says a prompter writes "ctx content and table rows". There is **no control-plane
table-row write route for anyone**: `GET /v1/tables/{id}/rows` reads, and
`POST /v1/tables/{id}/query` is explicitly read-only SQL (`table_routes.rs:85`). Rows are written only
by agents through `/v1/cli/write`. Granting a prompter row-writes therefore means designing
`POST /v1/tables/{id}/rows` and `DELETE /v1/tables/{id}/rows/{key}` — structured, never SQL, because
the SQL door is read-only on purpose and widening it would create the second SQL surface that comment
exists to prevent. **Deferred and named** rather than smuggled in: it is a second new engine surface
in a slice that is already adding one, and it wants its own column-validation design. Flagged for the
operator as the one part of the prompter tier that does not ship complete.

**(c) A consequence of "project lifecycle is admin" worth seeing now.** Agent start/stop is prompter;
*project* start/stop is admin. So a prompter arriving at a stopped sandbox cannot start it and
therefore cannot prompt anything until an admin appears. That follows from the ruling as given and is
implemented as ruled — but it is friction that lands hardest in M4, where a shared session is blocked
for everyone by a stopped sandbox. Recommendation, not a deviation: revisit whether project `start`
(additive) should be prompter while `stop`/`restart` (destructive to other members' running work) stay
admin.

## 3. Schema — migration **0006**, both dialects

**The brief said 0005. It is wrong.** `0005_api_token_sessions.sql` already exists in both
`crates/wheel-api/migrations/` and `migrations_sqlite/`, from the merged headless-first work. Verified
by listing the directories, not by reading the plan. Ours is **0006**.

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
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    role        text NOT NULL CHECK (role IN ('admin', 'prompter', 'guest')),
    token_hash  text NOT NULL UNIQUE,
    email       text,
    created_by  text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    max_uses    integer NOT NULL DEFAULT 1,
    uses        integer NOT NULL DEFAULT 0,
    revoked_at  timestamptz
);
CREATE INDEX project_invites_project_idx ON project_invites (project_id);
```

`user_id` is `text`, matching `projects.owner_id` and `api_tokens.user_id`, carrying whatever the
principal is: a Wheel uuid under `local` and `external`, a provider `sub` under legacy `jwks`
(`external-auth.md` §4.5). SQLite gets the same tables with `uuid → TEXT`, `timestamptz → TEXT`
RFC3339, `integer → INTEGER`, per `migrations_sqlite/0001_init.sql`'s stated translation rules.

### 3.1 The creator, and why there is no second source of truth

`projects.owner_id` stays, and means **the creator**. The creator is always admin, cannot be demoted
and cannot be removed.

`project_members.role` admits all three tiers, so a second person *can* be made admin — the ruling
says admins manage members, which implies a set, not a singleton. That would normally create two
answers to "is this user an admin", but the resolution here is monotone and so cannot contradict
itself:

    effective_tier(user) = Admin                if user == projects.owner_id
                         = member.role          otherwise
                         = <no access>          if neither

Being the creator only ever *adds* admin; it never subtracts. The one state that could look like a
disagreement — a member row saying `guest` for the creator — is refused at the API with a 409, so it
never exists. There is no backfill of creator rows into `project_members`, because a backfill is
exactly the second copy this paragraph exists to avoid.

Limit, stated: **no ownership transfer in v1.** It is a single `UPDATE projects`, and it should be a
deliberate change with its own tests rather than a side effect of this one.

## 4. `load_member` replaces `load_owned`

One query, one row, and the authorisation stays a predicate in the `WHERE` clause rather than a
comparison after the fetch — the property `load_owned`'s doc-comment insists on, kept for its stated
reason: "row does not exist" and "row belongs to someone else" must remain one code path returning one
`NotFound`, so they cannot drift into an enumeration oracle.

```sql
SELECT p.id, p.owner_id, p.name, p.capabilities, p.status, p.created_at, p.updated_at,
       CASE WHEN p.owner_id = $2 THEN 'admin' ELSE m.role END AS role
  FROM projects p
  LEFT JOIN project_members m
    ON m.project_id = p.id AND m.user_id = $2 AND m.revoked_at IS NULL
 WHERE p.id = $1
   AND (p.owner_id = $2 OR m.role IS NOT NULL)
```

`ProjectScope` becomes `{ user, project, tier }` and still has no public constructor, so a handler that
acts on a project still cannot be written without the check having run. The property `extractor.rs`
opens by describing is preserved and extended: from "is the owner" to "is a member, and which tier".

`GET /v1/projects` becomes the union of created and joined projects, and `Project` gains `tier`. The
per-user quota (`projects.rs:63`) stays counted over `owner_id` only: a project you were invited to is
not one you provisioned, and charging a guest for it would let anyone exhaust anyone's quota by
inviting them.

## 5. The policy table — default DENY

One table, consulted for every project-scoped route. **Anything not listed is refused.** Same idiom as
the wire matrix ("anything not listed is rejected at creation time"). Adding an engine route without
adding a policy row makes it unreachable through the API, which is the correct failure direction.

### 5.1 The 25 API routes, each in exactly one tier

| # | Route | Requires |
|---|---|---|
| 1 | `GET /healthz` | — public |
| 2 | `GET /v1/host/healthz` | — public |
| 3 | `POST /v1/auth/signup` | — public |
| 4 | `POST /v1/auth/login` | — public |
| 5 | `POST /v1/auth/logout` | — public by design |
| 6 | `GET /v1/auth/me` | authenticated; no project, no tier |
| 7 | `POST /v1/auth/password` | authenticated; no tier |
| 8 | `POST /v1/auth/users` | authenticated + existing operator check |
| 9 | `POST /v1/auth/tokens` | authenticated; no tier |
| 10 | `GET /v1/auth/tokens` | authenticated; no tier |
| 11 | `DELETE /v1/auth/tokens/{id}` | authenticated; scoped by `user_id` in SQL |
| 12 | `POST /v1/projects` | authenticated; **creator becomes admin** |
| 13 | `GET /v1/projects` | authenticated; returns projects where tier ≥ guest |
| 14 | `POST /v1/projects/instantiate` | authenticated; creator becomes admin — **`HttpBoardClient`** |
| 15 | `GET /v1/projects/{id}` | **guest** |
| 16 | `PATCH /v1/projects/{id}` | **admin** |
| 17 | `DELETE /v1/projects/{id}` | **admin** |
| 18 | `POST /v1/projects/{id}/start` | **admin** (project lifecycle; see §2.2c) |
| 19 | `POST /v1/projects/{id}/stop` | **admin** |
| 20 | `POST /v1/projects/{id}/restart` | **admin** |
| 21 | `POST /v1/projects/{id}/ws-ticket` | **guest** |
| 22 | `POST /v1/projects/{id}/board/apply` | **admin** — **`HttpBoardClient`** |
| 23 | `ANY /v1/projects/{id}/engine/v1/events` | **guest** — ticket branch special-cased, §5.4 |
| 24 | `ANY /v1/projects/{id}/engine/{*rest}` | per §5.2, default DENY |
| 25 | `ANY /p/{project_id}/{*rest}` | **outside the tier system**, §5.3 |

New in this slice: `GET/POST/PATCH/DELETE /v1/projects/{id}/members` (guest to read, **admin** to
change), `GET/POST/DELETE /v1/projects/{id}/invites` (**admin** throughout — an invite is a
credential, so even its metadata is admin-only), and `POST /v1/invites/accept` (authenticated, no
tier: the caller is not a member yet, which is the point).

Routes 14 and 22 are the two that reach the engine through `HttpBoardClient`
(`board_apply.rs:137-163`) rather than through `proxy.rs`. They are called out explicitly because any
policy or actor work applied only at the proxy silently misses them. Both are admin: a board apply is
board structure, and `instantiate` creates the project it applies to.

### 5.2 Engine control-plane routes (the `{*rest}` suffix of route 24)

Matched against the engine's own router (`crates/wheel-engine/src/api/mod.rs:158-204`).

| Path | Methods | Requires |
|---|---|---|
| `v1/engine`, `v1/board` | GET | **guest** |
| `v1/events` | GET | **guest** |
| `v1/agents/{id}/log`, `.../inbox`, `.../inbox/{mid}` | GET | **guest** |
| `v1/agents/{id}/auth` | GET | **guest** |
| `v1/tables/{id}/rows` | GET | **guest** |
| `v1/tools/{id}/ops` | GET | **guest** |
| `v1/nodes/{id}/content` *(new, §2.2a)* | PUT | **prompter** |
| `v1/agents/{id}/send` | POST | **prompter** |
| `v1/agents/{id}/start`, `.../stop`, `.../restart` | POST | **prompter** |
| `v1/agents/{id}/clear` | POST | **prompter** |
| `v1/tables/{id}/query` | POST | **prompter** |
| `v1/nodes`, `v1/nodes/{id}` | POST, PATCH, DELETE | **admin** |
| `v1/wires` | POST, DELETE | **admin** |
| `v1/vault/{id}`, `v1/vault/{id}/{key}` | GET, PUT, DELETE | **admin** |
| `v1/agents/{id}/auth` | DELETE | **admin** |
| `v1/agents/{id}/auth/begin`, `.../complete` | POST | **admin** |
| `v1/tools/import`, `v1/tools/{id}/import` | POST | **admin** |
| `v1/tools/{id}/call` | POST | **admin** |
| **`v1/cli/**`** | any | **DENIED to every tier, admin included** |
| anything else | any | **DENIED** |

Five of these rows are doing real work:

- **Vault and agent-auth are admin.** They are where third-party credentials live. A prompter who
  could `PUT /v1/vault/{id}/{key}` or run `auth/begin` would be attaching or replacing the *creator's*
  LLM accounts, and per ADVERSARY 037 a vault value is readable by every agent in the project.
  Sharing a project must not share the creator's Anthropic bill or their GitHub PAT. `GET
  /v1/vault/{id}` lists key *names* only, but it is still a map of where the secrets are, so it stays
  admin — the ruling says a prompter never sees vault values, and the smallest honest reading of that
  is that the vault is not a prompter surface at all.
- **`v1/tools/{id}/call` is admin, not prompter.** Invoking a tool makes outbound HTTP with
  vault-filled credentials. It is credential *use*, which is the vault boundary wearing a different
  hat, and the ruling puts endpoint invocation outside even the guest tier.
- **`v1/tables/{id}/query` is prompter, though it is a read.** It is read-only SQL
  (`table_routes.rs:85`) behind an authorizer whose function arm was allow-by-default as recently as
  ADVERSARY 044. `GET .../rows` gives a guest the same data through a door with no SQL in it, so the
  lowest-trust tier does not need to be the thing standing on that authorizer.
- **`v1/agents/{id}/clear` is prompter.** It resets an agent's session context. That is context
  management, which is the tier's stated job, and a prompter can already stop the agent.
- **`v1/cli/**` is denied outright, admins included.** It is the node-token realm, deliberately nested
  outside the engine-secret layer (`api/mod.rs:206-209`). ADVERSARY 002 and the M0 API plan review both
  flagged that the authenticated proxy forwards `v1/cli/*` to the engine with the host bearer. Nothing
  in the product calls it through the API — verified by grep; only `wheel-engine`, `wheel-cli` and
  redteam findings mention the path — so denying it costs nothing and closes a surface flagged twice.
  It is also the multiplayer design's rule made mechanical: **the actor is ignored entirely on the
  agent-token plane**, and the cleanest way to ignore it is to refuse to carry it there.

### 5.3 `/p/` ingress stays outside the tier system

Route 25 is unauthenticated by design — an endpoint node's whole purpose is that a sender with nothing
but a URL can reach it (operator ruling, ARCHITECTURE §1). It keeps exactly the gate it has today: the
project's `http` capability, the rate limit, the body cap, `load_unauthenticated_for_ingress`.

Stated in both directions, because this is where a tier system would be tempted to leak:

- **A tier is never a way into `/p/`.** Nothing about being an admin changes what an ingress hit does.
- **`/p/` is never a way around a tier.** An ingress hit arrives as `type=endpoint`, never `type=user`,
  and carries no actor (§6). A guest who cannot `POST /v1/agents/{id}/send` cannot get the same effect
  by calling the project's public URL, because the message is attributed to the endpoint node and is
  marked untrusted external input — which is a different thing from a member's prompt, and the
  preamble already says so.

Enabling `http` is admin (route 16, `PATCH`), which is the only place the two systems touch: an admin
decides whether the unauthenticated door exists at all.

### 5.4 `engine_events`: the one route that needs special-casing

`engine_events` (`proxy.rs:110`) builds its scope imperatively and has two doors, and its ticket door
never produces an `AuthUser` at all:

- **Header branch** — `ProjectScope::from_request_parts` runs by hand at `proxy.rs:127` and its result
  is currently discarded. It now yields the tier, checked against **guest**.
- **Ticket branch** — `ws_ticket::redeem` returns the user id of whoever minted the ticket, and
  `proxy.rs:122` **throws it away**. That is the identity of whoever opened the socket, already
  computed. It is wired through, and the tier is then resolved by `load_member` **at redemption, not
  at mint**.

Resolving at redemption is the same lesson `api_token::verify` already records: it walks the minting
chain on *use*, because "a child minted in the instant before the parent's revocation landed is
inserted after the family was revoked, and only a check at use time can see it". A ws-ticket minted a
second before a revocation and redeemed a second after must not open a socket, and only a check at
redemption sees that.

## 6. Actor headers — and a defect found on the way

### 6.1 The defect, reported as a finding in its own right

`forward_http` sanitises with an **empty** forbidden-prefix list:

```rust
// crates/wheel-api/src/routes/proxy.rs:199
    let headers = hop::sanitize_for_upstream(req.headers(), &[]);
```

Only the public ingress passes the `x-wheel-` prefix (`routes/ingress.rs:36,68`). So **today an
authenticated client can set arbitrary `x-wheel-*` headers on `/v1/projects/{id}/engine/*`, and they
reach the host and the engine unmodified** — `wheel-host` removes nine fixed names
(`wheel-host/src/proxy.rs:162-174`) and the prefix is not among them.

**Is it exploitable today? No. It is latent, and the honest statement is that nothing consumes those
headers on this path.** The engine reads `x-wheel-client-ip` and `x-wheel-secret` only in
`api/ingress.rs`, which serves the public `/ingress` router — not the `/v1` control plane the
authenticated proxy reaches. `x-wheel-ingress` is set by the API and read by nobody. So there is no
live privilege gain today. It becomes **identity forgery by any tenant the moment an actor or tier
header exists**, which is this proposal. Fixed here, with a test that forges the header and asserts it
never arrives upstream, and filed separately in `redteam/` so a real finding is not buried inside a
feature.

### 6.2 Injection, on all three paths

Strip the whole `x-wheel-` namespace, then set:

| Header | Value |
|---|---|
| `x-wheel-actor-id` | the verified Wheel principal |
| `x-wheel-actor-tier` | `admin` \| `prompter` \| `guest` |
| `x-wheel-actor-credential` | `session` \| `api_token` \| `external` \| `ws_ticket` |

Strip-then-set, in that order, is what `routes/ingress.rs:22-23` already documents. There are **three**
call sites, and a change applied only to the first would silently miss the others:

1. **`forward_http`** (`proxy.rs:199`) — pass `&["x-wheel-"]`, then insert.
2. **`bridge_websocket`** (`proxy.rs:253-268`) — builds a *fresh* request and forwards no client
   headers at all, so `sanitize_for_upstream` never runs on this path. The header must be added
   explicitly to the builder. Assuming the HTTP path's behaviour covers it is exactly the mistake
   available here.
3. **`HttpBoardClient`** (`board_apply.rs:137-163`) — bypasses `proxy.rs` entirely, used by routes 14
   and 22. It sends only `Authorization` today, so without this the two routes that create whole
   boards arrive unattributed — the place attribution matters most.

No `wheel-host` change is needed: it strips nine fixed names and passes everything else through. That
is precisely why the API-side strip is mandatory rather than belt-and-braces — the host will faithfully
relay a forged header to the engine.

### 6.3 Header values cannot carry an injection

The actor id is a principal string that may come from an external IdP. A newline or control character
in it is a header-injection and log-forging primitive. It is rejected **at the verification boundary**
(`external-auth.md` §6 #10) rather than sanitised at each use, so one place decides what a principal
may look like and every consumer inherits it.

## 7. `on_behalf_of`

`wheel_core::Message` gains `on_behalf_of: Option<String>`, the `messages` table gains an
`on_behalf_of TEXT` column, and the `<AgentPrompt>` envelope gains an `on_behalf_of` attribute — so
every agent input carries who asked for it.

Mechanics, from the code as it is:

- The envelope is built in exactly one place, `wheel_core::Message::envelope`
  (`crates/wheel-core/src/message.rs:282-295`). The attribute is **appended after `reply_to`**, and
  emitted only when present, following `reply_to`'s own pattern. Three byte-exact tests
  (`wheel-core/tests/envelope.rs:39-47`, `:86-105`, `:135-158`), one redteam re-implementation
  (`redteam/pocs/envelope-forgery/t_envelope_escape.py:30-31`) and the generated fixture
  (`qa/fixtures/envelope/cases.json`, via `qa/tools/gen_envelope_fixture.py --write`) are updated with
  it, plus the three copies of the normative block in `PROTOCOL.md:620-631`, `ARCHITECTURE.md:571-575`
  and `TESTPLAN.md:113-115`.
- The engine has no migration directory; `db/mod.rs:86-95` applies `schema.sql` then calls
  `add_column` for anything added after the first deploy. So: the column goes in `schema.sql` for
  fresh databases *and* `add_column(conn, "messages", "on_behalf_of TEXT")` for existing ones —
  exactly the precedent of `vault_values.expires_at`. The table is `STRICT`, so the column declares
  `TEXT`.
- There is exactly one INSERT into `messages` in the tree (`db/messages.rs:34-73`, `enqueue`), which is
  what makes this a contained change.
- `Event::Message` embeds the whole `Message` (`wheel-core/src/event.rs:63-65`), so the field reaches
  the events WebSocket with no event-type change — but `docs/schema/message.json` and
  `docs/schema/event.json` must be regenerated
  (`cargo run -p wheel-core --bin export-schema -- docs/schema`) and then
  `web/src/lib/schema/generated.ts` (`pnpm gen:types`), or the export gate fails.

Attribution follows ADVERSARY 001's invariant exactly: **the engine generates the attribute; the body
is opaque payload and is never parsed for framing.** `on_behalf_of` is written from the row, never
interpolated from a body, and the engine re-applies the principal charset check rather than trusting
the API's — "the layer above already checked" is how single-layer validation becomes no validation
(finding 009).

Where the value comes from, per plane:

| Plane | Source | Behaviour |
|---|---|---|
| `/v1/*` control plane (engine secret; the API's hop) | `x-wheel-actor-id` | Read, sanitised, stored |
| `/v1/cli/*` (node tokens) | — | **Header ignored entirely.** An agent cannot assert an actor |
| `/ingress/*` (public) | — | Ignored; the hit is already `type=endpoint` |

`POST /v1/agents/{id}/send` (`agent_routes.rs:141-166`) takes no `HeaderMap` today; it gains one and
reads the header following the `x-wheel-client-ip` pattern at `ingress.rs:156-165` — `.get(NAME)` →
`.to_str().ok()` → validate into a typed value → `unwrap_or_else` with an explicitly named fallback.

### 7.1 The limit that is documented rather than fixed

**Wheel attribution can be forged by an agent until per-node uids land** (ADVERSARY 037). Precisely,
because the shape matters:

- An agent that steals a *sibling's node token* can send as that node. Its message arrives on
  `/v1/cli/*`, where the actor header is ignored, so `on_behalf_of` is **NULL**. The failure is
  *missing* attribution, not forged attribution — the better of the two, and worth having designed for
  rather than stumbled into.
- An agent that lifts `WHEEL_ENGINE_SECRET` from the engine's environ (037 item 1, confirmed by run)
  can call the control plane *as the host* and set `x-wheel-actor-id` to anything. **That is forged
  attribution, and this proposal does not close it.** It closes with per-node uids (F007, §2 M2–M3),
  not here; a fix attempted here could not be tested, which is the same reason the chest born-safe
  checklist was deferred.

Recorded in `PROTOCOL.md` beside the envelope, so a reader of the attribution contract meets the limit
at the same moment as the guarantee.

## 8. Revocation: NOTIFY, and the bridge caps that close ADVERSARY 011

Revoking a member must end access that is *already open*, not merely refuse the next request. A member
watching `/v1/events` holds a WebSocket that no future authorisation check ever runs against.

Three controls, layered, because each covers what the others cannot:

1. **NOTIFY/LISTEN.** On revoke or tier downgrade, `NOTIFY wheel_membership, '<project>:<user>'`. Each
   API replica listens and closes matching live bridges. SQLite has no NOTIFY, so the same
   `MembershipEvents` trait is implemented over an in-process broadcast channel — complete there,
   because `wheeld` is one process. Fast, and correct across replicas on Postgres.
2. **Periodic re-check on the bridge.** Every N seconds a live bridge re-runs `load_member`; gone or
   downgraded closes it. This is what works when a NOTIFY is missed, when a replica's listener has
   dropped, and on either backend. NOTIFY makes revocation *fast*; this makes it *certain*.
3. **Lifetime caps**, which are also the answer to **ADVERSARY 011** (open, Medium, cross-tenant
   availability on the shared host):
   - **Per-project concurrent-bridge cap** (`WS_MAX_BRIDGES_PER_PROJECT`, default 16), refusing beyond
     it. 011 calls this the highest-priority of its three recommendations because it caps blast radius
     regardless of the idle story. Per-replica in v1, and `docs/API.md` will say so — the same honest
     caveat the existing rate limits already carry.
   - **Keepalive with a pong deadline.** Server-side ping every 30 s; no pong within the deadline
     closes. 011 is explicit that a plain idle-read timeout cannot distinguish a dead peer from a
     legitimately idle one on a long-lived push channel, and this can.
   - **Absolute lifetime cap** (`WS_MAX_LIFETIME_SECS`, default 3600), refreshed by taking a new
     ws-ticket. Defence in depth, and it bounds how long a missed revocation can persist even if
     controls 1 and 2 both fail.

011's second half — the authenticated HTTP proxy has no rate limit at all, so one tenant can flood the
shared host over plain HTTP — is **not** closed here. It is real, it is the same theme, and it is a
different change (a shared per-project counter, like the ingress limiter). Named, not silently
inherited.

## 9. Invites

`POST /v1/projects/{id}/invites` (admin) mints `wi_` + 32 random bytes, base64url, storing only its
SHA-256 — the `wht_` pattern (`auth/api_token.rs:30-47`) reused rather than reinvented, for the reason
given there: the digest is the lookup key, so an index probe takes time depending on nothing an
attacker can steer toward a stored value. `POST /v1/invites/accept` redeems it into a
`project_members` row for the calling principal.

Bounded by construction: `expires_at` (default 7 days), `max_uses` (default 1), and an optional
`email` lock checked against the *verified* principal's account address, never against a claim in the
request. Accepting an invite for a project you already belong to is idempotent and **never lowers an
existing tier** — otherwise a stale guest link becomes a way to demote a prompter. An invite may grant
`admin`, because the ruling puts member management in that tier; it can never grant more than admin,
because there is nothing more.

## 10. What a lower tier can attempt and be refused — the test matrix

Every cell is a test, and every test is mutation-checked: the check is removed, the test is watched
going red, the check is restored (§0b). A negative authorisation test that cannot fail looks exactly
like one that passes, which is why this list is explicit rather than "coverage".

**Honest path** — the caller's real tier is lower than the route requires:

| Actor | Attempt | Expected |
|---|---|---|
| guest | `POST .../engine/v1/agents/{id}/send` | 403 |
| guest | `POST .../engine/v1/agents/{id}/start` | 403 |
| guest | `PUT .../engine/v1/nodes/{id}/content` | 403 |
| guest | `POST .../engine/v1/tables/{id}/query` | 403 |
| guest, prompter | `PUT .../engine/v1/vault/{id}/{key}` | 403 |
| guest, prompter | `GET .../engine/v1/vault/{id}` | 403 |
| guest, prompter | `POST .../engine/v1/nodes`, `DELETE .../v1/wires` | 403 |
| guest, prompter | `PATCH .../engine/v1/nodes/{id}` (agent config) | 403 |
| guest, prompter | `POST .../engine/v1/tools/{id}/call` | 403 |
| guest, prompter | `POST .../engine/v1/agents/{id}/auth/begin` | 403 |
| guest, prompter | `POST /v1/projects/{id}/board/apply` | 403 |
| guest, prompter | `PATCH`/`DELETE /v1/projects/{id}`, `.../stop` | 403 |
| guest, prompter | `POST /v1/projects/{id}/members`, `.../invites` | 403 |
| prompter | invite themselves as admin, or `PATCH` their own member row | 403 |
| **every tier, admin included** | any `.../engine/v1/cli/*` | 403 |
| non-member | anything on the project | **404**, never 403 — no enumeration oracle |

**Forged path** — the caller supplies headers to claim a tier they do not hold. This is the
`x-wheel-` hole of §6.1, so it is tested as its own axis rather than assumed covered:

| Actor | Forgery | Expected |
|---|---|---|
| guest | `x-wheel-actor-tier: admin` on an admin route | 403, and the header never reaches upstream |
| guest | `x-wheel-actor-id: <an admin's principal>` | 403, and the injected value is the guest's own id |
| prompter | `x-wheel-actor-tier: admin` on `PUT .../vault/...` | 403 |
| any | `x-wheel-actor-*` on a route they *are* allowed | 200, and the value upstream is the server's, not theirs |
| any | forged `x-wheel-*` on the WebSocket handshake | never reaches the engine (the fresh-request path, §6.2 #2) |
| any | forged `x-wheel-*` on `board/apply` | never reaches the engine (`HttpBoardClient`, §6.2 #3) |

The last row of each table is the one that would be missed by testing only the obvious path: the
"allowed route" case proves the server's value *replaces* the client's rather than merely being added
alongside it, and the two bypass paths prove the fix was applied in all three places rather than one.

## 11. Coverage and gates

`wheel-api` sits at 89.02% against the 90% bar, under the standing ruling that a merge may not push a
crate further under it. This change adds a lot of code to that crate, so the obligation is on the new
code specifically rather than on the crate average as an excuse.

Postgres-only suites skip without `TEST_DATABASE_URL`, so every new predicate gets a SQLite parity
test in `sqlite_parity.rs`, which drives the real router — a members join that works on one backend
and not the other is exactly the class `sqlite_dialect.rs` exists to catch.

## 12. What M2–M5 can depend on from this

- **A principal** that is Wheel's own, stable, and the key for membership and attribution
  (`external-auth.md` §4).
- **`Tier`** (admin/prompter/guest) and **`ProjectScope { user, project, tier }`** — M2's `requireRole`
  on the local engine mirrors this vocabulary rather than inventing a second one.
- **A default-DENY policy table** that M3–M5 extend by adding rows: a presence socket, a
  shared-session queue endpoint and a collab role are each one row, and forgetting the row fails
  closed.
- **`x-wheel-actor-id` / `-tier` / `-credential`** arriving at the engine on every control-plane call,
  already stripped of anything the client supplied — M4's per-actor caps and turn holder, and M5's
  per-actor CRDT origin, read the actor from here.
- **`on_behalf_of`** on the message row, in the envelope and on the events WebSocket, with its forgery
  limit documented (§7.1) — M2's attribution journal should use the same field name so the two
  products' transcripts join.
- **`MembershipEvents`** (NOTIFY on Postgres, broadcast on SQLite) — M3 presence and M4 session
  teardown need exactly this signal.
- **Bridge caps and the keepalive** — M3's presence socket and M5's collab socket inherit them rather
  than each inventing their own.

## 13. What this does not do

- No presence, no shared agent sessions, no co-editing (M3–M5).
- **No structured table-row writes, so the prompter tier ships incomplete against the ruling** (§2.2b).
- No ownership transfer; the creator is permanent (§3.1).
- No project-scoped API tokens — the gap named in `external-auth.md` §8.1, and the recommended next
  slice.
- No rate limit on the authenticated proxy (§8, the second half of ADVERSARY 011).
- No closure of forged attribution by an agent holding the engine secret (§7.1, ADVERSARY 037).
