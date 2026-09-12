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

**Three distinct bad outcomes, not one**, per ADVERSARY: escaping the container to reach the host is
the headline case, but reaching **another project's** data or process on the same machine without
ever leaving any container is itself bad and explicitly in scope — Morgan deprioritized per-NODE
isolation *within* a project (F007), not per-PROJECT isolation between them. **The third is not
hypothetical — it has already been caught happening, not just theorized about**: redteam finding 048
is PM's live measurement of `wheel-host` (the Railway multi-tenant deployment, the same
`docker.rs`/`process.rs` backends §"Shape 3" and item 3 already touch) reaching
`postgres.railway.internal:5432` and `wheel-api.railway.internal:8080` in plain TCP from inside the
sandbox — the private-network segmentation §5b promises is not the segmentation that is actually
deployed, and only credential secrecy stands between that reachability and real use. This document
does not fix 048 (it is a Railway-topology / `infra/railway/` fix, not a sandbox-mechanism one), but
whoever picks up items 4–6 or the gVisor/Kata decision should know it is a confirmed instance, not a
"could theoretically happen" — this proposal's own choice of mechanism does not make 048 worse or
better, since Shape 1's private network exposure and Shape 3's would share the same fix regardless of
which sandboxing technology wraps the container. The rest of this document has to answer all three,
and — the finding that reshapes everything below — **the first two are not the same question on this
deployment**, because of which container topology `wheeld` actually runs.

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
- **gVisor structurally does not apply, and adopting it would partially undo "native"** — this half
  of the original claim holds without qualification. gVisor needs an OCI runtime slot
  (`--runtime=runsc`) to invoke it at all; there is no container being created for it to attach to
  once `wheeld` is just `exec`'d directly by systemd. Wrapping `wheeld` in a gVisor sandbox to get
  this protection back means running it AS a container again, undoing most of what "go native" was
  for.
- **Firecracker is a genuinely different case — ADVERSARY's correction, not fully "does not apply."**
  "Native" and "no VM boundary" got merged into one claim above, and only one of them is actually
  forced. #67's real value isn't "no container" in the abstract — it is systemd's OWN mechanisms
  (`OOMPolicy=continue`, real cgroup resource limits, `wheeld` updating its own root-owned binary,
  `ufw` being authoritative), and none of that requires bare metal specifically — it requires systemd
  running AS PID 1 with real cgroups v2 underneath it. **A Firecracker microVM gives exactly that**:
  it boots a real guest kernel, so `#67`'s entire measured systemd directive set
  (`ProtectKernelModules`, `ProtectClock`, the empty `CapabilityBoundingSet=`, all of it) works
  unmodified inside the guest — from `wheeld`'s own perspective it IS native, because a real kernel
  is what "native" was ever asking for. **"Native-in-Firecracker"** keeps `#67`'s systemd wins AND
  gets Firecracker's escape-resistance argument applied to the whole thing, rather than the two being
  mutually exclusive. Contingent on the SAME KVM-availability question that already gates plain
  Firecracker (§"Recommendation" below) — this does not remove that blocker, it changes what's on the
  other side of it if KVM is present. **Not yet verified by anyone actually booting it** — this
  document can name the reconciliation, it cannot confirm systemd-as-PID-1 inside a Kata/Firecracker
  guest behaves identically to bare metal for every directive #67 measured; that needs a real boot,
  not a reading of either codebase.
- **The same reconciliation does NOT confidently extend to gVisor — named, not leaned on.** gVisor's
  Sentry is a partial userspace reimplementation of the syscall surface, and a full init system
  running its own service manager with real cgroups v2 device/resource controllers is much shakier
  compatibility ground than "run one containerized process," which is gVisor's actual design center.
  A real unknown, not a claim either way — someone would have to try booting `wheeld`-under-`systemd`
  inside `runsc` to know, and this document does not have that answer.

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

**Why Shapes 1/2's lack of project-to-project isolation is not a self-inflicted risk to wave away —
two independent reasons, confirmed rather than assumed, both pointing at Shape 3:**

