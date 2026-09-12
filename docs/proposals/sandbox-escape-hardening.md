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

## The framing question this has to answer before the numbers mean anything (ADVERSARY)

Wheel has two deployment topologies, and they are not equally isolated today:

- **`wheel-host` + `DockerSandbox`** (the cloud, multi-tenant deployment, §5b) already runs **one
  container per project** (`crates/wheel-host/src/sandbox/docker.rs`). Escape-hardening this one is
  close to a pure "hard config" job: a given project's container is already its own isolation unit,
  so `cap_drop`/`no-new-privileges`/a stronger runtime (gVisor/Firecracker, see below) each apply
  per-project automatically, for free, because the boundary they'd reinforce already exists at the
  right granularity.
- **`wheeld`** (the VPS/self-hosted deployment — `infra/vps/compose.yml`, the actual container
  Morgan audited, `wheel-wheeld-1`) is a **single process, single container, running every
  project's engine and every project's agent children together**, all as the same uid (10001),
  sharing one filesystem and one kernel namespace. Confirmed independently by both ADVERSARY and
  me: `crates/wheeld/src/embedded.rs` has no `setuid`, `unshare`, or any privilege-dropping call
  anywhere — `EmbeddedSandbox` spawns each project's engine as a plain `tokio::spawn` task in the
  SAME process (§ "Isolation gap" in `script-execution-scope.md`'s §0 table already says as much
  for per-node; this is the same absence one level up, at per-PROJECT granularity, which nothing
  else in the codebase currently states this plainly). **There is no isolation unit smaller than
  "the whole `wheeld` container" today, at all** — not per-project, let alone per-node.

That absence means the compose-level hardening in §§1–2 below (what ships in this PR) raises the
cost of escaping `wheeld`'s ONE container to the host. It does **nothing** for project-to-project
reach *within* that container, because there is no boundary there for `cap_drop`/AppArmor/a
stronger runtime to reinforce — an attacker with code execution in project A's agent reads project
B's `/data/projects/B/...` over an ordinary file read, same uid, same mount namespace, no escape of
any kind required. This is true **regardless of whether §§5–6 (userns-remap, read-only rootfs) or
gVisor/Firecracker ship** — every one of those hardens the same single boundary, and project B is
inside it exactly as much as project A is.

**So the actual decision is not "which sandboxing technology" — it is which of two architectures
`wheeld` is:**

- **Option A — harden the one container.** Everything in §§1–2, 4–6 below, plus optionally
  wrapping the whole `wheeld` container in gVisor/Firecracker. Materially raises the cost of a
  VPS-host escape. Does not touch project-to-project reach; that stays exactly as open as it is
  today, at every layer of hardening this option can add.
