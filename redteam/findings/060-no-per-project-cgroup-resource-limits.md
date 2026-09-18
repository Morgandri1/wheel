# 060 — No per-project cgroup limits: one project's agent can OOM/CPU-starve the whole host, no escape required

- **Severity:** High. Distinct threat axis from sandbox-escape (#86/F007/037): this needs **zero**
  privilege escalation, container escape, or malicious intent — a single agent doing ordinary,
  legitimate, in-sandbox work (a large in-memory dataset, a runaway build, a tight loop) degrades
  or takes down **every other project on the same host**, not just its own. Live-verified, not
  reasoned: current `wheeld` container has `memory.max=max` (unlimited) and `cpu.max=max 100000`
  (unlimited) in its actual, running cgroup.
- **Owner:** API (`infra/vps/compose.yml`, `crates/wheel-host` sandbox spawn — wherever cgroup
  limits are set per project/container) + SDK if the `process` backend (Railway) needs the
  equivalent via `rlimit`/cgroup delegation.
- **Status:** OPEN. Discovered while investigating #86 (sandbox-escape hardening) from inside a
  running `wheeld` container (PM, with Morgan's authorization, self-only recon — see the same
  session's exchange). I flagged the theoretical gap; PM ran the confirming check.

## The gap, stated precisely
`#86`'s stated threat model is escape prevention: "attacker already reaches agent-runs-code via
`/p/` ingress... the open question is blast radius beyond that RCE (container/VM escape to host, or
to other projects on the same box)." gVisor, cap_drop, AppArmor, and read-only rootfs all answer
"can a compromised agent break OUT of its container." None of them answer "can an agent — compromised
or not — exhaust a shared resource that isn't namespaced at all." Memory and CPU are host-wide
resources; without a cgroup ceiling, one container's consumption is not contained by ANY of #86's
planned mechanisms, however strong — gVisor makes syscalls safer, it does not give a container less
RAM to allocate within.

## Live-verified facts (PM's recon, from inside the actual running container)
- `memory.max` = `max` — no cap. A single agent process allocating aggressively (a legitimate large
  dataset, a memory leak in agent-invoked tooling, or a deliberately hostile payload if #86's escape
  boundary is ever bypassed anyway) can exhaust host RAM, triggering the kernel OOM killer against
  **arbitrary processes on the host**, not necessarily the offending one — every other project's
  `wheeld`/engine/agent processes are fair game once the host is under memory pressure.
- `cpu.max` = `max 100000` — no cap. Unbounded CPU consumption starves every other project's agents
  of scheduler time on the same host.
- `pids.max` = `4600` — this ONE axis **is** capped, so a classic fork-bomb is bounded. Partial
  mitigation only: it stops one specific attack shape, not the resource class.

## Why this is worth its own finding rather than folding into #86
#86 as scoped is a mechanism comparison for escape resistance (Docker-hardening vs gVisor vs
Firecracker/Kata). Every one of those options still needs cgroup limits set independently — gVisor
containers run inside a cgroup exactly the same as a plain Docker container; picking gVisor over
Docker+caps does not create memory/CPU limits as a side effect. Folding this into #86 risks it being
implicitly "solved" once #86 ships, when it needs its own explicit line item regardless of which
escape-prevention mechanism wins.

## Recommendation
- Set `memory.max`/`memory.high` and `cpu.max` per project container at the same layer that already
  should be setting other Docker resource controls (`docker run --memory`/`--cpus`, or the compose
  equivalent — `deploy.resources.limits` under the `docker compose` v2 schema, or an explicit
  `HostConfig.Memory`/`NanoCpus` in `wheel-host`'s `bollard` calls if that's how containers are
  actually spawned). A conservative per-project default (with a documented, operator-adjustable
  override) closes the immediate gap; exact sizing is a product decision, not a security one — the
  finding is "zero" is wrong, not "what the number should be."
- For the `process` sandbox backend (Railway, §5b): the equivalent is a cgroup created per project
  uid (or `setrlimit`, though `RLIMIT_AS`/`RLIMIT_CPU` are weaker and easier for a process to evade
  than a real cgroup) — `crates/wheel-host/src/sandbox/process.rs` already does per-project uid
  privilege dropping and rlimits (nproc/fsize/nofile per the contract's M1.5 section); confirm
  whether memory/CPU are in that existing rlimit set or need adding.
- `pids.max` being already-capped is worth keeping as a reference implementation: whatever sets it
  today is the right place to add the other two ceilings alongside it.

## What would change my mind
If there's a host-level mechanism I haven't found that caps memory/CPU per project at a layer
outside the container's own cgroup (e.g., a cAdvisor-style external enforcer, or the Railway
platform itself already enforcing a per-service ceiling regardless of what the container's own
cgroup reports), this would be lower severity or a documentation-only fix (state that reliance is
intentional). I did not find any such thing referenced in the contract, `infra/`, or PROTOCOL.md —
the compose file's `wheeld` service has no `deploy.resources` block at all as of this finding, so I
have no reason to believe an external layer is doing this instead.