1. **Confidentiality: `wheeld` is not single-tenant by construction, whatever a specific deployment's
   own conventions assume.** `WHEEL_SIGNUP=open` (`infra/vps/compose.yml`'s own comment: "lets anyone
   who reaches this server create an account and run agents on it") lets any number of distinct
   people sign up, and even the `closed` default lets the owner add more accounts via `POST
   /v1/auth/users` (`docs/API.md`). "Every project belongs to the same person" is a fact about how a
   *specific* deployment happens to be used, not a guarantee the codebase makes — project-to-project
   reach on a `WHEEL_SIGNUP=open` (or multi-account `closed`) `wheeld` is a genuine cross-TENANT
   confidentiality breach, not a self-inflicted one.
2. **Availability — a DIFFERENT axis, ADVERSARY's addition, independently confirmed** (`compose.yml`
   on this branch still has no `mem_limit`/`pids_limit`/`cpus` on `wheeld` at all — not this PR's
   scope to add, a fact worth stating next to the shape decision regardless). Under Shapes 1/2
   (however hardened the single container/process gets), one project's runaway workload — a
   memory-hungry `npm install`, a build storm, anything a legitimate coding-agent task can produce —
   can OOM or exhaust the WHOLE box, taking down every OTHER project's board simultaneously, not just
   the offending one. This is not fixed by `cap_drop`/AppArmor/a stronger runtime wrapping the single
   container either, for the same reason project-to-project confidentiality isn't: those all harden
   the SAME shared boundary every project sits inside equally. Only Shape 3 (real per-project
   processes/sandboxes) makes PER-PROJECT resource limits possible at all, not just per-project
   confidentiality — the two reasons point the same direction independently, and Morgan should have
   both in front of him when this decision gets made, not just the confidentiality one.

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
axis IF Shape 2 means bare metal — but that may not be forced (ADVERSARY's later correction, see
§"Shape 2" above).** They agree that Shape 1 as currently deployed is under-hardened (no resource
limits at all; #67's own §1 says so) and that both proposals' compose/systemd-level hardening are
real, cheap wins worth taking regardless of which shape wins. Read strictly — `wheeld` as a bare
`systemd`-managed process on the host's own kernel — promoting Shape 2 to the default is **a
downgrade on the escape-to-host axis specifically**: trading a real pid/network/mount namespace
boundary (Shape 1) for systemd directives that recover most of the filesystem story and, by #67's
OWN admission, none of the process-visibility or network-reachability story, while gaining real
resource limits Shape 1 lacks entirely. **"Native-in-Firecracker" — running #67's entire measured
systemd hardening pass as PID 1 inside a Firecracker guest kernel, rather than on the bare host — is
a real reconciliation path, not a forced choice, IF Firecracker clears the same KVM-availability
check and boot-validation this document already gates it on elsewhere.** If it works, `wheeld` keeps
every one of #67's systemd wins (`OOMPolicy=continue`, cgroup limits, `ufw` authority) AND gains a
genuine kernel-separation escape boundary, rather than trading one for the other. Not yet verified —
someone has to actually boot it — so this does not resolve the tension today, it names the shape a
resolution could take.

**If "sandbox escape prevention is paramount" is the standing priority, and native-in-Firecracker
turns out not to be viable (KVM absent, or systemd-as-PID-1 inside a guest does not actually behave
like bare metal for #67's directive set), Shape 2 becoming the default on bare metal is a decision
that trades away part of what that priority is asking for, in exchange for real gains on a different
axis (resource limits, operational clarity).** That may still be the right call — a systemd-confined
process with real resource limits and no runaway-agent blast radius is a legitimate, defensible
posture, and #67's own measurement discipline is exactly what this document has been asking for
throughout. But it is Morgan's tradeoff to make with the axis named, not one that should get decided
by which PR merges first while the other is mid-review, and not one that should be treated as forced
before native-in-Firecracker has actually been tried. **Recommend against merging either #67
(promoting Shape 2 to default) or a Shape-3 commitment until this specific question — does the
default deployment keep a namespace boundary, and is that boundary
worth more than #67's resource-limit and operational gains — has an explicit answer from Morgan.**

Shape 1's compose hardening (§§1–2 below) is a different decision from the shape question above: it
improves the CURRENT deployment without foreclosing anything, costs little, and shipped in this PR
regardless. That framing needs one addition below — Morgan's ruling changes what "regardless" means.

## Combining Shape 1 and Shape 3, per Morgan's ruling: try the converged fix first

Morgan's instruction, exactly: adopt Shape 1 in a way that ALSO fixes the cross-project data-leak
gap, if possible — assess whether the same hardening mechanism (once landed on) can be applied
PER-PROJECT instead of to the one shared `wheeld` container, converging Shape 1 and Shape 3 into one
effort rather than "harden the shared container now, isolate projects later." Try the combined fix
first; if genuinely prohibitive, say so plainly with the real cost difference rather than defaulting
to the easier answer.

**Assessed: this converges, and it converges more cleanly than "Shape 1 now, Shape 3 later" would
have suggested — because the two pieces of work were never actually independent.**

- **The container/VM-hardening decision (which mechanism, gVisor/Kata/Firecracker/plain
  `cap_drop`+AppArmor) and the per-project architecture change are the SAME decision, not two.**
  Whatever mechanism the KVM check and the three spikes above land on has to be applied to SOME
  sandbox boundary. Today that boundary is "the one shared `wheeld` container" (Shape 1 as scoped).
  Converging with Shape 3 means the boundary is instead "each project's own sandbox" — the exact same
  directives (`cap_drop: [ALL]`, `no-new-privileges`, AppArmor confirmation, or a stronger runtime)
  apply verbatim, just N times instead of once. **Nothing already shipped in this PR (items 1–2) is
  wasted by converging** — it is the validated template that gets applied per-project once `wheeld`
  spawns per-project sandboxes, not a separate phase to redo.
- **The per-project uid/spawn primitive (Shape 3's own cost, already assessed above as porting from
  `wheel-host`'s `process.rs`, not inventing) and the container/VM mechanism are complementary, not
  sequential either — and one of them may turn out to be REDUNDANT with the other, worth stating
  plainly.** `process.rs`'s uid-drop exists specifically because Railway's `process` backend has NO
  container boundary at all — uid is the ONLY isolation it has. If Shape 3's per-project sandboxes
  each get a REAL container/VM boundary (Docker, gVisor, Kata, or Firecracker), that boundary is
  already doing the separating between projects on its own, the way `wheel-host`'s existing
  `DockerSandbox` already does today for the cloud deployment — a per-project uid becomes
  defense-in-depth on top of an already-isolated boundary, not the load-bearing mechanism. Cheap to
  add given the primitive already exists and is tested; not the critical path either way.
  **Hard boundary on this claim, so it does not read as more solved than it is (ADVERSARY): this is
  "per-PROJECT uid differentiation becomes redundant [confirmed, for cross-project separation
  specifically]," not "uid-based isolation is no longer needed anywhere."** Once a project has its own
  container/VM, that sandbox still runs MULTIPLE agents/nodes inside it — the new outer boundary does
  nothing for the layer BETWEEN nodes in the same project, same reason Shape 1's hardening today does
  nothing for cross-project reach (same shared boundary, different scope, argued twice now for two
  different pairs). Without per-NODE differentiation inside that now-isolated project, F007's exact
  problem just moves down one level: "any agent in any project reads any other project's vault secret"
  (today) becomes "any agent within one project reads any sibling agent's vault secret, inside that
  project's isolated sandbox." Per-node isolation is Morgan's own already-deprioritized F007, not
  automatically answered by this convergence — this document does not change that ranking, only notes
  that the redundancy claim above is scoped to cross-project separation and should not be read wider.
- **The cleanest shape this convergence can take: `wheeld` stops being a distinct architecture from
  `wheel-host` and becomes a THIN configuration of it.** `wheeld` already proxies each project over
  its own unix socket, the exact pattern `wheel-host` uses (`crates/wheeld/src/embedded.rs`'s own doc
  comment, cited above) — the control-plane side already converged before this ruling. What's
  different today is only `EmbeddedSandbox` (`tokio::spawn` per project, no isolation boundary at
  all) versus `wheel-host`'s real `Sandbox` implementations (`DockerSandbox`, `process.rs`, and
  whichever `FirecrackerSandbox` the spikes above justify). Converging Shape 1 into Shape 3 means
  retiring `EmbeddedSandbox` in favor of `wheeld` driving ONE OF THOSE SAME `Sandbox` implementations
  — reusing `wheel-host`'s own crate rather than building a parallel one. This is a real, larger
  architecture change (retiring a sandbox backend, not tuning a compose file), but it is not new
  invention: every piece (the trait, the uid-drop primitive, the Docker backend, and — per the spikes
  above — potentially a Firecracker backend) already exists or is already scoped.

**Where this leaves the shape/mechanism decisions above, restated with the convergence folded in
rather than treated as a later phase:**

1. The KVM check and the three spikes (resume latency at the correct parking granularity, storage
   mechanism, per-project networking cost at scale) are now gating the CONVERGED effort directly, not
   a separate "Shape 3, eventually" track — their results decide what `wheeld`'s per-project sandbox
   actually looks like, which is the same question as "what does Shape 1's hardening apply to."
2. Items 1–2 (this PR, already shipped) remain correct and worth keeping merged as-is: they harden
   the CURRENT single-container deployment while the convergence work above is scoped and built, and
   the same directives carry forward into the per-project template once `EmbeddedSandbox` is retired
   — nothing here is thrown away by converging, restated because it is the part most likely to be
   misread as wasted effort if the convergence proceeds.
3. **Honest sizing, since "genuinely prohibitive" was the bar Morgan set for saying no:** this is NOT
   prohibitive — every primitive it needs already exists in this codebase (the `Sandbox` trait, the
   uid-drop code, at least one working backend) — but it is a real architecture change to `wheeld`
   (retiring `EmbeddedSandbox`), not a config change, and it should be scoped and sequenced as its own
   piece of work once the mechanism spikes land, not squeezed into this PR's remaining scope. The
   convergence is the target; this PR is the first, already-shipped step toward it, not a competing
   "harden the shared container instead" path that needs to be walked back.
4. **Confirmed not mutually exclusive (ADVERSARY): this isn't a Shape-2-style hard conflict, because
   Shape 1 and Shape 3 sit on different axes and stack.** Shape 1 hardens what the whole container can
   do to the host; Shape 3 adds a boundary INSIDE that container between projects. Not theoretical —
   `wheel-host`'s own container already runs both today (hardened at the Railway/host level, and
   internally spawning per-project sandboxes via `DockerSandbox`). Converging `wheeld` onto Shape 3 is
   converging it onto an architecture this codebase already runs successfully, not inventing a new
   combination and hoping it holds.
