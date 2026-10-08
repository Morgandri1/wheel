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

Order: **M2 → M1 → M3 → M4 → M5** (M2 and M1 can be developed in parallel; M1 must not be *deployed* before M2 **and M3** — see the revisions).

## Revisions after adversary's review of this plan (2026-09-19)

Adversary's full review is on #141. Accepted, and where each lands:

**Changed in the plan**
- **Engine channel: unix socket, wheeld OFF the tenant network (M3 topology change).** Tenant containers get an
  egress-only network with no wheeld and no peers; each engine listens on `unix://` in a per-project subpath of one
  shared socket volume (`Mounts` + `VolumeOptions.Subpath`, Docker >= 26; wheeld creates `<uuid>/` first and mounts the
  whole volume). That removes tenant->tenant, tenant->wheeld and the `WHEEL_TRUSTED_PROXIES`/`proxy_header` (#136)
  exposure by construction instead of by firewall rule, and dissolves the `enable_icc=false` asymmetry. **M1 stays on
  TCP-by-name and is therefore dev/test-only until M3 lands** — not deployable before M2 *and* M3. M3's firewall work
  shrinks to: tenants may reach global unicast only (allow-list, v6 disabled or mirrored), an `INPUT` rule on the tenant
  bridge for host services (DOCKER-USER sees only forwarded traffic), `bridge.name` pinned.
- **M2 is stricter than first written** (below). **M1 gains the survive-restart correctness work** (below).
- **Boot guards are behavioural, not string matches.** `DOCKER_HOST` is resolved as bollard resolves it (unset =>
  `/var/run/docker.sock`); the socket must answer the proxy's identity endpoint AND refuse a call a real daemon answers
  (`GET /version` => 403). Not a unix socket => refuse (it cannot be identity-checked). The raw-socket dev flag also
  requires `WHEEL_ENV=dev`. `WHEEL_SANDBOX` and `SANDBOX_BACKEND` must not disagree.

**M2 additions**
- The proxy never forwards client bytes: it validates, then **builds a fresh body from the checked values** and rebuilds the
  request target from parsed components. Unknown/case-variant keys are refused (exact-case allowlist), so Go's
  case-insensitive decoding and serde cannot disagree about what was sent.
- Values must EQUAL the configured ones (limits included; the proxy and host get the same `CONTAINER_*`), not merely be
  present. Volume create: no `DriverOpts` (already enforced), local driver, exact name and label.
- Inspect is **projected**, not stripped: only `State.Status`, `State.Health.Status` and the `wheel.spec` label leave.
- An eighth call, `GET /containers/json` filtered to `label=wheel.project`, projected to name/labels/state, for orphan GC.
- Refusal tests cover the fields adversary listed (`NetworkingConfig`, `PidMode`/`IpcMode`/`UTSMode`, `Mounts`,
  `VolumesFrom`, `MaskedPaths`, `OomScoreAdj`, `LogConfig`, `Sysctls`, `Tmpfs`, ...) and case/duplicate/fold-variant keys.

**M1 additions (correctness of "containers survive a restart")**
- **`wheel.spec` label = hash of the full create config incl. env; `provision`/`start` recreate (stop, remove, keep volume)
  on mismatch** — today a rotated secret, a changed `WHEEL_HARNESS_AUTH` or a new image never reach a surviving container.
- Remove with `v=true` (no anonymous volumes holding tenant data after "delete").
- **Sandbox kind is sticky**: `wheeld` records it beside `host.db` and refuses a mismatch (embedded projects present =>
  never boot docker mode as if they did not exist).
- Reconcile also stops containers whose record says stopped/absent, and garbage-collects `wheel.project`-labelled orphans.

**Hardening carried forward from adversary's approval of M2 (non-blocking for M2; land in M1/M3/M4)**
- `MemorySwap` must equal `Memory` in the golden create (otherwise a container gets twice its memory in swap) — proxy and
  `DockerSandbox` together (M1).
- A ceiling on project containers and volumes per host (a compromised wheeld could otherwise create N at the per-container cap and
  OOM the box or fill the disk) — enforced by the proxy counting `wheel.project`-labelled objects (M3).
- `daemon.json`: `log-driver: local` with `max-size`/`max-file`; `/var/lib/docker` on its own filesystem (the proxy can't set a
  volume quota); `userns-remap` (M4 host preparation checklist).
- `umask` set before the proxy binds its socket (M1, with the mode set explicitly after).
- **M4 rehearsal asserts** `docker inspect wheeld` shows **no** `docker.sock` mount, no `Privileged`, no host network, and that
  the proxy container is on no network wheeld or tenants share (unix socket only).

**Not adopted:** digest-pinning as the image control (exact string equality from the proxy's own env is the control; use
`image@sha256:` in the production compose for reproducibility). `--internal` + egress gateway: a later, separate decision.

## M3 design delta — for adversary review BEFORE code

Adversary's topology change is accepted (PM ruling). This is what it means concretely; nothing below is built.

**Topology.** Two networks and no path between wheeld and a tenant:
- `wheel-front` — Caddy and wheeld. Never attached to a tenant.
- `wheel-tenants` — tenant containers only. A user-defined bridge named `wheel-tenants`
  (`com.docker.network.bridge.name`), `enable_icc=false`, masquerade on (they need the internet), **no IPv6** (not enabled on the
  network or daemon).
- Engines listen on a **unix socket**: `WHEEL_LISTEN=unix:///run/wheel/engine.sock`. One named volume `wheel-sockets` is mounted
  in wheeld at `/run/wheel-engines` and into each engine at `/run/wheel` as a `Mounts` entry with
  `VolumeOptions.Subpath = <project uuid>` — an engine sees only its own directory. wheeld makes `<uuid>/` (0700) before the create.
  `engine_base` becomes `unix:///run/wheel-engines/<uuid>/engine.sock`, which the host's proxy already speaks (the process
  backend's transport, `proxy.rs`). `await_healthy` probes the socket; the image `HEALTHCHECK` (today `curl 127.0.0.1:7000`)
  becomes `curl --unix-socket /run/wheel/engine.sock http://localhost/healthz`.
- Requires Docker >= 26 (`Subpath`). wheeld cannot check the daemon version through the proxy (`/version` is refused by design),
  so M4's preflight checks it and a create failure names the requirement.

**Proxy changes (M3a).** HostConfig may carry `Mounts` with exactly one entry, `{Type: volume, Source: <socket volume, from the
proxy's env>, Target: /run/wheel, VolumeOptions: {Subpath: <the uuid in the name>}}`, and nothing else; `WHEEL_LISTEN` must equal
the socket path; NetworkMode is the tenant network. A **ceiling**: the proxy counts `wheel.project`-labelled containers and
volumes (its own list call) and refuses a create past `DOCKER_PROXY_MAX_PROJECTS` (default 200).

**Host firewall (M3d) — nftables, not iptables.** `DOCKER-USER` sees forwarded traffic only; tenant->host itself is `INPUT`. A
separate table `inet wheel_tenants` hooked at `input` and `forward` (priority before Docker's chains) is independent of
Docker's iptables mode, survives Docker restarts (Docker does not flush tables it did not create), and covers both families:
```
table inet wheel_tenants {
  set denied4 { type ipv4_addr; flags interval; elements = {
      0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.0.0.0/24,
      192.0.2.0/24, 192.168.0.0/16, 198.18.0.0/15, 198.51.100.0/24, 203.0.113.0/24, 224.0.0.0/4, 240.0.0.0/4 } }
  chain input   { type filter hook input   priority -10; iifname "wheel-tenants" drop }   # any host service: gateway IP, public IP, sshd, Caddy admin, published 80/443
  chain forward { type filter hook forward priority -10;
      iifname "wheel-tenants" oifname "wheel-tenants" drop                                  # tenant -> tenant: seen by `forward` ONLY with br_netfilter loaded (see below)
      iifname "wheel-tenants" meta nfproto ipv6 drop                                        # tenants have no IPv6; nothing to send
      iifname "wheel-tenants" ip daddr @denied4 drop }                                      # 100.64/10 (tailnet/CGNAT), RFC1918, link-local/metadata, the provider's private net
}
```
The deny list is the complement of "global unicast" for IPv4; adversary preferred an allow-list and this is the same set stated
the way nft can express it. **Tenant->tenant on one bridge is L2-switched** and reaches `forward` only when `br_netfilter` is loaded; where it is not, the rule above is a silent no-op. M3d therefore also installs a `table bridge` filter (hook `forward`, drop between two tenant ports) so the property does not depend on a kernel module being loaded, and the verify script's second-probe-container case is what proves it either way. The ruleset ships as `infra/vps/tenant-firewall.nft` with a unit that re-applies it after Docker.

**Verification (M3d) — measured, from inside the tenant network, fail-closed.** `infra/vps/verify-tenant-isolation.sh` runs a
probe container on `wheel-tenants` (and a second one for tenant->tenant). Exit 0 isolated and every probe ran / 1 reachable /
2 could not measure — never "isolated" from a probe that did not run (the lesson of #138's script). **Positive controls first**
(a public IPv4 address on 443 must connect; DNS must resolve), then: the bridge gateway on 22/80/443/8080/2019, the host's public
address, the second tenant's IP, wheeld's IP on `wheel-front`, `169.254.169.254`, `10.0.0.1`, `172.17.0.1`, `100.64.0.1`, and an
IPv6 literal. Stub-tested with the same eight-case shape as `verify-network-isolation.test.sh`.

**Order.** M3a proxy allowlist + ceiling -> M3b `DockerSandbox` unix channel + healthz + image healthcheck -> M3c wheeld off the
network (compose) -> M3d nft ruleset + verify script + stub tests. Each its own small PR after #149.

**Questions for adversary.** (1) Is `Subpath` on a shared volume an acceptable isolation between tenants' socket directories, or
must each project get its own volume that wheeld attaches some other way? (2) Any tenant-reachable path to the host left with this
ruleset — e.g. the tenant bridge's own gateway answering ARP/ICMP, Docker's embedded DNS resolver forwarding to a host-local
resolver, `host.docker.internal`? (3) Is dropping IPv6 wholesale on the bridge right, or should the network be v6-capable with the
same denied set mirrored?

### M3 amendments after adversary's delta review (PM rulings; these are the M3a spec)

1. **`Mounts` is new surface and gets the same equality discipline as everything else.** The golden create carries exactly one
   `Mounts` entry, and the proxy pins every field of it: `Type == "volume"` (never `bind`, `tmpfs`, `image`, `npipe`),
   `Source ==` the proxy's configured socket-volume name, `Target == /run/wheel`, `ReadOnly` absent/false, and
   `VolumeOptions == {Subpath: <uuid>}` with **`Subpath` equal to the uuid in the container name, the volume name, the
   `wheel.project` label and `WHEEL_PROJECT_ID`** — otherwise a compromised wheeld hands one project's engine another's socket
   directory. `VolumeOptions` carries nothing else (no `DriverConfig`, no `Labels`, no `NoCopy`). Refusal table rows:
   `Type: bind` with `Source: /`, `Type: tmpfs`, another `Source`, another `Target`, a `Subpath` of another uuid / `..` /
   absolute / empty / with a slash, two mounts, `ReadOnly`, `BindOptions`, `TmpfsOptions`, `DriverConfig` — each refused for its
   own reason. `Mounts` was refused wholesale until now, so this is the identical class of hole to the original
   `DriverOpts`-bind-of-`/` finding, and gets the same mutation-check treatment.
2. **`Subpath` vs one volume per project — decided: do NOT rely on `Subpath` until measured; the fallback is stated.**
   Kubernetes' `subPath` had a multi-year symlink/TOCTOU record (CVE-2017-1002101, CVE-2021-25741). I have **no evidence**, from
   Docker's source or docs or a test, that Docker's `Subpath` resolves race-free, and I will not assert it. Plan:
   (a) M3b's first task is a measurement on a real Docker >= 26: from inside a tenant, plant symlinks in its socket directory
   (`engine.sock -> ../<other-uuid>/engine.sock`, `.. -> /`) and race container (re)creation of a *different* project against
   the swap; record what the daemon resolves. Result goes in this document either way.
   (b) **Fallback if (a) is not clean: one volume per project for the socket directory** (`wheel-p-<uuid>-run`, created through the
   same validated `POST /volumes/create`, mounted at `/run/wheel` as a plain `Binds` entry — no `Mounts`, no `Subpath`). wheeld
   itself cannot mount a new volume into its own running container, so it reaches the socket through the **host path of that
   volume** (`/var/lib/docker/volumes/wheel-p-<uuid>-run/_data`, bind-mounted read-only into wheeld as
   `/var/lib/docker/volumes` — which exposes every volume's data to wheeld, i.e. every tenant's data, which wheeld already
   holds the keys to). That costs wheeld one read-only mount; it does not cost the proxy anything new to check. Adversary: is there
   a better fallback that keeps wheeld unprivileged?
   **Trade-off, stated plainly:** either way the tenant boundary moves from the network namespace (kernel-enforced, mature) to a
   volume path (newer, less proven), and wheeld gains a new filesystem path into a tree tenants also have a view into.
   wheeld must only ever `connect()` to `<root>/<its own uuid>/engine.sock`, checking with `lstat` that neither the directory nor
   the socket is a symlink and that the socket is owned by the engine uid, and must fail closed on anything else.
3. **Verify script adds** (M3d): an **ICMP probe to the bridge gateway** (none of the listed probes exercised it; the
   unconditional per-interface input drop covers it, and the script proves it), and it **asserts the probe container has zero
   IPv6 addresses** (`ip -6 addr` empty — not just that v6 destinations are unreachable). Confirm `daemon.json` sets no global
   `host-gateway-ip` / `host.docker.internal` mapping (`ExtraHosts` is already off the M2 allowlist; the daemon-wide default is
   checked in M4's preflight).
4. **IPv6: no capability, not merely firewalled.** Linux assigns link-local IPv6 by SLAAC whatever Docker's network object says,
   so besides the wholesale drop the tenant bridge gets `net.ipv6.conf.<bridge>.disable_ipv6=1` (set by the M3d unit that installs
   the ruleset, before any tenant starts) and the tenant containers get none. IPv6's special-range list is long enough that
   "no v6 at all" beats "v6 everywhere filtered".

## Deliberately out of scope
- Per-node uid (037): SDK's proposal; docker mode fixes cross-*project* isolation, not agent-vs-agent inside one canvas.
- The Railway 048 migration and the limiter PR #140: both only matter if `wheel-host` is ever internet-reachable. Parked / lower priority.
- Migrating existing embedded projects into docker volumes.

## Ask of adversary
1. Is the body-validating allowlist enough at M2, or must the proxy also pin the *image digest*? (I lean: pin the tag the operator configured, refuse others.)
2. Tenant containers surviving a wheeld restart — any attack that becomes easier (stale secrets in env, orphaned engines after `destroy` races)?
3. M3: is dropping tenant→tenant at `DOCKER-USER` sufficient given Docker's own `POSTROUTING` handling and userland-proxy, or must the tenant network be `--internal` with an egress gateway?
