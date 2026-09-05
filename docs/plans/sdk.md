# SDK/Engine plan

Owner: SDK/Engine. Scope: `crates/wheel-core`, `crates/wheel-engine`, `crates/wheel-cli`, `docker/`,
`docs/PROTOCOL.md`, `docs/schema/`. (`crates/wheel-host` was reassigned to API; I own only the engine
spawn contract on that boundary — §4b.)

## Principle

The engine is the trust boundary between a user's agents and everything else. Every design call below
resolves in favour of *the engine never trusting a child process*. A node's wire set is its entire
authority; a token maps to exactly one node; deny is the default and denials are events, not silence.

## Status

| Milestone | State |
|---|---|
| M0 plan | this document |
| wheel-core + docs/schema + PROTOCOL.md v1 | **done**, merged to main |
| §3d `tool` types + 9×9×3 matrix | **done**, this commit |
| Engine M1 | next |
| CLI M1 | after engine board CRUD |
| Dockerfile.host + test image | after CLI |

## Layout

```
crates/wheel-core/     types, wire matrix, envelope, preamble, spawn contract   [done]
crates/wheel-engine/
  main.rs              config from env (§4b), listen tcp|unix, SIGTERM handling
  db/                  sqlite: migrations, WAL, node/wire/message/state repos
  api/                 axum router: board, agents, auth, data nodes, events WS, cli, ingress
  supervisor/          per-agent actor: spawn, stdin writer, stdout parser, queue drain
  harness/             claude adapter (M1), codex adapter (M2) behind one trait
  caps/                token -> node -> wire resolution; the single choke point
crates/wheel-cli/      the `wheel` binary
docker/Dockerfile.host engine + cli + claude + codex + python + node
```

## Engine M1, in dependency order

1. **sqlite layer.** WAL, `foreign_keys=ON`, migrations as ordered embedded SQL. Tables: `nodes`,
   `wires`, `messages`, `agent_state`, `node_tokens`, `logs`, plus user `t_*`. One writer connection,
   a read pool, and a *separate* read-only connection with an authorizer for user SQL.
2. **Board CRUD + wire validation** through `wheel_core::check_wire`, so engine and API cannot
   disagree about the matrix.
3. **Capability layer** (`caps/`). Every `/v1/cli/*` request resolves bearer → token hash → node →
   wire check. This is deliberately one function that every CLI route must call; there is no path to
   a data node that bypasses it.
4. **Agent supervisor.** One actor per agent node, owning the child. Detail in "Supervisor" below.
5. **Message delivery.** Persist → queue → single-writer stdin loop → state transitions → events.
6. **Events WS** with a broadcast channel; slow subscribers are dropped, never allowed to stall the
   supervisor.
7. **CLI**: `whoami`, `connections`, `ls`, `msg`, `read`, `write`, `inbox`, `list`, `ctx clear`.
8. **Dockerfile.host** + `wheel-engine:test` variant with QA's fakes.

## Supervisor design

The riskiest component, and the one three §3c defects point at. Design:

- **One actor per agent node**, owning the `Child` handle. It is the only code with a reference to
  the child's stdin. (§3c#12: single writer.)
- **A per-agent mutex around spawn**, and `start` is idempotent — a second start while running
  returns the existing session. (§3c#13: N quick messages must not become N processes.)
- **Messages never spawn.** They enqueue. The actor delivers when idle.
- **Strictly serial delivery**: write one line, wait for `result`, then the next.
- **Two-lane queue**: user messages ahead of node messages, but drain at most 3 consecutive user
  messages before one normal-lane message, and promote any normal-lane message older than 60 s.
  (Prevents operator chatter from starving agent traffic.)
- **Poison messages are consumed exactly once**: a turn ending in `result.is_error` marks the message
  `consumed` with `last_error`, never redelivers it, and moves the agent to `error`.
- **Stdout parsing is permissive by contract**: match the known event types, log everything else
  verbatim, never die on a non-JSON line. Verified against QA's `<<FAKE:NOISE>>` / `<<FAKE:GARBAGE>>`.

## Harness abstraction

One trait, two implementations, so codex can land in M2 without touching the supervisor:

```rust
trait Harness {
    fn argv(&self, spec: &SpawnSpec) -> Vec<OsString>;
    fn encode_turn(&self, envelope: &str) -> String;   // one stdin line
    fn parse_line(&self, line: &str) -> HarnessEvent;  // Unknown(line) is a normal outcome
    fn probe_auth(&self) -> AuthProbe;
}
```

`HarnessEvent::Unknown` being an ordinary variant rather than an error is the whole point.

## Auth (M2, spike complete)

The two CLIs need opposite shapes, which is why `AuthMode` keeps them distinct:

- **claude** — `paste_code`, a *submit*. `claude auth login` over pipes; the browser displays a code;
  the user pastes `<code>#<state>` back; we write it to the pending child's stdin. No localhost needed.
  Probe: `claude auth status --json` (no network, no quota). API key: `ANTHROPIC_API_KEY` or
  `CLAUDE_CODE_OAUTH_TOKEN`. Isolation: `CLAUDE_CONFIG_DIR` per node (relocates credentials *and*
  `.claude.json`); no Linux keyring exists, so no store setting is needed.
- **codex** — `device_code`, a *poll*. CLI generates the code, browser consumes it, nothing is pasted
  back. API key env var is `CODEX_API_KEY` (**not** `OPENAI_API_KEY`, which `codex doctor` misleadingly
  reports as fine but which is not in the auth chain). Isolation needs
  `cli_auth_credentials_store = "file"` in each node's `$CODEX_HOME/config.toml`, because `CODEX_HOME`
  does not isolate the OS keyring.

**Container gotcha, already folded into the spawn path:** `--permission-mode bypassPermissions` is
refused when running as root, exiting 1 with empty stdout — indistinguishable from an auth failure.
We run the agent as non-root and set `IS_SANDBOX=1`, and the engine discriminates on stderr rather
than classifying `needs_auth` from an exit code.

## Risks

| Risk | Mitigation |
|---|---|
| Harness protocol drifts under us (CLI updates) | Permissive parser; QA's fake pinned to a verified real-CLI capture; version recorded in `system/init`. |
| A child escapes its wire set | Single capability choke point; tokens rotate every start; exhaustive matrix tests; ADVERSARY review. |
| `bypassPermissions` means unrestricted tools inside the sandbox | Accepted and explicit: the sandbox boundary is the security story. Flagged to ADVERSARY. |
| Tool nodes as an SSRF surface (§3d) | Deny-by-default pre-filter in core; engine must re-check after DNS and every redirect — pre-filter alone is not the control. |
| Supervisor deadlock/stall blocking all delivery | Per-agent actors are independent; WS subscribers are droppable; every wait has a deadline. |
| Scope growth (§3c, §3d, §3e landed after kickoff) | Core types absorb it cheaply; anything not M1 is typed but unimplemented, and PROTOCOL.md marks the milestone per route. |

## Open questions for PM

1. **`script → tool`?** §3d specifies `agent → tool (read)` and `tool → vault (read)` only. Scripts can
   already reach vault/table/chest "same as agent", so a script arguably should be able to call a tool.
   I have implemented the narrow reading (denied). Say if you want it opened.
2. **`ToolMethod` is a separate enum** from `HttpMethod`, because `endpoint` is contractually
   GET/POST/PUT/DELETE while imported specs need PATCH/HEAD/OPTIONS. Sharing one enum would silently
   widen the endpoint contract. Flagging since it means two method enums in the schema.