- **Option B — `wheeld` becomes a supervisor of per-project sandboxes**, structurally the same
  shape `wheel-host` already is. This is NOT a small change: `wheeld` would move from "one process
  hosting every engine as an async task" to "one process that spawns and supervises N sandboxed
  children," which is a different architecture, not a config flag. The good news: **this
  mechanism already exists and is already tested** — `wheel-host`'s `process` sandbox backend
  (`crates/wheel-host/src/sandbox/process.rs`) does real per-project `setuid`/`setgid` via
  `pre_exec` (`drop_privileges`, line 231/395 — `libc::setuid`, `setgid` before `setuid`,
  `setgroups([])`, `no_new_privs`), allocates a distinct uid range per project (`allocate_uid`),
  and is covered by its own test suite. The natural shape of Option B is **converging `wheeld` on
  that existing backend** rather than inventing a second, parallel isolation mechanism — `wheeld`
  already proxies each project over a unix socket the same way `wheel-host` does
  (`crates/wheeld/src/embedded.rs`'s own doc comment: "the host still proxies over a unix socket
  per project, so the API's engine proxy and events bridge run exactly the code they run in
  production"), so the control-plane side of this is already shared; what's missing is the process
  boundary underneath it. Under Option B, gVisor/Firecracker would wrap EACH per-project sandbox,
  which is where a stronger-than-namespaces runtime actually buys project-to-project isolation, not
  just VPS-host escape resistance.

**Not deciding between them here.** Option A is what this PR ships (it is strictly good regardless
of which way B goes, and costs little). Option B is a real project, sized closer to a milestone
than a hardening pass, and the deciding fact is one this document does not have: whether a given
`wheeld` deployment is actually single-tenant. It is NOT single-tenant by construction — `WHEEL_
SIGNUP=open` (`infra/vps/compose.yml`'s own comment: "lets anyone who reaches this server create an
account and run agents on it") lets any number of distinct people sign up, and even under the
`closed` default the owner can add more accounts via `POST /v1/auth/users` (`docs/API.md`). So
"every project belongs to the same person" is a fact about how a *specific* deployment is being
used, not a guarantee this codebase makes — project-to-project reach on a `WHEEL_SIGNUP=open` (or
multi-account `closed`) `wheeld` is a genuine cross-TENANT confidentiality breach, not a
self-inflicted one. Whether that risk is acceptable for THIS deployment, and therefore whether
Option B is worth its cost, is Morgan's call to make with that fact in hand — not mine to assume
away.

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

**Which topology this table is costing, tying back to the framing question above:** the
integration story below (`--runtime=runsc`, `HostConfig.Runtime`) is written against
`wheel-host`'s `DockerSandbox` — which already runs one container per project, so gVisor/Firecracker
slot in per-project for free there, under EITHER option. Applied to `wheeld` as it exists today
(Option A, the one container), either technology would wrap the SINGLE `wheeld` container and
raise the cost of a VPS-host escape — it would do nothing for project-to-project reach, for the
same reason §§1–2/5–6 don't: there is no per-project boundary inside that container for a stronger
runtime to reinforce. Getting project-to-project isolation from either technology on `wheeld`
specifically requires Option B first (a sandbox per project) — the runtime choice below then
applies to EACH of those, which is exactly where the startup-latency number in this table starts
compounding: N parked agents across N projects each paying gVisor/Firecracker's per-sandbox resume
cost independently, not once.

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
| **Docker/wheel-host integration** | Drop-in OCI runtime: `runsc` binary on the host, `--runtime=runsc` (or the per-container `HostConfig.Runtime` field `bollard` already exposes) — no change to `DockerSandbox`'s architecture, one field | Not a drop-in `docker run` flag on its own. Needs either Kata Containers (itself installable as a `--runtime=kata` OCI runtime, comparable integration effort to gVisor if going this route) or a dedicated `firecracker-containerd` stack, which is a different, heavier control plane than the `bollard`-driven `dockerd` this project already runs against |
| **Host requirement** | Works without KVM at all (ptrace platform); KVM used only if present, as an optimization | **Requires KVM** (hardware virtualization exposed to the host). This is the practical blocker worth flagging loudly: `infra/vps/README.md` targets a generic VPS, and nested virtualization is NOT universally offered by VPS/cloud providers — some budget/shared-CPU tiers explicitly disable it. This has to be confirmed for whatever host Wheel actually deploys to before Firecracker is viable at all, independent of every other tradeoff here |
| **Published startup overhead** | Modest, additive to an ordinary container start — commonly cited as tens to low-hundreds of milliseconds beyond `runc`, since it is still "start a container," just under a stricter syscall filter | Firecracker's own published figure for booting a minimal guest is ~125ms — but that is kernel+init only; a guest that then needs to bring up Wheel's actual engine+harness environment inside it adds real time on top, likely comparable to or more than gVisor's overhead once the FULL resume path (not just "a VM exists") is counted |
| **Published memory overhead** | Roughly 10–25MB per sandbox for the Sentry process, on top of the workload's own usage | Firecracker's own design target is ~5MB per microVM for the hypervisor itself — but a full guest kernel + whatever the harness needs resident (not a shared host kernel's page cache) is the real comparison, and that is workload-dependent, not a fixed hypervisor number |
| **Steady-state overhead** | Syscall-heavy workloads (lots of small I/O) see the most cost — often cited as a meaningful hit on syscall-bound throughput; CPU-bound work (a `cargo build` mostly compiling) is much less affected, since compute itself isn't intercepted | Near-native once running — the guest has its own real kernel, so steady-state syscall cost is not the concern the way it is for gVisor; the cost is entirely at boot and in per-VM fixed memory |
| **Operational maturity for THIS shape of workload** | Used in production by GKE Sandbox / Cloud Run for exactly this "many short-lived, frequently-recycled sandboxes" shape | Used in production by AWS Lambda / Fargate for the same shape, but typically inside AWS's own bare-metal fleet with guaranteed KVM access — the VPS-generic deployment target is the less-proven case for it specifically |

**Reading this for Wheel:** gVisor is the cheaper integration (a runtime flag, no new control
plane, no host virtualization requirement) and its overhead profile fits a `cargo build`-shaped,
CPU-bound agent workload better than a syscall-throughput-bound one. Firecracker is the stronger
isolation guarantee (a real kernel boundary, not policy over a shared one) but carries a hard
infrastructure precondition (KVM) that has not been confirmed for Wheel's actual VPS target, and a
heavier integration lift than a runtime flag. Neither number above is measured against Wheel's own
resume path — that is the spike I'd recommend before committing to either, not a substitute for
picking one now.

**Not deciding this here — Morgan's call**, per the brief. If gVisor is the direction, the next
step is a `runsc` spike on `infra/vps/rehearse.sh`'s stack measuring actual resume latency. If
Firecracker/Kata, the KVM-availability question on the actual target VPS provider has to be
answered first, before any integration work.

## Summary — what's asked of whoever reads this next

- **The framing question, first**: is `wheeld` staying "one hardened container" (Option A) or
  becoming "a supervisor of per-project sandboxes" (Option B, converging on `wheel-host`'s existing
  `process` backend)? Everything else here is Option A — real, worth shipping, but it does not
  touch project-to-project reach, which is currently unbounded on any `wheeld` deployment with more
  than one tenant. This is the one decision that changes the shape of the rest of the work, not
  just its size.
- Items 1–3: code changes exist (this PR + #83). **Needs a `rehearse.sh` run with docker access**
  before merge — not yet done, called out explicitly above rather than assumed.
- Item 4: needs someone with production shell access to run the two `docker inspect`/`/proc`
  commands above against `wheel-wheeld-1` and record the actual result.
- Items 5–6: proposed, not implemented, with the specific breakage each would need to survive
  (volume ownership; `$CARGO_HOME` write behavior) named rather than hand-waved. Both are Option-A
  shaped — they harden the single container, not project-to-project reach within it.
- The real boundary: gVisor vs. Firecracker costed above, explicitly tied to which option they're
  wrapping — for Morgan to choose between (or defer). Sandbox escape prevention is paramount, but a
  decision this consequential and this expensive to reverse is his to make with the numbers and the
  A/B framing in front of him, not mine to preempt.
