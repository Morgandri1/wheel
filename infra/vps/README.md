# Wheel on one server

<!--
Copyright Morgan Metz
Licensed under the PolyForm Noncommercial License 1.0.0.
See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
-->

Wheel on a single Linode, used from the browser and from AgentGrid, with HTTPS from Let's Encrypt.
The steps assume Ubuntu 24.04.

```
internet ──► :80 / :443  Caddy ──┬── /v1/*  /p/*  ──►  wheeld :8080 ──► one engine per project (unix sockets)
                                 └── everything else ──►  web :3000  ──►  wheeld  (server to server)
```

- **Caddy is the only thing with a published port.** wheeld and the web app sit on an internal
  Docker network that has no gateway: nothing outside the server can reach them except through
  Caddy, and the web app cannot reach the internet at all.
- **`/v1` is Wheel's API.** Every request needs a credential: a session or a `wht_` API token.
  AgentGrid and scripts use it with a token. Project engines are never exposed; the API is the
  only way to them, and it checks that you own the project first.
- **`/p/<project>/<path>` is public webhook ingress.** It is off for every project until you
  switch it on, and anything that reaches it becomes a message to whatever agent the endpoint is
  wired to. See [Webhooks](#webhooks-p).
- **Signup is closed.** You hold the operator token, and you add accounts with it.

The files:

| File | What it is |
|---|---|
| `compose.yml` | The stack: `wheeld`, `web`, `caddy`, and a `preflight` that refuses a bad `.env` |
| `Caddyfile` | The proxy: TLS, routing, body limits, forwarded headers, security headers |
| `.env.example` | Your settings; copy it to `.env` |
| `compose.tunnel.yml` | The no-proxy variant: nothing public, reached over SSH |
| `install.sh`, `systemd/` | The same thing without Docker, built from source |
| `rehearse.sh`, `rehearsal/` | The whole stack on a laptop, and every check below |

## 1. The server

- A Linode with **Ubuntu 24.04**. Docker builds Wheel from source on the server, including a Rust
  release build, so give it room: 4 GB of RAM at the least, 8 GB to be comfortable, or add swap.
  (Guidance, not a measurement on a Linode.)
- A **domain** whose **A record** points at the Linode's IPv4 address. Add an AAAA record for IPv6
  if you want it. Check it resolves before going further:
  ```bash
  dig +short wheel.example.com     # must print the Linode's address
  ```
  Let's Encrypt checks the domain from the internet on port 80 or 443, so the certificate cannot be
  issued until this is right. No domain? See [Without a domain](#without-a-domain).

### Firewall

In Linode's **Cloud Firewall**, set the inbound policy to drop, then accept:

| Port | Why |
|---|---|
| 22/tcp | SSH. Narrow it to your own address if you can. |
| 80/tcp | Let's Encrypt's HTTP challenge, and the redirect to HTTPS |
| 443/tcp | HTTPS |
| 443/udp | HTTP/3 (optional) |

A cloud firewall applies before traffic reaches the server. `ufw` on the server does not cover
Docker: Docker writes its own iptables rules for published ports and bypasses `ufw`. That is
harmless here only because Caddy's 80 and 443 are the only ports this stack publishes.

## 2. Docker

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

## 3. Wheel

```bash
sudo git clone https://github.com/Morgandri1/wheel.git /opt/wheel
cd /opt/wheel/infra/vps
sudo cp .env.example .env && sudo chmod 600 .env
sudoedit .env
```

In `.env`, set `WHEEL_DOMAIN` to your domain and, optionally, `ACME_EMAIL`. Leave `WHEEL_SIGNUP`
at `closed`. `.env` holds no secrets by default: wheeld generates its master key and operator token
inside its own volume. It is still `600` and git-ignored, because what goes in it next may be.

```bash
sudo docker compose up -d --build
sudo docker compose ps                     # preflight exited 0; wheeld and web healthy; caddy running
sudo docker compose logs caddy | grep -i certificate
```

The first build compiles everything and takes a while. If `.env` is wrong, `preflight` exits 1,
nothing else starts, and `docker compose logs preflight` says what to fix.

## 4. The operator token

On first start, wheeld creates a token-only owner account and writes its token into its volume:

```bash
sudo docker compose exec wheeld cat /data/operator-token
```

It is the only credential that can add accounts. Keep it like a root password; `wheeld token`
inside the container lists and revokes tokens (`sudo docker compose exec wheeld wheeld token list`).

## 5. Your account, and signing in

Signup is closed, so add your own account with the operator token. From your laptop, with the
token and password read without echoing, and neither in your shell history nor on a command line:

```bash
read -rs WHEEL_OPERATOR_TOKEN; read -rs PASSWORD
printf '{"email":"you@example.com","password":"%s"}' "$PASSWORD" |
  curl -fsS https://wheel.example.com/v1/auth/users \
    -H @<(printf 'x-auth-token: %s\n' "$WHEEL_OPERATOR_TOKEN") \
    -H 'content-type: application/json' -d @-
```

`201` means done. Then sign in at `https://wheel.example.com/sign-in`. The session lives in an
`HttpOnly`, `Secure` cookie that the browser sends only to the web app. The browser never talks to
the API: the web server does, from inside the server.

## 6. AgentGrid, and other API clients

AgentGrid needs the server's URL and a `wht_` token:

- **URL:** `https://wheel.example.com`. AgentGrid calls `/v1/...` there, and the events socket at
  `wss://wheel.example.com/v1/projects/<id>/engine/v1/events` with the token in a header.
- **Token:** mint one for **your** account, so AgentGrid sees the projects you made in the browser.
  The operator token belongs to the token-only owner, which owns no projects of yours.
  ```bash
  sudo docker compose exec wheeld wheeld token create --name agentgrid --email you@example.com
  ```
  It prints the token once. Revoke it with `wheeld token revoke <id>`; that also revokes every
  token minted with it.

Any HTTP client works the same way:

```bash
curl -fsS https://wheel.example.com/v1/projects -H @<(printf 'x-auth-token: %s\n' "$TOKEN")
```

The `wheel` CLI has no operator mode yet. Today it is the agent-side CLI that runs inside a
project's sandbox. `wheel login`/`wheel projects` against a server is follow-up F1 in
`docs/proposals/headless-first.md`; until then use AgentGrid or the API directly.

## Webhooks (`/p`)

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
sudo docker compose stop wheeld
sudo docker run --rm -v wheel_wheel-data:/data:ro -v "$PWD:/backup" debian:bookworm-slim \
  tar -C /data -czf "/backup/wheel-data-$(date +%F).tar.gz" .
sudo docker compose start wheeld
```

Whoever has that file has every secret on the board. Encrypt it before it leaves the server, for
example with `gpg --symmetric`. To restore, `docker compose down`, extract into a fresh
`wheel_wheel-data` volume the same way, then `docker compose up -d`. `caddy-data` holds the
certificates and the ACME account. Losing it only means Caddy asks for new ones.

## Upgrading

```bash
cd /opt/wheel && sudo git pull --ff-only
cd infra/vps && sudo docker compose build --pull && sudo docker compose up -d
sudo docker compose logs --tail 50 wheeld
```

wheeld gets 30 seconds on SIGTERM to stop every engine and each agent's processes. Agents come back
parked and pick their sessions up on the next message.

## Without a domain

**Plain HTTP.** Set `WHEEL_PUBLIC_HOST=<the server's IP>` instead of `WHEEL_DOMAIN`. Caddy serves on
port 80 with no certificate. **Everything crosses the network in the clear: your password, your
session, every `wht_` token.** Anyone on the path can take them. Use it on a network you trust, or
not at all.

**No proxy, over SSH.** Nothing public at all, and no webhooks. In `.env`, set
`WHEEL_PUBLIC_HOST=localhost:3000`; then:

```bash
sudo docker compose -f compose.yml -f compose.tunnel.yml up -d wheeld web
ssh -N -L 3000:127.0.0.1:3000 -L 8080:127.0.0.1:8080 you@server     # on your laptop
```

The board is at `http://localhost:3000`, and AgentGrid connects to `http://localhost:8080`.

## The web app somewhere else (Vercel)

Supported, but not the default. The web app's server has to reach the API, so wheeld's `/v1` must be
public, as it already is through Caddy here. Deploy `web/` with `WHEEL_API_URL=https://wheel.example.com`
and `WHEEL_PUBLIC_ORIGIN=<the web app's own origin>` (see `web/DEPLOY.md`). Caddy then only needs
`/v1` and `/p`. What you give up is the internal network: the web server's calls cross the
internet, authenticated by the user's session.

## Without Docker: `install.sh`

The same topology as systemd services on Ubuntu 24.04, built from source:

```bash
sudo git clone https://github.com/Morgandri1/wheel.git /opt/wheel-installer
sudo /opt/wheel-installer/infra/vps/install.sh --domain wheel.example.com --email you@example.com
```

It installs Node 22, a shared Rust toolchain, the `claude` and `codex` CLIs, and Caddy from Caddy's
apt repository. Wheel is compiled by an unprivileged `wheel-build` user, so no dependency's build
script runs as the account that can read `master.key`. wheeld runs as the `wheel` system user on
`127.0.0.1:8080`, with data in `/var/lib/wheel` (`0700`). The web app runs on `127.0.0.1:3000` as
a throwaway systemd user. The script is idempotent: run it again with another `--ref` to upgrade.
`--public-host` and `--no-proxy` are the no-domain modes above. `--firewall` applies the `ufw` rules.
Your own settings go in `/etc/wheel/wheeld.local.env` and `/etc/wheel/web.local.env`, which the
script never touches. The operator token is at `/var/lib/wheel/operator-token`.

Caddy runs with its admin API off, so change its configuration with `systemctl restart caddy`, not
`reload`.

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

`infra/vps/rehearse.sh` brings this exact `compose.yml` up on your machine, behind Caddy with a
certificate from Caddy's own CA instead of Let's Encrypt. It then checks, each check as its own
process with its own exit code:

- the web app is served, with the security headers;
- signup is refused, at the edge and by wheeld itself;
- the operator adds an account, and signing in works;
- a project, an agent, and a message that reaches `queued`, or `delivered` with the fake harness;
- `/v1` with and without a `wht_` token;
- the events WebSocket with header auth, and the web app's event stream through Caddy;
- webhook ingress, and that forged `X-Forwarded-*` headers never reach wheeld or change who the
  rate limit counts;
- the body limits;
- that wheeld and the web app cannot be reached except through Caddy, and that Caddy's admin API
  is off.

```bash
infra/vps/rehearse.sh                                        # https://localhost
REHEARSE_DOMAIN=wheel.rehearsal.test infra/vps/rehearse.sh   # any name; no DNS needed
REHEARSE_FAKE_HARNESS=1 infra/vps/rehearse.sh                # messages reach `delivered`
```

It needs ports 80 and 443 free, and it builds from `git archive` of a commit, never your working
tree.

A gate that has never failed proves nothing, so `rehearsal/mutate.sh edge` breaks the Caddyfile
the way people break proxies, and publishes wheeld's port. It exits 0 only if every check that
guards those layers comes back red. The broken layers:

- trusting every proxy
- dropping HSTS
- raising the webhook limit
- un-blocking sign-up
- forwarding the cookie
- buffering the event stream
- turning the admin API on

`rehearsal/mutate.sh config` does the same for wheeld's signup flag and the web app's origin.

## When something is wrong

| Symptom | Cause |
|---|---|
| No certificate; Caddy logs ACME errors | The A record does not point here yet, or 80/443 are closed in the cloud firewall |
| `preflight` exited 1 | `.env` is wrong; `docker compose logs preflight` names the setting |
| Sign-in answers `403 cross_origin` | The browser's origin is not `WHEEL_PUBLIC_ORIGIN`: you opened the server by IP while `.env` names the domain, or the reverse |
| `/v1` answers `403` naming `WHEEL_ALLOWED_HOSTS` | The request used a host name wheeld was not told about |
| `413 ... at the proxy` | Over the edge limit: 256 KiB for webhooks, 5 MiB for everything else (wheeld and the web app refuse more anyway) |
| Webhook `403` | The project's `http` capability is off |
