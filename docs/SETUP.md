# Setup

Everything beyond the README's fastest path: building from source, the board UI in depth, production
deployment, and every environment variable that shapes how `wheeld` runs.

## Building `wheeld` from source

Needs: Rust stable 1.88+ ([rustup.rs](https://rustup.rs) — not your distro's packaged `rustc`, it's usually too
old), a C compiler (`build-essential` on Debian/Ubuntu, Xcode Command Line Tools on macOS), and `git` (also used
at runtime, for agent workspaces backed by a repo). Building doesn't need Node — but running an agent does:
`npm install -g @anthropic-ai/claude-code @openai/codex`, or use Docker, which bundles both.

```bash
cargo build --release -p wheeld
./target/release/wheeld          # API + sandbox host + agents in one process, sqlite store, on http://127.0.0.1:8080
```

- Listens on `127.0.0.1:8080` by default. `--bind` / `BIND_ADDR` changes that; any address other people can reach
  logs a warning saying so.
- Everything lives in `~/.wheel` (`--data-dir` / `WHEEL_DATA_DIR`): the store, `master.key`, every project. The
  directory is `0700` — treat it like an SSH key.
- First start writes an operator token to `~/.wheel/operator-token` (`0600`). The log shows the path, never the
  token.

## Tokens

```bash
wheeld token create --name laptop         # prints the new token, on stdout, this once
wheeld token list                         # id, account, name, created, last used, revoked
wheeld token revoke <id>                  # revokes it and every token it minted
```

Tokens are managed against the data directory itself — whoever can read it can already read `master.key`, so
this is no new power. Any signed-in client can also mint its own over HTTP (`POST /v1/auth/tokens`, in
`docs/API.md`).

## Shutdown

SIGTERM or ctrl-c lets turns in flight finish (up to 20s), then stops every project's agents and their process
groups before `wheeld` exits; allow 30s. A process an agent deliberately detaches (`setsid`) is beyond that.

As a systemd service, use `KillMode=mixed`, not the default `control-group`: `mixed` sends SIGTERM to `wheeld`
alone, so it runs its own drain and stops its agents itself, rather than systemd signalling every agent process
at the same instant `wheeld` gets its own signal — which would kill a turn already running mid-flight.
`TimeoutStopSec` is the backstop: past it, systemd SIGKILLs the whole cgroup, which is what still cleans up a
detached (`setsid`) process `wheeld` itself can no longer reach.

```ini
[Service]
ExecStart=/usr/local/bin/wheeld --data-dir /var/lib/wheel
KillMode=mixed
TimeoutStopSec=30
```

## Signup

`WHEEL_SIGNUP=open|closed` (closed is the default). An account's agents run as your user, so a stranger's signup
is a stranger's code on your box — open it only where nobody else can reach it. While closed, the owner adds
people directly, keeping the password off the command line:

```bash
printf '{"email":"%s","password":"%s"}' you@example.com "$PASSWORD" | wh /v1/auth/users -d @-
```

## Network and CORS

The browser-facing settings are closed by default: `CORS_ALLOWED_ORIGINS` is empty, and a request is refused
unless its `Host` is an IP address, `localhost`, the bind address, or a name in `WHEEL_ALLOWED_HOSTS` — that's
what stops a web page reaching `wheeld` through DNS rebinding.

**Behind a reverse proxy** (the VPS kit in `infra/vps/` sets all of these):
- `PUBLIC_BASE_URL=https://<domain>` — ingress URLs and sessions name the public address.
- `WHEEL_TRUSTED_PROXIES=<proxy address>` — `X-Forwarded-For` is believed from this address and no one else.
  Trusting `127.0.0.1` trusts every local process, agents included; the Docker layout trusts the proxy's
  container.
- `WHEEL_ALLOWED_HOSTS=<domain>` — the proxy passes the public `Host` through, and it needs to be admitted.

## Docker, in depth

Needs: Docker Engine with the Compose v2 plugin (`docker compose version` should print `v2.x`; Ubuntu's `docker.io`
apt package is often too old — use Docker's own repository) and `git`, to get this repo (the image is built from
it, not pulled).

```bash
docker build -f docker/Dockerfile.wheeld -t wheeld .              # or: make wheeld-image
docker run -d --name wheeld --stop-timeout 30 -v wheel-data:/data -p 127.0.0.1:8080:8080 wheeld
(umask 077; docker exec wheeld cat /data/operator-token > ~/.wheel-token)
export WHEEL_TOKEN_FILE=~/.wheel-token                             # then `wh` as in the README
docker exec wheeld wheeld token create --name ci                   # more tokens, the same way
```
The same thing as a compose file: `docker compose -f infra/compose.wheeld.yml up -d --build`.

- One non-root image: `wheeld`, the `wheel` CLI, `claude`, `codex`, and a development toolchain. `/data` is its
  volume.
- Inside the container `wheeld` listens on `0.0.0.0`, which is only the container's own network — publish it on
  `127.0.0.1` only, as above.
- `docker stop` sends SIGTERM to `wheeld` (PID 1 is `tini`, which forwards it) and waits out its stop timeout.
  Docker's own default is 10s, too short for the drain above — that's why `--stop-timeout 30` is in the
  `docker run` example and `stop_grace_period: 30s` is in `infra/compose.wheeld.yml`.

## The board UI, in depth

Needs: Node.js 22.x (`npx` ships with it). `wheel-web` is a prebuilt package, not a build from source.

```bash
WHEEL_API_URL=http://127.0.0.1:8080 npx wheel-web                                # against wheeld on this machine
docker compose -f infra/compose.wheeld.yml --profile web up -d --build           # or both in compose: UI on http://127.0.0.1:3000
```

The UI signs in with email and password. Signup is closed by default, so the owner adds your account with
`POST /v1/auth/users` (above); on a machine only you use, `WHEEL_SIGNUP=open` lets you sign up on the login page
instead. The UI calls the API from its own server, and the browser never talks to the API directly.

Projects belong to the account that created them. To script the boards you use in the UI, mint a token for that
account, either from the UI or with `wheeld token create --email you@example.com`.

## On your own cloud

Nothing here is built from source by you — the images are what's built, per the Docker section's prerequisites,
wherever you build or pull them.

- **Railway**: needs a Railway account and the `railway` CLI. Fork this repo, create services from
  `docker/Dockerfile.api` and `docker/Dockerfile.host` (+ Postgres), apply `infra/railway/settings.json` with
  `infra/railway/apply-settings.sh`; env vars are listed in `infra/railway/README.md` and `web/DEPLOY.md`. The
  host runs agents as per-project unix users on one machine (no Docker daemon needed) — size it for your agents'
  builds.
- **Any VM / Kubernetes**: needs Docker (or a way to run OCI images) plus your own Postgres. Run the two images
  with Postgres; the host needs a persistent volume at `/data` and must NOT be publicly reachable (the API talks
  to it over a private network with `WHEEL_HOST_SECRET`). The web app is a standard Next.js server that reaches
  the API at `WHEEL_API_URL`.
- API tokens work on the cloud API too, whatever identity provider signs its sessions, so a desktop or CI client
  authenticates the same way everywhere.

## Developing Wheel: the multi-service stack

Before this: Docker Engine with the Compose v2 plugin, `git` — Postgres, the API and the host all come from the
images built below, not from anything installed by hand.

```bash
docker network create wheel
docker compose -f infra/docker-compose.yml up --build              # postgres + api + host, API on 127.0.0.1:8080
docker compose -f infra/docker-compose.yml --profile web up --build   # plus the board UI on 127.0.0.1:3000
```
