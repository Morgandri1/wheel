# Wheel

They always say "don't reinvent the wheel" — sometimes you have to.

Wheel is a per-project, per-user container that runs Claude Code / Codex agents as child processes, wired to each
other and to tables, endpoints, scripts, MCP servers, vaults and chests on a visual board.

- `docs/ARCHITECTURE.md` — the shared contract every team builds against. Read it first.
- `docs/PROTOCOL.md` — engine control plane + `wheel` CLI (SDK/Engine team)
- `docs/API.md` — api.wheel.dev (API team)
- `docs/TESTPLAN.md` — acceptance criteria (QA)
- `redteam/` — threat model and findings (ADVERSARY)

Workflow: one worktree per team under `/Users/metatron/wheel-wt/<role>`, merge to `main` after `make check`.

## Running Wheel

Wheel is source-available (PolyForm Noncommercial 1.0.0) — free to use, modify, and share for any noncommercial purpose.

It's **headless-first**: a shell and `curl` are enough to run, drive and script it. The board UI is an optional
add-on that calls the API from its own server. (Design + threat model: `docs/proposals/headless-first.md`.)

### 1. `wheeld` — one executable
Needs: Rust stable 1.88+ ([rustup.rs](https://rustup.rs) — not your distro's packaged `rustc`, it's usually too
old), a C compiler (`build-essential` / Xcode CLT), and `git` (also used at runtime, for agent workspaces). Running
an agent (not just building) needs Node.js 22+ with `npm install -g @anthropic-ai/claude-code @openai/codex`, or
use Docker (§2), which bundles both.

```bash
cargo build --release -p wheeld
./target/release/wheeld          # API + sandbox host + agents in one process, sqlite store, on http://127.0.0.1:8080
```
- Listens on `127.0.0.1:8080` by default (`--bind` / `BIND_ADDR` to change).
- Everything lives in `~/.wheel` (`--data-dir` / `WHEEL_DATA_DIR`) — treat it like an SSH key.
- First start writes an operator token to `~/.wheel/operator-token`.

Drive it with that token:
```bash
wh() { local path=$1; shift
       curl -fsS -H @<(printf 'x-auth-token: %s\n' "$(cat "${WHEEL_TOKEN_FILE:-$HOME/.wheel/operator-token}")") \
            -H 'content-type: application/json' "http://127.0.0.1:8080$path" "$@"; }

P=$(wh /v1/projects -d '{"name":"hello"}' | jq -r .id)            # a project; its engine starts with it

wh /v1/projects/$P/board/apply -d '{"board": {
  "nodes": [ {"name": "keys",   "type": "vault", "config": {"keys": []}},
             {"name": "worker", "type": "agent", "config": {"harness": "claude", "system_prompt": "Be brief."}} ],
  "wires": [ {"from": "worker", "to": "keys", "type": "read"} ] }}'

id() { wh /v1/projects/$P/engine/v1/board | jq -r --arg n "$1" '.nodes[] | select(.name == $n) | .id'; }
printf '{"value":"%s"}' "$ANTHROPIC_API_KEY" | wh /v1/projects/$P/engine/v1/vault/$(id keys)/ANTHROPIC_API_KEY -X PUT -d @-
wh /v1/projects/$P/engine/v1/agents/$(id worker)/start -X POST
wh /v1/projects/$P/engine/v1/agents/$(id worker)/send -d '{"body": "Say hello."}'
wh "/v1/projects/$P/engine/v1/agents/$(id worker)/log?limit=50"
```

More tokens:
```bash
wheeld token create --name laptop         # prints the new token, on stdout, this once
wheeld token list                         # id, account, name, created, last used, revoked
wheeld token revoke <id>                  # revokes it and every token it minted
```

As a systemd service, use `KillMode=mixed` (not the default `control-group`) so `wheeld` gets to drain in-flight
turns before its agents are killed:
```ini
[Service]
ExecStart=/usr/local/bin/wheeld --data-dir /var/lib/wheel
KillMode=mixed
TimeoutStopSec=30
```

**Signup** is closed by default (`WHEEL_SIGNUP=open` to allow self-signup — only on a box nobody else can reach,
since a stranger's signup runs a stranger's agent code as your user). While closed, the owner adds people:
```bash
printf '{"email":"%s","password":"%s"}' you@example.com "$PASSWORD" | wh /v1/auth/users -d @-
```

**Behind a reverse proxy** (the VPS kit in `infra/vps/` sets all of these): `PUBLIC_BASE_URL=https://<domain>`,
`WHEEL_TRUSTED_PROXIES=<proxy address>`, `WHEEL_ALLOWED_HOSTS=<domain>`.

### 2. Docker, headless
Needs: Docker Engine with the Compose v2 plugin, and `git`. Everything else (Node, `claude`/`codex`, Rust) is
inside the image.
```bash
docker build -f docker/Dockerfile.wheeld -t wheeld .              # or: make wheeld-image
docker run -d --name wheeld --stop-timeout 30 -v wheel-data:/data -p 127.0.0.1:8080:8080 wheeld
(umask 077; docker exec wheeld cat /data/operator-token > ~/.wheel-token)
export WHEEL_TOKEN_FILE=~/.wheel-token                             # then `wh` as above
docker exec wheeld wheeld token create --name ci                   # more tokens, the same way
```
Or as compose: `docker compose -f infra/compose.wheeld.yml up -d --build`.

### 3. The board UI (optional)
Needs: Node.js 22.x. `wheel-web` is a prebuilt package, not a build from source.
```bash
WHEEL_API_URL=http://127.0.0.1:8080 npx wheel-web                                # against wheeld on this machine
docker compose -f infra/compose.wheeld.yml --profile web up -d --build           # or both in compose: UI on http://127.0.0.1:3000
```
The UI signs in with email + password (same signup rules as above), and calls the API from its own server — the
browser never talks to the API directly. To script boards you use in the UI, mint a token for that account
(`wheeld token create --email you@example.com`).

### 4. On your own cloud
- **Railway**: fork this repo, create services from `docker/Dockerfile.api` and `docker/Dockerfile.host` (+
  Postgres), apply `infra/railway/settings.json` with `infra/railway/apply-settings.sh`. Env vars:
  `infra/railway/README.md` and `web/DEPLOY.md`.
- **Any VM / Kubernetes**: run the two images with Postgres; the host needs a persistent volume at `/data` and must
  NOT be publicly reachable (API talks to it privately via `WHEEL_HOST_SECRET`). The web app is a standard Next.js
  server reaching the API at `WHEEL_API_URL`.

### Developing Wheel: the multi-service stack
```bash
docker network create wheel
docker compose -f infra/docker-compose.yml up --build              # postgres + api + host, API on 127.0.0.1:8080
docker compose -f infra/docker-compose.yml --profile web up --build   # plus the board UI on 127.0.0.1:3000
```

### Agents and credentials
Agents are Claude Code / Codex processes. Give them credentials through a **vault** node (one per account; wire
the agent to it) or the agent's Authenticate panel (in-browser login, `claude setup-token`, or an API key). See
`docs/ARCHITECTURE.md` for the model and `docs/WHEEL-ON-WHEEL.md` for a board that develops Wheel itself.

# Development
Wheel develops itself; there is a cloud board (template available for free) that handles each moving piece separately so that the agents can figure out what they need and build it themselves. If you want to contribute to wheel, you can clone it and get started in the `crates/`, `web/`, or `docker/` directory.  

# Legal disclaimer, asshole
Wheel is independent, original work. It shares no code, assets, copy, designs, or other protected material with any other product, and it wasn't built using anyone's confidential or proprietary information.

Ideas aren't ownable; expression is. Wheel is my own expression of ideas that are common to this category of tooling. If you think otherwise, the contact address is in the LICENSE — put it in writing.
