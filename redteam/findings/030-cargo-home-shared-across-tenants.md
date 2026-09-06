# 030 — CARGO_HOME lands one level too high: one cargo dir (registry, git checkouts, credentials.toml) shared across every tenant uid

- **Severity:** High on the **process backend** (multi-tenant prod / Wheel-on-Wheel target): a shared,
  world-readable-and-writable cargo dir across tenant uids is a cross-tenant build-poisoning (code-execution
  in a victim tenant's build) and credential-exposure surface. Low on the docker/M1 backend (single uid).
  Owner: **SDK/Engine** (`crates/wheel-engine/src/supervisor/mod.rs:390`).
- **Status:** CONFIRMED IN SOURCE (I verified the code, not the relay). Could NOT run live — needs a
  multi-tenant `process`-backend host image with ≥2 project uids sharing `/data` (the same combined
  host+engine runtime image that has blocked the F003/F007 cross-tenant probe all along). PoC shape below.
- **Boundary:** multi-tenant host (process backend), TB "all tenants share one kernel". Realises decision 029's
  CARGO_HOME warning as a concrete bug.

## What (confirmed in source)
`supervisor/mod.rs:387-392`:
```rust
// A private crate cache per project. ... a shared CARGO_HOME would put one project's downloaded
// sources -- and its registry credentials, if it ever configures any -- where the next project can read them.
let cargo_home = self.cfg.data_dir.join("cargo");   // <-- data_dir, NOT the per-project dir
std::fs::create_dir_all(&cargo_home).ok();          // <-- 0755, error swallowed
cmd.env("CARGO_HOME", &cargo_home);
```
`data_dir` defaults to `/data` (`config.rs:87`, `WHEEL_DATA_DIR`). On the **process backend**, `/data` is the
SHARED host data root; per-project data lives at `/data/projects/<id>`. So `cargo_home = /data/cargo` is **one
level too high** — a single dir shared by EVERY project's children, created `0755` (error swallowed). The code
comment describes the correct per-project design; the implementation does the exact thing it warns against.

## Impact (process backend — the multi-tenant prod target)
Every tenant uid gets `CARGO_HOME=/data/cargo`, a shared writable tree holding the registry cache + index,
**git checkouts**, config, and (if any tenant runs `cargo login`) `credentials.toml`:
1. **Cross-tenant build poisoning (code execution):** tenant A writes a crafted crate source / git checkout /
   registry entry into `/data/cargo`; tenant B's `cargo build` compiles it → A's code runs in B's build. This
   is the sharp harm and needs no credential.
2. **Credential exposure:** a `credentials.toml` (crates.io token) written by any tenant is readable by all
   (0755) — the exact leak the code comment names.
3. **Private-source disclosure:** a private crate a tenant fetched (git dep resolved with a vault token) leaves
   its SOURCE in `/data/cargo/git`, readable by other tenants.
This defeats the per-tenant isolation M1.6 mandates ("per-project CARGO_HOME under the project data dir so
builds cache per tenant") and that decision 029 required.

## Fix
`let cargo_home = <this project's data dir>.join("cargo");` — i.e. under `/data/projects/<id>` (or wherever the
per-project root is), not `data_dir`. And DON'T swallow the perms: create it `0700` owned by the project uid
(the same hygiene as the node config dir at mod.rs:325), and don't `.ok()`-drop the error — a cargo dir that
can't be made private should fail the spawn, not silently share. Same treatment for any RUSTUP_HOME writable
state (the toolchain itself stays shared+read-only per decision 029).

## PoC shape (for a successor, once a process-backend host image exists)
Two projects A, B on a `process`-backend host (per-project uids sharing `/data`):
1. As A's uid: write `/data/cargo/config.toml` with a `[source.crates-io] replace-with` pointing at a local
   registry you control, OR drop a poisoned checkout under `/data/cargo/git/checkouts/...`.
2. As B's uid: run a `cargo build` in B's workspace → observe it consume A's planted source (code exec in B).
3. Separately: as A, `cargo login <token>` → as B, read `/data/cargo/credentials.toml` (0755 → readable).
Expected after fix: `/data/projects/<A>/cargo` and `/data/projects/<B>/cargo` are distinct, 0700, and B cannot
read or influence A's. Ties into the staged `redteam/pocs/child-isolation/t_process_backend_isolation.py`.
