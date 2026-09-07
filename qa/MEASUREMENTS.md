# A10 — efficiency measurements

Numbers, not adjectives. PM's A10 asked for a gate on crate count and release binary size,
and for `cargo tree -d` to be clean. This file is the durable record: messages between
agents get truncated (§3c #11 — three of mine to PM were beheaded in one evening), and the
contract already says git is the system of record and a message is only a notification.

Re-measure with `make deps` (cheap, no compile) and `make size` (real release build).
Both budgets RATCHET DOWN: an improvement lowers the ceiling and locks itself in, a
regression is a red build. So these numbers are a floor to argue against, not a snapshot.

Measured on `e4cd743`, macOS aarch64 host, 2026-09-06.

## Crate count — 281 per platform

`aarch64-apple-darwin` and `x86_64-unknown-linux-gnu` resolve to the same 281.

PM's figure of 346 was the unfiltered resolve. 62 of those are windows-only and compile on
nothing we own, so the gate uses `--filter-platform`: a number that moves when an unrelated
crate adds a windows target is a number nobody can act on, and a gate people cannot act on
is a gate people learn to ignore.

| member | crates pulled (linux) |
|---|---|
| wheeld | 278 |
| wheel-api | 243 |
| wheel-host | 203 |
| wheel-engine | 196 |
| wheel-cli | 63 |
| wheel-sqlite | 32 |
| wheel-core | 29 |

## Release binary size

Fat LTO, `codegen-units = 1`, symbols stripped — the profile Railway actually ships.
First real measurement; there was no prior number.

| binary | size | where it runs |
|---|---|---|
| `wheel` | 0.87 MiB | every sandbox image (the agent CLI) |
| `wheel-host` | 6.49 MiB | the engine machine |
| `wheel-engine` | 6.98 MiB | one per project sandbox |
| `wheel-api` | 7.53 MiB | api.wheel.dev |
| `wheeld` | 10.25 MiB | what Railway runs |

The gate DISCOVERS these from `cargo metadata` rather than a hand-written list. The list it
replaced named `wheel-cli` and `wheel-api`; `wheel-cli` builds a binary called `wheel`, and
it omitted `wheeld` entirely. It measured three of five and said nothing about the two it
missed — one of which ships in every sandbox image. A name that produces no file simply
fell out of the loop. Missing is now a failure, not a silent subset.

## `cargo tree -d` — NOT clean, and "returning nothing" is the wrong target

12 duplicated crate names per platform:

    bitflags, cpufeatures, getrandom, hashbrown, hashlink, indexmap,
    rand, rand_chacha, rand_core, schemars, syn, webpki-roots

**Eleven of the twelve are upstream version skew we do not control.** `getrandom` is at
0.2, 0.3 *and* 0.4 simultaneously; `hashbrown` at 0.14/0.15/0.17; `rand` at 0.8/0.9; `syn`
at 2/3; `webpki-roots` at 0.26/1.0. Different dependencies pin different majors of the same
crate and nothing in our diff moves them.

A gate demanding zero would therefore be red forever for a reason nobody can act on, which
is precisely the permanent-red-X failure written into the contract at `f897af8`: a red
signal that is not a defect trains the team to stop reading CI, and the one real failure
then arrives disguised as the usual noise.

**Built instead:** a ratcheting allowlist in `qa/deps-budget.json`. The 12 are budgeted, a
13th is a red build, and a duplicate that goes away must be deleted from the file or the
gate fails — so the list cannot silently re-permit what we already fixed.

## sqlx, for API's A10 item

Dropping sqlx removes **34 crates** from wheeld: 278 → 244, a 12% cut.

    allocator-api2, atoi, byteorder, crc, crc-catalog, crossbeam-queue,
    crossbeam-utils, dotenvy, either, event-listener, flume, foldhash,
    futures-executor, futures-intrusive, hashbrown, hashlink, heck, hkdf,
    home, md-5, parking, sqlx, sqlx-core, sqlx-macros, sqlx-macros-core,
    sqlx-mysql, sqlx-postgres, sqlx-sqlite, stringprep, tokio-stream,
    unicode-bidi, unicode-normalization, unicode-properties, whoami

Note `sqlx-mysql`: wheeld currently compiles a MySQL driver it cannot possibly load.

**But it fixes exactly ONE duplicate** (`hashlink`); 12 → 11. Crate count and duplicate
count are close to independent here, so removing sqlx must not be reported as cleaning the
tree. Claim the 34 crates and the MySQL driver; do not claim the duplicates.
