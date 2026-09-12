# Native `wheeld` as the default production deployment

<!--
Copyright Morgan Metz
Licensed under the PolyForm Noncommercial License 1.0.0.
See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
-->

Owner: API lane. Branch `api/wheeld-production`, from `234dfe0` (#65, the VPS kit).

Operator directive, in two parts:

> "can you get an agent working on getting wheeld ready for production? i'd rather use that than
> docker."

> "you can use docker for this instance but going forward i'd like the default to be wheeld."

So: the live Linode stays on Docker; **the native systemd path becomes the recommended deployment
for everything new**, and Docker is demoted from headline to supported alternative. Migration has
to exist and has to be rehearsed, but nobody runs it this week.

ADVERSARY: §9 ("What native loses") and §3 (the guarantee ledger) are the review targets. §9 in
particular is written to be attacked — if any claim in it is too generous to native, that is the
bug.

---

## 0. What this lane is not

- It does not build a second auto-update mechanism. `sdk/auto-update` owns the daemon-initiated
  path; this lane owns the operator-initiated one. §7 draws the line and lists the three rules that
  keep them from fighting.
- It changes nothing under `crates/`. Every property below is earned with systemd, shell, and the
  binary's existing behaviour. Where a real fix needs Rust, it is written down as a follow-up with
  an owner (§11) rather than reached for here — four other lanes are in the tree.
- It does not run anything against `172.232.172.115`. The migration is written and rehearsed
  against a synthesised volume; the operator executes deployments.

## 1. The premise, restated accurately

Before designing anything it is worth correcting the brief's own framing, because two of its
assumptions are about the *host* backend and do not describe native `wheeld`.

**"An agent can read `WHEEL_VAULT_KEY` from `/proc` today."** True of `wheel-host`'s `process`
backend, which passes the key in the engine's environment
(`crates/wheel-host/src/sandbox/process.rs:130`) — that is redteam finding 037's first carrier. It
is **not** true of native `wheeld`: the embedded backend never puts the key in an environment at
all, it hands it to the engine in-process as a struct field
(`crates/wheeld/src/embedded.rs:134-139`). The engine also unsets both secrets at startup
(`crates/wheel-engine/src/config.rs:131-148`) and no child inherits them
(`crates/wheel-engine/src/supervisor/mod.rs:268-272`, `env_clear` plus a pinned allowlist).

That is not good news, it is *different* news. On native the equivalent fact is worse and simpler:
`master.key` sits at `<data-dir>/master.key`, mode `0600`
(`crates/wheeld/src/supervise.rs:20-57`), it decrypts every project's engine secret and vault key,
and **every agent runs as the same uid that owns it**. `crates/wheeld/src/tokens.rs:7-8` already
says the consequence out loud: *"Whoever can read the data directory can already read `master.key`,
which is strictly more power than any token, so the directory is the boundary here."* `0600` is not
a boundary against a process running as the owner. So native does not have the `/proc` carrier and
does not need it.

**"Docker gave isolation between agents."** It did not. Every agent in the `wheeld` image runs as
uid 10001 in one container, exactly as every agent natively runs as `wheel` on one host — redteam
037 is open in both. What the container gave is isolation between **agents and the host**, which is
a real and different thing, and §9 is about that and only that.

One more fact worth having before §5: **the Docker deployment running today has no resource limits
at all.** `infra/vps/compose.yml` sets `stop_grace_period` and nothing else — no `mem_limit`, no
`pids_limit`, no `cpus`. The per-project container caps in
`crates/wheel-host/src/sandbox/docker.rs:132-134` belong to the `docker` sandbox backend, which the
`wheeld` image does not use. So every limit in §5 is a net gain over what is deployed, not a
catch-up.

## 2. Service identity and the filesystem

Mostly already right in `install.sh`; the changes are small and specific.

| | Today | Change |
|---|---|---|
| `wheel` | `useradd --system`, home `/var/lib/wheel`, shell `nologin` | keep |
| `wheel-build` | separate uid that compiles, so no dependency's `build.rs`/`postinstall` runs as the account that can read `master.key` | keep — this is the best thing in the current installer |
| `/var/lib/wheel` | `install -d -m 0700 -o wheel -g wheel` | **`StateDirectory=wheel` in the unit** as well, so the mode and owner are re-asserted by systemd on every start and the directory self-heals if deleted. `install -d` is a fact about one moment; `StateDirectory=` is a fact about every boot. |
| `master.key`, `operator-token` | `0600`, created `O_EXCL` by wheeld itself | keep; **verify** in preflight rather than trust (§8) |
| `/opt/wheel/{src,bin}` | `root:root`, or `wheel:wheel` with `--updatable` | keep; ownership is the auto-update switch and there is no third state |
| Backups | **nothing** for the native path | `infra/vps/backup.sh` (§10) |

`StateDirectoryMode=0700`. One nuance recorded because it would otherwise be discovered as a
latency mystery: systemd only re-chowns a `StateDirectory=` tree when the top-level ownership does
not already match, so this costs nothing per start on a data directory with thousands of project
files. It is checked in the rehearsal rather than asserted here.

## 3. The guarantee ledger — where each promise comes from once compose is gone

This is the crux of the operator's second message. The Docker kit earned several of its guarantees
from compose and from a proxy sitting in front. If native is the default, those have to come from
systemd, from the binary, or from an explicit host step — **not from a README asking the operator
to be careful.** One row per guarantee, and one row is honestly marked WEAKER.

| Guarantee | Docker earns it with | Native earns it with | Verdict |
|---|---|---|---|
| Nothing published to the network | `ports: "127.0.0.1:8080:8080"`, and Caddy exists only under `COMPOSE_PROFILES=tls` | `DEFAULT_BIND` in the binary (`crates/wheeld/src/config.rs:37`), `--bind 127.0.0.1:8080` in `ExecStart`, **`ExecStartPre` refuses a non-loopback bind** unless the operator sets `WHEEL_ALLOW_EXPOSED_BIND=1`, **`ExecStartPost` measures the listening socket** with `ss` and fails the unit if anything but loopback is bound | **STRONGER.** Docker's version shipped with a hole the README documents at length: `ufw` does not see Docker's published ports, because Docker writes iptables rules ahead of ufw's chain. Native has no docker-proxy and no rules nobody wrote; `ufw` is authoritative. And the check is now a measurement of the real socket rather than a statement about a compose file. |
| Signup closed | wheeld's own default, plus a Caddy matcher, plus the `verify-signup-gate` service | wheeld's own default (unchanged), the same Caddy matcher in TLS mode, plus **`wheel-signup-gate.service`**, a `Type=oneshot` running the same `verify-signup-gate.sh` | **EQUAL, and better than `install.sh` has today** — see the next row. |
| The gate runs *before* the board starts | `depends_on: { verify-signup-gate: service_completed_successfully }` | `wheel-web.service` gains `Requires=wheel-signup-gate.service` + `After=` | **EQUAL — and this fixes a real native regression.** Today the gate is a block of shell inside `install.sh`, so it runs once at install time and never again. Reboot the box and the board comes up in front of an unverified wheeld with nothing having checked. A systemd dependency re-runs on every boot, which is what compose's version always did. |
| Operator-token bootstrap | wheeld writes `/data/operator-token` `0600`; read it with `docker compose exec` | identical file at `/var/lib/wheel/operator-token`; read it with `sudo cat` | EQUAL, and one less moving part |
| Body limits, HSTS, security headers, `Cookie` stripped to wheeld, forged `X-Forwarded-*` overwritten | Caddy | the same Caddy, the same `Caddyfile`, installed from Caddy's own apt repository by `install.sh` — this is already true and needs nothing | EQUAL |
| Honest client attribution behind the proxy | `WHEEL_TRUSTED_PROXIES=<the Caddy container's IP>` — one address on an internal network that no agent can send from | `WHEEL_TRUSTED_PROXIES=127.0.0.1/32` — **every local process, agents included** | **WEAKER. Cannot be reproduced.** See below. |
| A ceiling on what a runaway agent can take | nothing (§1) | §5 | STRONGER |
| Agents cannot touch the host's filesystem | container mount namespace, `/data` volume | systemd mount namespace: `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `ReadWritePaths=` | PARTIAL — §9 |

### The one that cannot be reproduced

In native TLS mode Caddy dials `127.0.0.1:8080`, and so can any agent, because agents run as a
normal uid on the same host. There is no way to tell the two apart at the socket, so
`WHEEL_TRUSTED_PROXIES=127.0.0.1/32` means wheeld believes `X-Forwarded-For` from an agent exactly
as it believes it from Caddy. The consequences are bounded but real: an agent could evade wheeld's
per-IP rate limits and could write false client addresses into the log of a security event.

What does **not** fix it, so nobody tries:

- Binding wheeld to some other loopback address. Any local process can connect to any loopback
  address; the destination is not an identity.
- An nftables `meta skuid` rule admitting only Caddy's uid. wheeld and its agents share uid `wheel`,
  so the rule cannot separate the thing it needs to separate — and it would have to special-case
  `wheel-web`'s `DynamicUser=` uid, which changes on every boot.

What actually fixes it is a channel an agent cannot forge into: **Caddy reaching wheeld over a unix
socket** whose peer credentials wheeld checks (the engine already does exactly this,
`crates/wheel-engine/src/peercred.rs`), or a shared secret header Caddy sets that wheeld requires
before believing any forwarded header. Both are `crates/` changes. Follow-up **F2**, API lane.

What genuinely mitigates it today: **tunnel mode is the default, and tunnel mode does not set
`WHEEL_TRUSTED_PROXIES` at all.** With no trusted proxy configured, wheeld believes no forwarded
header from anybody and the weakness does not exist. It appears only when an operator moves to TLS
mode, so that is where `install.sh` prints it and where the README states it. `install.sh` also
stops writing `::1` into the list, which was never dialled by anything.

## 4. Hardening: the frame, then the table

The frame first, because it decides every row and it is the difference between hardening and
breaking the product.

**Every agent runs inside `wheeld.service`'s cgroup and mount namespace.** A directive on that unit
is not only a constraint on a daemon, it is a constraint on arbitrary code that Wheel exists to
run — an agent that clones a repository, installs dependencies, compiles, and possibly drives a
browser. So every candidate directive falls into one of two buckets:

- **host-protective and agent-neutral** — it removes something a daemon and an agent both have no
  business doing (loading kernel modules, setting the clock, taking realtime priority, reading
  `/root`). Take all of these; they are nearly free.
- **agent-functional** — it removes something a plausible agent workload actually uses (system
  calls, namespaces, `/proc/meminfo`, devices, memory). Each of these needs a measurement, not an
  opinion, and some must be rejected.

A maximal sandbox scores well in `systemd-analyze security` and silently breaks agents in ways that
surface days later as "the model seems dumber". The recorded rejections below are as much a part of
this design as the acceptances.

### Accepted, on `wheeld.service`

| Directive | What it protects | What it costs |
|---|---|---|
| `NoNewPrivileges=yes` *(kept)* | no setuid/setgid binary can raise privilege from inside the unit, ever | `sudo`/`su` inside an agent cannot work. They could not anyway — `wheel` is in no sudoers file — so this closes a path rather than removing a capability. |
| `ProtectSystem=strict` *(kept)* | the entire filesystem is read-only except `/dev`, `/proc`, `/sys` and `ReadWritePaths=` | everything an agent writes must be under the data directory. It already is: workspaces, bare-clone store (`supervisor/workspace.rs:57`), per-node creds dirs and `TMPDIR` are all derived from the data dir, and `$HOME` for `wheel` *is* `/var/lib/wheel`. An agent that tries to write `/opt` or `/usr/local` gets `EROFS` — loudly, which is the good failure. |
| `ProtectHome=yes` *(kept)* | `/home`, `/root` and `/run/user` are empty to this unit | the single highest-value directive here. An agent cannot read the operator's SSH keys, `~/.aws`, git credentials or shell history. Costs nothing, because nothing Wheel runs lives in a human's home. |
| `PrivateTmp=yes` *(kept)* | agents cannot use `/tmp` as a channel to anything else on the box, and cannot read what another service left there | debugging is harder: the agent's `/tmp` is not yours. The README gets the `nsenter` recipe rather than a shrug. |
| `ProtectKernelTunables`, `ProtectKernelModules`, `ProtectControlGroups`, `RestrictSUIDSGID`, `LockPersonality` *(kept)* | `/proc/sys` and `/sys` read-only, no module load, no cgroup edits, no setuid bit creation, no personality switch | nothing measurable. Agent-neutral. |
| **`ProtectKernelLogs=yes`** | `dmesg` and `/dev/kmsg` are denied — kernel pointers and other services' activity stop being readable | an agent cannot read kernel logs. Nothing in the toolchain does. |
| **`ProtectClock=yes`** | no `settimeofday`/`adjtimex`. An agent that skews the box's clock breaks TLS validation, token expiry and every log timestamp at once | none; nothing legitimate sets the clock from inside a service. |
| **`ProtectHostname=yes`** | no `sethostname` | none. |
| **`RestrictRealtime=yes`** | no `SCHED_FIFO`/`SCHED_RR`. On 2 vCPU a realtime-priority spin is a complete denial of service on the box, reachable by any agent | none; no build tool asks for realtime scheduling. |
| **`CapabilityBoundingSet=`** and **`AmbientCapabilities=`** (both empty) | nothing in this unit can ever hold a capability, including via a file capability on a binary | no raw sockets (`tcpdump`, `traceroute -I`) and no binding a port below 1024 — an agent testing a server uses a high port. **Not** `ping`, though I first wrote that it was: see §4a, where measuring it is what corrected the claim. |
| **`RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK`** | `AF_PACKET` is gone, so no raw packet capture on the VPS's interfaces, along with every exotic family | `AF_NETLINK` is deliberately **kept**, and the reason is not the one I first wrote down — see §4a. Measured: dropping it does **not** break DNS. It breaks `ss`, `ip` and `getifaddrs()`, and `ss` is in this kit's own critical path. (`AF_PACKET` also needs `CAP_NET_RAW`, which the empty bounding set already removed, so this row is belt and braces and costs nothing.) |
| **`ProtectProc=invisible`** | an agent can no longer see, or read the `cmdline`/`environ` of, processes belonging to other users — `sshd`, `caddy`, `wheel-web`, root's | `ps aux` inside the unit shows only `wheel`'s own processes. That is an improvement in every case except confusion. **It does not touch redteam 037**: same-uid siblings stay fully visible to each other, which is 037's entire premise. Claiming otherwise would be the exact "a fix that looks like one" `docs/ARCHITECTURE.md:736-749` warns about. |
| **`PrivateDevices=yes`** | a minimal private `/dev` — no raw block devices, no `/dev/kvm`, no `/dev/fuse`, no GPU | an agent cannot run a VM, a FUSE mount, or CUDA. None of those exist on a 2 vCPU Linode. `/dev/null`, `/dev/urandom`, `/dev/tty`, `/dev/ptmx` and a private `devpts` are all provided, so pty-allocating CLIs still work — measured in §8, because that is the failure this directive would plausibly cause. |
| **`PrivateMounts=yes`** | mount propagation is private, so a mount an agent manages to make cannot appear host-wide | none; already implied by the `Protect*` set, stated so it survives a future edit. |
| **`LimitCORE=0`** | **a core dump of `wheeld` contains `master.key`, the operator token, and every vault value decrypted in flight.** `systemd-coredump` would write that to `/var/lib/systemd/coredump` and announce it in the journal | no post-mortem core for a wheeld crash. The README documents the temporary `systemctl edit` to get one back when you actually need it, and the fact that doing so puts the vault on disk. |
| **`ProcSubset=all`** *(set explicitly)* | nothing — this is the **non**-hardening entry | see the rejection below. Written out so that a future tightening to `pid` is a visible edit someone has to justify, rather than a default drifting under us. |

### Rejected, with reasons

**`SystemCallFilter=@system-service`** — rejected on `wheeld.service`; **applied to
`wheel-web.service`**. `@system-service` denies `@mount` and `@debug` among others. `@mount`
(`mount`, `umount2`, `pivot_root`) is what Chromium's sandbox, `bwrap` and rootless container
tooling use, and an agent doing browser QA is a first-class Wheel workload. `@debug` (`ptrace`,
`process_vm_readv`) is what debuggers and profilers use. A seccomp filter on this unit is inherited
by every descendant with no per-workload escape. Against that, the protection is thin: the denied
families are mostly privileged operations that `NoNewPrivileges=yes` plus an empty
`CapabilityBoundingSet=` already put out of reach for an unprivileged uid. So it buys little and can
break a lot — exactly the trade this section exists to refuse. Measured in §4a, including the part
that is *not* obvious: plain `unshare` survives the filter, and only the `mount` that follows it
fails, so a check that ran `unshare` alone would have concluded the filter was harmless.
`wheel-web.service` is the opposite case: one known Node program that spawns nothing, so it gets the
filter.

*Residual acknowledged:* denying `@debug` would have narrowed redteam 037 slightly, by removing
`process_vm_readv` between same-uid siblings. It would not have closed it — `/proc/<pid>/environ`
needs no `ptrace` — and Ubuntu's default `kernel.yama.ptrace_scope=1` already restricts `ptrace` to
descendants. Not enough to pay the price above.

**`RestrictNamespaces=`** — rejected, and measured (§4a). User, mount and pid namespaces are what
Chromium, `bwrap` and rootless Podman need. This is the single most likely directive to silently
break an agent. The security cost of leaving it open is real (unprivileged user namespaces have a
history as a kernel LPE surface) and is addressed at the host level instead, where it belongs:
Ubuntu 24.04 ships `kernel.apparmor_restrict_unprivileged_userns=1` on by default. The README says
to check it rather than assume it.

**`ProcSubset=pid`** — rejected. It hides `/proc/meminfo`, `/proc/cpuinfo`, `/proc/stat` and
`/proc/loadavg` along with the rest of `/proc`. Build tools size their parallelism from those,
Node's `os` module reads them, and anything that asks "how much memory does this box have" gets a
wrong answer instead of an error. The breakage is silent and is attributed to the model, not the
unit file. `ProtectProc=invisible` already delivers the part that matters — other users' processes —
without this.

**`LimitNPROC=`** — rejected in favour of `TasksMax=`. `RLIMIT_NPROC` is counted per **real uid
across the whole system**, not per cgroup. Every agent and `wheeld` itself share uid `wheel`, and a
`wheel` process started outside the unit would count too. The cgroup `pids` controller is the tool
that actually means what the limit is trying to say.

**`LimitFSIZE=`** — rejected. `wheel-host`'s process backend sets 8 GiB and is right to, but its
failure mode is `SIGXFSZ`, which kills a build with a signal nobody recognises, and the real risk
here is total disk rather than one large file. The host already refuses to start a sandbox onto a
full volume (`crates/wheel-host/src/disk.rs`, floor 256 MB) and reports free space on `/healthz`.
The README gets a `du` recipe instead of a limit that misfires.

**`IPAddressDeny=any`** on `wheeld.service` — rejected as obviously impossible: agents need `git`,
npm and the Anthropic API. **Applied to `wheel-web.service`**, which only ever dials
`127.0.0.1:8080` and serves `127.0.0.1:3000`. If the board is ever RCE'd it cannot exfiltrate
anything. Cost, stated in the unit: pointing `WHEEL_API_URL` at a remote API or switching
`WHEEL_AUTH_MODE` to `jwks` requires relaxing it, and the unit says so where someone would hit it.

### 4a. What was actually measured, including where it contradicted me

Every claim above is checked by `infra/vps/rehearsal/native/harden-probe.sh`, which lifts the
`[Service]` directives out of the shipped `wheeld.service` and its drop-in **verbatim** and runs a
real workload under them — so it cannot pass against a copy that has drifted from what `install.sh`
installs. Results on Ubuntu 24.04 / systemd 255, in the container test bed:

| Measured | Result |
|---|---|
| `git clone`, `npm install` (a package with a build step), and a real build, all under the full directive set | **work** — 13 checks pass, 0 fail |
| `ProcSubset=pid` | **hides `/proc/meminfo` and `/proc/cpuinfo`.** Confirms the rejection. `nproc` still works (it uses `sched_getaffinity`), which is exactly why the breakage is easy to miss |
| `ProtectProc=invisible` | works — `/proc` remounted `hidepid=invisible`, root's `/proc/1/cmdline` is `ENOENT` to the service user. `/proc/meminfo` unaffected |
| `PrivateDevices=yes` | pty allocation still works (`script -qec` succeeds) |
| **`RestrictAddressFamilies` without `AF_NETLINK`** | **DNS still works.** `getent hosts`, Node's `dns.lookup` and `npm install` all succeeded. **The folklore I originally wrote down — that dropping `AF_NETLINK` breaks `getaddrinfo` via `__check_pf()` — is false here; glibc falls back.** What actually breaks is `ss`, `ip` and Node's `os.networkInterfaces()`: *"Cannot open netlink socket: Address family not supported by protocol"*. That still settles the question, because **`ss` is what `wheeld-ready` uses to prove the loopback bind** — dropping `AF_NETLINK` would turn §3's strongest row into an unverifiable claim |
| `RestrictNamespaces=yes` | breaks `unshare -Ur` outright: *"unshare failed: Operation not permitted"* |
| `CapabilityBoundingSet=` (empty) | **The claim I wrote first was wrong, and this is the clearest example of why the probe exists.** `/usr/bin/ping` does carry `cap_net_raw=ep`, and in an isolated unit with an empty bounding set it does die with `EPERM` — so the isolated experiment agreed with me. Under the *full* directive set it works fine, for two compounding reasons: `ProtectSystem=strict` puts the unit in a mount namespace where `/usr` is `nosuid`, so the kernel ignores the file capability rather than refusing the `execve`; and Ubuntu 24.04 defaults `net.ipv4.ping_group_range` to `0 2147483647`, so `ping` uses an unprivileged ICMP socket needing no capability. **And the test bed cannot measure the real costs either** — Docker sets `ip_unprivileged_port_start=0`, so binding port 80 succeeds there with no capability and would not on a VM. The probe therefore asserts only that systemd *applied* an empty bounding set, and the README states the cost from the directive's semantics rather than from a measurement this container is capable of making |
| `SystemCallFilter=@system-service` | plain `unshare -Ur` **survives**; `unshare -Urm` + `mount -t tmpfs` fails at *"cannot change root filesystem propagation"*. The namespaced mount is what `bwrap` and Chromium do, so the filter does break them — but only a probe that mounts finds out |
| Same-uid `/proc/<pid>/environ` under `SystemCallFilter` | **still readable.** Confirms §9's claim that denying `@debug` would not close redteam 037 — `environ` needs no `ptrace` |
| `OOMPolicy` | §5, and it is the sharpest result here |

Three of these changed the design or the reasoning behind it. That is the argument for measuring
rather than reading a hardening guide: the `AF_NETLINK` row would have shipped with a false
justification, the `SystemCallFilter` rejection would have rested on a claim about `unshare` that is
wrong, and `ProtectSystem`/`ProtectHome` were briefly "verified" by a probe that was really only
observing ordinary file permissions — `wheel` cannot write `/usr/local` or read `/root` whether or
not those directives exist. The mutation harness caught that last one by removing both directives
and watching nothing turn red; the probe now uses a world-writable directory outside
`ReadWritePaths=` and a world-readable file under `/home` as decoys, and additionally requires the
`ProtectSystem` failure to be `EROFS` rather than any refusal at all.

A gate that has never failed proves nothing, so `infra/vps/rehearsal/native/mutate-native.sh`
breaks each layer on purpose and exits 0 only if the checks guarding it come back red:

- `harden` applies the four tightenings a security review would suggest — `ProcSubset=pid`, dropping
  `AF_NETLINK`, `SystemCallFilter=@system-service`, `RestrictNamespaces=yes` — and requires the
  three corresponding agent-capability checks to fail. **This is the mutation that matters**, because
  a sandbox tightened past what agents need breaks nothing visible: wheeld starts, the board serves,
  `systemd-analyze security` scores *better*, and the damage surfaces days later as "the model seems
  worse".
- `sandbox` removes `ProtectHome`, `ProtectSystem` and `ProtectProc` and requires the three
  host-protection checks to fail.
- `oom` removes `OOMPolicy=continue` and requires `oom-containment.sh` to fail.

## 5. Resource limits on a 2 vCPU, 3.8 GB box

The design goal is not "cap the agent". It is: **when a runaway agent hits a limit, the agent dies
and the engine keeps serving — never the reverse — and the box stays reachable over SSH.**

All of this ships as `/etc/systemd/system/wheeld.service.d/10-resources.conf`, a drop-in that
`install.sh` owns and overwrites. An operator on a larger box writes `90-local.conf`, which is never
touched — the same managed/`.local` split as `wheeld.env` / `wheeld.local.env`, and drop-ins apply
in lexical order so `90-` wins.

| Setting | Value | What happens when it is hit |
|---|---|---|
| **`OOMPolicy=continue`** | — | **The single most important line in this lane.** systemd's default is `DefaultOOMPolicy=stop`: when *any* process in a unit's cgroup is OOM-killed, systemd **stops the unit**. Left at the default, one runaway agent hitting `MemoryMax` takes down `wheeld` and every other project's agents with it. `continue` makes systemd ignore the kill; the engine sees its child die and marks the turn interrupted, which is the behaviour the product already has for a killed agent (`INTERRUPTED_BY_SHUTDOWN`, `supervisor/mod.rs:163`). Nobody would think to write this line, and without it every other limit here is a loaded gun pointed at the daemon. Proven in the rehearsal by actually OOMing a child and asserting `wheeld` is still `active (running)` and still serving. |
| `MemoryHigh=2G` | soft | the cgroup is throttled and reclaimed hard above this. Nothing dies. This is what turns "memory-hungry agent" into "slow agent" for the common case. |
| `MemoryMax=2.8G` | hard | the kernel OOM-kills a process **in this cgroup**. 2.8 of 3.8 GB leaves ~1 GB for the kernel, `sshd`, `journald`, Caddy and the board, so **the host's own OOM killer never fires** and you never lose `sshd` to an agent. That is what this number buys; it does not protect wheeld from being the victim. |
| | | *Residual, stated rather than papered over:* the kernel picks by `oom_score`, roughly proportional to RSS. `wheeld` resident is tens of MB against a runaway build's hundreds or thousands, so the agent is overwhelmingly likely to be chosen — **but it is not guaranteed.** If `wheeld` loses, `Restart=on-failure` brings it back in 2 s having lost its drain. Losing a drain is worse than losing a turn and better than losing the box. The real fix is `oom_score_adj` on agent children, an engine change: follow-up **F3**. |
| `MemorySwapMax=1G` | hard | bounds swap so a thrashing agent cannot hold the box unresponsive for minutes while the OOM killer declines to fire. Cost: a build that would have limped through on swap now dies. On 2 vCPU, dying in seconds beats thrashing for an hour. |
| `TasksMax=4096` | cgroup `pids.max` | `fork()` returns `EAGAIN`. A fork bomb stops at 4096 and the box survives. The number matches `wheel-host`'s `RLIMIT_NPROC` default for the same population (`wheel-host/src/config.rs:169-180`), and is close to Ubuntu's own `DefaultTasksMax=15%` (~4900) — **the value is in it being stated, testable, and scoped to the cgroup rather than the uid.** Cost, and it is real: the budget is shared across all agents in the unit, so one agent leaking processes starves its siblings and can stop `wheeld` spawning a new one. Per-agent accounting needs per-node cgroups, which needs per-node uids (M2/M3). |
| `LimitNOFILE=65536` | per process | `EMFILE` in the one process that ran out — contained and loud. systemd's 1024 soft default is below what Node, pnpm, watchers and `rust-analyzer` want, and `EMFILE` out of a bundler is a famously unattributable failure. |
| `CPUQuota=150%` | cgroup | of 200% on 2 vCPU, leaving half a core for `sshd`, `journald` and Caddy. **This is the difference between "the box is slow" and "I cannot ssh in to stop it."** Cost: an agent-driven build is ~25% slower at saturation. Note it does **not** slow the install: `install.sh` compiles as `wheel-build`, outside this unit. |
| `IOWeight=` | unset | deliberately. The Linode is SSD-backed and `io.latency` tuning without measurement is guesswork dressed as rigour. |

## 6. Documentation and the kit's entry point

**Two scripts, not one.** `install.sh` becomes the documented default; `deploy.sh` stays exactly
what it is, the Docker entry point, and gains a banner naming itself the alternative. A single
front door that branched into two deployment models would be one script with two disjoint flag sets
and two incompatible meanings of `--dry-run` (`install.sh --dry-run` resolves a *commit*;
`deploy.sh --dry-run` resolves compose *settings*). Two honest names beat one dishonest dispatcher.

`infra/vps/README.md` is reordered so the reading path is the recommended path: the server, install
(native), tunnel mode, AgentGrid, credentials, **operating it** (logs, health, what agents are
doing, backups, upgrade, rollback), **hardening and limits** (what is on and what it costs), **what
native loses**, then **"Choosing Docker instead"** carrying the whole existing compose flow intact,
then migration, then rehearsing.

The root `README.md` gains a server section straight after §1 (`wheeld`, one executable) and demotes
the Docker section beneath it. Someone deploying Wheel to a machine should reach systemd before
they reach a Dockerfile.

**How someone chooses Docker deliberately, and what it costs them.** Stated as a short list rather
than implied: you get a mount/pid/network namespace between agents and the host (§9), and you give
up the resource limits in §5, `ufw` being authoritative over your published ports, `systemd-cgls`
as a way to see what agents are doing, and the ability for `wheeld` to update itself (the image
runs `USER 10001` against a root-owned binary — `sdk/auto-update`'s R8). Choose it when the
host-isolation boundary matters more than any of that.

## 7. Install, upgrade, rollback — and the seam with `sdk/auto-update`

**The split.** This lane owns the **operator-initiated** lifecycle: first install, a deliberate
upgrade to a named `--ref`, and rolling that back. It runs as root, from outside the daemon.
`sdk/auto-update` owns the **daemon-initiated** lifecycle: noticing `main` moved, checking CI,
draining, swapping binaries, health-checking and rolling back. The seam is the filesystem contract
frozen by #65 and unchanged here — `/opt/wheel/src`, `/opt/wheel/bin`, `/var/cache/wheel/update`,
`WHEEL_UPDATE_*`, exit 75, and `.prev`.

Three rules this lane adopts so the two cannot fight, each of which is a change:

1. **`install.sh` never deletes `/opt/wheel/bin/*.prev`.** That file is the other lane's rollback
   artefact.
2. **`install.sh` uses the same swap shape the daemon uses** — write `.new`, hard-link the current
   to `.prev`, `rename(2)` over. Today it writes `.new` and `mv -f`s over the top, keeping no
   `.prev` at all, so **an operator upgrade silently destroys the daemon's rollback point.** One
   artefact, one meaning, whoever wrote it.
3. **`install.sh` refuses to move the binaries backwards by default.** If the installed
   `wheeld --version` SHA is a descendant of the target `--ref`, that is a self-applied update about
   to be clobbered; it stops and names `--allow-downgrade`.

Neither lane writes `WHEEL_AUTO_UPDATE`. It is the operator's, in `wheeld.local.env`, default off.

**Upgrade drains rather than kills.** It already does, and the parts are load-bearing: build first
as `wheel-build` (so downtime is a restart, not a compile), then `systemctl restart wheeld`, where
`KillMode=mixed` sends SIGTERM to `wheeld` alone so its own ~28 s drain runs instead of racing
systemd's signal to the agents, with `TimeoutStopSec=35` as the backstop.

**Rollback is new.** After a restart, `install.sh` already waits for `/healthz` and verifies the
signup gate. What it does with a failure is the change: **restore `.prev` and restart again**, then
report which generation is running and exit non-zero. A failed upgrade should leave a serving box
and a red exit code, not a down box and a red exit code. `install.sh --rollback` does the same swap
on demand without a rebuild.

`StartLimitIntervalSec=300` / `StartLimitBurst=10` stops a crashloop: at `RestartSec=2` that is
caught in about 20 seconds. The window is deliberately loose enough not to collide with
`sdk/auto-update`'s restart cadence (`WHEEL_UPDATE_FETCH_SECS` is 300, minimum 60), and it is
flagged here as a constant the two lanes share.

## 8. Operational surface, and the toolchain Docker used to provide

**`systemctl status` that tells the truth.** `Type=simple` reports `active (running)` the instant
`execve` succeeds — before the store is opened, before migrations, before anything binds. So
`ExecStartPost=/opt/wheel/libexec/wheeld-ready` polls `/healthz` until it answers and then asserts
with `ss` that every listening socket is loopback. For `Type=simple`, systemd does not consider the
start job finished until `ExecStartPost` exits, so `systemctl start wheeld` blocks until the daemon
is genuinely serving and `systemctl status` stops lying. This is the honest native equivalent of
compose's healthcheck **with no `crates/` change**. The real fix is `Type=notify` and one
`sd_notify(READY=1)` after the bind: follow-up **F1**, API lane.

**Running versus serving versus working.** `wheel-doctor` is a read-only diagnosis in three tiers —
the unit is active; `/healthz` answers 200; the operator token authenticates a real `GET
/v1/projects`, which is the only one of the three that proves the store opened and auth works. It
also prints the installed generation and `.prev`, the toolchain versions, the effective sandbox
(`systemd-analyze security wheeld.service`), and current `MemoryCurrent`/`TasksCurrent` against the
limits.

**Seeing what agents are doing** is a place native is simply better than Docker: agents are
processes in `wheeld.service`'s cgroup, so `systemd-cgls /system.slice/wheeld.service` prints the
live tree — every `claude`, `node`, `cargo` and `git` with its arguments — and `systemd-cgtop`
prints their CPU and memory. No `docker exec` required.

**Logs.** `SyslogIdentifier=wheeld`, and `LogRateLimitIntervalSec=30s` / `LogRateLimitBurst=10000`
because journald's default of 1000 messages per 30 s per service silently drops lines under a busy
board — and an incident is the worst possible time to discover your logs were rate-limited.
journald's own `SystemMaxUse` still bounds the disk. Genuine structured logging is a one-line
`crates/` change (`wheeld` uses `tracing_subscriber::fmt()`; `wheel-api` already uses `.json()`):
follow-up **F4**. Until then the README carries `journalctl` recipes that work against the text
format instead of pretending fields exist.

**The toolchain.** Docker bundled Node, `git` and the CLIs; native means the host supplies them, so
they get pinned in one file, `infra/vps/toolchain.env`, which `install.sh` sources and preflight
re-checks:

| | Pin | Why this one |
|---|---|---|
| Node | major `22` | `web/package.json` `engines.node` is `22.x` |
| pnpm | `9.15.4` | equals `web/package.json`'s `packageManager`, and a gate asserts they stay equal |
| `@anthropic-ai/claude-code` | `2.1.269` | **and a separate floor of `2.1.269`**, which PR #64's headless OAuth refresh requires |
| `@openai/codex` | `0.154.0` | current |

The pin and the floor are separate on purpose. The pin is for reproducibility; the floor is the
product requirement, and it is asserted **at install and again at every service start**, against
`claude --version` output — never against what `npm` was asked to install, because a cache, a
pre-existing global install or a failed upgrade all produce a box where the pin says one thing and
the binary is another. A gate also asserts the pin is not below the floor, so a careless bump cannot
drop the box under the OAuth requirement. Version comparison is written as a pure-shell numeric
compare with `2.1.269` vs `2.1.27` in its test table, because that pair is exactly where string
comparison and a naive `sort -V` fallback diverge.

## 9. What native loses, plainly

Docker gave **isolation between agents and the host**. It never gave isolation between agents (§1).
Removing it costs the following, and the honest summary is that systemd's sandbox recovers most of
the filesystem story and none of the rest.

**What is genuinely lost:**

1. **No pid namespace.** Agents' processes are host processes. Under `ProtectProc=invisible` an
   agent sees only `wheel`'s own processes, but it sees *all* of them — every other project's
   agents, their full command lines, and `wheeld` itself — and it can signal them. In the container
   the same uid-sharing existed, but the blast radius stopped at the container's pid namespace.
2. **No network namespace.** An agent shares the host's network stack, so it reaches every service
   bound to `127.0.0.1` on the box — `wheeld` itself, the board, anything else the operator runs
   there. That is the mechanism behind the trusted-proxy weakening in §3. `RestrictAddressFamilies`
   removes raw sockets; it does not remove reachability.
3. **`master.key` is same-uid readable.** `0600` protects it from other accounts on the box and from
   nothing else. An agent that reads `/var/lib/wheel/master.key` holds every vault secret for every
   project, and an agent that reads `/var/lib/wheel/operator-token` holds the account that can add
   users. This was equally true inside the container, where `docker exec -u 10001 wheeld cat
   /data/master.key` worked; what the container added was that you had to already be on the host to
   arrange it. Natively there is no such step.
4. **A weaker filesystem boundary than a container's, in one direction.** `ProtectSystem=strict`
   makes the host read-only, not invisible. An agent can still *read* `/etc/passwd`, `/etc/hostname`,
   installed package contents, and any world-readable file belonging to any other service on the
   box. A container's mount namespace simply would not have those paths. `ProtectHome=yes` closes
   the part that matters most (the operator's own home), and `ProtectProc=invisible` closes other
   services' `/proc`, but "read-only" and "absent" are not the same promise and this proposal will
   not pretend they are.
5. **The declared "laptop mode" safety rail does not engage.** `docs/PROTOCOL.md:876-902` defines
   shared-uid mode as opt-in only, requires the engine to log `SHARED_UID_WARNING` on every boot,
   and says *"the host must refuse to start a second project in this mode."* `wheeld` never sets
   `WHEEL_ALLOW_SHARED_UID` and never consults `UidIsolation`, so `UidIsolation::from_env()` reports
   `per_project`, the warning never fires, and the one-project rule is not enforced — while
   `wheeld` is, in fact, shared-uid by construction (`crates/wheeld/src/embedded.rs:3-6` says so in
   its own header). **This is a live discrepancy between the declared contract and the running
   code, and promoting native to the default is what makes it matter.** Follow-up **F5**, and the
   README states the property directly in the meantime rather than relying on a warning that does
   not print.

**What native does *not* lose, so nobody over-corrects:**

- Per-**node** isolation. Nobody has it, in any backend; redteam 037 is open everywhere and closes
  only with per-node uids (M2/M3).
- Secrets leaking into agent environments. `child_command`'s `env_clear` plus a pinned allowlist is
  backend-independent (`supervisor/mod.rs:268-272`), gate-enforced by `qa:env-allowlist`.
- `WHEEL_VAULT_KEY` in an environ — native never creates that carrier (§1).
- Data-directory permissions. `0700` is applied in the embedded path too.

**What mitigates the losses, and what only looks like it does.** Real: `ProtectHome` and
`ProtectProc=invisible`; tunnel mode publishing nothing; signup closed with a real gate; the §5
limits; and the `wheel-build` split keeping dependency build scripts off the account that can read
`master.key`. Not real: the operator token (it is not a boundary against your own agents —
`infra/vps/README.md` already says so and it is equally true here); `0600` modes (same uid); the
vault's never-shown-back property (that is anti-echo, not containment); and `ProtectProc=invisible`
with respect to 037 specifically.

**How this interacts with the webhook path.** Unchanged and still the thing to be careful about: an
`auth:none` endpoint wired to an agent turns an internet POST into a prompt (redteam 043), and an
injected agent natively reads `master.key` in one step instead of two. TLS mode is where public
ingress exists at all, so the README puts that sentence in the TLS section rather than the tunnel
one.

## 10. Migration from the running Docker deployment

Not urgent, per the operator's second message; written and rehearsed so it exists when it is wanted.

`infra/vps/migrate-from-docker.sh`, with `--dry-run`. The design is one property repeated: **the
Docker volume is only ever read.**

- The volume is mounted `:ro` in every container this script runs. There is no `docker volume rm`,
  no `docker compose down -v`, and no `-v` flag anywhere in the file — which is grep-assertable and
  is asserted by a test, the same way `deploy.sh --stop-legacy` earns its "never `-v`" claim.
- **`wheel_hostdata` and `wheel_pgdata` belong to a retired stack and must never be touched.** The
  script carries them as an explicit deny-list and refuses to run if asked to read one, even
  read-only, so a typo cannot start a conversation with them. The rehearsal creates decoys with
  those exact names and asserts they are byte-identical afterwards.
- It requires the wheeld container to be **stopped** first, which is also what drains it. SQLite in
  WAL mode has `-wal` and `-shm` alongside the database; a copy taken while a writer is live is a
  copy of a half-written transaction. The whole volume is copied verbatim rather than a curated file
  list, because the curated list is what goes stale.
- Verification is before and after: sha256 of every file on both sides must match, `master.key` and
  `operator-token` must be present and land at `0600`, the data directory at `0700 wheel:wheel`, and
  `PRAGMA integrity_check` runs when `sqlite3` is available and says so loudly when it is not.
- A non-empty `/var/lib/wheel` is refused without `--replace`, and `--replace` **moves** the old
  tree to `/var/lib/wheel.pre-migration-<timestamp>` rather than deleting it.
- Then it starts the native stack and proves the migration on the live box: `/healthz`, the
  *migrated* operator token authenticating `GET /v1/projects`, and the project list matching the
  count captured before the move.
- **Rollback is trivial by construction** and is the reason for all of the above: the Docker volume
  is untouched, so `systemctl stop wheeld wheel-web && docker compose -p wheel up -d` is the whole
  undo, and `/var/lib/wheel.pre-migration-*` is the native-side undo.

**How it is tested, and how that differs from the real box.** The rehearsal boots a native install,
lets a real `wheeld` create real state (`master.key`, `operator-token`, the store, a project), tars
that data directory into a Docker volume, and then migrates it into a *fresh* container, asserting
the operator token still authenticates and the project survives. The bytes are real and
wheeld-produced; what is synthesised is their having come from the `wheeld` image rather than from a
native install. The on-disk layout is identical because it is the same binary and the same
`prepare_data_dir` — `/data` versus `/var/lib/wheel` is a mount point, not a format. That is the
gap, and it is stated rather than hidden.

## 11. Follow-ups this lane deliberately did not take

| | What | Owner | Why not here |
|---|---|---|---|
| **F1** | `Type=notify` + `sd_notify(READY=1)` after the bind | API | `crates/` change; `ExecStartPost` gets the property today |
| **F2** | Caddy → wheeld over a unix socket with peercred, or a proxy secret header, so forwarded headers are trustworthy natively | API | `crates/` change; the one guarantee §3 marks WEAKER |
| **F3** | `oom_score_adj` on agent children so the kernel never picks `wheeld` | SDK/Engine | `crates/` change; §5's stated residual |
| **F4** | `WHEEL_LOG_FORMAT=json` on `wheeld` | API | one line in `lib.rs`, but it is `crates/` and four lanes are live |
| **F5** | `wheeld` should either engage `WHEEL_ALLOW_SHARED_UID` (warning + one-project refusal) or `PROTOCOL.md` should stop claiming it does | SDK/Engine + API | §9.5 — a contract/code discrepancy, not an infra bug |
| **F6** | Per-node uids and per-node cgroups (redteam 037/038) | SDK/Engine | M2/M3. The thing that actually closes agent-to-agent isolation, natively and in Docker alike |