5. **The one concrete adjustment Shape 1 needs to make room for, not a blocker but worth landing
   correctly (ADVERSARY):** Shape 3's per-project `setuid`/`setgid` (§3's `process.rs` pattern) needs
   `wheeld` to hold `CAP_SETUID`/`CAP_SETGID` — dropping privilege to a project's uid is impossible
   without it. Item 2's `cap_drop: [ALL]`, nothing added back for `wheeld` is correct **today** (§ item
   2's table — `wheeld` never calls setuid/setgid yet) and should ship exactly as scoped; the one line
   this adds back is `wheeld`'s own row gaining `cap_add: [SETUID, SETGID]` **when, and only when, Shape
   3's actual spawn code lands**, with the same inline justification `docker.rs` now carries post-#83
   ("wheeld drops each per-project sandbox to its own uid, which needs exactly these two and nothing
   else"). That is the mirror image of the bug #83 fixed — a grant landing WITH real code behind it,
   not a grant sitting unused waiting for code that never showed up. No reason to sequence-block item 2
   on this; it is a one-line follow-up gated on Shape 3's spawn code existing, not a redesign.
6. **One interaction to verify, not assume, if userns-remap (item 5, still proposal-only) is EVER
   adopted alongside Shape 3 (ADVERSARY):** they are two different uid-remapping layers — userns-remap
   at the Docker/host level (the whole container's uid range remapped), Shape 3's internal
   `setuid`/`setgid` at `wheeld`'s own process level (project A → uid X, project B → uid Y, inside that
   remapped container). Stacking two remapping layers is exactly the shape of bug that produces an
   off-by-mapping error nobody notices until it accidentally grants MORE host privilege than intended,
   not less. Not asserting it breaks — there is no way to test the actual mapping arithmetic from this
   sandbox — but per this document's own "measured, not assumed" standard (§ items 2 and 4's live-run
   requirements), userns-remap and Shape 3 must not ship together without an explicit test proving what
   HOST uid a Shape-3-dropped-to-uid-Y process actually runs as once userns-remap is also active. This
   is a gate on combining item 5 with the convergence, not a reason to hold either back individually.

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
| `wheeld` | fixed uid 10001 (`docker/Dockerfile.wheeld` `USER 10001`) | Never calls setuid/setgid; the embedded sandbox backend runs every agent as **wheeld's own uid** too (`crates/wheeld/src/embedded.rs`'s doc comment: "the tenants are all the same person" — single-tenant by design, a stated boundary, not F007 scope) | none today — **will need `CAP_SETUID`/`CAP_SETGID` the moment Shape 3's per-project setuid lands (§ "Combining Shape 1 and Shape 3", point 5, ADVERSARY)**, added then with the same inline justification `docker.rs` carries post-#83, not before | — |
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

