# Where to host many AgentGrid canvases — Railway vs wheeld vs VPS compose

**Status:** decision memo (draft). **Ask (Morgan, via PM):** whichever shape is most efficient and cheapest
for us as deployer; minimum overhead is P1, then escape-security, then operator UX. **Requirement:** one
container per project, many canvases. Every claim below was checked against code; where I could not check
(prices, Railway platform behaviour) it says so.

## Recommendation

**(C), on one VPS, with the Docker sandbox backend — and only after three things are built (below).** It is
the only shape that literally delivers container-per-project, it has the least fixed overhead, and it makes
finding 048 and the Railway project split moot. **(B) wheeld is the lightest but cannot host more than one
tenant — by construction, not by a missing feature.** **(A) Railway cannot deliver container-per-project at
all** (no docker daemon), costs the most to run and operate, and is where all of 048 lives.

If "one container per project" can relax to "one unprivileged uid per project", (A) or a VPS running the
*process* backend meets it — with 037 and 048 still open — and the choice becomes pure cost/ops.

## What the code actually does (verified)

| Claim | Evidence |
|---|---|
| **wheeld cannot use the docker backend.** It only ever builds `EmbeddedSandbox`; `build_sandbox`'s docker arm is unreachable from it. | `wheeld/src/lib.rs:244-250`, `embedded.rs` |
| **wheeld is single-tenant by design.** Every engine is a task in one process, every agent runs as wheeld's uid; its own boot warning says "never for a deployment serving anyone else". | `embedded.rs:1-9`, `wheel-core/src/host.rs` `SHARED_UID_WARNING` |
| **Docker backend = one container per project**, `cap_drop ALL`, `no-new-privileges`, memory/CPU/pids caps, no published ports. | `wheel-host/src/sandbox/docker.rs:117-145` |
| …but **it joins one shared docker network** (`DOCKER_NETWORK`, default `wheel`) — the same network Postgres and the API sit on in `infra/docker-compose.yml`. That is finding 048 again, by config. | `docker.rs:139`, `infra/docker-compose.yml` |
| **The docker backend needs the docker socket in the host container** — root-equivalent on the machine if the host is compromised. Railway has no daemon, so Railway can only ever run the *process* backend. | `infra/docker-compose.yml:6-9`, `docker.rs` |
| **Per-node uid (037/F007) is not implemented in either backend.** No `setuid`/`pre_exec` for agent children; all agents of one project share the engine's uid. Cross-*project* isolation holds (container, or uid + 0700 on process). | `docker.rs:123-131` (comment says so), `process.rs` |
| **Auto-update (#59) is wheeld-only**: its hook is carried by `EmbeddedSandbox`; standalone `wheel-host` has none. | `embedded.rs:57-60`, `docs/proposals/auto-update.md` |
| The API can run on **SQLite** (`STORE=sqlite://…`); `Dockerfile.api` builds the Postgres flavour. Postgres is not mandatory on a single box. | `wheel-api/Cargo.toml` features, `config.rs:246` |
| Cost driver: an engine idles at **~18 MB**, an idle agent process at **~162 MB** (measured, `wheeld-vs-yoke-gap.md`); agents park after 300 s by default. | doc figures, not re-measured here |

## Comparison

| | **A — Railway** (api ×2 + host + Postgres, + 048 split) | **B — VPS, wheeld** | **C — VPS, api + host + (sqlite/Postgres), docker backend** |
|---|---|---|---|
| Container per project | **No** — process backend, one shared container | **No** — one process | **Yes** |
| Multi-tenant safe today | Cross-project uid only; 048 open (host + Postgres reachable from tenants) | **No** (single user) | Cross-project yes; **network not yet** (shared docker network) |
| Cost, order of magnitude* | usage-billed RAM/CPU across 3–4 services + volume + 2nd project; likely the priciest per resident GB | one small box, ~$5–25/mo flat | one box, ~$10–50/mo flat (8 GiB covers ~100 mostly-parked canvases + ~30 running agents: 100×18 MB + 30×162 MB ≈ 6.7 GB) |
| Deploy | push to `main`, watch paths (works today) | `install.sh`/compose, **auto-update covers it** | `deploy.sh`-style compose; **no auto-update** (manual pull/build; compiling Rust on the box is the heavy step — prebuild images) |
| Backup | Railway volume/Postgres backups (platform feature; unverified) | you: `/data` + sqlite | you: `/data`, docker volumes, DB dump |
| Ops burden | highest: two Railway projects, cross-project public host domain, volume copy migration, its own limiter blocker | lowest | low–moderate: one box, plus docker hardening |
| Does the limiter / 048 work apply? | **Yes, both** (public host domain ⇒ edge-shared peer) | No (no separate host) | **Limiter: harmless/optional. 048: no Railway split — but its *intent* (tenants must not reach Postgres/API) needs a tenant docker network + egress rules** |

\*Prices are my estimates of typical list rates, not checked against current pricing — treat as ±2×.
The dominant cost in every column is concurrently *running* agents (162 MB each), which is shape-independent;
the shapes differ only in fixed overhead and per-service minimums, which is where Railway loses.

## What (C) still needs built, in order

1. **A tenant docker network** holding only host + engine containers, with Postgres/API on another, plus
   `DOCKER-USER` egress rules so tenant containers cannot reach the VPS's own bridge gateway/services. Config +
   an isolation check adapted from `infra/railway/verify-network-isolation.sh`. (Closes 048's intent.)
2. **A production compose** for api + host (+ Caddy, and Postgres or SQLite). `infra/vps/compose.yml` is
   wheeld-only and `infra/docker-compose.yml` is a dev stack with dev secrets. Pre-built images, not on-box compiles.
3. **A decision on the docker socket** — the host holding it is the escape hatch (`#86`): socket proxy limited to
   container create/start/stop/rm, or rootless docker / gVisor / sysbox. This is the security P2 item and the
   real cost of (C).
4. Later: **037** for agents that must not trust each other *inside* one canvas (SDK's proposal); auto-update for a
   non-wheeld deployment.

## Pre-scope: the docker-socket decision (build #3)

Two different threats hide in "the socket is dangerous", and they need different answers:
**(a) host compromised → root on the VPS and every tenant**, and **(b) a tenant escapes its container through
the shared kernel.** In the docker backend a tenant's agents are in a *different container* from the host, so
the host's bearer and socket are not reachable from an agent — (a) needs a bug in the host, not a sandbox
escape. Still worth closing, because the payoff is total.

What the host actually asks of the daemon (`docker.rs`, bollard): exactly **seven calls** — `inspect_container`,
`create_volume`, `create_container`, `start_container`, `stop_container`, `remove_container`, `remove_volume`.
Every name is derived from a uuid the API generated. That makes an allowlist unusually cheap to state.

| Option | Stops (a)? | Stops (b)? | Cost / complexity | Verdict |
|---|---|---|---|---|
| **Endpoint-only socket proxy** (e.g. tecnativa/docker-socket-proxy) | **No.** `POST /containers/create` stays allowed, and a create with `Privileged` or a `/` bind *is* the escape. It only removes `exec`, image build, swarm, etc. | No | ~0: one small container, config only | Not a decision — false comfort on its own |
| **Body-validating proxy** (~200 lines in this repo, over the unix-socket client `wheel-host` already depends on) allowing only those seven calls, on `wheel-p-<uuid>` names, and refusing any `create` whose body is not exactly today's: engine image, `cap_drop ALL`, `no-new-privileges`, tenant network, `wheel-p-<uuid>-data:/data` only, no `Privileged`/`CapAdd`/`Devices`/`PidMode`/`UsernsMode`/`Binds` outside that one, unknown `HostConfig` keys denied | **Yes** for the create-a-privileged-container route (the obvious one) | No | ~1–2 days incl. tests + adversary review; no runtime cost; the expected body already exists as the golden request in `tests/sandbox_docker_fake.rs` | **Recommended, required before real multi-tenant** |
| **Rootless docker** | Yes (daemon is unprivileged) | Partly (container root ≠ host root) | High: uidmap/subuid, `Delegate=yes` for cgroup limits, and on Ubuntu 24.04 the AppArmor unprivileged-userns restriction needs a profile change. **Conflicts with build #1**: no `DOCKER-USER` chain, so per-tenant egress filtering must be redone inside the rootless netns; slirp4netns/pasta adds network overhead | Skip — it trades away the egress control we need more |
| **gVisor (`runsc`) runtime** | No | **Yes, strongly** (user-space kernel; tenants stop touching the host kernel's syscall surface) | Install + register the runtime, set `runtime` in the create body (one line + config). **Real overhead:** syscall/filesystem-heavy work (cargo/pnpm builds — the Wheel-on-Wheel workload) is materially slower, and Claude Code/Codex under runsc is **unverified** | Do as an **opt-in per-project runtime after measuring**, not day one |
| **Sysbox** | Partly | Partly | Distro/kernel-specific; upstream maintenance status unclear to me | Skip |

**Recommendation:** ship (C) with the **body-validating proxy** as the socket boundary, and put the proxy's
allowlist under the same mutation-tested discipline as `policy.rs` (a test that walks every bollard call the host
makes and asserts the proxy admits it, and a set of forbidden creates it refuses). Treat gVisor as the answer to
(b) *if* Morgan's escape-security priority is raised above operator cost after we measure a build inside it;
until then (b) rests on `cap_drop ALL` + `no-new-privileges` + a dedicated tenant network, the same posture as
today's process backend but with a per-project mount/pid/network namespace on top. None of this needs building
until Morgan confirms the requirement.

## What I am NOT recommending, and why

- **Adding a docker arm to wheeld.** The API would then sit in the same process as a docker socket — exactly what
  `infra/docker-compose.yml` says must never happen (an internet-facing process must not see it).
- **Continuing the 048 migration now.** It is parked. The limiter fix (draft PR, in progress) stays worthwhile
  because it is correct for any host reachable through a shared proxy, and costs nothing on (C).

## Open questions for PM/Morgan

1. Is "one container per project" a hard AgentGrid requirement, or a proxy for "a tenant cannot see another
   tenant"? The latter is met by uid + 0700 today; the former rules out A and B.
2. Expected canvas count and how many run agents concurrently — this sizes the box and is the only thing that
   moves the cost meaningfully.
3. Is a single VPS an acceptable single point of failure? (Railway's host is one replica too; neither shape is HA.)
