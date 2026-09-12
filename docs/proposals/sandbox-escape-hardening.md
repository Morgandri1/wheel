# Sandbox escape prevention (API, P1 per Morgan)

Scope: Morgan personally audited the production `wheel-wheeld-1` container and found
`privileged=false`, no docker socket mounted, `user=10001`, Docker's default seccomp profile
active, network `wheel_edge`, `CapEff=0000000000000000` — but `security_opt` was **empty** (no
`no-new-privileges`) and `cap_drop` was **empty** (`CapEff` reads zero only because the process is
non-root; anything that reached root inside that container would get the full default capability
set back). Independently confirmed against `infra/vps/compose.yml`: no `cap_drop`/`security_opt`/
`read_only` on `wheeld`; `web` has `read_only: true`, `wheeld` has neither.

Priority, stated directly by Morgan and relayed by PM: **sandbox escape prevention is now the top
security priority.** Per-node isolation within a project (F007, `script-execution-scope.md`) is
explicitly lower priority by comparison — not eliminated, just not the thing to escalate further
right now. This document does not change F007's status; it is a different boundary.

## Threat model, stated once so every item below can be checked against it

`ANY /p/{project_id}/{*rest}` is public, unauthenticated ingress (`docs/API.md`). A hit on an
endpoint node delivers an untrusted webhook body as an agent prompt (§3, the `<AgentPrompt
type="endpoint">` envelope). Agents execute code **by design** — that is the product. So the
threat model this document assumes, per Morgan's framing, is: **the attacker already reaches
"agent runs attacker-chosen code."** Nothing here is about preventing that — the wire matrix,
ingress rate limits and body caps are what bound what an agent can reach through the *board*, and
F007 is what would stop it presenting as a sibling *node*. This document is about the layer
*after* that: given a shell inside one agent's context (ADVERSARY's framing, exact) — no tampering
assumed beyond what an untrusted, possibly prompt-injected agent already legitimately has under
`bypassPermissions` — what's reachable from there that shouldn't be?

**Two distinct bad outcomes, not one**, per ADVERSARY: escaping the container to reach the host is
the headline case, but reaching **another project's** data or process on the same machine, without
ever leaving any container, is itself bad and explicitly in scope — Morgan deprioritized per-NODE
isolation *within* a project (F007), not per-PROJECT isolation between them. The rest of this
document has to answer both, and — the finding that reshapes everything below — **they are not the
same question on this deployment**, because of which container topology `wheeld` actually runs.

Everything below is measured against `main`/`dev` as of this commit, with `file:line`, the same
discipline `script-execution-scope.md` used.

## The framing question this has to answer before the numbers mean anything (ADVERSARY, expanded per PM)

**A note on "Docker" in what follows, per Morgan's sharpest correction yet: Docker itself is not
important — containerization, the PROPERTY, is.** Where "Docker" appears below (Shape 1, the
`DockerSandbox` backend, `bollard`) it is naming what is LIVE TODAY, not what should stay the
substrate. Nothing in this document should be read as weighting a recommendation toward keeping
Docker specifically; where a different mechanism wins on the merits — gVisor, Firecracker, something
else, integrated through Docker's runtime-flag mechanism OR bypassing Docker entirely (`containerd`
+ Kata directly, a bespoke Firecracker-jailer integration) — that is the recommendation, full stop,
not a niche alternative to a Docker-shaped default.

Wheel has **three** deployment shapes worth costing separately, not two, and — the finding that
reshapes this whole document — **they are not equally isolated today, in ways that run in opposite
directions from what their other tradeoffs suggest.**

### Shape 1 — `wheeld` in Docker (live today, `wheel-wheeld-1`)

A **single container, single process**, running every project's engine and every project's agent
children together, all as the same uid (10001), sharing one filesystem and one kernel namespace.
Confirmed independently by ADVERSARY and me: `crates/wheeld/src/embedded.rs` has no `setuid`,
`unshare`, or any privilege-dropping call anywhere — `EmbeddedSandbox` spawns each project's engine
as a plain `tokio::spawn` task in the SAME process. **There is no isolation unit smaller than "the
whole `wheeld` container" today, at all** — not per-project, let alone per-node.

- **Escape-to-host protection:** a real container boundary (pid namespace, network namespace, mount
  namespace) exists, and §§1–2 below (this PR) harden it further (`cap_drop`, `no-new-privileges`).
  Still weak in absolute terms — no AppArmor confirmed (§4), no userns-remap (§5) — but a genuine
  namespace boundary is there to harden.
- **Project-to-project isolation:** **zero.** An attacker with code execution in project A's agent
  reads project B's `/data/projects/B/...` over an ordinary file read — same uid, same mount
  namespace, no escape required. §§1–2/5–6 below harden the container's OUTER boundary; none of
  them create an inner one, so project B is exactly as reachable after every item in this document
  ships as before.