**One check missing from that list, added per ADVERSARY: headless Chromium (`playwright`/
`puppeteer`) inside `wheeld` with `cap_drop: [ALL]` applied.** #67's own systemd unit deliberately
left `RestrictNamespaces=` OFF specifically because it breaks `bwrap`/Chromium — which means browser
automation is a real, anticipated agent workload here, not a hypothetical one. Docker's `cap_drop`
plus its default seccomp profile is a DIFFERENT mechanism from systemd's namespace restriction, and
I cannot tell from reading alone whether Chromium's own sandbox (which commonly wants
`unshare(CLONE_NEWUSER)`, and falls back to `--no-sandbox` in constrained containers) survives zero
capabilities the way it needed that one systemd directive left alone. Exactly the class of claim
`harden-probe.sh` exists to measure rather than assume on the systemd side — this side needs the
same discipline: install and launch a headless Chromium (`npx playwright install chromium && node -e
"require('playwright').chromium.launch()"` or equivalent) as part of the `rehearse.sh` run above,
alongside the `git clone`/`npm install`/build checks already planned. **Not yet added to
`rehearse.sh` itself** — naming it here so it is not silently assumed to work.

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

**Second condition, added once Shape 3's convergence is in play (ADVERSARY):** userns-remap and
Shape 3's internal per-project `setuid`/`setgid` are two different uid-remapping layers stacked on
top of each other (host-level container remap, then `wheeld`'s own process-level per-project remap
inside it) — see "Combining Shape 1 and Shape 3", point 6. Do not enable both together without an
explicit test proving what HOST uid a Shape-3-dropped-to-uid-Y process actually runs as under
userns-remap; the two conditions are independent and both must hold before combining all three
(userns-remap + Shape 1's cap posture + Shape 3).

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

**Why Docker was ever in this document, and why the actual target is broader than "escape
resistance" alone (Morgan, directly):** Docker was suggested because it was the known solution, not
a preferred one — Morgan does not know what else fits and asked for that to be found out, not
assumed. The real requirement, at scale, is **micro-isolation per BOARD** (a project — matches the
per-project framing Shape 3 already uses, not per-agent or per-node), satisfying four concrete needs
together, not escape-prevention in isolation:

1. **Per-board isolation** — the property this whole section has been costing.
2. **Networking** — agents need real outbound access (`git`, `npm`, the Anthropic/OpenAI APIs, tool
   calls), and the control plane needs to stay reachable from `wheel-host` (today, a pathname unix
   socket per project — §"Shape 3" above).
3. **Persistent storage** — a board's `/data` (sqlite store, workspaces, vault ciphertext) has to
   survive the sandbox being torn down and recreated, because that is what a parked-agent resume
   already is at the process level, and a sandboxing layer that could not resume ITS OWN state on
   the next message would undo the entire cost model this document keeps returning to.
4. **Multiplayer** — PR #70 (`shared-projects`) lets more than one human hold a board open at once
   (admin/prompter/guest tiers, concurrent connections including the live events socket). This is
   mostly an application-layer property, not a sandbox-mechanism one — any of the options below
   serve concurrent TCP/unix-socket connections to the engine identically — but it is worth stating
   as a requirement explicitly, because it is one more confirmation that **per-board** (one sandbox
   serving every member of that board) is the right granularity, not per-connection or per-human.

Requirements 2 and 3 are where gVisor and Firecracker/Kata actually diverge in COST, not just in
escape-boundary strength, and neither was priced against them in this document's first draft —
correcting that here.

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
| **Integration into `wheel-host`** | Like Firecracker/Kata, not tied to Docker specifically — `runsc` is a general OCI runtime. Drop-in under today's Docker orchestration (`--runtime=runsc`, or the per-container `HostConfig.Runtime` field `bollard` already exposes), or equally usable via `containerd`/CRI-O directly if `wheel-host` moves off `bollard` for another reason | **THREE paths, not two, and the cheapest is not through Kata at all — ADVERSARY's finding, and it changes the costing.** (a) `--runtime=kata` under today's Docker orchestration, comparable integration effort to gVisor. (b) Bypass Docker/`bollard` entirely and drive `containerd` + Kata directly. (c) **`wheel-host`'s own `Sandbox` trait (`crates/wheel-host/src/sandbox/mod.rs`) is already backend-agnostic by design** — its own doc comment: "nothing above the host knows which backend is in use; that is the whole point of the trait" — and `process.rs` already proves a THIRD backend (raw child processes, no daemon, no OCI runtime at all, in production on Railway) works fine against it. A `FirecrackerSandbox` driving the `firecracker` binary's own process + unix-socket REST API directly, per project, is the SAME shape `process.rs` already uses for raw child-process lifecycle management — reusing this codebase's own existing abstraction rather than adopting Kata's shim-translation layer on top of it. Likely CHEAPER than (a)/(b), not the same cost with a different name; worth costing as its own line if Firecracker survives the KVM check, not folded into "comparable to Kata" |
| **Host requirement** | Works without KVM at all (ptrace platform); KVM used only if present, as an optimization | **Requires KVM** (hardware virtualization exposed to the host). This is the practical blocker worth flagging loudly: `infra/vps/README.md` targets a generic VPS, and nested virtualization is NOT universally offered by VPS/cloud providers — some budget/shared-CPU tiers explicitly disable it. This has to be confirmed for whatever host Wheel actually deploys to before Firecracker is viable at all, independent of every other tradeoff here |
| **Networking (requirement 2), AT SCALE — ADVERSARY's sharpening** | Free, effectively — a gVisor sandbox runs INSIDE the same container/`containerd` network setup that already exists; the outbound path, the veth pair, the bridge, the per-project unix socket for the control plane are all unchanged. gVisor intercepts syscalls, it does not replace the network stack's plumbing. Cost does not grow per project beyond what plain containers already cost | **Real per-VM infrastructure that SCALES LINEARLY with concurrent projects, not a one-time integration cost** — each microVM needs its own TAP device plus bridge/routing, N projects means N of them, live, at once. Kata automates the setup (CNI-compatible, production-proven) but does not remove the marginal per-VM resource cost; a bespoke `FirecrackerSandbox` (the direct-integration path costed above) would have to build that TAP/bridge lifecycle itself, losing Kata's automation entirely — so the "direct integration is cheaper" case from the prior revision holds for the ESCAPE MECHANISM specifically, not for networking, where going direct is MORE work, not less. Neither path is free at scale the way gVisor's is |
| **Persistent storage (requirement 3), AT SCALE — ADVERSARY's sharpening, then grounded in a real incident** | Free — a gVisor sandbox mounts the SAME Docker volumes/bind mounts a plain container would; nothing about resuming a parked board's `/data` changes, at any scale | **Not one storage decision for the whole `/data/projects/<id>/` tree — split by what's actually in it, because Wheel has already suffered a real production outage from exactly this class of filesystem incompatibility.** `crates/wheel-sqlite/src/lib.rs` documents it inline: WAL mode's index lives in a `-shm` file needing real shared-memory mmap/resize support; Railway's bind mount couldn't provide it ("disk I/O error … xShmMap"), and because journal mode persists in the file header, every reboot rediscovered WAL and retried the same failure — "which is what took production down at 12:31." The fix (`set_journal_mode`'s escalator: drain under an exclusive lock, fall back to a shared-memory-free rollback journal) is real and tested, but it is a RECOVERY path, not something to plan a new deployment's steady state around. **virtio-fs is FUSE-based passthrough, and FUSE's mmap/shared-memory semantics have historically been the exact weak point for database workloads** — same shape of problem as the Railway bind mount, different specific mechanism; recent `virtiofsd`/QEMU has improved this significantly, but "significantly improved" is not the same bar as "proven safe for WAL's `-shm` file," and this project has been burned once already by assuming a mount was POSIX-complete enough without checking. **Recommendation, not just a named risk: virtio-blk (a real local filesystem inside the guest) for wherever `wheel.db` lives — correctness is the deciding factor there, not performance; virtio-fs for the workspace tree specifically (git clones, build artifacts), where there is nothing WAL-shaped to worry about and the host-side-inspection/backup convenience is a real, uncomplicated win.** If virtio-fs ends up carrying the database path anyway for architectural-simplicity reasons, `WHEEL_SQLITE_JOURNAL=TRUNCATE` already exists as a deterministic escape hatch and should be set PROACTIVELY for that deployment rather than relying on the runtime escalator to recover after the fact — proven against the actual chosen mechanism with something like the existing `open_configured_recovers_on_a_volume_that_cannot_host_a_shm_file` test, not assumed safe by extrapolation from "virtiofsd has gotten better." Same discipline as the resume-latency spike, for the same reason: this project has already paid for the alternative once |
| **Boot latency AT PROJECT scale, not just per-turn — new dimension, ADVERSARY** | A gVisor sandbox starts about as fast as an ordinary container; nothing changes if project-level idle-parking (below) becomes real | **If Wheel's cost-frugality argument (§3c#14, "one live process forever is what made YOKE unusable") extends one level up — parking whole PROJECT sandboxes when idle, the same way individual agents already park — Firecracker's boot cost is paid on every RESUME, not once at cold-start.** Whether that extension is needed depends on scale this document does not know: keeping every project's sandbox resident 24/7 is fine at a handful of projects and is exactly the "one live process forever" cost problem at hundreds. If project-level parking is real, ~125ms (kernel+init alone, before the engine/harness inside is answering) is paid on every wake across every mostly-idle project — a materially different frequency than "once when a project is created." Not costed here because whether project-level parking is even planned is not decided; flagged because the escape-mechanism choice and the parking-granularity choice are coupled and neither was stated as depending on the other before this |
| **Published startup overhead** | Modest, additive to an ordinary container start — commonly cited as tens to low-hundreds of milliseconds beyond `runc`, since it is still "start a container," just under a stricter syscall filter | Firecracker's own published figure for booting a minimal guest is ~125ms — but that is kernel+init only; a guest that then needs to bring up Wheel's actual engine+harness environment inside it adds real time on top, likely comparable to or more than gVisor's overhead once the FULL resume path (not just "a VM exists") is counted |
| **Published memory overhead, AT SCALE — ADVERSARY's correction to the first draft's framing** | Roughly 10–25MB per sandbox for the Sentry process, on top of the workload's own usage. The Sentry shares far more with the host than a guest kernel does — most of the memory story is the workload's, not the sandbox mechanism's | **The ~5MB figure is the hypervisor alone, not the real per-project cost — citing it without qualification (the first draft's error) undersells Firecracker's actual footprint.** A FULL guest kernel resident per project — not a shared host kernel's page cache, a genuinely separate one — is the real comparison, and at true multi-tenant scale (the scale this section is now explicitly about) that is a bigger number than gVisor's Sentry, which shares far more with the host by design. Workload-dependent either way, but the honest baseline for Firecracker is "hypervisor + guest kernel + harness," not "hypervisor alone" |
| **Steady-state overhead** | Syscall-heavy workloads (lots of small I/O) see the most cost — often cited as a meaningful hit on syscall-bound throughput; CPU-bound work (a `cargo build` mostly compiling) is much less affected, since compute itself isn't intercepted | Near-native once running — the guest has its own real kernel, so steady-state syscall cost is not the concern the way it is for gVisor; the cost is entirely at boot and in per-VM fixed memory |
| **Operational maturity for THIS shape of workload** | Used in production by GKE Sandbox / Cloud Run for exactly this "many short-lived, frequently-recycled sandboxes" shape | Used in production by AWS Lambda / Fargate for the same shape, but typically inside AWS's own bare-metal fleet with guaranteed KVM access — the VPS-generic deployment target is the less-proven case for it specifically |

**Where this landed, honestly, after ADVERSARY's own follow-up walked their earlier lean back: the
full four-requirement picture does NOT resolve cleanly, and saying otherwise would be false
confidence, not the plain recommendation Morgan asked for.** Morgan's instruction was to say plainly
if the numbers favor a non-Docker mechanism rather than hedge toward the familiar one — that holds,
and is followed below — but it is not an instruction to assert more certainty than two rounds of
adversarial review actually support. What is genuinely settled, and what is not:

**Settled, WITH three qualifications ADVERSARY added by pressure-testing their own earlier phrasing
rather than re-confirming it — "categorically stronger" does not survive unqualified, and is
corrected here before it can be quoted back later as "we solved sandbox escape":**

- gVisor still runs the untrusted workload on the HOST's own kernel: `runsc` intercepts syscalls in
  userspace, but the interception layer is a large, complex reimplementation of a huge Linux syscall
  surface, and a bug in it is still a path to the real host kernel — not hypothetical, gVisor has
  real sandbox-escape CVEs in its own history, precisely BECAUSE reimplementing that much of the
  kernel IS a large attack surface. Removing that specific class of bug — the syscall-emulation layer
  itself as attack surface — for a workload that never talks to a syscall emulator at all is a real,
  structural difference, and the right reason to prefer a VM boundary for a "run attacker-chosen
  code" threat model. That much holds.
- **But the correct phrase is "removes gVisor's specific attack surface, at the cost of a different
  one that is smaller and more scrutinized but not risk-free" — not "categorically stronger" standing
  alone, for three reasons:**
  1. **Raw Firecracker and Kata Containers are not the same trusted computing base.** Kata adds
     `containerd-shim-kata-v2` plus Kata's own in-guest agent between the attacker's process and the
     hypervisor — real extra surface the pure-Firecracker security argument does not account for.
     Whichever integration path (§ above) ends up costed, the strength claim is for THAT combination,
     not for "Firecracker" as an abstraction — a raw `FirecrackerSandbox` against `wheel-host`'s own
     trait is closer to the pure argument than going through Kata is.
  2. **Firecracker's boundary is KVM's boundary, and KVM is not a zero-risk dependency.**
     Firecracker's own code is small with a clean record, but the real TCB includes the host kernel's
     KVM subsystem and the CPU's virtualization extensions underneath it — KVM has had real
     escape-class CVEs (nested-virtualization bugs, device-emulation issues in QEMU-class
     components). "Hardware boundary" is a smaller, more scrutinized, structurally DIFFERENT attack
     surface than gVisor's userspace emulation — not a claim that the risk goes to zero.
  3. **Neither option addresses side-channel attacks on its own, and this cuts hardest against
     Firecracker specifically because it is the one being sold as the stronger boundary.**
     Spectre-class attacks (cache timing, branch-predictor state) exploit shared microarchitectural
     state on the same physical CPU and cross a software isolation boundary regardless of whether
     that boundary is a container, a gVisor sandbox, or a VM — unless the deployment ALSO does
     CPU-level isolation (core pinning, disabling SMT/hyperthreading between tenants). If that
     operational work is not budgeted as part of choosing Firecracker, a Firecracker VM sharing a
     hyperthread sibling with another tenant's VM is not meaningfully safer from a cache-timing
     attack than two gVisor sandboxes would be from each other. Not costed anywhere in this document;
     named here so the recommendation below does not imply it is solved.
- The workload profile (syscall-heavy `git`/`npm`/`cargo`) independently favors Firecracker's
  near-native steady-state over gVisor's worst-fit overhead case — undisputed, and not affected by
  the three qualifications above, which are about the security claim specifically.

**NOT settled — ADVERSARY's own correction, taken at face value rather than defended against:** once
per-board scale, real networking, persistent storage and multiplayer are weighed TOGETHER rather than
escape-resistance in isolation, the case complicates rather than resolves:

- **Networking scales linearly with concurrent projects for Firecracker** (a TAP device + bridge per
  microVM, live, at once) and is free for gVisor (inherited container plumbing) — not costed at all
  in the first draft, and a real operational difference at genuine scale, not a one-time integration
  line.
- **Persistent storage multiplies the same way** — real provisioning/growth/backup surface for
  Firecracker per board, versus gVisor's free inherited volumes — and it is not even ONE mechanism
  decision: grounded in a real production incident (§ table above, Wheel's own `-shm`/WAL outage on a
  Railway bind mount), the honest split is virtio-blk for wherever `wheel.db` lives (correctness) and
  virtio-fs for the workspace tree (convenience, nothing WAL-shaped at risk there).
- **Boot latency may be paid far more often than "once per project" if cost-frugality (§3c#14's own
  argument against keeping idle processes resident forever) extends to project-level parking at real
  scale** — a dimension the first draft never named as coupled to the mechanism choice at all.
- **The published memory comparison undersold Firecracker's real footprint** — ~5MB is the hypervisor
  alone; a full guest kernel resident per project, at genuine multi-tenant scale, is the honest
  number, and it is bigger than gVisor's Sentry, which shares far more with the host.

None of this reverses the escape-resistance argument. It means "structurally stronger boundary" and
"practical to operate at per-board scale, with networking, persistent storage, and multiplayer" are
different questions, the second is not yet answered, and a defensible recommendation needs it
answered — not asserted past what either agent's analysis currently supports.

**What this means for the order of work (ADVERSARY's process point still stands, strengthened by the
walk-back rather than undercut by it):**

1. **Immediately, ahead of anything else in this document, including items 1–3's already-shipped
   Docker polish: confirm KVM availability on whichever host is the actual deployment target** —
   `ls /dev/kvm` / `kvm-ok`, one cheap check. If disabled, gVisor is the only real option, full stop,
   and nothing below this line needs doing at all. This is now an even higher-value first step than
   the earlier draft gave it credit for: if KVM is absent, the entire networking/storage/latency-
   at-scale analysis below is moot before it starts, for free.
2. **If KVM is available, the recommendation is NOT yet "target Firecracker" — it is "run the
   requirement-grounded spikes below, because the escape-resistance case alone is not sufficient
   given the fuller requirement set."** Three spikes, not the earlier draft's two, and none
   optional given what ADVERSARY's follow-up surfaced:
   - **Resume latency**, measured at the granularity project-level parking would actually use (not
     assumed to be "once per project" — that assumption itself needs to be confirmed or refuted
     first, since it changes how often the latency cost below is even paid).
   - **Storage mechanism** — not "virtio-fs vs. virtio-blk" as a single either/or, but confirming the
     split-by-content-type call above (virtio-blk for `wheel.db`, virtio-fs for the workspace tree)
     against Wheel's real sqlite-plus-workspace access pattern, including what it costs to
     provision/grow/back up at N-project scale for whichever mix that turns out to be.
   - **Networking cost at scale** — TAP/bridge resource cost per concurrent project, not just "does
     it work," since ADVERSARY's point is specifically that this is a marginal, multiplying cost, not
     a fixed integration one.
3. **The integration path (Kata vs. a direct `FirecrackerSandbox` against `wheel-host`'s own
   backend-agnostic trait) is a fourth, separate decision, and it INTERACTS with the networking/
   storage spikes above rather than sitting beside them** — going direct is likely cheaper for the
   escape mechanism itself (reuses `process.rs`'s existing child-process-lifecycle shape) but loses
   Kata's automatic CNI-compatible networking and its built-in virtio-fs/virtio-blk device wiring
   (mounting each backend to the right path, per the split-by-content-type call above), meaning
   `wheel-host` would have to build that lifecycle itself. Which integration path is cheaper OVERALL
   depends on the networking/storage spikes' results, not on the escape-mechanism cost alone.
4. **gVisor is not "the fallback if Firecracker is ruled out" — it is the CURRENT honest baseline
   recommendation until the three spikes above actually run**, given it already satisfies all four
   requirements today at effectively no marginal cost, and Firecracker's advantage is proven only on
   one of the four. Escape-resistance alone does not outweigh three unanswered operational questions
   at genuine scale — that would be exactly the "structurally stronger boundary" vs. "practical to
   operate" conflation ADVERSARY named. If the spikes come back favorable, Firecracker is very likely
   the right target given §"Settled" above; until they do, recommending it outright would be the
   overconfidence Morgan's instruction asked this document to avoid in the OTHER direction.

This recommendation does not apply to Shape 2 at all — native systemd structurally cannot use either
mechanism (above), which is itself one more point in the tension Morgan should weigh when deciding
whether Shape 2 becomes the default.

## Summary — what's asked of whoever reads this next

- **Morgan's ruling, first, since it reframes everything below: converge Shape 1 into Shape 3 rather
  than sequence them.** Assessed and NOT genuinely prohibitive — every primitive the converged effort
  needs already exists in this codebase (`wheel-host`'s `Sandbox` trait, its uid-drop primitive, its
  Docker backend). The container/VM-hardening mechanism decided by the KVM check + three spikes below
  and the per-project architecture change are the same decision, not two phases: whatever mechanism
  wins gets applied per-project instead of to the one shared container, and items 1–2 (already
  shipped) are the validated template for that, not wasted work. The real, honestly-stated cost: this
  is a genuine architecture change to `wheeld` (retiring `EmbeddedSandbox` for one of `wheel-host`'s
  real `Sandbox` implementations), not a config change — sized as its own piece of work once the
  mechanism spikes land, not squeezed into this PR. See § "Combining Shape 1 and Shape 3" above for
  the full assessment.
- **The shape question, and it now has three answers instead of two**: Shape 1 (`wheeld` in
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
- Items 1–3 (Shape 1's compose hardening): code changes exist (this PR + #83). Proceeding now, per PM's
  instruction not to pause them — reframed by the ruling above as the validated per-project template
  for the converged effort, not a separate track that could later turn out to have been wasted work.
  **Needs a `rehearse.sh` run with
  docker access** before merge — not yet done, called out explicitly above rather than assumed —
  **including a headless-Chromium launch under `cap_drop: [ALL]`** (ADVERSARY: browser automation is
  a real, anticipated agent workload per #67's own `RestrictNamespaces=` exception for exactly this),
  not just `git clone`/`npm install`/build.
- Item 4: needs someone with production shell access to run the two `docker inspect`/`/proc`
  commands above against `wheel-wheeld-1` and record the actual result.
- Items 5–6: proposed, not implemented, with the specific breakage each would need to survive
  (volume ownership; `$CARGO_HOME` write behavior) named rather than hand-waved. Both are Shape-1
  scoped — they harden the single container, not project-to-project reach within it, and (userns-
  remap specifically) would need re-deriving for Shape 2, where there is no container to remap.
- The real boundary: gVisor vs. Firecracker, costed against the actual target — per-BOARD
  micro-isolation plus networking, persistent storage and multiplayer (#70) together, not
  escape-resistance alone — applies to Shapes 1 and 3, structurally does not apply to Shape 2.
  **Honest state after two more rounds of adversarial review, including ADVERSARY walking their own
  earlier lean back on the fuller picture AND then pressure-testing their own "categorically
  stronger" phrasing: escape-resistance settles in Firecracker's favor on its own terms (removes
  gVisor's specific syscall-emulation attack surface, which has real escape CVEs of its own) —
  qualified three ways, not unqualified (§ above): Kata vs. raw Firecracker are different TCBs; KVM
  itself is a smaller, more scrutinized, not-risk-free attack surface, not "solved"; and neither
  option addresses CPU-level side-channel attacks without separately budgeted core-pinning/SMT
  isolation. The full requirement set does NOT resolve cleanly either, and this document should not
  claim more certainty than either finding supports.** Networking and persistent storage scale LINEARLY with concurrent projects for
  Firecracker (real per-VM infrastructure) and are free for gVisor (inherited from the container);
  whether project-level idle-parking is needed at real scale — which would make Firecracker's boot
  cost a per-resume tax rather than a one-time cost — is not decided; and the published ~5MB
  hypervisor figure undersold Firecracker's real footprint (a full guest kernel resident per project
  is the honest number). **Current baseline recommendation: gVisor, because it already satisfies all
  four requirements today at effectively no marginal cost, and Firecracker's advantage is proven on
  only one of the four so far.** Confirm KVM availability on the real target host FIRST regardless
  (moots everything else immediately if absent); if present, three spikes — resume latency at the
  correct parking granularity, confirming the split-by-content-type storage call (virtio-blk for
  `wheel.db`, virtio-fs for the workspace tree — grounded in a real production incident, § table
  above) at N-project scale, and per-project networking cost at scale — decide whether Firecracker's
  escape-resistance case is worth its
  operational cost. The integration path (Kata vs. a direct `FirecrackerSandbox` against
  `wheel-host`'s own backend-agnostic `Sandbox` trait, which `process.rs` already proves works for a
  non-OCI backend in production) is a further, coupled decision once Firecracker clears those spikes,
  not a default settled here.
