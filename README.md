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

It is **headless-first**: a shell (bash or zsh — the `wh()` helper below uses process substitution,
which POSIX `sh`/`dash` don't have) and `curl` are enough to run it, drive it and script it. The
board UI is an optional add-on that calls the API from its own server. The design and its threat
model: `docs/proposals/headless-first.md`.

### 1. `wheeld` — one executable
Before this: a **recent Rust stable, at least 1.88** (https://rustup.rs — nothing in this workspace
pins an exact version, but `crates/wheel-core/Cargo.toml`'s direct dependency on `time` and several
locked transitive crates declare that floor, and `Cargo.lock`'s own format needs cargo ≥ 1.78
regardless. **Do not use your distro's packaged `rustc`** — Ubuntu 24.04's apt package is 1.75 and
will not build this workspace; `rustup` is the one that tracks current stable) and a **C compiler**
(`build-essential` on Debian/Ubuntu, Xcode Command Line Tools on macOS: `rusqlite`'s bundled SQLite
and `ring`'s assembly both need one to build; nothing here needs OpenSSL, every TLS path in the
dependency graph is `rustls`). `git` — to get this repo, and also at **runtime**: the engine shells
out to `git clone --bare` for any agent workspace backed by a repo
(`crates/wheel-engine/src/supervisor/workspace.rs`), not only to build `wheeld` once. The examples
below also use `curl` and `jq`. Building `wheeld` does **not** need Node — but running an agent
does: install Node.js 22+ and `npm install -g @anthropic-ai/claude-code @openai/codex` yourself, or
use Docker, which bundles both.
```bash
cargo build --release -p wheeld
./target/release/wheeld          # API + sandbox host + agents in one process, sqlite store, on http://127.0.0.1:8080
```
- It listens on **127.0.0.1:8080**. `--bind` / `BIND_ADDR` changes that, and any address other people can reach logs a
  warning saying so.
- Everything lives in `~/.wheel` (`--data-dir` / `WHEEL_DATA_DIR`): the store, `master.key` and every project. The directory
  is `0700`. Treat it like an SSH key.
- The first start writes an **operator token** to `~/.wheel/operator-token` (`0600`), for an owner account that can only
  sign in with tokens. The log shows the path, never the token.

Drive it with that token. `wh` reads the token from the file, so it never appears on a command line, where `ps` would show
it to every user on the machine:

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

Tokens are managed against the data directory itself. Whoever can read it can already read `master.key`, so this is no
new power:
```bash
wheeld token create --name laptop         # prints the new token, on stdout, this once
wheeld token list                         # id, account, name, created, last used, revoked
wheeld token revoke <id>                  # revokes it and every token it minted
```
Any signed-in client can also mint its own over HTTP (`POST /v1/auth/tokens`, in `docs/API.md`). SIGTERM or ctrl-c
lets turns in flight finish (up to 20 s), then stops every project's agents and their process groups before `wheeld`
exits; allow 30 s. A process an agent deliberately detaches (`setsid`) is beyond that. As a service, run it under
systemd with **`KillMode=mixed`** (not `control-group`, systemd's default): `mixed` sends SIGTERM to `wheeld` alone, so
it runs its own drain and stops its agents itself rather than losing the race to systemd signalling them directly —
`control-group` SIGTERMs every agent process at the same instant as `wheeld`, before the drain even starts, which
turned a turn already running into one killed mid-flight. `TimeoutStopSec` remains the backstop: if `wheeld` has not
exited by then, systemd SIGKILLs the whole cgroup, which is still what cleans up a process an agent has detached from
its group (`setsid`) and so is unreachable to `wheeld` itself.
```ini
[Service]
ExecStart=/usr/local/bin/wheeld --data-dir /var/lib/wheel
KillMode=mixed
TimeoutStopSec=35
```
That is the minimum. For a real server, §2 below installs a unit that also refuses to start on a
bad configuration, proves it is actually serving before reporting success, and bounds what a
runaway agent can take.

The browser-facing settings are closed by default. `CORS_ALLOWED_ORIGINS` is empty, and a request is refused unless
its `Host` is an IP address, `localhost`, the bind address, or a name in `WHEEL_ALLOWED_HOSTS`. That is what stops a
web page from reaching wheeld through DNS rebinding.

**Signup** (`WHEEL_SIGNUP=open|closed`):
- Closed unless you set `WHEEL_SIGNUP=open`, on every wheeld. An account's agents run as your user, so a stranger's
  signup is a stranger's code on your box, and loopback is reachable by every account on the machine and by anything a
  proxy forwards. Open it only where nobody else can reach it.
- A closed signup answers `403`. The owner still adds people, using the operator token and keeping the password off the
  command line:
  ```bash
  printf '{"email":"%s","password":"%s"}' you@example.com "$PASSWORD" | wh /v1/auth/users -d @-
  ```

**Behind a reverse proxy** (the VPS kit in `infra/vps/` sets all of these):
- `PUBLIC_BASE_URL=https://<domain>`, so ingress URLs and sessions name the public address.
- `WHEEL_TRUSTED_PROXIES`, the proxy's address, so `X-Forwarded-For` is believed from it and from no one else.
  Trusting `127.0.0.1` trusts every local process, agents included; the Docker layout trusts the proxy's container.
- `WHEEL_ALLOWED_HOSTS=<domain>`, because the proxy passes the public `Host` through.

### 2. On a server — systemd, no Docker
**This is the recommended way to run Wheel on a machine that is not your laptop.** `infra/vps/` is
a kit that builds Wheel from source on Ubuntu 24.04 and runs it under systemd: `wheeld` as a
`wheel` system user on `127.0.0.1:8080`, the board on `127.0.0.1:3000`, and Caddy in front only if
you give it a domain. The only thing you install by hand is `git`.
```bash
sudo apt-get update && sudo apt-get install -y git
sudo git clone https://github.com/Morgandri1/wheel.git /opt/wheel-installer
sudo /opt/wheel-installer/infra/vps/install.sh --no-proxy --dry-run   # resolves everything, changes nothing
sudo /opt/wheel-installer/infra/vps/install.sh --no-proxy             # tunnel mode: nothing published
sudo cat /var/lib/wheel/operator-token                                # then add your account, as in §1
```
`--domain wheel.example.com --email you@example.com` instead of `--no-proxy` gets HTTPS from
Let's Encrypt. It is idempotent — run it again with another `--ref` to upgrade, which builds first
and then restarts, so the downtime is a drain (~28 s for turns in flight) rather than a kill. A
build that does not come up is rolled back to the previous generation automatically, and
`--rollback` goes back on demand without rebuilding.

- **The host supplies the toolchain**, which is the one thing this asks of you that Docker did not:
  Node 22, pnpm, `claude` and `codex` are installed at pinned versions and **verified against what
  the binaries actually report** — including that `claude` is at or above `2.1.269`, which headless
  OAuth refresh needs. Checked at install and again on every service start.
- **Data lives in `/var/lib/wheel`** (`0700`, owned by `wheel`), with `master.key` at `0600`.
  **Back it up** — `infra/vps/backup.sh` — because losing `master.key` loses every vault secret on
  the board, permanently.
- **Wheel is compiled by a separate `wheel-build` user**, so no dependency's `build.rs` or
  `postinstall` ever runs as the account that can read `master.key`.
- **The units are hardened deliberately, not maximally.** Agents are processes inside
  `wheeld.service`, so every sandbox directive constrains the code Wheel exists to run. Directives
  that break `npm install`, a build, or a browser sandbox are refused and the refusals are recorded
  and measured. `sudo wheel-doctor` reports what is on; `infra/vps/README.md` §7 says what each
  costs you.
- **Limits fail towards the agent, not the daemon.** `OOMPolicy=continue` plus a cgroup
  `MemoryMax` means a runaway agent is killed and the engine keeps serving — without that one line,
  systemd's default stops the whole unit and takes every other project's agents with it.
- **Operating it:** `sudo wheel-doctor` (running vs serving vs actually authenticating),
  `systemd-cgls -u wheeld.service` (every agent process, live), `journalctl -fu wheeld`.

What you give up relative to §3 is the container boundary between agents and the host: agents run
as the `wheel` user on the real machine. `infra/vps/README.md` §8 is an honest account of exactly
what that means, what mitigates it and what only appears to. Read it before choosing.

### 3. Docker, headless — an alternative to §2
Before this: **Docker Engine with the Compose v2 plugin** (`docker compose version` should print a
`v2.x`; on Ubuntu, `docker.io` from apt is old enough to lack it — use Docker's own repository) and
`git`, to get this repo (the image is built from it, not pulled). Everything else — Node, the
`claude`/`codex` CLIs, the Rust toolchain used to compile `wheeld` itself — is inside the image.
```bash
docker build -f docker/Dockerfile.wheeld -t wheeld .              # or: make wheeld-image
docker run -d --name wheeld --stop-timeout 30 -v wheel-data:/data -p 127.0.0.1:8080:8080 wheeld
(umask 077; docker exec wheeld cat /data/operator-token > ~/.wheel-token)
export WHEEL_TOKEN_FILE=~/.wheel-token                             # then `wh` as above
docker exec wheeld wheeld token create --name ci                   # more tokens, the same way
```
The same thing as a compose file: `docker compose -f infra/compose.wheeld.yml up -d --build`.

- One non-root image: `wheeld`, the `wheel` CLI, `claude` and `codex`, and a development toolchain. `/data` is its volume.
- Inside the container wheeld listens on `0.0.0.0`, which is only the container's own network. Publish it on
  `127.0.0.1` only, as above.
- `docker stop` sends SIGTERM to `wheeld` (PID 1 is `tini`, which forwards it) and waits out its stop timeout — Docker's
  own default is 10 s, too short for the drain above, which is why `--stop-timeout 30` is in the `docker run` example
  and `stop_grace_period: 30s` is in `infra/compose.wheeld.yml`. A running `docker stop` still honours `-t 30` too.

### 4. The board UI (optional)
Before this: **Node.js 22.x** (`node --version`; `npx` ships with it). Nothing else — `wheel-web`
is a prebuilt package, not a build from source. The `docker compose` variant below needs only what
§3 already lists.
```bash
WHEEL_API_URL=http://127.0.0.1:8080 npx wheel-web                                # against wheeld on this machine
docker compose -f infra/compose.wheeld.yml --profile web up -d --build           # or both in compose: UI on http://127.0.0.1:3000
```
The UI signs in with email and password. Signup is closed by default, so the owner adds your account with
`POST /v1/auth/users` (above); on a machine only you use, `WHEEL_SIGNUP=open` lets you sign up on the login page
instead. The UI calls the API from its own server, and the browser never talks to the API directly.

Projects belong to the account that created them. To script the boards you use in the UI, mint a token for that account,
either from the UI or with `wheeld token create --email you@example.com`.

### 5. On your own cloud
Before this: a Railway account and the `railway` CLI for that path; for any VM/Kubernetes, Docker
(or a way to run OCI images) plus your own Postgres. Nothing here is built from source by you —
the images are what's built, per §3's prerequisites, wherever you build or pull them.
- **Railway**: fork this repo, create services from `docker/Dockerfile.api` and `docker/Dockerfile.host` (+ Postgres), apply
  `infra/railway/settings.json` with `infra/railway/apply-settings.sh`; env vars are listed in `infra/railway/README.md` and `web/DEPLOY.md`.
  The host runs agents as per-project unix users on one machine (no Docker daemon needed) — size it for your agents' builds.
- **Any VM / Kubernetes**: run the two images with Postgres; the host needs a persistent volume at `/data` and must NOT be publicly
  reachable (the API talks to it over a private network with `WHEEL_HOST_SECRET`). The web app is a standard Next.js server
  that reaches the API at `WHEEL_API_URL`.
- API tokens work on the cloud API too, whatever identity provider signs its sessions, so a desktop or CI client
  authenticates the same way everywhere.

### Developing Wheel: the multi-service stack
Before this: the same as §3 (Docker Engine with the Compose v2 plugin, `git`) — Postgres, the API
and the host all come from the images built below, not from anything installed by hand.
```bash
docker network create wheel
docker compose -f infra/docker-compose.yml up --build              # postgres + api + host, API on 127.0.0.1:8080
docker compose -f infra/docker-compose.yml --profile web up --build   # plus the board UI on 127.0.0.1:3000
```

### Agents and credentials
Agents are Claude Code / Codex processes. Give them credentials through a **vault** node (one vault per account; wire the agent to it) or the
agent's Authenticate panel (in-browser Anthropic login, `claude setup-token`, or an API key). Nothing in Wheel ever shows a stored secret back.
See `docs/ARCHITECTURE.md` for the model and `docs/WHEEL-ON-WHEEL.md` for a board that develops Wheel itself.

# Development
Wheel develops itself; there is a cloud board (template available for free) that handles each moving piece separately so that the agents can figure out what they need and build it themselves. If you want to contribute to wheel, you can clone it and get started in the `crates/`, `web/`, or `docker/` directory.  

# Legal disclaimer, asshole
Wheel is independent, original work. It shares no code, assets, copy, designs, or other protected material with any other product, and it wasn't built using anyone's confidential or proprietary information.

Ideas aren't ownable; expression is. Wheel is my own expression of ideas that are common to this category of tooling. If you think otherwise, the contact address is in the LICENSE — put it in writing.