- **gVisor/Firecracker:** applies cleanly — wrap the one container, drop-in via `--runtime=runsc`
  or a `firecracker-containerd`/Kata shim. Raises the cost of a host escape. Does nothing for
  project-to-project reach, for the same reason as above: one sandboxed container is still one
  shared namespace for every project inside it.

### Shape 2 — `wheeld` native via systemd (PR #67, `api/wheeld-production`, **open, not merged, not deployed**)

Morgan's separate, live directive — *"i'd rather use that than docker… going forward i'd like the
default to be wheeld [native]"* — proposes making this the **default production deployment**,
Docker demoted to a supported alternative. That PR's own hardening work is extensive and already
measured (not re-derived here — citing rather than duplicating):

- **Accepted directives** (`wheeld.service`): `NoNewPrivileges`, `ProtectSystem=strict`,
  `ProtectHome=yes`, `PrivateTmp`, `ProtectKernelTunables/Modules/ControlGroups`, `RestrictSUIDSGID`,
  `LockPersonality`, `ProtectKernelLogs`, `ProtectClock`, `ProtectHostname`, `RestrictRealtime`,
  empty `CapabilityBoundingSet=`/`AmbientCapabilities=`, `RestrictAddressFamilies=` (keeping
  `AF_NETLINK` — dropping it silently breaks `ss`, which the kit's own readiness check depends on),
  `ProtectProc=invisible`, `PrivateDevices`, `PrivateMounts`, `LimitCORE=0`.
- **Rejected, each with a measurement, not an opinion** (this is the "several standard restrictions
  break things agents need" PM referenced): `SystemCallFilter=@system-service` (denies `@mount` —
  breaks `bwrap`/Chromium/rootless-container tooling an agent doing browser QA needs; only
  `wheel-web.service`, a single known Node program, gets it), `RestrictNamespaces=` (breaks
  `unshare` outright — Chromium, `bwrap`, rootless Podman all need user/mount/pid namespaces),
  `ProcSubset=pid` (hides `/proc/meminfo`/`/proc/cpuinfo` — build tools size parallelism from
  those, breakage is silent and reads as "the model seems dumber"), `LimitNPROC=` (counted per
  **real uid system-wide**, not per cgroup — every agent shares uid `wheel`, so it counts the wrong
  population; `TasksMax=` on the cgroup is the correct tool and is used instead).
- **Resource limits genuinely gained**, real and stated: `OOMPolicy=continue` (default `stop` would
  take down `wheeld` — and every other project's agents with it — the instant any one agent is
  OOM-killed), `MemoryMax=2.8G`/`MemoryHigh=2G`/`MemorySwapMax=1G`, `TasksMax=4096`, `CPUQuota=150%`
  reserving headroom so an agent cannot take `sshd` down with it. **None of this exists on the
  Docker deployment today** — `infra/vps/compose.yml` sets no `mem_limit`/`pids_limit`/`cpus` at
  all, so Shape 2 is a strict resource-limit upgrade over Shape 1 as currently deployed.
- **What #67's own §9 states is genuinely LOST versus Shape 1, in its own words**: *"No pid
  namespace… an agent sees… every other project's agents, their full command lines, and `wheeld`
  itself — and it can signal them."* *"No network namespace… an agent shares the host's network
  stack, so it reaches every service bound to `127.0.0.1`."* *"A weaker filesystem boundary than a
  container's, in one direction… `ProtectSystem=strict` makes the host read-only, not invisible."*
  And, load-bearing for THIS document specifically: *"The declared 'laptop mode' safety rail does
  not engage"* — `PROTOCOL.md` says shared-uid mode must warn loudly and refuse a second project,
  and `wheeld` does neither, in Docker OR native — a real contract/code discrepancy (#67's follow-up
  F5), not something Shape 2 introduces, but something Shape 2's promotion to default makes matter
  more.
- **Project-to-project isolation: also zero**, and by #67's own honest accounting, WORSE than Shape
  1's zero — Shape 1 at least confines the blast radius of "sees and can signal every process" to
  one container's pid namespace; Shape 2 has no pid namespace at all, so that blast radius is the
  whole host.
- **gVisor/Firecracker: structurally does not apply, and adopting either would partially undo
  "native."** Both are container/VM runtimes — something has to hand them a container or a VM
  boundary to enforce. A bare `systemd` unit running a process tree is neither. Wrapping `wheeld`
  itself in a gVisor/Firecracker sandbox to get this protection back would mean running it as (or
  inside) a container again — which is most of what "go native" was for. This is not a cost line in
  a table; it is a **structural incompatibility** between Shape 2 as designed and "wrap execution in
  a stronger-than-namespaces sandbox" as a mitigation. Worth stating plainly rather than leaving a
  blank cell that reads as "not costed yet."

### Shape 3 — `wheeld` converges on `wheel-host`'s per-project sandbox architecture (hypothetical)

Formerly "Option B" in this document's first draft. `wheeld` moves from "one process hosting every
engine as an async task" to "a supervisor that spawns and isolates one sandbox per project" —
structurally what `wheel-host` already is. **Not a config flag; a real architecture change** — but
ADVERSARY's follow-up (building the base for this exact comparison) found the cost is smaller than
"a real architecture change" alone suggests, because **the isolation primitive itself is not new to
invent — it is already written, tested, and previously ADVERSARY-reviewed against F003/F007** in
`wheel-host`'s `process` sandbox backend (`crates/wheel-host/src/sandbox/process.rs`, 963 lines):

- Real per-project `setuid`/`setgid` via `pre_exec` (`drop_privileges`: `setgroups([])` → `setgid` →
  `setuid` → `no_new_privs`, in that order — the same order #67's own `NoNewPrivileges=` reasoning
  independently arrives at for a different mechanism — then VERIFIES the uid actually changed rather
  than trusting the syscalls silently), and a distinct uid range allocated per project
  (`allocate_uid`).
- 0700 project data directories, chowned to that allocated uid.
- **Unix sockets only, no TCP, for the control plane** — the backend's own comment: "on a shared
  kernel every loopback port is reachable by every other tenant, so a per-project port would undo
  the whole exercise." **Pathname sockets specifically, not abstract ones** — the abstract namespace
  ignores filesystem permissions, so it wouldn't actually gate access by the directory's 0700 mode.
  `wheeld` already does the socket-per-project half of this for its own control plane
  (`crates/wheeld/src/embedded.rs`'s `socket_path`/`ListenAddr::Unix` — confirmed: every embedded
  engine already gets its own pathname unix socket, not a shared port), so this part genuinely IS
  reused today, not just reusable.
- No secrets on argv, env only — already the pattern `wheeld`'s own agent-spawn path follows
  elsewhere (`child_command`'s `env_clear` plus an allowlist, §2).

So the real cost of Shape 3 is **porting this primitive into `wheeld`'s own spawn path — which
currently has none of it (confirmed against `embedded.rs`: no `setuid`, no per-project uid
allocation, no privilege drop of any kind) — not inventing a new mechanism from a blank page.**
Sizing it as "build per-project sandboxing from scratch" overstates the unknown; the honest sizing
question is the porting/adaptation effort plus whatever `wheeld`-specific integration work surfaces
(the two run different lifecycles — `wheel-host` supervises whole containers/processes from outside;
`wheeld` embeds engines as in-process tasks today, so SPAWNING a genuinely separate, privilege-
dropped process per project is the actual new work, even with the drop-privileges primitive itself
already in hand).

- **Escape-to-host protection:** whatever the per-project sandbox choice earns — Shape 1's Docker
  container hardening, Shape 2's systemd directives applied per-project instead of once, or a
  stronger runtime (below) — but now earned N times, once per project, rather than once for the
  whole box.
- **Project-to-project isolation: real, for the first time in any shape.** A distinct uid per
  project, ideally a distinct sandbox (container or systemd scope) per project, closes the "ordinary
  file read across projects" path that Shapes 1 and 2 both leave open.
- **gVisor/Firecracker: this is the ONLY shape where either technology buys project-to-project
  isolation, not just host-escape resistance** — wrapping each per-project sandbox, exactly as this
  document's original costing (below) described. It is also the only shape where the technology
  choice compounds per project: N parked-agent populations, each independently paying whichever
  runtime's per-sandbox resume cost, not one payment for the whole box.

**A related, narrower finding for Shape 2, from the same reuse angle:** the "unix-socket-only, no
TCP" pattern above is a partial answer to Shape 2's network-namespace gap — for the CONTROL PLANE
specifically. It does not fix the gap as a whole: an agent's own outbound work (`git`, `npm`, tool
calls, and — the specific weakness §"Shape 2" names — reaching whatever else is bound to
`127.0.0.1`) still goes over the host's shared network stack regardless of how `wheeld` talks to its
own engines, because that traffic was never a control-plane concern to begin with. `wheeld` already
gets the control-plane half of this for free (confirmed above); the part #67's own §3 marks WEAKER —
honest client attribution behind Caddy — needs its own fix (#67's follow-up **F2**, a unix socket
with peer-credential checking or a shared secret header between Caddy and `wheeld` specifically,
already scoped there) and is a different problem from general agent network reachability, which no
shape in this document closes without a real network namespace (Shape 1's container, or Shape 3's
per-project sandbox if it earns one).

### The tension Morgan should see stated, not discover after (PM's ask, directly)

**PR #67 and "sandbox escape prevention is paramount" pull in opposite directions on ONE specific
axis, and agree on another.** They agree that Shape 1 as currently deployed is under-hardened (no
resource limits at all; #67's own §1 says so) and that both proposals' compose/systemd-level
hardening are real, cheap wins worth taking regardless of which shape wins. They disagree on the
namespace question: promoting Shape 2 to the default, as #67 proposes, is **a downgrade on the
escape-to-host axis specifically** — trading a real pid/network/mount namespace boundary (Shape 1)
for systemd directives that recover most of the filesystem story and, by #67's OWN admission, none
of the process-visibility or network-reachability story. It is simultaneously an upgrade on other
axes that matter (resource limits that prevent one agent from taking the whole box down; a cleaner,
measured, honestly-scored hardening posture; less operational surprise from Docker's iptables
interaction with `ufw`). And it forecloses gVisor/Firecracker as an escape-hardening path outright,
structurally, not as a matter of cost.

**If "sandbox escape prevention is paramount" is the standing priority, Shape 2 becoming the
default is a decision that trades away part of what that priority is asking for, in exchange for
real gains on a different axis (resource limits, operational clarity).** That may still be the
right call — a systemd-confined process with real resource limits and no runaway-agent blast radius
is a legitimate, defensible posture, and #67's own measurement discipline is exactly what this
document has been asking for throughout. But it is Morgan's tradeoff to make with the axis named,
not one that should get decided by which PR merges first while the other is mid-review. **Recommend
against merging either #67 (promoting Shape 2 to default) or a Shape-3 commitment until this
specific question — does the default deployment keep a namespace boundary, and is that boundary
worth more than #67's resource-limit and operational gains — has an explicit answer from Morgan.**

Shape 1's compose hardening (§§1–2 below) is a different decision: it improves Shape 1 without
foreclosing anything, costs little, and is worth shipping regardless of which shape ends up
default. That is why it proceeds in this PR while the shape question stays open, per PM's explicit
instruction not to pause it.

## 1. `security_opt: [no-new-privileges:true]` on every service — done

`infra/vps/compose.yml`. Added to `preflight`, `wheeld`, `verify-signup-gate`, `web`, `caddy`.
Cheap and unconditionally safe: it blocks privilege *gain* through `execve` of a setuid/setgid
binary or one carrying file capabilities. None of these five services relies on that mechanism to
function (verified per-service below), so there is nothing for this to break.

## 2. `cap_drop: [ALL]`, with capabilities added back only where verified necessary — done, needs a live run before merge

Per service, what was actually checked before writing `cap_drop`/`cap_add` (not assumed):

| Service | Runs as | What it does | Capability needed | `cap_add` |
|---|---|---|---|---|
| `preflight` | `caddy:2` image, `network_mode: none` | Shell script validating env vars (`preflight.sh`) | none | — |
| `wheeld` | fixed uid 10001 (`docker/Dockerfile.wheeld` `USER 10001`) | Never calls setuid/setgid; the embedded sandbox backend runs every agent as **wheeld's own uid** too (`crates/wheeld/src/embedded.rs`'s doc comment: "the tenants are all the same person" — single-tenant by design, a stated boundary, not F007 scope) | none | — |
| `verify-signup-gate` | `curlimages/curl` image | Shell script, one `curl` call (`verify-signup-gate.sh`) | none | — |
| `web` | fixed uid 10001 (`docker/Dockerfile.web` `USER 10001`) | `node server.js` on an unprivileged port (3000) | none | — |
| `caddy` | root (official `caddy:2` image default) | Binds `:80`/`:443` (privileged ports), ACME (HTTP-01/TLS-ALPN, same two ports), writes certs to its own volumes | `CAP_NET_BIND_SERVICE` (root loses low-port binding once `cap_drop: ALL` removes it too — dropping ALL from root is not equivalent to leaving root alone) | `["NET_BIND_SERVICE"]` |

`preflight`/`verify-signup-gate`/`wheeld`/`web` get nothing added back — reasoned from what their
own entrypoints (their Dockerfiles' `USER`/`ENTRYPOINT`/`CMD`, their shell scripts) actually do,
not from a generic "containers usually need X" assumption. `caddy` is the one service that
genuinely needs something, for the specific, checkable reason in the table.

**What this could not verify: none of it was run against a live Docker daemon.** This sandbox has
no `docker` binary at all (confirmed: `docker version` → command not found), matching the standing
gap `sandbox_docker.rs`'s own tests already carry (ADVERSARY's review of #83: "Could not run
`sandbox_docker.rs` itself — no docker daemon in my sandbox... someone with docker available should
give it one real run before merge"). The same caveat applies here, at higher stakes: a YAML change
that looks right by inspection and actually fails to bind `:80` in TLS mode, or fails wheeld's
healthcheck, is a production outage, not a test failure. **`infra/vps/rehearse.sh` is exactly the
tool that proves this** — it stands up the whole compose stack locally and runs every check the
real deployment must pass. Before this merges, someone with a docker daemon needs to run:

```
infra/vps/rehearse.sh                    # tunnel mode: preflight, wheeld, web all healthy
REHEARSE_DOMAIN=... WHEEL_DOMAIN=... infra/vps/rehearse.sh   # TLS mode: caddy actually binds :80/:443, gets a cert
```

I have reasoned through why each `cap_drop`/`cap_add` should work; I have not watched it work.

## 3. `cap_add: [SETUID, SETGID]` removed from the per-project docker sandbox — done, PR #83

This is the OTHER container layer — `wheel-host`'s `DockerSandbox` (`crates/wheel-host/src/
sandbox/docker.rs`), one container per *project* on the multi-tenant cloud deployment, not the
`wheeld` single-user product `infra/vps/compose.yml` runs. Already fixed: PR #83 corrected the
comment falsely claiming per-node uid dropping was implemented and dropped the unused
`CAP_SETUID`/`CAP_SETGID` grant entirely (`cap_drop: ALL`, nothing added back — confirmed no
`setuid`/`setgid`/`pre_exec` call exists anywhere in `wheel-engine`). No new work here; it needs to
merge, which is tracked on that PR, not this document.

## 4. AppArmor — confirm it actually applies, don't assume it

**Not verified from this sandbox — no docker daemon, no production access.** The exact commands,
so whoever has access can run them and report back:

```
# On the HOST, once: does dockerd itself see AppArmor support?
docker info --format '{{.SecurityOptions}}'          # expect apparmor in the list

# Per CONTAINER: which profile is actually loaded for the running wheeld?
docker inspect wheel-wheeld-1 --format '{{json .AppArmorProfile}}'
# OR, from inside the container's pid namespace, the ground truth the kernel itself enforces:
cat /proc/$(docker inspect -f '{{.State.Pid}}' wheel-wheeld-1)/attr/current
# expect: "docker-default (enforce)" — "unconfined" means nothing is applied regardless of what
# docker inspect's OWN metadata claims (that field can lag/misreport; /proc is what the kernel
# actually consults on each syscall).
```

Why this matters enough to check rather than assume: Docker applies `docker-default` automatically
on every container *if and only if* (a) the host kernel has AppArmor compiled in and enabled —
Ubuntu 24.04 (the documented target, `infra/vps/README.md:54`) ships it enabled by default, so this
is likely true but not certain on every VPS image, and (b) the docker daemon successfully loaded
the profile at its own startup, which can silently fail on a host where `apparmor_parser` is
missing or the AppArmor kernel module was never loaded (some minimal cloud/container-optimized
base images strip it). A host where (a) or (b) silently failed would still report `privileged:
false`, still show the seccomp default active, still show `CapEff: 0` — every signal Morgan's audit
already checked — while the AppArmor layer specifically is silently absent, and nothing in a
`docker inspect` of the container's declared config would say so on its own; only the two commands
above, read together, prove it.

**Ask:** whoever has shell access to the production host runs both commands against
`wheel-wheeld-1` and reports the actual profile name and enforce/complain/unconfined state back on
this document (or the tracking issue). Until that is recorded, "AppArmor is protecting this
container" is an assumption, not a checked fact — the same distinction Morgan's own audit already
drew between `CapEff` reading zero (a fact) and capabilities actually being *dropped* (they were
not, until item 2).

## 5. userns-remap — proposal, not a decision

**What it does:** maps container UID 0 (and the whole in-container UID range) to an *unprivileged*
range on the host, cluster-wide, via one dockerd config change (`/etc/docker/daemon.json`,
`userns-remap`). A process that is root *inside* its container is a random unprivileged UID
*outside* it — including after a container-to-host escape. This is the strongest anti-escape
measure available here that changes nothing about how a container runs, only what its root
capabilities are worth if they ever reach the host.

**What it costs / breaks, concretely for this deployment:**

- **Volume ownership.** `wheel-data` is `chown agent:agent /data && chmod 0700` (uid 10001,
  `docker/Dockerfile.wheeld`) at image-build time, inside the container's own UID namespace.
  Under userns-remap, uid 10001 *inside* the container is a DIFFERENT uid on the *host* filesystem
  where the named volume actually lives — Docker remaps ownership on the volume transparently for
  files the remapped daemon itself creates, but an operator inspecting `/var/lib/docker/volumes/
  .../data` directly from the host (debugging, backup tooling, `docker cp`) sees host-remapped
  UIDs that do not match what `ls -l` shows *inside* the container. Operationally survivable — it
  is Docker's own supported feature, not a hack — but it is friction anyone debugging a production
  volume issue needs to know about going in, and it is worth stating plainly rather than discovered
  mid-incident.
- **Bind mounts break the same way**, more sharply: this compose file has none from the host into
  `wheeld`/`web` (only named volumes and `:ro` config files that don't need write access), so this
  specific risk is not live today — but it constrains any FUTURE bind mount (a host path an agent's
  workspace needs, say) to either living inside the remapped range or needing its own explicit
  exception.
- **`docker exec`/interactive debugging** as root inside a remapped container is still root
  *inside*, so day-to-day operator experience is unaffected; the change is invisible until someone
  needs host-side access to the same files.
- **Scope:** userns-remap is a *dockerd-wide* setting in this deployment's current form (one
  remap range for every container the daemon runs) — it cannot be turned on for `wheeld` alone
  without also affecting `web`/`caddy`/`preflight`/`verify-signup-gate` on the same host. Docker
  does support per-container opt-out (`--userns=host`) but not a clean per-container opt-IN to a
  *different* remap range without more daemon-level configuration than this single-VPS deployment
  currently carries.

**Is it worth it here:** I think yes, conditionally — it is Docker-native (no new runtime, no
integration work beyond a daemon restart and the volume-ownership adjustment), it directly answers
Morgan's stated priority (raises the value of a root-inside-container escape to zero rather than to
"root on the host"), and this deployment has no bind mounts today for it to complicate. The
condition: it needs a `rehearse.sh` run with `userns-remap` actually enabled on the rehearsing
machine before it ships, specifically to catch the volume-ownership interaction above rather than
finding it in production. **Proposing it; not implementing it in this document** — Morgan's call
given it is a daemon-wide setting on the one host everything already runs on.

## 6. Read-only rootfs with explicit writable mounts — proposal, not a decision

`web` already has this (`read_only: true` + a `tmpfs` for `/tmp`). Extending it:

- **`wheeld` cannot go read-only as simply.** Agents write real workspaces — `crates/wheel-engine/
  src/config.rs`'s `workspace_dir` (`/data/ws/<name>`, §3e) is where an agent's own git clones,
  build artifacts and files live, and that is the *point* of a workspace, not incidental state. The
  container's rootfs (everything outside `/data`) is a much smaller surface — the `wheeld`/`wheel`
  binaries, the Rust/Node/Python toolchain installed at image-build time (`docker/Dockerfile.wheeld`)
  — and none of that should ever need to be written to at runtime, so `read_only: true` on the
  rootfs *with `/data` staying a normal read-write volume* (not `tmpfs` — it must persist across
  restarts, unlike `web`'s `/tmp`) looks free on inspection. What is NOT free: `CARGO_HOME`/
  `RUSTUP_HOME` (`ENV RUSTUP_HOME=/opt/rust/rustup CARGO_HOME=/opt/rust/cargo`,
  `docker/Dockerfile.wheeld`) — if an agent's `cargo build` ever writes into the toolchain
  directory itself (a plugin install, a registry cache miss touching `$CARGO_HOME` rather than
  `$CARGO_HOME/registry` under `/data`) rather than purely reading from it, `read_only: true` would
  break builds that work today. This needs to be traced through an actual build, not assumed either
  way.
- **`caddy`/`preflight`/`verify-signup-gate`** are much simpler candidates — caddy writes only to
  its two declared volumes (`caddy-data`, `caddy-config`) and its own logs (stdout, not a file by
  default); preflight and verify-signup-gate are one-shot scripts that write nothing persistent at
  all. These look like the safe, mechanical wins here.

**Proposing:** `read_only: true` on `preflight`, `verify-signup-gate`, `caddy` now (low-risk,
mirrors what `web` already proved out); `wheeld` conditional on tracing `$CARGO_HOME`/`$RUSTUP_HOME`
write behavior under a real agent build first, via `rehearse.sh` with `REHEARSE_FAKE_HARNESS`
disabled so a real toolchain invocation actually runs. Not implementing either in this document.

## The real boundary — why none of the above is the actual answer

Every item in 1–6 raises the *cost* of an escape: no privilege regain, no ambient capabilities, a
confined AppArmor/seccomp profile, no ambient root-equivalence even if achieved, no writable rootfs
to persist a foothold in. None of them changes *what* is being escaped from: a shared Linux kernel.
**A kernel vulnerability — a bug in the syscall surface every container on the host shares —
defeats every layer above at once**, because all of it is enforced by that same kernel. This is not
a hypothetical: container-escape CVEs via kernel bugs (not misconfiguration) are a real, recurring
category, and Wheel's own threat model already assumes the attacker reaches "agent runs
attacker-chosen code" — a container's whole syscall surface is exactly what that code then has
unrestricted, adversarial access to probe.

The measures above are still worth doing — they close the gap between "reachable today" and "the
platform's own defaults," and they are what stops a *config* mistake or a known, patched CVE in a
component (not the kernel itself) from being an immediate escape. But "paramount," Morgan's own
word for this priority, points past them to the boundary that does not share a kernel at all.

### gVisor vs. Firecracker, costed for Wheel's specific workload

Both replace "protect the shared kernel with policy" with "give each sandbox a kernel of its own,"
by different mechanisms. Neither has been benchmarked against Wheel's actual workload in this
document — the figures below are the mechanisms' own published characteristics, presented so Morgan
can weigh them against what Wheel needs, not a substitute for a real spike if either is chosen.

**Which shape this table is costing, tying back to the three shapes above:** the integration story
below (`--runtime=runsc`, `HostConfig.Runtime`) is written against `wheel-host`'s `DockerSandbox`,
which already runs one container per project — this table applies to it directly, and to Shape 3
(each per-project sandbox gets wrapped the same way). Applied to Shape 1 (`wheeld`'s one container
as it exists today), either technology wraps that SINGLE container: raises the cost of a VPS-host
escape, does nothing for project-to-project reach, for the same reason §§1–2/5–6 don't — no
per-project boundary exists inside it for a stronger runtime to reinforce. **Does not apply to
Shape 2 at all** — native systemd has no container/VM boundary to hand either runtime, and
retrofitting one would mean partially reversing what "native" was for (§ Shape 2 above). Under
Shape 3 specifically, the startup-latency number below starts compounding: N parked agents across N
projects each paying gVisor/Firecracker's per-sandbox resume cost independently, not once for the
whole box.

**Why startup latency and memory are the two numbers that matter here, specifically:** Wheel's
whole compute-cost story is idle parking (§3c#14) — an agent's process stops after
`idle_timeout_secs` and the NEXT message pays a resume cost before the agent can answer. Today that
cost is "start a process, `--resume <session_id>`," measured in the engine's own tests at well
under a second for the harness itself. Whatever sandboxing layer wraps that resume path becomes
part of every parked agent's first-reply latency, on every wake, forever — not a one-time deploy
cost.

| | gVisor (`runsc`) | Firecracker (via Kata Containers, `containerd-shim-kata-v2`, or `firecracker-containerd`) |
|---|---|---|
| **Mechanism** | Userspace kernel (the "Sentry") intercepts and reimplements the guest's syscalls; no nested virtualization required in its default `ptrace` platform, or KVM-accelerated in its `kvm` platform where available | A real, minimal guest kernel + init inside a purpose-built microVM; the isolation boundary is genuine hardware virtualization (KVM), not syscall interception |
| **Integration into `wheel-host`** | Like Firecracker/Kata, not tied to Docker specifically — `runsc` is a general OCI runtime. Drop-in under today's Docker orchestration (`--runtime=runsc`, or the per-container `HostConfig.Runtime` field `bollard` already exposes), or equally usable via `containerd`/CRI-O directly if `wheel-host` moves off `bollard` for another reason | Two equally legitimate paths, not one cost line: (a) `--runtime=kata` under today's Docker orchestration, comparable integration effort to gVisor; or (b) bypass Docker/`bollard` entirely and drive `containerd` + Kata (or `firecracker-containerd`) directly from `wheel-host` — a DIFFERENT control plane, not a worse one. Since containerization is the property that matters, not `dockerd` specifically, (b) is worth evaluating on its own terms during the spike — it removes a layer of indirection (`bollard` → `dockerd` → `containerd-shim-kata-v2` → Firecracker collapses to `wheel-host` → `containerd` → Firecracker directly) rather than adding one |
| **Host requirement** | Works without KVM at all (ptrace platform); KVM used only if present, as an optimization | **Requires KVM** (hardware virtualization exposed to the host). This is the practical blocker worth flagging loudly: `infra/vps/README.md` targets a generic VPS, and nested virtualization is NOT universally offered by VPS/cloud providers — some budget/shared-CPU tiers explicitly disable it. This has to be confirmed for whatever host Wheel actually deploys to before Firecracker is viable at all, independent of every other tradeoff here |
| **Published startup overhead** | Modest, additive to an ordinary container start — commonly cited as tens to low-hundreds of milliseconds beyond `runc`, since it is still "start a container," just under a stricter syscall filter | Firecracker's own published figure for booting a minimal guest is ~125ms — but that is kernel+init only; a guest that then needs to bring up Wheel's actual engine+harness environment inside it adds real time on top, likely comparable to or more than gVisor's overhead once the FULL resume path (not just "a VM exists") is counted |
| **Published memory overhead** | Roughly 10–25MB per sandbox for the Sentry process, on top of the workload's own usage | Firecracker's own design target is ~5MB per microVM for the hypervisor itself — but a full guest kernel + whatever the harness needs resident (not a shared host kernel's page cache) is the real comparison, and that is workload-dependent, not a fixed hypervisor number |
| **Steady-state overhead** | Syscall-heavy workloads (lots of small I/O) see the most cost — often cited as a meaningful hit on syscall-bound throughput; CPU-bound work (a `cargo build` mostly compiling) is much less affected, since compute itself isn't intercepted | Near-native once running — the guest has its own real kernel, so steady-state syscall cost is not the concern the way it is for gVisor; the cost is entirely at boot and in per-VM fixed memory |
| **Operational maturity for THIS shape of workload** | Used in production by GKE Sandbox / Cloud Run for exactly this "many short-lived, frequently-recycled sandboxes" shape | Used in production by AWS Lambda / Fargate for the same shape, but typically inside AWS's own bare-metal fleet with guaranteed KVM access — the VPS-generic deployment target is the less-proven case for it specifically |

**A real recommendation, not a hedge — Morgan's explicit instruction: don't default toward Docker
or the familiar option for the isolation MECHANISM specifically; say plainly if the numbers favor
something else.** They do, directionally: **if escape prevention is genuinely paramount, Firecracker
(via Kata) is the mechanism that actually matches that priority, and gVisor is the fallback, not the
default.** The reasoning:

- **The isolation guarantees are not the same category of thing.** gVisor's Sentry re-implements the
  syscall surface in userspace — a smaller, more scrutable attack surface than the full Linux kernel,
  but still a piece of software an attacker's syscalls reach directly, and it has had real sandbox-
  escape CVEs (not hypothetical — this is the mechanism's own track record, the reason "smaller
  surface" is not "no surface"). Firecracker's boundary is genuine hardware virtualization: escaping
  it needs a hypervisor or CPU-level vulnerability, a categorically higher bar, and it is the
  mechanism AWS itself picked for Lambda/Fargate specifically BECAUSE the workload is "run
  arbitrary tenant-chosen code" — which is exactly Wheel's own threat model, stated at the top of
  this document. When the stated priority is "paramount," the stronger category of guarantee is the
  one that priority is actually asking for.
- **The KVM precondition is a provisioning decision, not a structural blocker.** It rules out
  Firecracker on a VPS tier that has it disabled; it does not rule out Firecracker for Wheel, because
  which VPS tier to deploy on is Morgan's choice to make, not a fixed constraint this document
  inherited. Many providers offer KVM-capable tiers at a modest cost step up from the cheapest
  shared-CPU instances. Worth confirming for whichever host is actually chosen, not worth treating as
  disqualifying by default.
- **The honest risk, and the one thing this recommendation is conditional on**, is resume latency —
  Wheel's parked-agent model pays sandbox-startup cost on every wake, and Firecracker's published
  ~125ms figure is kernel+init only, not "Wheel's engine and harness are answering." Neither number
  in the table above is measured against Wheel's real resume path. **If a real spike shows
  Firecracker's resume cost is unacceptable for the product's UX, gVisor is the correct fallback** —
  cheaper integration, no KVM precondition, and a meaningfully better security posture than either
  Shape 1's current cap-drop-only hardening or Shape 2's namespace-free systemd confinement. That
  would be a real, evidenced reason to step down from the stronger mechanism, not a default settled
  in advance.

**Recommendation: target Firecracker/Kata for whichever of Shape 1 or Shape 3 is chosen as the
default deployment, contingent on a `runsc`-vs-`kata` resume-latency spike on
`infra/vps/rehearse.sh`'s stack (or its successor) deciding between them with real numbers instead of
published ones.** Confirm KVM availability on the target host as part of scoping that spike, not as
a reason to skip evaluating Firecracker first. This recommendation does not apply to Shape 2 at all —
native systemd structurally cannot use either mechanism (above), which is itself one more point in
the tension Morgan should weigh when deciding whether Shape 2 becomes the default.

## Summary — what's asked of whoever reads this next

- **The shape question, first, and it now has three answers instead of two**: Shape 1 (`wheeld` in
  Docker, live today), Shape 2 (`wheeld` native via systemd, PR #67, proposed as the new default),
  or Shape 3 (`wheeld` as a per-project sandbox supervisor, converging on `wheel-host`'s existing
  `process` backend — hypothetical, unbuilt). Shapes 1 and 2 both leave project-to-project reach
  completely open; Shape 2 is honestly worse on that specific axis (no pid namespace at all) while
  gaining real resource-limit protection Shape 1 lacks entirely today. Shape 3 is the only one where
  gVisor/Firecracker or per-project uid isolation actually closes project-to-project reach — and the
  only one not yet started. **This is the decision everything else's shape depends on, and #67 (push
  Shape 2 to default) and "sandbox escape prevention is paramount" are in real tension on the
  namespace question specifically** — see the dedicated section above. Recommend Morgan decide this
  explicitly before either #67 merges or Shape 3 work starts, rather than have it settled by
  whichever ships first.
- Items 1–3 (Shape 1's compose hardening): code changes exist (this PR + #83), proceeding regardless
  of the shape decision per PM's instruction not to pause them. **Needs a `rehearse.sh` run with
  docker access** before merge — not yet done, called out explicitly above rather than assumed.
- Item 4: needs someone with production shell access to run the two `docker inspect`/`/proc`
  commands above against `wheel-wheeld-1` and record the actual result.
- Items 5–6: proposed, not implemented, with the specific breakage each would need to survive
  (volume ownership; `$CARGO_HOME` write behavior) named rather than hand-waved. Both are Shape-1
  scoped — they harden the single container, not project-to-project reach within it, and (userns-
  remap specifically) would need re-deriving for Shape 2, where there is no container to remap.
- The real boundary: gVisor vs. Firecracker costed above, explicitly tied to which shape each one
  actually wraps — applies to Shapes 1 and 3, structurally does not apply to Shape 2. **Actual
  recommendation, per Morgan's own instruction not to hedge toward the familiar option: target
  Firecracker/Kata, gVisor as the evidenced fallback if a real resume-latency spike rules it out.**
  Firecracker's hardware-virtualization boundary is the category of guarantee "paramount" is asking
  for; gVisor's syscall-interception layer is real but has its own CVE history, a smaller surface
  than the shared kernel and not a categorically different kind of boundary. The one open input is
  measured resume latency against Wheel's actual engine+harness startup, not published kernel-boot
  numbers — that spike, plus confirming KVM on whichever host is chosen, is what should decide
  between them, not a default settled here.
