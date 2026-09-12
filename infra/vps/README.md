# Wheel on one server

<!--
Copyright Morgan Metz
Licensed under the PolyForm Noncommercial License 1.0.0.
See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
-->

Wheel on a single Linode, used from the browser and from AgentGrid.

**The default is native: `wheeld` and the board as systemd services, built from source, no Docker
daemon on the box.** That is [section 2](#2-install). Docker is still fully supported and is
[section 9](#9-choosing-docker-instead) — it is a deliberate choice with a stated cost, not the
path you end up on by following the README.

Two modes, chosen by whether you have a domain yet, and they work the same on both paths:

```
TUNNEL MODE (default, no domain yet — nothing published to the network)
  you ── ssh -L 3000:127.0.0.1:3000 -L 8080:127.0.0.1:8080 ──► the server
                                          127.0.0.1:3000 ──► web ──► wheeld
                                          127.0.0.1:8080 ──► wheeld

TLS MODE (--domain, once its A record points here)
  internet ──► :80 / :443  Caddy ──┬── /v1/*  /p/*  ──►  wheeld :8080 ──► one engine per project
                                   └── everything else ──►  web :3000  ──►  wheeld
```

There is no third mode that serves plain HTTP to the network. Both paths refuse to start on a
configuration that would do that.

- **`/v1` is Wheel's API.** Every request needs a credential: a session or a `wht_` API token.
  AgentGrid and scripts use it with a token. Project engines are never exposed.
- **`/p/<project>/<path>` is public webhook ingress**, TLS mode only, off per project until you
  switch it on. See [Webhooks](#webhooks-p).
- **Signup is closed.** You hold the operator token and add accounts with it. **It is not a
  boundary against your own agents** — [section 8](#8-what-native-loses-versus-docker).

| File | What it is |
|---|---|
| `install.sh` | **The entry point.** systemd services built from source. `--dry-run` first |
| `systemd/` | `wheeld.service`, `wheel-web.service`, `wheel-signup-gate.service`, and the resource drop-in |
| `libexec/` | `wheel-preflight` (refuses a bad config before the daemon binds), `wheeld-ready` (proves it serves), `wheel-doctor` |
| `toolchain.env` | The pinned Node / pnpm / `claude` / `codex` versions, and the `claude` floor |
| `backup.sh` | Back up and restore `/var/lib/wheel`. **Read [section 6](#backups-read-this-one)** |
| `rehearse-native.sh`, `rehearsal/native/` | The whole native install on a laptop, and every check below |
| `migrate-from-docker.sh` | Docker volume → native layout, read-only against the volume |
| `deploy.sh`, `compose.yml`, `rehearse.sh` | The Docker path — [section 9](#9-choosing-docker-instead) |
| `Caddyfile` | The proxy, shared by both paths: TLS, routing, body limits, security headers |

## 1. The server

- **Ubuntu 24.04.** `install.sh` refuses anything else, by design.
- **The only thing you install by hand is `git`**, to clone this repo far enough to run the script.
  Node 22, pnpm, the `claude`/`codex` CLIs, the Rust toolchain and (in TLS mode) Caddy are all
  installed by `install.sh` at pinned versions. This is the job Docker's image used to do, and
  moving it to the host is the main thing native asks of you.
- **A small box works, with tuning** — see [Running on a small box](#running-on-a-small-box-2-vcpu-38-gb).
  The first build compiles a Rust workspace and a Next.js app and is the heaviest moment in the
  server's life.
- **A domain is optional.** Start in tunnel mode; nothing here waits on DNS.

### Firewall reality

Native removes the worst of this, which is one of the reasons it is the default now:

1. **There is no Docker, so there are no iptables rules you did not write.** On the Docker path,
   `ufw status` can show a port closed while `docker compose`'s `ports:` mapping is reachable,
   because Docker writes its rules ahead of ufw's chain. Natively `ufw` is authoritative over what
   is reachable.
2. **A cloud firewall (Linode's, in front of the VM) still applies before anything on the box**,
   and is the one control that is not yours to misconfigure away from inside.
3. **`0.0.0.0:PORT` and `127.0.0.1:PORT` are different promises.** wheeld and the board bind
   `127.0.0.1` in every mode, and `wheeld-ready` measures the real socket on every start rather
   than trusting the unit file.

| Port | Why |
|---|---|
| 22/tcp | SSH. Narrow it to your own address if you can. |
| 80/tcp | TLS mode only: Let's Encrypt's HTTP challenge, and the redirect to HTTPS |
| 443/tcp, 443/udp | TLS mode only: HTTPS (443/udp is HTTP/3, optional) |

`install.sh --firewall` sets exactly these in ufw and enables it.

## 2. Install

```bash
sudo apt-get update && sudo apt-get install -y git
sudo git clone https://github.com/Morgandri1/wheel.git /opt/wheel-installer
```

Not `/opt/wheel` — that is the directory `install.sh` manages itself, as root.

```bash
# Tunnel mode: nothing published to the network. Start here if you have no domain yet.
sudo /opt/wheel-installer/infra/vps/install.sh --no-proxy --dry-run
sudo /opt/wheel-installer/infra/vps/install.sh --no-proxy

# Or, with a domain whose A record already points here:
sudo /opt/wheel-installer/infra/vps/install.sh --domain wheel.example.com --email you@example.com
```

`--dry-run` resolves every setting and the target commit, prints every command it would run, and
changes nothing — no package manager, no service, no firewall, no user, no file.

It is idempotent: run it again with another `--ref` to upgrade. Every input is a flag, nothing
reads stdin, so it works over `ssh` non-interactively — see
[Deploying over SSH](#deploying-over-ssh-non-interactively).

What it does, in the order it matters:

- installs the toolchain at the versions in `toolchain.env`, then **checks what the binaries
  actually report** — in particular that `claude` is at or above `2.1.269`, which PR #64's headless
  OAuth refresh requires. A pinned install that silently resolved to something older is the failure
  this catches, and it is checked again on every service start, not just here;
- creates two system users: `wheel`, which runs the daemon and owns `/var/lib/wheel` (`0700`), and
  `wheel-build`, which compiles. **No dependency's `build.rs` or `postinstall` ever runs as the
  account that can read `master.key`**;
- builds `wheeld` and the board as `wheel-build`, then installs the binaries with the previous
  generation kept as `/opt/wheel/bin/*.prev`;
- installs the units and restarts `wheeld` — a **drain**, not a kill (`KillMode=mixed`, ~28 s for
  turns in flight, `TimeoutStopSec=35`);
- **if the new build does not come up, puts the previous one back and restarts it**, then exits
  non-zero. A failed upgrade leaves a serving box and a red exit code.

```
/opt/wheel/src   git checkout        /opt/wheel/bin      wheeld, wheel, and *.prev
/opt/wheel/web   board server        /opt/wheel/libexec  preflight, ready, doctor
/opt/wheel/rust  shared toolchain    /var/cache/wheel    build caches, `wheel-build`
/var/lib/wheel   data, 0700 `wheel`  /etc/wheel          settings
```

Your settings go in `/etc/wheel/wheeld.local.env` and `/etc/wheel/web.local.env`, which
`install.sh` never touches and which are read second, so they win. Resource limits are a drop-in:
`/etc/systemd/system/wheeld.service.d/90-local.conf`, likewise never touched.

## 3. Without a domain yet (tunnel mode)

Nothing above published a port to the network. Reach it from your own machine:

```bash
ssh -L 3000:127.0.0.1:3000 -L 8080:127.0.0.1:8080 <user>@<server>
```

Leave that running. The operator token is a file, so reading it is one command:

```bash
ssh <user>@<server> "sudo cat /var/lib/wheel/operator-token"
```

Add your account and sign in — signup is closed, so this token is the only way in:

```bash
read -rs WHEEL_OPERATOR_TOKEN; read -rs PASSWORD
printf '{"email":"you@example.com","password":"%s"}' "$PASSWORD" |
  curl -fsS http://localhost:8080/v1/auth/users \
    -H @<(printf 'x-auth-token: %s\n' "$WHEEL_OPERATOR_TOKEN") \
    -H 'content-type: application/json' -d @-
```

Then open `http://localhost:3000/sign-in` through the tunnel.

Why there is no plain-HTTP fallback: a VPS's published ports are reachable the moment anything
upstream lets traffic through, and a password or a `wht_` token does not get a second chance once
it has crossed the network in the clear. Loopback plus a tunnel, or a real certificate.

### Going live: flipping to TLS mode

Once your domain's A record points here (`dig +short wheel.example.com`):

```bash
sudo /opt/wheel-installer/infra/vps/install.sh --domain wheel.example.com --email you@example.com
```

Caddy is installed and starts publishing 80 and 443. `/var/lib/wheel` is untouched, so nothing you
did in tunnel mode is lost. One thing genuinely changes for the worse, and it is
[section 8](#the-one-guarantee-native-cannot-reproduce)'s subject: in TLS mode wheeld is told to
believe `X-Forwarded-For` from `127.0.0.1`, which on this box means every local process, agents
included.

## 4. AgentGrid, and other API clients

AgentGrid needs a URL and a `wht_` token — `http://localhost:8080` through a tunnel, or
`https://wheel.example.com` in TLS mode. Mint a token for **your own account**:

```bash
sudo -u wheel /opt/wheel/bin/wheeld token create --name agentgrid --email you@example.com --data-dir /var/lib/wheel
```

It prints the token once. `wheeld token revoke <id>` revokes it and every token minted with it.

```bash
curl -fsS http://localhost:8080/v1/projects -H @<(printf 'x-auth-token: %s\n' "$TOKEN")
```

## 5. Agent credentials

An agent's Claude/Codex login lives in a **vault node**, never in an env file and never in a
systemd unit. In the board, add a `vault` node, add the key `ANTHROPIC_API_KEY`, enter the value in
the vault inspector (write-only; never shown back), and wire your agent to it with a `read` wire.

From the API, with `$API` being your tunnel or domain:

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

Other recognised keys: `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) and `CODEX_API_KEY`.
The OAuth path needs `claude >= 2.1.269`, which `install.sh` and every service start verify.

## 6. Operating it

### Is it up? Three different questions

```bash
sudo wheel-doctor            # everything
sudo wheel-doctor health     # just the three tiers; exit 0 only if all three pass
```

| Tier | Proves | Does not prove |
|---|---|---|
| **running** | systemd says the unit is active | that anything is listening |
| **serving** | `/healthz` answers 200 | that the store opened — `/healthz` is a static answer plus the auth mode |
| **working** | the operator token authenticates a real `GET /v1/projects` | — this is the one that proves the database opened, migrations ran and auth verifies |

`systemctl status wheeld` tells the truth here, which took work: `Type=simple` reports
`active (running)` the instant `execve` succeeds. `ExecStartPost=wheeld-ready` polls `/healthz` and
then measures the listening socket, and systemd does not finish the start job until it returns — so
`systemctl start wheeld` blocks until wheeld is genuinely serving.

### What are the agents doing?

Native is simply better than Docker here. Agents are ordinary processes in `wheeld.service`'s
cgroup:

```bash
systemd-cgls -u wheeld.service     # the live tree: every claude, node, cargo, git, with arguments
systemd-cgtop                      # what they are costing in CPU and memory
sudo wheel-doctor agents           # the same, plus context
```

### Logs

```bash
journalctl -fu wheeld                          # follow
journalctl -u wheeld -u wheel-web -u wheel-signup-gate -u caddy --since -1h
journalctl -u wheeld -p warning --since today  # warnings and worse
```

journald's default rate limit (1000 messages / 30 s / service) is raised in the unit, because an
incident is the worst possible moment to find out your logs were dropped. `wheeld` currently logs
text, not JSON — `wheel-api` already emits JSON and giving `wheeld` the same switch is a one-line
follow-up (F4 in the proposal), so `journalctl -o json` gives you journald's own fields today and
not wheeld's.

### Backups — read this one

**Losing `master.key` loses every vault secret on the board, permanently.** It is not derived from
anything and it is not recoverable: it decrypts every project's engine secret and vault key. The
database is replaceable by comparison; the key is not.

```bash
sudo infra/vps/backup.sh --to /var/backups/wheel     # stops wheeld (a drain), archives, VERIFIES
sudo infra/vps/backup.sh --verify <archive>          # check one without restoring it
sudo infra/vps/backup.sh --restore <archive>         # moves the current tree aside, never deletes
```

It stops `wheeld` first, because SQLite in WAL mode copied live is a half-written transaction and
you find that out at restore time. It writes a sha256 manifest into the archive and reads it back,
because an archive nobody has ever read is a hope. And **it does not encrypt the archive for you** —
that file contains `master.key` in the clear:

```bash
gpg --symmetric --cipher-algo AES256 /var/backups/wheel/wheel-data-*.tar.gz
scp /var/backups/wheel/wheel-data-*.tar.gz.gpg you@elsewhere:
```

A backup on the disk you are backing up is a copy, not a backup.

### Upgrading, and rolling back

```bash
cd /opt/wheel-installer && sudo git pull --ff-only
sudo infra/vps/install.sh --no-proxy --ref main --dry-run
sudo infra/vps/install.sh --no-proxy --ref main
```

The build happens as `wheel-build` before anything stops, so downtime is one restart. That restart
drains: `wheeld` gets SIGTERM alone, finishes turns in flight (~28 s), stops every agent's process
group, and `TimeoutStopSec=35` is the backstop. Agents come back parked and resume on the next
message.

If the new build does not serve, `install.sh` puts the previous generation back and restarts it
automatically. To go back later:

```bash
sudo infra/vps/install.sh --rollback     # no build, no clone, no network
```

That swaps `/opt/wheel/bin/{wheeld,wheel}` with their `.prev`, and the generation you rolled back
*from* becomes the new `.prev` — so it is reversible.

**An upgrade does not drop the board.** `wheel-web` `Requires=` the signup gate, which `Requires=`
`wheeld` — and systemd propagates a *stop* along `Requires=` but not a *restart*. So
`systemctl restart wheeld` leaves the board serving, while `systemctl stop wheeld` deliberately
takes it down with it (a board in front of a stopped daemon is a board showing errors). Both halves
are checked in the rehearsal rather than assumed.

**`install.sh` refuses to move the box backwards** by default. If the installed binary's commit is
a descendant of your `--ref`, that is a self-applied update (`WHEEL_AUTO_UPDATE`) about to be
clobbered; it stops and names `--allow-downgrade`.

### Auto-update

`sdk/auto-update` is a separate lane: `wheeld` noticing `main` moved, checking CI is green,
draining, swapping its own binaries, health-checking and rolling back. This kit owns the
**operator-initiated** half and never writes `WHEEL_AUTO_UPDATE` — that is yours, in
`wheeld.local.env`, off unless you set it. The two share only a filesystem contract:

| Hook | Here |
|---|---|
| Checkout (`WHEEL_UPDATE_REPO`) | `/opt/wheel/src` |
| Binaries (`WHEEL_UPDATE_BIN_DIR`) | `/opt/wheel/bin` |
| Rollback artefact | `/opt/wheel/bin/*.prev` — **written by both, deleted by neither** |
| Build staging (`WHEEL_UPDATE_STAGING`) | `/var/cache/wheel/update`, off the data directory |
| Restart | `Restart=on-failure` also restarts after `WHEEL_UPDATE_RESTART=exit` (exit 75) |
| Policy | `WHEEL_AUTO_UPDATE` in `wheeld.local.env`, off unless you set it |

By default `/opt/wheel/src` and `/opt/wheel/bin` belong to root, so nothing `wheeld` runs — agents
included — can rewrite them. `install.sh --updatable` hands both to `wheel` and widens the unit's
`ReadWritePaths=`. That is what lets `wheeld` replace itself. **It also lets any agent do so**,
because agents run as `wheeld`'s user. Enable it knowingly.

## 7. Hardening and resource limits: what is on, and what it costs

Full reasoning and every measurement: `docs/proposals/wheeld-native-production.md` §4, §4a and §5.
The short version, because these are the things that will surprise you.

**Every agent runs inside `wheeld.service`'s cgroup and mount namespace.** So every directive is a
constraint on arbitrary code that clones repos, installs dependencies and compiles. The sandbox is
deliberately *not* maximal, and the rejections are as considered as the acceptances.

What is on, and what you will notice:

| | What you will notice |
|---|---|
| `ProtectHome=yes` | Agents cannot read `/root` or `/home` — your SSH keys, `~/.aws`, shell history. Nothing Wheel runs lives there, so it costs nothing. |
| `ProtectSystem=strict` | Everything outside `/var/lib/wheel` is read-only to agents. An agent writing `/usr/local` gets `EROFS`. |
| `PrivateTmp=yes` | The agent's `/tmp` is not yours. To look inside: `sudo nsenter -t $(systemctl show -P MainPID wheeld) -m ls /tmp` |
| `ProtectProc=invisible` | `ps` inside the unit shows only `wheel`'s processes. **This does nothing for agent-to-agent isolation** — same-uid siblings stay fully visible. |
| `CapabilityBoundingSet=` | **`ping` stops working** (Ubuntu ships it with `cap_net_raw+ep`). `curl`, `getent` and `nc` are unaffected. |
| `LimitCORE=0` | No core dump for a `wheeld` crash. Deliberate: a core contains `master.key`, the operator token and every in-flight vault value. To get one temporarily, `systemctl edit wheeld` — and know what you are putting on disk. |
| `PrivateDevices=yes` | No `/dev/kvm`, `/dev/fuse` or GPU. Ptys still work. |

What is deliberately **not** on, because it breaks agents (all measured):

- **`SystemCallFilter=`** — `@system-service` breaks `unshare -Urm` + `mount`, which is what
  `bwrap`, Chromium's sandbox and rootless containers do. `wheel-web.service` *does* get it: it
  runs one known program that spawns nothing.
- **`RestrictNamespaces=`** — breaks `unshare` outright. Handled at the host level instead: Ubuntu
  24.04 ships `kernel.apparmor_restrict_unprivileged_userns=1` on by default. Check it
  (`sysctl kernel.apparmor_restrict_unprivileged_userns`) rather than assume it.
- **`ProcSubset=pid`** — hides `/proc/meminfo` and `/proc/cpuinfo`, which build tools size their
  parallelism from. The breakage is silent and gets blamed on the model.

### Resource limits

Sized for 2 vCPU / 3.8 GB, in `/etc/systemd/system/wheeld.service.d/10-resources.conf`. Override in
`90-local.conf`, never by editing the managed file.

| Setting | When it is hit |
|---|---|
| **`OOMPolicy=continue`** | **The line everything else depends on.** systemd's default is `stop`: without this, one agent being OOM-killed makes systemd stop `wheeld` and every other project's agents. With it, the agent dies and the engine keeps serving. |
| `MemoryHigh=2G` | Throttled and reclaimed. Nothing dies. Turns "memory-hungry agent" into "slow agent". |
| `MemoryMax=2.8G` | The kernel OOM-kills a process in the cgroup. 2.8 of 3.8 GB leaves room for the kernel, `sshd`, `journald` and Caddy, so **the host's own OOM killer never fires and you never lose `sshd`**. It does not guarantee the *agent* is the victim — the kernel picks by RSS, which usually means the agent, but not always. |
| `TasksMax=4096` | `fork()` returns `EAGAIN`. A fork bomb stops here. The budget is shared across all agents, so one leaking agent can starve its siblings. |
| `LimitNOFILE=65536` | `EMFILE` in one process. systemd's 1024 default is below what Node and pnpm want. |
| `CPUQuota=150%` | Of 200%. Leaves half a core for `sshd` — the difference between "the box is slow" and "I cannot ssh in to stop it". Does not affect `install.sh`, which builds outside this unit. |

### Running on a small box (2 vCPU, 3.8 GB)

- **The first build is the heaviest moment.** It compiles a Rust workspace and a Next.js app.
- **Add swap** before the first install:
  ```bash
  sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile
  sudo mkswap /swapfile && sudo swapon /swapfile
  echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
  ```
  `MemorySwapMax=1G` bounds how much of it one runaway agent can thrash.
- **There is no per-host cap on concurrently running agents** in the engine today. Keep few agents
  with `run_on_startup: true`, and set a short `idle_timeout_secs` (default 300) on ones that need
  not stay warm.
- **An agent building Rust in its own workspace** can use every core. `CPUQuota` bounds the damage
  to the box; to bound it per agent, have that agent's own `~/.cargo/config.toml` set
  `[build]\njobs = 1`.

## 8. What native loses versus Docker

Docker gave **isolation between agents and the host**. It never gave isolation *between agents* —
every agent in the `wheeld` image ran as uid 10001 in one container, exactly as every agent
natively runs as `wheel` on one host. Redteam 037 is open on both. What follows is only about the
host boundary.

**Genuinely lost:**

1. **No pid namespace.** An agent sees every other agent's processes and full command lines, and
   can signal them. In the container this stopped at the container's boundary.
2. **No network namespace.** An agent reaches anything bound to `127.0.0.1` on the box.
3. **`master.key` is same-uid readable.** `0600` protects it from other accounts and from nothing
   else. An agent that reads `/var/lib/wheel/master.key` holds every vault secret for every project;
   one that reads `operator-token` holds the account that adds users. This was equally true inside
   the container (`docker exec -u 10001 wheeld cat /data/master.key` worked) — what the container
   added was a step.
4. **Read-only is not invisible.** `ProtectSystem=strict` leaves `/etc`, installed packages and any
   world-readable file readable. A container's mount namespace would not have had those paths at
   all. `ProtectHome` closes the part that matters most.
5. **The declared "laptop mode" rail does not engage.** `docs/PROTOCOL.md` defines shared-uid mode
   as opt-in, requires a `SHARED_UID_WARNING` on every boot, and says the host must refuse a second
   project in that mode. `wheeld` never sets `WHEEL_ALLOW_SHARED_UID` and never consults
   `UidIsolation` — so the warning never prints and the rule is not enforced, while `wheeld` *is*
   shared-uid by construction. Tracked as follow-up F5; stated here because promoting native to the
   default is what makes it matter.

**Gained, and worth weighing against the above:**

- Resource limits that actually exist. The Docker deployment sets **no** `mem_limit`, `pids_limit`
  or `cpus` — section 7's table is a net gain, not a catch-up.
- `ufw` is authoritative (section 1).
- `systemd-cgls` shows you what agents are doing without `docker exec`.
- `wheeld` can update itself (the image runs `USER 10001` against a root-owned binary and cannot).
- One less daemon, and one less thing that can hold a port or write firewall rules you did not.

**What mitigates the losses, and what only looks like it does.** Real: `ProtectHome`,
`ProtectProc=invisible`, tunnel mode publishing nothing, the signup gate, the limits in section 7,
and `wheel-build` keeping dependency build scripts off the account that can read `master.key`. Not
real: the operator token (not a boundary against your own agents), `0600` modes (same uid), and the
vault's never-shown-back property (that is anti-echo, not containment).

### The one guarantee native cannot reproduce

In TLS mode, Caddy dials `127.0.0.1:8080` — and so can any agent, because agents run as a normal
uid on the same host. Nothing at the socket distinguishes them. So
`WHEEL_TRUSTED_PROXIES=127.0.0.1/32` means wheeld believes `X-Forwarded-For` from an agent exactly
as it believes it from Caddy. The Docker path trusted one container IP on an internal network that
no agent could send from.

The consequences are bounded but real: an agent could evade per-IP rate limits and write false
client addresses into the log of a security event. It is **not** an authentication bypass.

What does not fix it: binding wheeld to another loopback address (any local process can reach any
loopback address), or an nftables `skuid` rule (wheeld and its agents share a uid). What would fix
it is Caddy reaching wheeld over a unix socket whose peer credentials wheeld checks — follow-up F2.

**Tunnel mode, the default, is unaffected**: it sets no trusted proxy at all, so wheeld believes no
forwarded header from anybody.

### The webhook path

Unchanged from the Docker path and still the thing to be careful about. `/p/<project>/<path>` is
unauthenticated public ingress by design, TLS mode only, and whatever reaches it becomes a message
to the agent it is wired to (redteam 043). Internet → webhook → prompt injection → an agent that
reads `master.key` because someone else's request body asked it to. Natively that is one step
rather than two. Keep endpoints wired to agents with the fewest wires you can, prefer a shared
secret or HMAC, and treat the operator token as protecting against strangers, not against your own
agents.

## 9. Choosing Docker instead

Fully supported, and the right choice when **the host-isolation boundary in section 8 matters more
to you than everything in section 7**. That is the trade in one sentence: you get a mount, pid and
network namespace between agents and your machine, and you give up the resource limits, `ufw` being
authoritative, `systemd-cgls`, and `wheeld`'s ability to update itself.

Prerequisites: Docker Engine with the Compose v2 plugin, and `git`. See
[Installing Docker](#installing-docker).

```bash
git clone https://github.com/Morgandri1/wheel.git /opt/wheel-compose
cd /opt/wheel-compose/infra/vps
cp .env.example .env && chmod 600 .env
./deploy.sh --dry-run     # prints resolved settings and every command
./deploy.sh
```

Not `/opt/wheel` — that path belongs to `install.sh`. The two paths are independent; giving them
independent directories keeps it that way if you ever try both.

- **`deploy.sh` is the only thing that should run `docker compose` against this directory.** It
  computes settings `compose.yml` needs from `.env`.
- Leave `WHEEL_DOMAIN` unset for tunnel mode. Set it (plus `ACME_EMAIL`) and re-run for TLS.
- The operator token: `docker compose -p wheel exec wheeld cat /data/operator-token`.
- Tokens: `docker compose -p wheel exec wheeld wheeld token create --name agentgrid --email you@example.com`.

### Replacing an existing deployment (`--stop-legacy`)

```bash
./deploy.sh --stop-legacy --dry-run    # shows what it found and what it would stop
./deploy.sh --stop-legacy              # docker compose -p <project> down — never -v
```

**It never passes `-v`**, so volumes are left exactly as they were.

### Backups, Docker path

```bash
cd /opt/wheel-compose/infra/vps
docker compose -p wheel stop wheeld
docker run --rm -v wheel_wheel-data:/data:ro -v "$PWD:/backup" debian:bookworm-slim \
  tar -C /data -czf "/backup/wheel-data-$(date +%F).tar.gz" .
docker compose -p wheel start wheeld
```

Same warning as section 6: that file holds `master.key` in the clear. Encrypt it before it leaves
the server.

### Upgrading, Docker path

```bash
cd /opt/wheel-compose && git pull --ff-only
cd infra/vps && ./deploy.sh --dry-run && ./deploy.sh
```

### Installing Docker

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

## 10. Migrating a Docker deployment to native

A rehearsed procedure, not an urgent one. The design is one property repeated: **the Docker volume
is only ever read.** Every container the script runs mounts it `:ro`; there is no `docker volume
rm`, no `down -v`, and no `-v` flag anywhere in the file — which
`infra/tests/native-migration.test.sh` asserts on every commit.

```bash
# 1. Install natively, but do not let it take over yet.
sudo /opt/wheel-installer/infra/vps/install.sh --no-proxy

# 2. Stop the Docker wheeld. This also drains it.
cd /opt/wheel-compose/infra/vps && docker compose -p wheel stop wheeld

# 3. Look before you leap.
sudo /opt/wheel-installer/infra/vps/migrate-from-docker.sh --dry-run

# 4. Move it.
sudo /opt/wheel-installer/infra/vps/migrate-from-docker.sh --replace
```

It refuses to run while the container is up (a live SQLite copy is a half-written transaction),
copies the whole volume verbatim, compares **every file's sha256** on both sides, checks
`master.key` by name, runs `PRAGMA integrity_check` on every database, and then proves the move on
the live box by authenticating with the **migrated** operator token and listing projects.

`--replace` moves any existing `/var/lib/wheel` to `/var/lib/wheel.pre-migration-<stamp>` rather
than deleting it.

**Rollback, in full:**

```bash
sudo systemctl stop wheeld wheel-web
cd /opt/wheel-compose/infra/vps && ./deploy.sh
```

The volume is exactly as it was, so this is not a procedure that has to work — it is a consequence
of never having written to it.

> **Two volumes on the current production box belong to a retired stack and must never be removed:
> `wheel_hostdata` and `wheel_pgdata`.** `migrate-from-docker.sh` carries them as an explicit
> deny-list and refuses to read one even read-only. When you eventually retire the compose project,
> `docker compose -p wheel down` — never `-v`.

Back up immediately afterwards (section 6). The native install is now the live one; the volume is a
snapshot that stops ageing from that moment.

## 11. Rehearsing

```bash
infra/vps/rehearse-native.sh --only harden   # the shipped unit's directives + a real agent workload
infra/vps/rehearse-native.sh --only oom      # a runaway agent must not take down the daemon
infra/vps/rehearse-native.sh                 # everything, including a real install.sh run
```

It boots stock Ubuntu 24.04 with **real systemd** in a container and runs `install.sh` for real. The
full run compiles the workspace, so it takes what a first install on a server takes.

The phases, and what each proves:

- **harden** — lifts the `[Service]` directives out of the *shipped* `wheeld.service` verbatim and
  runs a real workload under them: `git clone`, `npm install` of a package with a build step, a
  real build, pty allocation, `unshare` + `mount`, `ss`/`ip`, DNS. Plus three that must *fail*:
  writing outside `ReadWritePaths`, reading a world-readable file under `/home`, reading root's
  `/proc`.
- **oom** — runs the same workload with and without `OOMPolicy=continue` and requires the default
  to stop the unit and `continue` not to. If the default ever stops reproducing, the drop-in would
  be fixing a problem that no longer exists, and this says so rather than passing quietly.
- **install / serve / upgrade / migrate** — `install.sh` end to end, then the guarantees measured
  against what it produced: loopback-only binding, the signup gate having run *as a unit* and being
  enabled for the next boot, `0700`/`0600` modes, preflight refusing a non-loopback bind, the
  `claude` floor, and a failed upgrade rolling back to a serving box.

**A gate that has never failed proves nothing**, so `rehearsal/native/mutate-native.sh` breaks each
layer on purpose and exits 0 only if the checks guarding it come back red:

```bash
infra/vps/rehearsal/native/mutate-native.sh harden    # the four tightenings a review would suggest
infra/vps/rehearsal/native/mutate-native.sh sandbox   # remove the host protections
infra/vps/rehearsal/native/mutate-native.sh oom       # drop OOMPolicy=continue
```

`harden` is the one that matters. A sandbox tightened past what agents need breaks *nothing
visible* — wheeld starts, the board serves, `systemd-analyze security` scores better — and the
damage shows up days later as "the model seems worse".

**Where the container differs from a real VM**, stated because a rehearsal that hides its own gaps
is worse than none: it runs `--privileged` with a delegated cgroup tree; the `cpu` controller is
often not delegated, so `CPUQuota` is reported rather than asserted; the kernel is the Docker host's;
and there is no ufw and no cloud firewall, so `--firewall` is not exercised at all.
`rehearsal/native/Dockerfile`'s header has the full list.

The fast subset runs in `make check` with no Docker at all — `infra:native-units`,
`infra:native-toolchain`, `infra:native-migration` — covering the invariants whose violation is
catastrophic *and* statically detectable.

## Deploying over SSH, non-interactively

Every input to `install.sh` is a flag; nothing reads stdin.

```bash
HOST=203.0.113.5 SSH_USER=root KEY=~/.ssh/wheel_deploy_key REF=main

ssh_run() { ssh -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=accept-new "$SSH_USER@$HOST" "$@"; }
ssh_run 'sudo -n true' || { echo "no passwordless sudo" >&2; exit 1; }

ssh_run bash -s -- "$REF" <<'REMOTE'
set -euo pipefail
ref="$1"
[ -d /opt/wheel-installer/.git ] || git clone --quiet https://github.com/Morgandri1/wheel.git /opt/wheel-installer
git -C /opt/wheel-installer fetch --quiet origin "$ref"
git -C /opt/wheel-installer -c advice.detachedHead=false checkout --quiet --force "origin/$ref"
REMOTE

ssh_run sudo /opt/wheel-installer/infra/vps/install.sh --no-proxy --ref "$REF" --firewall --dry-run
ssh_run sudo /opt/wheel-installer/infra/vps/install.sh --no-proxy --ref "$REF" --firewall
TOKEN=$(ssh_run sudo cat /var/lib/wheel/operator-token)
```

`BatchMode=yes` fails instead of prompting; `StrictHostKeyChecking=accept-new` accepts a host key on
first connection and verifies it on every one after.

## Webhooks (`/p`)

TLS mode only — there is no public ingress in tunnel mode.

`https://wheel.example.com/p/<project>/<path>` is public by design: a webhook sender can be given
nothing but a URL. It is off for every project until the owner turns on the project's `http`
capability. The edge caps a webhook body at 256 KiB.

**Whatever reaches an endpoint becomes a message to the agent wired to it.** See
[the webhook path](#the-webhook-path) above for why that matters more natively than it did in a
container.

## The board somewhere else (Vercel)

Supported, not the default. The board's server has to reach the API, so wheeld's `/v1` must be
public — TLS mode's topology. Deploy `web/` with `WHEEL_API_URL=https://wheel.example.com` and
`WHEEL_PUBLIC_ORIGIN=<the board's own origin>` (see `web/DEPLOY.md`). Note that
`wheel-web.service`'s `IPAddressDeny=any` exists precisely because the local board needs no
internet; if you point it at a remote API you must relax that, in a drop-in rather than by editing
the unit.

## When something is wrong

| Symptom | Cause |
|---|---|
| `systemctl start wheeld` hangs then fails | `ExecStartPost` waited 60 s for `/healthz`. The daemon is running but not serving: `journalctl -u wheeld -n 50` |
| wheeld refuses to start, naming `BIND_ADDR` | `wheel-preflight` caught a non-loopback bind. Intentional? `WHEEL_ALLOW_EXPOSED_BIND=1` in `wheeld.local.env`, after reading section 1 |
| wheeld refuses to start, naming `claude` | Below the `2.1.269` floor headless OAuth needs. `npm install -g @anthropic-ai/claude-code@<pinned>` and check `command -v claude` for something shadowing it |
| The board will not start | `wheel-signup-gate` failed, so `Requires=` blocked it: `journalctl -u wheel-signup-gate -n 20` |
| An agent's build fails with `EROFS` | It wrote outside `/var/lib/wheel`. `ProtectSystem=strict`, section 7 |
| An agent cannot find `/tmp` content you put there | `PrivateTmp=yes`. Use `nsenter`, section 6 |
| `ping` does not work inside an agent | `CapabilityBoundingSet=`, section 7. Use `curl`/`getent` |
| The box got slow and `sshd` was unresponsive | Raise or clear `CPUQuota` in `90-local.conf`, and see the small-box notes |
| `wheeld` restarted by itself, journal says `oom-kill` | An agent hit `MemoryMax` and the kernel picked wheeld. Rare; follow-up F3 is the fix. Check `systemd-cgtop` for what was large |
| Sign-in answers `403 cross_origin` | The browser's origin does not match `WHEEL_PUBLIC_ORIGIN`: in tunnel mode use `http://localhost:3000`, not the server's IP |
| `/v1` answers `403` naming `WHEEL_ALLOWED_HOSTS` | The request used a host name wheeld was not told about |
| `413 ... at the proxy` | Over the edge limit: 256 KiB for webhooks, 5 MiB otherwise |
| Webhook `403` | The project's `http` capability is off, or you are in tunnel mode |
| No certificate; Caddy logs ACME errors | The A record does not point here, or 80/443 are closed in the cloud firewall |
