# Wheel on one server

<!--
Copyright Morgan Metz
Licensed under the PolyForm Noncommercial License 1.0.0.
See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
-->

Wheel on a single Linode, used from the browser and from AgentGrid. Two modes, chosen by whether
you have a domain yet:

```
TUNNEL MODE (default, no domain yet — nothing published to the network)
  you ── ssh -L 3000:127.0.0.1:3000 -L 8080:127.0.0.1:8080 ──► the server
                                          127.0.0.1:3000 ──► web ──► wheeld
                                          127.0.0.1:8080 ──► wheeld

TLS MODE (WHEEL_DOMAIN set, once its A record points here)
  internet ──► :80 / :443  Caddy ──┬── /v1/*  /p/*  ──►  wheeld :8080 ──► one engine per project (unix sockets)
                                   └── everything else ──►  web :3000  ──►  wheeld  (server to server)
```

There is no third mode that serves plain HTTP to the network. `deploy.sh`'s `preflight` refuses to
start on a configuration that would do that — see [Without a domain yet](#without-a-domain-yet-tunnel-mode)
for why that matters even on a box behind a firewall.

- **`deploy.sh` is the only thing that should ever run `docker compose` against this directory.**
  It computes settings `compose.yml` needs from `infra/vps/.env` and can safely coexist with (and
  retire) an older deployment on the same box.
- **`/v1` is Wheel's API.** Every request needs a credential: a session or a `wht_` API token.
  AgentGrid and scripts use it with a token. Project engines are never exposed; the API is the
  only way to them, and it checks that you own the project first.
- **`/p/<project>/<path>` is public webhook ingress**, TLS mode only. It is off for every project
  until you switch it on, and anything that reaches it becomes a message to whatever agent the
  endpoint is wired to. See [Webhooks](#webhooks-p).
- **Signup is closed.** You hold the operator token, and you add accounts with it.

The files:

| File | What it is |
|---|---|
| `deploy.sh` | The entry point. `--dry-run`, `--stop-legacy` for an older deployment on the same box |
| `compose.yml` | The stack: `wheeld`, `web`, `caddy` (TLS mode only), a `preflight` that refuses a bad `.env` |
| `lib/derive-env.sh` | Computes the settings that follow from `WHEEL_DOMAIN`; shared by `deploy.sh` and `rehearse.sh` |
| `Caddyfile` | The proxy: TLS, routing, body limits, forwarded headers, security headers |
| `.env.example` | Your settings; copy it to `.env` |
| `install.sh`, `systemd/` | The same thing without Docker, built from source (not the path below) |
| `rehearse.sh`, `rehearsal/` | The whole stack on a laptop, and every check below |

## 1. The server

- Ubuntu 24.04, Docker and the Compose plugin already installed (`docker --version`,
  `docker compose version`). If not, see [Installing Docker](#installing-docker).
- **A small box works, with tuning** — see [Running on a small box](#running-on-a-small-box-2-vcpu-4-gib).
  Wheel compiles Rust to build the images the first time, which is the heaviest moment in the
  server's life.
- **A domain is optional, and most of this README works without one.** If you have one already,
  its A record should point at this server before you flip to [TLS mode](#going-live-flipping-to-tls-mode).
  If you don't yet, start in tunnel mode — nothing here waits on DNS.

### Firewall reality, read before trusting `ufw`

Three things are true on a typical VPS and worth checking on yours before you rely on any of them:

1. **`ufw` does not see Docker's published ports.** Docker writes its own `iptables` rules ahead
   of `ufw`'s chain, so `docker compose`'s `ports:` mapping can be reachable even when `ufw status`
   shows the port is not allowed. A rule you did not write is not a rule you can trust.
2. **A cloud firewall (Linode's Cloud Firewall, in front of the VM) applies before any of that.**
   It is the one control that is not Docker's or `ufw`'s to bypass.
3. **`0.0.0.0:PORT` and `127.0.0.1:PORT` are different promises.** The former is reachable from
   the network (modulo #2); the latter is reachable only from this machine, tunnel or no tunnel.

The consequence for this kit: **wheeld and web publish on `127.0.0.1` only, in every mode**, and
Caddy — the one thing that publishes on `0.0.0.0` — runs at all only when you have set
`WHEEL_DOMAIN`. Set the cloud firewall regardless, because it is the layer that is not conditional
on anything this kit does right:

| Port | Why |
|---|---|
| 22/tcp | SSH. Narrow it to your own address if you can. |
| 80/tcp | TLS mode only: Let's Encrypt's HTTP challenge, and the redirect to HTTPS |
| 443/tcp, 443/udp | TLS mode only: HTTPS (443/udp is HTTP/3, optional) |

## 2. Deploy

```bash
git clone https://github.com/Morgandri1/wheel.git /opt/wheel
cd /opt/wheel/infra/vps
cp .env.example .env && chmod 600 .env
```

Leave `WHEEL_DOMAIN` unset for now if you don't have one yet — that's tunnel mode, the default,
and section 3 covers it. If **another Wheel deployment is already running on this box** (an older
dev stack, a previous install), read [Replacing an existing deployment](#replacing-an-existing-deployment-stop-legacy)
first; `deploy.sh` will otherwise just fail to bind the ports it needs.

```bash
./deploy.sh --dry-run     # prints the resolved settings and every command; changes nothing
./deploy.sh               # the real thing
```

The first build compiles everything and takes a while — see
[Running on a small box](#running-on-a-small-box-2-vcpu-4-gib) if it's slow or gets killed.
`deploy.sh` prints `docker compose ps` when it finishes; `preflight` should show `Exited (0)` and
`wheeld`/`web` should show `healthy`.

### Replacing an existing deployment (`--stop-legacy`)

```bash
./deploy.sh --stop-legacy --dry-run    # shows exactly what it found and what it would stop
./deploy.sh --stop-legacy              # docker compose -p <project> down — never -v
```

This stops an older compose project (default name `wheel`) if one is running. **It never passes
`-v`**, so that project's volumes are left exactly as they were — read them later, or remove them
yourself once you've confirmed you don't need them (`docker volume ls`, then `docker volume rm`).
Without `--stop-legacy`, an old deployment holding the ports this one needs just makes
`docker compose up` fail to bind them, loudly, which is the safe default: nothing here stops
another deployment by surprise. `--legacy-project <name>` targets a different project name.

## 3. Without a domain yet (tunnel mode)

This is the default — nothing above published a single port to the network. Reach it from your
own machine:

```bash
ssh -L 3000:127.0.0.1:3000 -L 8080:127.0.0.1:8080 <user>@<server>
```

Leave that running, then from **your own machine** (through the tunnel):

```bash
# the operator token, read once
ssh <user>@<server> "docker compose -p wheel exec wheeld cat /data/operator-token"
```

Add your account and sign in — signup is closed, so this token is the only way in:

```bash
read -rs WHEEL_OPERATOR_TOKEN; read -rs PASSWORD
printf '{"email":"you@example.com","password":"%s"}' "$PASSWORD" |
  curl -fsS http://localhost:8080/v1/auth/users \
    -H @<(printf 'x-auth-token: %s\n' "$WHEEL_OPERATOR_TOKEN") \
    -H 'content-type: application/json' -d @-
```

Then open `http://localhost:3000/sign-in` in your browser (through the tunnel) and sign in.
**AgentGrid** connects to `http://localhost:8080` (through the same tunnel) with a `wht_` token —
see [AgentGrid and other API clients](#4-agentgrid-and-other-api-clients).

Why tunnel mode has no plain-HTTP fallback: a VPS's published ports are reachable the moment
anything upstream — a cloud firewall rule you forgot, `ufw` not covering Docker's rules as above —
lets traffic through, and a password or a `wht_` token does not get a second chance once it has
crossed the network once in the clear. Loopback-plus-tunnel, or a real certificate: nothing in
between.

## Going live: flipping to TLS mode

Once your domain's A record points at this server:

```bash
dig +short wheel.example.com     # must print this server's address
```

Edit `.env`: set `WHEEL_DOMAIN=wheel.example.com` (and, optionally, `ACME_EMAIL=you@example.com`).
Then:

```bash
./deploy.sh --dry-run    # confirm it now says TLS mode
./deploy.sh
docker compose -p wheel logs caddy | grep -i certificate
```

Caddy starts (it did not exist as a running service in tunnel mode), gets a Let's Encrypt
certificate, and starts publishing 80 and 443. wheeld and web keep publishing on `127.0.0.1` too —
that's still your tunnel if you want it, now alongside the public site at
`https://wheel.example.com`. Nothing you did in tunnel mode (your account, your projects) is lost;
`wheel-data` is untouched by this flip.

## 4. AgentGrid, and other API clients

AgentGrid needs a URL and a `wht_` token — `http://localhost:8080` through a tunnel, or
`https://wheel.example.com` once you're in TLS mode. Either way, the events socket is at
`.../v1/projects/<id>/engine/v1/events` with the token in a header.

Mint a token for **your own account** (not the operator's, which owns no projects of yours):

```bash
docker compose -p wheel exec wheeld wheeld token create --name agentgrid --email you@example.com
```

It prints the token once. Revoke it with `wheeld token revoke <id>`; that also revokes every token
minted with it. Any HTTP client works the same way:

```bash
curl -fsS http://localhost:8080/v1/projects -H @<(printf 'x-auth-token: %s\n' "$TOKEN")   # tunnel
curl -fsS https://wheel.example.com/v1/projects -H @<(printf 'x-auth-token: %s\n' "$TOKEN") # TLS
```

The `wheel` CLI has no operator mode yet. Today it is the agent-side CLI that runs inside a
project's sandbox. `wheel login`/`wheel projects` against a server is follow-up F1 in
`docs/proposals/headless-first.md`; until then use AgentGrid or the API directly.

## 5. Agent credentials

An agent's Claude/Codex login lives in a **vault node**, never in `.env`, never in a compose file,
never in this document with a real value in it. Start with a single Anthropic API key.

**In the web app** (`http://localhost:3000/app` through a tunnel, or `https://wheel.example.com/app`):
add a `vault` node, name it (e.g. `anthropic`), add the key `ANTHROPIC_API_KEY`, and enter the
value in the vault inspector — it is write-only and is never shown back, on the board, in an
export or in a log. Wire your agent to the vault with a `read` wire. The key is exported into the
agent's environment the next time it starts.

**From the API**, with your own session or a `wht_` token (`$API` below is `http://localhost:8080`
through a tunnel, or `https://wheel.example.com`), three calls against the project's engine:

```bash
VAULT=$(curl -fsS "$API/v1/projects/$PID/engine/v1/nodes" \
  -H "x-auth-token: $TOKEN" -H 'content-type: application/json' \
  -d '{"name":"anthropic","type":"vault","config":{"keys":["ANTHROPIC_API_KEY"]}}' | jq -r .id)

curl -fsS "$API/v1/projects/$PID/engine/v1/wires" \
  -H "x-auth-token: $TOKEN" -H 'content-type: application/json' \
  -d "{\"from\":\"$AGENT_ID\",\"to\":\"$VAULT\",\"type\":\"read\"}"

read -rs ANTHROPIC_API_KEY   # typed, not in shell history or argv
printf '{"value":"%s"}' "$ANTHROPIC_API_KEY" |
  curl -fsS -X PUT "$API/v1/projects/$PID/engine/v1/vault/$VAULT/ANTHROPIC_API_KEY" \
    -H "x-auth-token: $TOKEN" -H 'content-type: application/json' -d @-
```

Restart the agent (or start it for the first time) and its `GET .../agents/$AGENT_ID/auth` reports
`mode: "env"`: authenticated, no browser step. Other recognised keys: `CLAUDE_CODE_OAUTH_TOKEN`
(from `claude setup-token`, the native OAuth flow rather than an API key) and `CODEX_API_KEY`. One
vault per account, so wiring the same agent to two vaults that both define `ANTHROPIC_API_KEY` is
refused at wire-creation time — see `docs/ARCHITECTURE.md` M1.6.

## Running on a small box (2 vCPU, ~4 GiB)

Wheel runs on a small VPS; a few things are worth knowing before you find them the hard way.

- **The first build is the heaviest moment.** `deploy.sh`/`docker compose up --build` compiles a
  Rust workspace and a Next.js app on a 2-vCPU box. It will take a while and will use most of the
  machine's CPU; that's expected and it finishes. If the build process is OOM-killed (`docker
  compose build` exits with no clear error, or the daemon log mentions `Killed`), add swap first
  (below) and retry — Rust's linker step is the usual spike.
- **Add swap** if you have not already; a 2 GiB file is enough headroom for the build without
  meaningfully slowing steady-state operation on a box with SSD-backed storage:
  ```bash
  sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile
  sudo mkswap /swapfile && sudo swapon /swapfile
  echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
  ```
- **There is no per-host cap on concurrently running agents in the engine today** — `wheel-engine`
  will run every agent you start. On a small box, that is a discipline you hold, not a knob the
  software gives you yet: keep few agents with `run_on_startup: true`, and set a short
  `idle_timeout_secs` (default 300; see `docs/ARCHITECTURE.md` §3c#14) on ones that don't need to
  stay warm — an idle agent's process stops and resumes transparently on the next message, which
  is real memory back on a 4 GiB box.
- **An agent building Rust inside its own workspace** (a `wheel-on-wheel`-style board) can use
  every core and a lot of RAM on `cargo build`'s own account. There is no wiring today for the
  engine to cap this per-agent; if you hit it, have the agent's own `~/.cargo/config.toml` (inside
  its workspace) set `[build]\njobs = 1`, or ask it to.

## Webhooks (`/p`)

TLS mode only — there is no public ingress in tunnel mode, because there is nothing public to hit.

`https://wheel.example.com/p/<project>/<path>` is public by design: a webhook sender can be given
nothing but a URL. It is off for every project until the owner turns on the project's `http`
capability. The edge caps a webhook body at 256 KiB, the size of one message.

**Whatever reaches an endpoint becomes a message to the agent wired to it.** An endpoint with no
auth, wired to an agent that can write to the board, message other agents or read a vault, is an
open line from the internet to that agent (redteam finding 043). Give endpoints a shared secret or
HMAC where the sender supports it, and wire them to an agent with the fewest wires you can.

## Backups

Everything is in the `wheel-data` volume: the database, every project, `master.key` (which
decrypts every secret) and the operator token. Stop wheeld while copying it, because SQLite is
live:

```bash
cd /opt/wheel/infra/vps
docker compose -p wheel stop wheeld
docker run --rm -v wheel_wheel-data:/data:ro -v "$PWD:/backup" debian:bookworm-slim \
  tar -C /data -czf "/backup/wheel-data-$(date +%F).tar.gz" .
docker compose -p wheel start wheeld
```

Whoever has that file has every secret on the board. Encrypt it before it leaves the server, for
example with `gpg --symmetric`. To restore, `docker compose -p wheel down`, extract into a fresh
`wheel_wheel-data` volume the same way, then `./deploy.sh`. `caddy-data` (TLS mode) holds the
certificates and the ACME account. Losing it only means Caddy asks for new ones.

## Upgrading

```bash
cd /opt/wheel && git pull --ff-only
cd infra/vps && ./deploy.sh --dry-run   # confirm the settings still resolve the way you expect
./deploy.sh
docker compose -p wheel logs --tail 50 wheeld
```

wheeld gets 35 seconds on SIGTERM to drain in-flight turns (about 28s) and stop every agent's own
process group. Agents come back parked and pick their sessions up on the next message.

## Installing Docker

Docker's own packages, not Ubuntu's:

```bash
sudo apt-get update && sudo apt-get install -y ca-certificates curl git
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
sudo chmod a+r /etc/apt/keyrings/docker.asc
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
  | sudo tee /etc/apt/sources.list.d/docker.list >/dev/null
sudo apt-get update && sudo apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
```

## The web app somewhere else (Vercel)

Supported, but not the default. The web app's server has to reach the API, so wheeld's `/v1` must
be public — this is TLS mode's topology already. Deploy `web/` with
`WHEEL_API_URL=https://wheel.example.com` and `WHEEL_PUBLIC_ORIGIN=<the web app's own origin>` (see
`web/DEPLOY.md`). Caddy then only needs to route `/v1` and `/p`. What you give up is the internal
network: the web server's calls cross the internet, authenticated by the user's session.

## Without Docker: `install.sh`

Not the path above — this is systemd services built from source, for a box without Docker at all.
See its own `--help` for the full flag set (`--domain`, `--public-host`, `--no-proxy`, `--updatable`,
`--firewall`, `--dry-run`); it installs Node 22, a shared Rust toolchain, the `claude`/`codex` CLIs
and, when a proxy mode is chosen, Caddy from Caddy's own apt repository. Wheel is compiled by an
unprivileged `wheel-build` user, so no dependency's build script runs as the account that can read
`master.key`. wheeld runs as the `wheel` system user on `127.0.0.1:8080`, data in `/var/lib/wheel`
(`0700`); the web app runs on `127.0.0.1:3000` as a throwaway systemd user. It is idempotent: run
it again with another `--ref` to upgrade. `--dry-run` resolves settings and the target commit and
prints every command it would run — verified on a bare `ubuntu:24.04` container to leave no user,
directory, file or package behind.

```bash
sudo git clone https://github.com/Morgandri1/wheel.git /opt/wheel-installer
sudo /opt/wheel-installer/infra/vps/install.sh --domain wheel.example.com --email you@example.com --dry-run
sudo /opt/wheel-installer/infra/vps/install.sh --domain wheel.example.com --email you@example.com
```

### Deploying over SSH, non-interactively

Every input to `install.sh` is a flag; nothing reads stdin. Given a host, a user with passwordless
sudo, and a dedicated SSH key:

```bash
HOST=203.0.113.5 SSH_USER=root KEY=~/.ssh/wheel_deploy_key DOMAIN=wheel.example.com EMAIL=you@example.com REF=main

ssh_run() { ssh -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=accept-new "$SSH_USER@$HOST" "$@"; }

ssh_run 'sudo -n true' || { echo "no passwordless sudo for $SSH_USER@$HOST" >&2; exit 1; }

ssh_run bash -s -- "$REF" <<'REMOTE'
set -euo pipefail
ref="$1"
if [ ! -d /opt/wheel-installer/.git ]; then
    git clone --quiet https://github.com/Morgandri1/wheel.git /opt/wheel-installer
fi
git -C /opt/wheel-installer fetch --quiet origin "$ref"
git -C /opt/wheel-installer -c advice.detachedHead=false checkout --quiet --force "origin/$ref"
REMOTE

ssh_run sudo /opt/wheel-installer/infra/vps/install.sh --domain "$DOMAIN" --email "$EMAIL" --ref "$REF" --firewall --dry-run
ssh_run sudo /opt/wheel-installer/infra/vps/install.sh --domain "$DOMAIN" --email "$EMAIL" --ref "$REF" --firewall
TOKEN=$(ssh_run sudo cat /var/lib/wheel/operator-token)
```

`BatchMode=yes` fails instead of ever prompting; `StrictHostKeyChecking=accept-new` accepts a
host's key on first connection and still verifies it on every one after.

### Auto-update hook points

`sdk/auto-update` targets exactly this layout, a source checkout that wheeld rebuilds itself from:

| Hook | Here |
|---|---|
| Checkout (`WHEEL_UPDATE_REPO`) | `/opt/wheel/src` |
| Binaries (`WHEEL_UPDATE_BIN_DIR`) | `/opt/wheel/bin` |
| Build staging (`WHEEL_UPDATE_STAGING`) | `/var/cache/wheel/update`, off the data directory |
| Restart | `wheeld.service`: `Restart=on-failure` also restarts after `WHEEL_UPDATE_RESTART=exit` (exit 75) |
| Policy | `WHEEL_AUTO_UPDATE` in `/etc/wheel/wheeld.local.env`, off unless you set it |

By default the checkout and the binaries belong to root, so nothing wheeld runs, agents included,
can rewrite them. `install.sh --updatable` hands both to `wheel` and adds those paths to the unit.
That is what lets wheeld replace itself. It also lets any agent do so, because agents run as
wheeld's user (the auto-update proposal's T7). Enable it knowingly.

## Rehearsing on a laptop

`infra/vps/rehearse.sh` brings this exact `compose.yml` up on your machine in TLS mode, behind
Caddy with a certificate from Caddy's own CA instead of Let's Encrypt. It then checks, each check
as its own process with its own exit code:

- the web app is served, with the security headers;
- signup is refused, at the edge and by wheeld itself;
- the operator adds an account, and signing in works;
- a project, an agent, and a message that reaches `queued`, or `delivered` with the fake harness;
- `/v1` with and without a `wht_` token;
- the events WebSocket with header auth, and the web app's event stream through Caddy;
- webhook ingress, and that forged `X-Forwarded-*` headers never reach wheeld or change who the
  rate limit counts;
- the body limits;
- that wheeld and web publish on `127.0.0.1` only, and that path actually works;
- that Caddy's admin API is off.

```bash
infra/vps/rehearse.sh                                        # https://localhost
REHEARSE_DOMAIN=wheel.rehearsal.test infra/vps/rehearse.sh   # any name; no DNS needed
REHEARSE_FAKE_HARNESS=1 infra/vps/rehearse.sh                # messages reach `delivered`
```

It needs ports 80 and 443 free, and it builds from `git archive` of a commit, never your working
tree. Tunnel mode is exercised by `deploy.sh`'s own logic (it is what actually resolves the
settings for it) rather than by `rehearse.sh`, which always drives the stack in TLS mode so the
public-facing behaviour — the part with a bigger blast radius — gets the full check suite every
time; verify tunnel mode by hand the same way section 3 describes, against a `deploy.sh` run with
`WHEEL_DOMAIN` left unset.

A gate that has never failed proves nothing, so `rehearsal/mutate.sh edge` breaks the Caddyfile
the way people break proxies, and publishes wheeld on a non-loopback address. It exits 0 only if
every check that guards those layers comes back red. The broken layers:

- trusting every proxy
- dropping HSTS
- raising the webhook limit
- un-blocking sign-up
- forwarding the cookie
- buffering the event stream
- turning the admin API on
- a non-loopback publish

`rehearsal/mutate.sh config` does the same for wheeld's signup flag and the web app's origin.

**One known gap in this rehearsal, not in the kit:** `rehearse.sh` also checks that a container on
a different Docker network cannot reach wheeld's or web's address on the `edge` network (the
promise of `internal: true`). Measured directly: OrbStack's engine does not enforce this (a
container on another network reaches it anyway); stock `dockerd` does (confirmed via `docker:dind`
— the same probe gets a connection timeout there). If you're rehearsing on a Mac with OrbStack,
expect this one check red; the real VPS runs stock `dockerd` and it passes there.

## When something is wrong

| Symptom | Cause |
|---|---|
| `deploy.sh` fails to bind a port | Another deployment already holds it — see [Replacing an existing deployment](#replacing-an-existing-deployment-stop-legacy) |
| No certificate; Caddy logs ACME errors | The A record does not point here yet, or 80/443 are closed in the cloud firewall |
| `preflight` exited 1 | `.env` is wrong; `docker compose -p wheel logs preflight` names the setting |
| Sign-in answers `403 cross_origin` | The browser's origin doesn't match `WHEEL_PUBLIC_ORIGIN`: in tunnel mode use `http://localhost:3000`, not the server's IP directly |
| `/v1` answers `403` naming `WHEEL_ALLOWED_HOSTS` | The request used a host name wheeld was not told about |
| `413 ... at the proxy` | Over the edge limit: 256 KiB for webhooks, 5 MiB for everything else (wheeld and the web app refuse more anyway) |
| Webhook `403` | The project's `http` capability is off, or you're in tunnel mode (no ingress) |
| `docker compose build` seems to hang or the daemon logs `Killed` | Out of memory on the first Rust build — see [Running on a small box](#running-on-a-small-box-2-vcpu-4-gib) |
