# Plan: container-per-project for wheeld (an opt-in docker sandbox arm)

**Status:** plan for adversary review, before any code. **Why:** per-project containerization is P1
("fundamental for wheel", Morgan; AgentGrid #133 makes it a hard requirement). Chosen direction is
`docs/proposals/agentgrid-hosting-shape.md` option C. This plan tests the cheaper reframing PM asked for:
**one binary, one service** — `wheeld` gains `WHEEL_SANDBOX=docker` — instead of a separate api + host + Postgres compose.

## Verdict: feasible, and the smallest thing that meets the requirement

The seam is one function. `wheeld/src/lib.rs::build_host_state` builds `EmbeddedSandbox` and hands it to
`wheel_host::HostState { sandbox: Arc<dyn Sandbox>, .. }`; `wheel-host`'s `DockerSandbox` implements the same trait,
and `wheeld` already links `wheel-host`. Nothing above the trait changes (API, proxy, events bridge, reconcile,
local auth, operator token — all sandbox-agnostic). Verified blockers, each with its answer:

| What could block it | Verified state | Answer |
|---|---|---|
| **Names don't resolve.** `DockerSandbox::engine_base` is `http://wheel-p-<id>:7000` (container name). | `config.rs:222-228`, `docker.rs:308` | wheeld must itself run **in a container on the tenant network**. So docker mode is compose-only; the systemd/`install.sh` (native) path stays embedded. Say so in docs. |
| **`Host.sandbox` is the concrete `EmbeddedSandbox`**, used for `shutdown_all()`. | `wheeld/src/lib.rs:112-116`, `:70-72` | Make it `Arc<dyn Sandbox>` + an optional `shutdown_all` (no-op for docker). **Decision:** tenant containers *survive* a wheeld restart (`restart_policy: unless-stopped`, boot `reconcile_on_boot` already re-adopts them). This flips today's "nothing this daemon started may outlive it" — deliberate, and it is what makes updates cheap. |
| **The update lane** hooks `EmbeddedSandbox` (`with_update`). Engines are separate images in docker mode, so wheeld↔engine version skew becomes real. | `embedded.rs:57-60`, `auto-update.md` | M1: with `WHEEL_SANDBOX=docker`, **refuse the update lane at boot with a named reason.** Docker-mode upgrades are `compose pull && up` (M4). Not solved here; not hidden. |
| **Engine image.** `DockerSandbox` needs `ENGINE_IMAGE` on the daemon; `Dockerfile.wheeld` (agent toolchains + `wheeld`) and `Dockerfile.host` (same toolchains + `wheel-engine`/`wheel-host`) are two multi-GB near-duplicates. | both Dockerfiles | M4: **one image** — add `wheeld` to `Dockerfile.host` and a `WHEEL_ROLE=wheeld` arm to `entrypoint.sh`; compose runs it as wheeld and `ENGINE_IMAGE` = the same tag for tenants. One build, one pull. (M1 can use the two existing images.) |
| **Local auth / operator token / data dir.** | `supervise.rs` `composed_env`; `/data` holds keys, API sqlite, `host.db` | Unchanged. Only *project* data moves: from `/data/projects/<id>` to docker volume `wheel-p-<id>-data`. **No migration of existing embedded projects** — docker mode is for new deployments; documented. Backups must include docker volumes. |
| **`WHEEL_SANDBOX=docker` must never silently degrade.** | — | Explicit opt-in only. If the daemon/proxy is unreachable or the image is absent, **refuse to boot** — no fallback to embedded (a silent downgrade of isolation is exactly what `UidIsolation` was designed against). |
| **The docker socket in the same process as the internet-facing API** — `infra/docker-compose.yml` says this must never happen. | `docker-compose.yml:6-9` | wheeld never gets the real socket: it talks to the **M2 proxy** through `DOCKER_HOST`. Until M2, boot **refuses** a raw `/var/run/docker.sock` unless `WHEEL_ALLOW_RAW_DOCKER_SOCKET=1` (dev only, warned every boot). |
| **Tenant containers can reach wheeld's own listeners** on the shared network (API `:8080`, public routes). The host listener is `127.0.0.1:0`, so unreachable — good. | `lib.rs:217-224` | M3: DOCKER-USER rules drop tenant→wheeld and tenant→tenant. |
| Engine env carries `WHEEL_ENGINE_SECRET`/`WHEEL_VAULT_KEY`; `inspect_container` returns it. | `docker.rs:151-163` | M2: the proxy strips `Config.Env` from inspect responses. |

Not a blocker, checked: `bollard::Docker::connect_with_local_defaults` honours `DOCKER_HOST` (**verify in M1's test**,
it is the one thing I am asserting from memory); `wheel-host` config for the docker arm needs only `ENGINE_IMAGE`,
`DOCKER_NETWORK`, and `WHEEL_HOST_SECRET` (which `composed_env` already sets).

## Milestones — each its own small draft PR, adversary reviews this plan first

| # | PR | Scope | Done when |
|---|---|---|---|
| **M2** *(first, independent)* | `host: docker socket proxy` | Small standalone binary in `wheel-host` (or its own crate), listening on a unix socket, forwarding to the real one. Allowlist = the **seven calls the host makes** (`inspect/create/start/stop/remove` container, `create/remove` volume) on `wheel-p-<uuid>[-data]` names only. `create` body must equal today's golden request: engine image, `cap_drop ALL`, `no-new-privileges`, tenant network, exactly `wheel-p-<id>-data:/data`, memory/cpu/pids caps present; **deny-by-default on unknown `HostConfig` keys** (`Privileged`, `CapAdd`, `Devices`, `PidMode`, `UsernsMode`, `Binds`…). Strip `Config.Env` from inspect. | Test walks every bollard call `DockerSandbox` makes and asserts admission; a table of forbidden creates asserts refusal; mutation-checked (drop one check ⇒ a named test fails). |
| **M1** | `wheeld: opt-in docker sandbox` | `WHEEL_SANDBOX=docker`; `Arc<dyn Sandbox>` seam; refuse-to-boot rules above; update lane refused in docker mode; `DOCKER_HOST` honoured. | Test against the existing fake-daemon harness (`tests/sandbox_docker_fake.rs`) drives wheeld's `start_host` in docker mode end to end; embedded mode's tests untouched and green. Real-daemon run documented. |
| **M3** | `infra: tenant network + egress + isolation check` | Dedicated `wheel-tenants` network (wheeld + engines only); `DOCKER-USER` rules: allow wheeld→tenant:7000 and tenant→public internet, drop tenant→tenant, tenant→wheeld, tenant→RFC1918/link-local/VPS gateway. `infra/vps/verify-tenant-isolation.sh` run **from inside a tenant container**, adapted from `verify-network-isolation.sh`. | Script exits 0 on a real box and non-zero when a rule is removed (proved by removing it). |
| **M4** | `infra: production compose + docs` | `infra/vps/compose.docker.yml`: wheeld + proxy + Caddy (+ web); one image (see table); pull-not-compile; secrets; backup of `/data` **and** `wheel-p-*` volumes; upgrade procedure; explicit "no auto-update in docker mode". `docs/SETUP.md`/`infra/vps/README.md` updated. | `rehearse.sh`-style run brings up a fresh box config and passes M3's script. |
| **M5** | `measure: gVisor opt-in` | Measurement only first: `runsc` runtime, a cargo build and a Claude Code turn inside it vs runc (wall time, RSS). Then, if acceptable, a per-project opt-in field in the create body (proxy allowlists `Runtime`). | A number and a recommendation; code only if the number is acceptable. |

Order: **M2 → M1 → M3 → M4 → M5** (M2 and M1 can be developed in parallel; M1 must not be *deployed* before M2).

## Deliberately out of scope
- Per-node uid (037): SDK's proposal; docker mode fixes cross-*project* isolation, not agent-vs-agent inside one canvas.
- The Railway 048 migration and the limiter PR #140: both only matter if `wheel-host` is ever internet-reachable. Parked / lower priority.
- Migrating existing embedded projects into docker volumes.

## Ask of adversary
1. Is the body-validating allowlist enough at M2, or must the proxy also pin the *image digest*? (I lean: pin the tag the operator configured, refuse others.)
2. Tenant containers surviving a wheeld restart — any attack that becomes easier (stale secrets in env, orphaned engines after `destroy` races)?
3. M3: is dropping tenant→tenant at `DOCKER-USER` sufficient given Docker's own `POSTROUTING` handling and userland-proxy, or must the tenant network be `--internal` with an egress gateway?
