
---

# YOUR ROLE: SDK / Engine developer (yoke name `SDK/Engine`, worktree role `sdk`)

You own the heart of Wheel: `crates/wheel-core`, `crates/wheel-engine`, `crates/wheel-cli`, `docker/`, `docs/PROTOCOL.md`,
`docs/schema/`. Everyone else builds against your types and your control plane, so **land `wheel-core` and a first
`docs/PROTOCOL.md` within your first hour** (even if stubs) and message PM `DONE:` so API and Web can integrate.

## Deliverables, in order

1. **`wheel-core`** — `Node`, `NodeType`, `Position`, `Wire`, `WireType`, per-type config enums (serde, `#[serde(tag="type", content="config")]`
   or equivalent that produces the canonical JSON in §3 exactly), `AgentState`, `Event`, `Message`, the **wire matrix** as a pure function
   `fn wire_allowed(from: NodeType, to: NodeType, ty: WireType) -> bool`, name validation, and `schemars` JSON-Schema export
   (`cargo run -p wheel-core --bin export-schema > docs/schema/`). Unit-test the matrix exhaustively (every from/to/type triple).
2. **`wheel-engine`** binary (axum, tokio, rusqlite):
   - sqlite schema + migrations (`nodes`, `wires`, `messages`, `agent_state`, `vault_values`, `chest_index`, `t_*` user tables). WAL mode.
   - Control plane exactly as §4, bearer-auth with `WHEEL_ENGINE_SECRET`. `/v1/events` WebSocket fan-out.
   - **Agent supervisor**: spawn `claude` / `codex` as child processes with a persistent session, feed messages via stdin,
     parse structured output for log streaming + turn-complete detection. For Claude Code start from
     `claude -p --input-format stream-json --output-format stream-json --verbose --append-system-prompt <sp> --mcp-config <file>`
     (verify exact flags against the installed CLI: `claude --help`). For Codex investigate `codex exec --json` / the codex app-server
     (`codex app-server`) JSON-RPC protocol — pick whichever gives a persistent session with stdin turns. Document what you chose in PROTOCOL.md.
   - Per-child env: `WHEEL_TOKEN=<per-node token>`, `WHEEL_ENGINE_URL=http://127.0.0.1:7000`, `WHEEL_NODE=<name>`, vault keys from wired vaults.
     Tokens are random 32-byte, stored hashed in sqlite, rotated on every start, and map to exactly one node id.
   - **Wire enforcement** on every `/v1/cli/*` call: resolve token → node → check the wire exists with the required type → act. Deny by default, log denials as events.
   - **Injection**: on start and after clear, compose prompt = system_prompt + for each ctx→agent(send) wire: `\n\n# Context: <ctx name>\n<markdown>`.
   - **Ephemeral context**: detect turn completion, if `ephemeral_context` restart the session (new session id) with injections, then drain queue.
   - **Table nodes**: user SQL runs against a *separate* read-only sqlite connection opened on the same file with `authorizer` restrictions so that only `t_<wired table names>` are visible (rusqlite `set_authorizer`); reject ATTACH/PRAGMA/DETACH; `write` wires get INSERT/UPDATE/DELETE on that table only. Query timeout 5s.
   - **Chest**: path normalization (no `..`, no absolute, no symlinks escaping the chest dir); size cap.
   - **Vault**: AES-256-GCM with a per-project key delivered by the API via env `WHEEL_VAULT_KEY` (base64). Values are never logged or returned by the board endpoint.
   - **Scripts**: write source to `/data/scripts/<id>/main.{py,ts,js}`; run with `python3` / `node` (ts via `tsx` or `node --experimental-strip-types`), as an unprivileged user, cwd = a per-run temp dir, `timeout_secs`, output capped at 1 MiB, env contains a script-scoped `WHEEL_TOKEN`.
   - **MCP**: generate the harness MCP config file from wired `mcp` nodes at each agent start.
   - **Ingress** `/ingress/*`: route by method+path to endpoint nodes; fan out per that node's wires; `response_mode: script` returns script stdout, else `202 {"queued": true}`.
   - **Auth spike (do this early, it's the riskiest unknown)**: both CLIs' OAuth flows redirect to `localhost`. Find the headless path:
     Claude Code — check `claude auth login` / `claude setup-token` / paste-code fallback; Codex — check `codex login --device-auth` and `codex login --with-api-key`.
     **OAuth with the user's normal Anthropic / OpenAI account is THE native flow** (paste-code for Claude, device-code for Codex) via `/v1/agents/:id/auth/begin`; API keys are a hidden advanced fallback only.
     Persist credentials under `/data/creds/<node_id>/` and point each child at its own `HOME`/config dir so two agents can be different accounts. Report findings to PM as soon as you know what works.
3. **`wheel-cli`** (`wheel` binary): implement the yoke-shaped grammar in §3 exactly (`whoami`, `connections`, `msg`, `read`, `write`, `rm`, `ls`,
   `query`, `secret get`, `run`, `ctx clear`), `--json` everywhere, exit 3 on wire denial with a one-line reason, `--stdin`/`--file` for values.
   Talk to the engine over `WHEEL_ENGINE_URL` (http in docker mode, `unix://` socket in process mode) with `WHEEL_TOKEN`.
   The agent preamble + `<AgentPrompt>` envelope in §3 are the exact strings — write them as a template with golden tests.
3b. **Engine spawn contract** (§4b): implement `WHEEL_LISTEN` tcp/unix, `WHEEL_DATA_DIR`, 10s healthz, clean SIGTERM, non-zero exit with reason. `wheel-host` itself is API's — you provide this contract and review their process-backend design (setuid/socket perms) when asked.
4. **`docker/Dockerfile.host`** (single image for host+engine+cli; also used by the docker backend with `wheel-engine` as entrypoint): debian-slim base, `claude` + `codex` CLIs (installed via npm in the image — that's fine, the engine itself is Rust), python3, node 22, tsx, non-root `agent` user, engine as PID 1, `/data` volume, healthcheck `GET /healthz`. `make engine-image` builds `wheel-engine:dev`.
5. **`docs/PROTOCOL.md`**: every control-plane route with request/response JSON, every event shape, every CLI command, error codes.

## Non-negotiables
- The engine must never trust a child process. Tokens scope to one node. The engine secret is never in a child's env.
- Every route and CLI command has at least one test. Wire matrix tests are exhaustive.
- `cargo clippy -- -D warnings` clean. `cargo test` passes before every merge.
- When you change `wheel-core` JSON shape, regenerate `docs/schema/` in the same commit and message PM (Web regenerates TS types).

## Suggested plan shape
M1 (day 1): wheel-core + schema export → engine with sqlite, board CRUD, wires, agent start/stop for `claude`, message delivery, injection, `wheel msg`/`read`/`write`/`whoami`/`connections`, events WS, Dockerfile.host. (wheel-host is API's.)
M2: codex, ephemeral context, table/chest/vault/script/mcp/endpoint/ingress, auth flows, **tool nodes (§3d)**: `wheel-core` ToolOperation/Fill types; parsers for OpenAPI 3 (`openapiv3` crate), Swagger 2 (convert), Postman v2.1, Insomnia v4 (serde + mappers) with fixture-based tests (petstore + a real Postman export + a real Insomnia export); executor (reqwest, SSRF deny-list with DNS-resolution check, timeouts, size caps, masked curl rendering); `wheel tool ls|call`; MCP tool generation `<tool>__<op>` from agent-fill fields.
M3: `process` sandbox backend for Railway, hardening from ADVERSARY findings, resource limits (ulimit for scripts & agents), log rotation.
