# 030 — CARGO_HOME location: over-called as cross-tenant; the host sets WHEEL_DATA_DIR per-project (see CORRECTION)

- **Severity:** DOWNGRADED to Low / NEEDS-RESOLUTION. Originally filed High/cross-tenant; that was MY ERROR —
  the host overrides `WHEEL_DATA_DIR` to the per-project dir in process mode (`process.rs:119`), so
  `cargo_home` is per-project, NOT the shared `/data/cargo` my premise assumed. **Read the CORRECTION section
  at the bottom first — it supersedes the High framing above.** Owner: SDK/Engine
  (`crates/wheel-engine/src/supervisor/mod.rs:390`).
- **Status:** RETRACTED-as-cross-tenant; open only as "why is QA's `WOW-toolchain-cargo-per-project` test red
  given process.rs:119?" — resolve on the process backend. The High text below is preserved for the record but
  is corrected at the end.
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

## CORRECTION (2026-09-06) — I OVER-CALLED THIS; downgrade to NEEDS-RESOLUTION
I filed the "shared across tenants" premise from `supervisor/mod.rs:390` (`data_dir.join("cargo")`) plus
`config.rs:87` (data_dir DEFAULT `/data`) WITHOUT checking what `WHEEL_DATA_DIR` the host actually sets per
engine. It does override it per-project:
- `crates/wheel-host/src/sandbox/process.rs:119`: `("WHEEL_DATA_DIR", self.project_dir(id)...)` → in PROCESS
  mode each engine gets `WHEEL_DATA_DIR=/data/projects/<id>`, so `cargo_home = /data/projects/<id>/cargo` —
  **PER-PROJECT**, not `/data/cargo`.
- `docker.rs:141`: `WHEEL_DATA_DIR=/data`, but the docker backend is one container per project (single uid),
  so `/data/cargo` is that container's own — also effectively per-project.
So the cross-tenant "shared cargo dir" claim is WRONG for both backends as the host actually spawns them. The
twin's note in finding 029 (cargo is per-project in process mode) was correct; this finding's High severity
was my error — I asserted from a partial read (the engine default) instead of tracing the host's per-engine
`WHEEL_DATA_DIR`. Lesson: trace the value that actually reaches the engine, not the default.

**BUT unresolved:** QA left `WOW-toolchain-cargo-per-project` RED, which contradicts "per-project." Two
possibilities the successor must settle: (a) QA's test observes `/data/cargo` because in its harness the host
does NOT set `WHEEL_DATA_DIR` per-project (or runs a single-engine mode), i.e. a TEST-setup gap, not a product
bug; or (b) a real deployment path where the per-project override doesn't apply. Also confirm the isolation
does not rely on the cargo SUBDIR perms: `create_dir_all(&cargo_home).ok()` leaves 0755, so cross-tenant
safety depends on the PARENT `/data/projects/<id>` being `0700` (per §5b) — verify that parent mode on the
process backend (same probe as F003/F007). **Net:** downgrade from High/cross-tenant to Low/needs-resolution;
the operative question is why QA's test is red given process.rs:119. Do not treat as a confirmed cross-tenant
leak until that is resolved.
