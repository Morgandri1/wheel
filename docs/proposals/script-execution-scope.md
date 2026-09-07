# Script execution — scope (SDK, P1)

Scoping only. No runtime is being built under this document; PM has the first wake as pure observation and
this is deliberately behind it. The point of writing it now is that the wake will tell us which parts matter,
and I would rather have the shape ready than discover it under time pressure.

Everything below is measured against `main`, with `file:line`, not recalled.

## Why this is the keystone rather than a feature

The board already **promises** script execution to every agent. `crates/wheel-core/src/preamble.rs:69` puts
`wheel run <script>` in the board-memory line of every agent's system prompt, and `preamble.rs:28` renders an
`agent --read--> script` wire as *"you can run it"*. There is no `run` arm in `crates/wheel-cli/src/main.rs` —
it falls through to the catch-all and exits 1 with `unknown command "run"`.

So today we tell every agent it can do a thing, and the thing errors. That is worse than absence: it teaches a
model the board is unreliable, and the failure is indistinguishable from the agent getting the syntax wrong.

## What already exists — the reusable set

More is built than I expected. The type system, the security gates and the spawn choke-point are all done and
tested; only the runtime is missing.

| Piece | State | Location |
|---|---|---|
| `ScriptConfig` + validation | done | `wheel-core/src/node.rs:380-394`, `validate.rs:117-128` |
| Wire matrix rows (`agent→script read`, `script→*`) | done, exhaustively tested | `wire.rs:129,153,160-167`; enforced in `db/board.rs:418` |
| Script tokens admitted to the CLI plane | done | `caps.rs:171-173` `may_use_cli(Agent \| Script)` |
| Token mint → 0600 file, node-generic | done, reusable as-is | `db/tokens.rs:32-86`, `supervisor/mod.rs:539-549,1493-1503` |
| Per-node run dir | done | `config.rs:159-162` `node_run_dir(uuid)` |
| `child_command` (env_clear choke-point) | done | `supervisor/mod.rs:220-226` |
| `scripts_dir()` | exists, **never called** | `config.rs:136-138` |
| `MAX_SCRIPT_OUTPUT_BYTES` (1 MiB) | **const with zero call sites** | `wheel-core/src/lib.rs:98` |

The token mechanism is the happy surprise: `mint`, `write_secret_file`, `node_run_dir` and `may_use_cli` all
take a `Uuid` and know nothing about agents. A script gets its own capability token, scoped to its own wires,
with no new security machinery — and `Caller::require` re-reads wires from the DB per check (the 047 fix), so a
script's capabilities are live rather than snapshotted at spawn.

## What has to be built

1. **Runtime** — write `source` to `scripts_dir()/<node_id>/main.{py,ts,js}`, spawn the interpreter through
   `supervisor::child_command`, cwd = a per-run temp dir, mint a script-scoped token into a 0600 file, revoke
   it when the run ends.
2. **Control-plane route** — `POST /v1/cli/run` on the node-token realm. Shape to be agreed with API.
3. **CLI verb** — `wheel run <script> [args…]`, `--json`, exit 3 on wire denial like every other verb.
4. **MCP `run` tool** — see trap 1.
5. **Timeout + output cap** — neither exists generically; see below.

## Three traps I want on the record before anyone starts

1. **A test actively pins `run` as absent.** `wheel-engine/src/mcp.rs:309` asserts the MCP surface does NOT
   offer `run`, with the reason *"run has no engine route yet and must not be offered"*. That test is correct
   today and adding `run` to the builtins fails CI until it is changed in the same commit. This is a good trap —
   it is the mechanism that stopped us advertising a broken tool over MCP, even while the preamble advertises
   the broken CLI verb. Worth noting the asymmetry: MCP was disciplined about this and the preamble was not.
2. **There is no generic child timeout or output cap.** The pattern to copy is `oauth.rs:176-189,429`
   (concurrent stdout/stderr pumps into a shared buffer, `tokio::time::timeout`, `kill_on_drop(true)` plus an
   explicit kill). The pattern NOT to copy is `workspace::git` (`workspace.rs:61-88`), which uses `output().await`
   with no timeout and no cap — a hung `git clone` blocks forever. That is my code and this scope should fix it
   rather than replicate it.
3. **`timeout_secs` max is 300 in code and 600 in my own PROTOCOL.md.** `validate.rs:67` says 300, and 300 is
   baked into the exported JSON Schema that Web generates types from, so it is a cross-repo contract. §3 of the
   ARCHITECTURE contract also says 300. PROTOCOL.md:703 was simply wrong; fixed in this commit.

## The security precondition, and it is not small

`child_command` clears the environment but **does not drop uid** — there is no `setuid`/`pre_exec` anywhere in
wheel-engine, and `config.rs:98-105` records this as the known §2 gap. Every child on a project runs as one uid
today.

A 0600 token file protects against other uids. It does not protect against a same-uid sibling. So the moment
scripts execute, a script can read any other node's token file under `run/<uuid>/token` and act as that node.
Today the same-uid population is agents we placed; scripts widen it to arbitrary user-authored code, which is a
different risk even though the uid gap is unchanged.

I am not claiming this blocks the work. I am claiming it should be a stated, accepted precondition rather than
something discovered later — either per-node uid drop lands first (§2, M3), or we ship scripts knowing the
boundary is the project and PROTOCOL.md says so in the script section explicitly.

**ADVERSARY gate:** a script is the first place a user's own code runs on our host, so egress is theirs to
attack before this ships — the SSRF policy we wrote for tool nodes (`validate.rs` `host_is_denied`) governs
`tool` and `mcp` URLs and does not constrain a Python script at all. `wheel-host` is currently deployed in the
SAME Railway project as Postgres, so the private network is reachable from the sandbox; that is F003 and it is
worth re-checking against this scope specifically.

## Requirements PM has already set

- **Concurrency cap** on concurrently running scripts, with the per-host running-agent cap as precedent.
- **Shared store** — reuse the A9/A8 materialisation rather than a second mechanism.
- **QA ≥90%** per crate, as everywhere.

## Adjacent, cheap, and worth doing in the same pass

`wheel ls <chest>` returns `{"keys":[]}` (`api/cli_routes.rs:198`) — a **false empty** rather than an
unimplemented error, so a chest reads as a real empty chest. Read/write/rm on the same node type all return an
honest 400 "not implemented yet" (`cli_routes.rs:256,393,441`). The `ls` arm is inconsistent with its three
siblings and is the kind of thing that costs someone an afternoon. One line.
