# Proposal: make `sqlx` optional, and put wheel-api's SQLite backend on rusqlite

Status: **proposed, not started.** Requires a PM ruling to merge; ADVERSARY review before merge regardless.
Author: API. Date: 2026-09-06.

This exists because PM asked for it in git rather than in a message: five of my messages have arrived
beheaded today, and an authentication change ruled on from a fragment is how we approve one thing and
ship another.

## Why this is being proposed at all

Efficiency is P1. `sqlx` is the largest single line item in the workspace.

Measured with `cargo tree -e normal`, deduplicated by name+version:

| build | crates |
|---|---|
| `wheel-api`, default features (postgres + sqlite) | 214 |
| `wheel-api`, `--no-default-features --features postgres` | 209 |
| `wheel-api`, `--no-default-features --features sqlite` | 200 |
| workspace total | 239 |

Release binaries today, with the fat-LTO profile: `wheel-api` 7,897,376 B (7.53 MB), `wheeld`
10,747,248 B (10.25 MB).

**43 of wheel-api's 214 crates — 20% — are reachable only through `sqlx`.** That set contains both
duplicate `hashbrown` versions, the duplicate `rand`, and `libsqlite3-sys`, which `rusqlite` already
pulls in anyway. We compile two SQLite drivers into one binary today. `rusqlite`'s entire closure is
19 crates and `wheel-sqlite`'s is 22, and both are already in the tree.

The feature gate we have is real but partial:

- `cargo build -p wheeld` → `sqlx`, `sqlx-core`, `sqlx-sqlite`, `libsqlite3-sys`, `rusqlite`. No
  `sqlx-postgres`. The gate holds.
- `cargo build --workspace` → all of the above **plus `sqlx-postgres`**. Cargo unifies features, the
  workspace also builds wheel-api's own binary with default features, and wheeld's copy inherits
  Postgres. A laptop compiles a Postgres driver it will never call.

## What this proposal is NOT

It is not an auth-mode change, and it has nothing to do with Clerk, `AUTH_MODE`, or what ships in
the web bundle. Those are a separate decision in a separate document
(`api-auth-mode-and-client-bundle.md`).

The phrase that caused the confusion is mine and I should have been more precise: this rewrites the
code path that *reads the rows* the login path reads. It does not change what a valid session is,
which provider issues one, how a token is verified, or what any client sends. `AuthMode`,
`x-auth-token`, the JWKS verifier and the local session issuer are all untouched, and no file under
`src/auth/` changes except the store calls inside it.

A useful test of the distinction: this change is invisible from outside the process. A client cannot
tell which driver read the row.

## What the current code path does

`crates/wheel-api/src/db.rs` holds `enum Db { Pg(sqlx::PgPool), Sqlite(sqlx::SqlitePool) }`, chosen by
the scheme of `STORE`/`DATABASE_URL` — the URL is the whole of the decision, so no mode flag can
disagree with the connection string. `Db::connect` also runs `sqlx::migrate!` against
`./migrations` or `./migrations_sqlite`.

Every query goes through five macros (`db_execute!`, `db_fetch_one!`, `db_fetch_optional!`,
`db_fetch_all!`, `db_scalar!`) which dispatch on the enum and pick the dialect's placeholder style.
There are **24 call sites**, of which **12 are on the authentication path**:

| file | sites | what it does |
|---|---|---|
| `auth/local.rs` | 8 | signup, login, session lookup, password change |
| `auth/extractor.rs` | 2 | loads the project and asserts `owner_id == jwt.sub` |
| `http/authlimit.rs` | 2 | login rate limit |
| `routes/projects.rs` | 7 | project CRUD |
| `routes/ws_ticket.rs` | 3 | single-use WS tickets |
| `http/ratelimit.rs` | 2 | ingress rate limit |

## What the new one would do

1. `sqlx` becomes an **optional** dependency, enabled only by the `postgres` feature.
2. The `sqlite` feature stops meaning `sqlx/sqlite` and starts meaning a `rusqlite` backend, opened
   through `wheel-sqlite` — the same journal-mode negotiation `wheel-host` and `wheel-engine`
   already use, which is what survived the full-volume incident this afternoon.
