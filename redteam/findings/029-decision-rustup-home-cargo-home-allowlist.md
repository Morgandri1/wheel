# 029 — DECISION (F015 allowlist widening, rule 42137cd): RUSTUP_HOME approved; CARGO_HOME conditional (must be per-project, never inherited-shared)

- **Type:** Red-team review decision (PM requested; QA gate cd2c9d0 pins whatever is approved). Owner of the
  change: SDK/Engine (`INHERITED_ENV` in `supervisor` / the spawn env). Rule: 42137cd.
- **Context:** QA traced the Wheel-on-Wheel cargo build failure to `env_clear()` stripping `RUSTUP_HOME` and
  `CARGO_HOME` from the child. This is a REAL runtime need (any child running cargo), not test convenience —
  so 42137cd's "refuse for test convenience" does not apply; the question is HOW each is supplied, not whether.

## RUSTUP_HOME — APPROVED for INHERITED_ENV
A filesystem path to the image's rustup toolchain install (rustc/std/components). No secret material; the
toolchain is a machine fact the engine can't compute (per SDK: `/opt/rust/rustup` in the image, elsewhere on a
laptop), so inheriting it beats hard-coding. Same non-secret class as PATH / SSL_CERT_* / NODE_EXTRA_CA_CERTS.
**Condition:** it must resolve INSIDE the image's read-only toolchain dir; refuse if `RUSTUP_HOME` resolves
under `/data` or a project dir (that would be neither a machine fact nor read-only).

## CARGO_HOME — NOT approved as a plain inherited-shared var; must be PER-PROJECT
CARGO_HOME is a different security class from RUSTUP_HOME. It is **writable per-user cargo state**: the registry
cache and index, **git checkouts**, the cargo config, AND `credentials.toml` — where `cargo login` writes a
crates.io token. So:
1. **Secret-bearing risk:** CARGO_HOME can contain `credentials.toml` (a registry token). A shared CARGO_HOME
   inherited into every child would expose it — exactly the class 42137cd's companion (proxy-URL) watch guards.
2. **Tenant isolation (contract M1.6):** M1.6 mandates "per-project CARGO_HOME/RUSTUP_HOME under the project
   data dir so builds cache per tenant." A single inherited CARGO_HOME shared across projects breaks that: on
   the shared-kernel host, tenants would share the registry cache, index, and **git checkouts** — a
   cross-tenant CACHE-POISONING / supply-chain surface (one tenant plants a crate/checkout another builds).
3. **Read-only won't work:** cargo WRITES to CARGO_HOME (registry, git). A read-only shared dir breaks builds;
   a writable shared dir is the poisoning surface. So it cannot be "read-only under the toolchain dir" like
   RUSTUP_HOME — it must be per-project + writable-by-that-uid-only.

**Verdict:** set CARGO_HOME **explicitly, per-project**, under this project's own data dir
(`/data/projects/<id>/.cargo` or equivalent), writable only by the project uid — restoring the M1.6 design and
SDK's own original framing ("CARGO_HOME is engine-computed per project, so set it explicitly"). The build
failure means the explicit per-project set was dropped/never wired; the fix is to RESTORE it, not to inherit a
shared one. **If** the engine's own `CARGO_HOME` is already guaranteed per-project (one engine per project in
process mode, host-set under the project dir), then inheriting THAT is acceptable — but only under the
condition below; if it is a single shared value, inheriting is REFUSED.

## Condition to pin (for QA's gate cd2c9d0)
- `RUSTUP_HOME`: on the allowlist; assert it resolves inside the image's read-only toolchain dir; refuse if it
  resolves under `/data` or a project dir.
- `CARGO_HOME`: NOT a plain shared inherited value. Permit only when it resolves under THIS project's data dir
  (unique per tenant) and is writable solely by the project uid; refuse if it resolves to a path shared across
  project uids or is group/other-writable. (Simplest safe implementation: set it explicitly per-project rather
  than inherit.)
- Belt (applies to both, and to the earlier RUSTUP_HOME review): the toolchain and cargo dirs must not be
  group/other-writable by child uids — otherwise one child poisons the shared toolchain/registry for siblings.
  That's a filesystem-perms check, orthogonal to the allowlist but worth asserting alongside it.

## Net
RUSTUP_HOME: yes (read-only machine fact). CARGO_HOME: yes to the NEED, no to a shared inherited value — it
must be per-project (secret-bearing via credentials.toml + writable cross-tenant state). Not test convenience;
42137cd is satisfied because the requirement is a genuine runtime need supplied without widening the boundary
across tenants.

## VERIFIED (2026-09-06, fresh image 05:37Z from d206b95) — 029 conditions met
Child (pid 15, uid 10001) env read as the child uid:
- `RUSTUP_HOME=/opt/rust/rustup` — inherited, NOT under `/data` (029 condition met). And the child uid
  CANNOT write it: `touch /opt/rust/rustup/... -> Permission denied`. So SDK's claim "the agent uid cannot
  write the shared toolchain" is CONFIRMED — no cross-tenant toolchain poisoning via a writable RUSTUP_HOME.
- `CARGO_HOME=/data/cargo` (= `$WHEEL_DATA_DIR/cargo`; in process mode `WHEEL_DATA_DIR=/data/projects/<id>`,
  so this is PER-PROJECT) — NOT the shared `/opt/rust/cargo`, exactly as 029 required. The child can write its
  OWN CARGO_HOME (expected — cargo needs it); the shared `/opt/rust/cargo` is NOT child-writable either
  (`Permission denied`). So the shared cargo/toolchain cannot be poisoned across tenants.
- RESIDUAL (process backend, ties to the open cross-project probe): I verified CARGO_HOME is per-data-dir and
  the SHARED dirs are read-only to the child, in DOCKER mode (single uid). The per-project CARGO_HOME being
  0700 and a SIBLING project uid being unable to write it is a PROCESS-backend cross-uid property — to confirm
  when the process backend is live (same probe as F003/F007 cross-tenant).
- Process note: a first run mis-parsed the child uid (empty) and ran the write test as ROOT, which produced a
  false "child wrote into RUSTUP_HOME". Caught it (root can write; the child uid cannot), re-ran with the
  correct uid. The corrected result is above.

## 027 create=call VERIFIED (same image)
With `validate_config_with(cfg, allow_hosts)`: an allowlisted `127.0.0.1:18080` tool is now CREATABLE
(previously 400) AND callable; non-allowlisted `127.0.0.2:18080`, `127.0.0.1:19090`, `169.254.169.254` are
refused at CREATE (matching call) — create and call are one decision. Full allowed-path re-confirmed via the
official loopback: header-CRLF REJECTED (this time with the clear `:328` message), 026 all five v6 spellings +
Teredo client DENIED, redirect no-replay/per-hop/limit, 5 MiB cap, 30 s timeout, prod-boot-refusal.