3. `Db` gains a third shape and `db_dispatch!` a third arm. The macros keep their signatures, so the
   24 call sites are not rewritten — only the arm underneath them.
4. `rusqlite` is blocking, so the SQLite arm runs inside `spawn_blocking` over a small connection
   pool. See the risk section: this, not the SQL, is the part that can go wrong.
5. A migration runner for the rusqlite arm, reading the **same** `migrations_sqlite/*.sql` files.

Postgres is untouched: same `sqlx`, same `tls-rustls-ring`, same queries, same migrations.

## What specifically made me want to rewrite it

A dependency cut, not a bug. There is no correctness gap in the current SQLite backend that I know
of, and I am not proposing this because anything is broken. If the ruling is no, nothing is worse
than it is today — we just keep paying 43 crates and two SQLite drivers.

## What changes for a user who is currently logged in

**Nothing, on the deployment that matters.** Production runs Postgres, and this proposal does not
touch the Postgres arm at all. The rewritten path is the one `wheeld` uses on a laptop.

I want to be precise rather than reassuring about that: "not production" is not the same as "not
important" — wheeld authenticates real users against a real password database, and it is the local
story we are asking people to run.

For a wheeld user:

- No one is logged out. Session JWTs are HS256 signed with a key that does not change, the `sessions`
  rows are not touched, and validation is the same code.
- No password needs resetting. Argon2id hashes are stored as PHC strings and read back as text.
- The schema does not change. Same three migrations, same tables, same columns.

## Whether any session or token issued under the old path stops working

No. Sessions, WS tickets and password hashes are all data, and none of their formats change. The
verification code above the store layer is untouched — this is a swap of *how rows are read*, not of
what a valid session is.

**The one real compatibility hazard is migration bookkeeping, and it is not about tokens.** `sqlx`
records applied migrations in a `_sqlx_migrations` table with checksums. A new rusqlite runner that
does not read that table would consider `0001_init` unapplied and re-run it against a populated
database. On an existing wheeld database that is, at best, a failed boot. The runner must therefore
read `_sqlx_migrations` and treat its rows as applied, and there must be a test that opens a database
created by the sqlx path and boots it on the rusqlite path with the data intact. I would treat that
test as the acceptance criterion for the whole change.

## Risks, priced honestly

1. **Blocking a runtime thread.** `rusqlite` is synchronous. If a call site ends up blocking the
   async executor instead of going through `spawn_blocking`, the API stalls under load and it will
   not show up in a unit test. Highest risk in the change, and it is a concurrency risk, not a SQL one.
2. **Re-running migrations on an existing database.** Above. Mitigated by reading `_sqlx_migrations`
   and by the round-trip test.
3. **A rewritten auth query behaving differently.** Mitigated by `tests/sqlite_parity.rs`, which
   already exists to prove the two backends agree, and which would have to pass unchanged.
4. **Losing a bound parameter.** `rusqlite` uses named/positional params. The owner check
   (`owner_id == jwt.sub`) stays a bound parameter; no `format!` goes anywhere near a query. This is
   the first thing ADVERSARY has pre-committed to checking and it should be greppable, not argued.

## The cheaper alternative, which I want on the record

Most of the *build-time* complaint can be paid without touching auth at all:

**Make `postgres` a non-default feature.** `wheeld` and a laptop `cargo build --workspace` then stop
compiling the Postgres driver and its TLS stack — 14 crates — and the deployed API opts in
explicitly. That is a `Cargo.toml` change plus whatever build commands need the flag added. Zero
auth-path risk, an afternoon at most.

It does not get the 43 crates or remove the second SQLite driver. Only the rewrite does that.

## What I recommend

Do the cheap one now, and rule on the rewrite separately. They are independent, and bundling them
means the 14 free crates wait behind an auth review.

## Cost

About a day for the rewrite. The cheap alternative is an afternoon.
