# 015 — Agent children inherit the engine's env: WHEEL_VAULT_KEY + WHEEL_ENGINE_SECRET leak to untrusted code

- **Severity:** CRITICAL (S1). CVSS-ish: AV:L/AC:L/PR:L/UI:N/S:C/C:H/I:H/A:H — an in-sandbox actor
  (a prompt-injected or otherwise compromised agent) gains project-wide secret disclosure AND full
  control-plane authority; scope changes to the whole project.
- **Owner:** SDK/Engine (`crates/wheel-engine/src/supervisor/mod.rs` child spawn, ~line 203).
- **Status:** **FIXED @ e09e1ec, VERIFIED FIXED (2026-09-05).** Was CONFIRMED LIVE against
  `wheel-engine:dev` @ 39cbcb0. Fix = `env_clear()` + a hard-coded allowlist at BOTH spawn sites
  (`supervisor/mod.rs` inherit_platform_env, `oauth.rs` login child). Verified independently against
  `wheel-engine:dev` @ ed68f67 (image 21:21Z) by `redteam/pocs/vault/verify_env_fix.sh` — see the
  Verification section at the bottom. Closed with SDK.
  PoCs: `verify_env_fix.sh` (the correct scoped detector). The original `run_env_inheritance.sh` /
  `run_env_exploit.sh` OVER-REPORT on a fixed build (they scan all of `/proc` and match their own
  `docker exec` probe shell, which inherits the container's `-e` env) — see the Detector-correction
  note. They must NOT be turned into a regression test as-is; use `verify_env_fix.sh`.
- **Boundary:** TB5 (engine ↔ child process) / TB6 (child ↔ wheel CLI). Defeats the vault write-only +
  wire-gated model (contract §2 "the sandbox boundary is the whole security story") wholesale.

## What
The supervisor builds the child `tokio::process::Command` at `supervisor/mod.rs:203` and **never calls
`.env_clear()`** (confirmed: zero `env_clear`/`env_remove` in the engine crate). A tokio/std `Command`
inherits the parent's entire environment by default. The host populates the ENGINE's env with
`WHEEL_ENGINE_SECRET` and `WHEEL_VAULT_KEY` (host `sandbox/process.rs:284-285`, `docker.rs:129-130`), so
every agent child inherits both — readable in its own `/proc/self/environ` **regardless of per-node uid
isolation** (a process can always read its own environ). The careful token handling right above it
(mod.rs:213-216: "WHEEL_TOKEN is deliberately NOT set … a file rather than an env var keeps it out of
/proc/<pid>/environ") is nullified, because the engine's own secrets sit in that same environ.

## Live proof (docker backend)
`run_env_inheritance.sh` → child PID (the `sh`/harness launcher) environ contains:
```
WHEEL_ENGINE_SECRET=ef575c5e66a874ceac8b0b2f897730a695ceda286410a4f8
WHEEL_VAULT_KEY=+3r1g6durjPJDRxSONullGPVZetby3Au+6ydaqI3q2Y=
```
`run_env_exploit.sh` → `leaked secret == engine bearer: YES`; then from inside the sandbox using ONLY
the leaked value: `GET /v1/board -> 200`, `POST /v1/wires worker->othervault -> 204` — the agent
**self-granted a read wire to a vault it was never wired to.**

## Impact (both realised)
1. **WHEEL_VAULT_KEY** is the sole input to `VaultKey::from_base64` and decrypts EVERY `vault_values`
   ciphertext for the whole project (their own test `a_different_project_key_cannot_read_the_values`
   proves the key is necessary AND sufficient). A compromised agent decrypts every vault — including
   vaults it has no wire to — bypassing write-only + wire-gating + the ambiguity rule entirely.
2. **WHEEL_ENGINE_SECRET** is the `/v1/*` bearer = host authority: create/delete any wire, start/stop/
   read/delete any node, PUT any vault, create endpoints. Wire enforcement — "the whole security story"
   — is fully bypassed. Demonstrated by the unauthorized wire above; equally reachable are all `/v1/*`.

Docker backend (M1/M2, the currently bootable target): child shares the engine's network namespace →
reaches `127.0.0.1:7000` directly, so #2 is live now. Process backend: `/v1/*` is a 0600 unix socket
owned by the engine uid so a base+n child cannot open it (#2 blunted there), but **#1 is unmitigated in
both backends** — the vault key in the child's own environ needs no socket.

## Fix (mirror the host's own hygiene at process.rs:281)
At `supervisor/mod.rs:203`, `env_clear()` then re-add ONLY the minimal allowlist the child needs.
`harness.env()` sets HOME/CLAUDE_CONFIG_DIR/IS_SANDBOX but NOT `PATH`, so PATH must be added explicitly
or the harness binary won't resolve:
```rust
let mut cmd = tokio::process::Command::new(self.harness.program());
cmd.env_clear()                                   // <-- the fix
   .env("PATH", "/usr/local/bin:/usr/bin:/bin")   // harness binary lookup (matches host process.rs:291)
   .args(self.harness.argv(&spec))
   .current_dir(&spec.cwd)
   .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
   .kill_on_drop(true);
// then the existing additive env is fine: harness.env(), WHEEL_TOKEN_FILE, WHEEL_ENGINE_URL,
// WHEEL_NODE, CARGO_HOME, credential_env(), vault_env — none of which is a HOST secret.
```
Explicitly NEVER re-add: `WHEEL_VAULT_KEY`, `WHEEL_ENGINE_SECRET`, `WHEEL_HOST_SECRET`, `WHEEL_PROJECT_ID`,
`WHEEL_ROLE`, `WHEEL_LISTEN`. Add a regression test: spawn a child, assert its env has none of these
(the harness test double at mod.rs:911 makes this cheap).

## Verify after fix
Re-run both PoCs → the child environ must contain neither secret (leak PoC → INCONCLUSIVE/clean, exploit
PoC → cannot obtain the bearer). Confirm the harness still launches (PATH preserved).

## Verification (2026-09-05, ADVERSARY, independent of SDK)
`wheel-engine:dev` @ ed68f67 (image built 21:21Z, after the fix). `verify_env_fix.sh` boots the engine
with a **canary** `WHEEL_ENGINE_SECRET`, starts an agent, then reads the engine-spawned child's OWN
environ **as the child's uid** (`docker exec -u <uid>` — root cannot read it, no CAP_SYS_PTRACE in the
default docker cap set, which is itself defence-in-depth) and asserts on it:
```
caught agent child pid=15 uid=10001 comm=claude
PASS A/agent-child: clean — no engine-secret canary, no WHEEL_ENGINE_SECRET, no WHEEL_VAULT_KEY.
  child env names: CARGO_HOME CLAUDE_CONFIG_DIR HOME IS_SANDBOX PATH WHEEL_ENGINE_URL WHEEL_NODE WHEEL_TOKEN_FILE
```
Exactly the allowlist + explicitly-set vars; the token is a FILE path (`WHEEL_TOKEN_FILE`), never the
token value. SDK independently confirmed the same, plus the mutation-checked unit test
`supervisor::tests::a_child_is_not_given_the_engines_own_secrets` (fails without `env_clear`, passes with
it). oauth.rs login-child path carries the identical `env_clear()` + `inherit_platform_env` fix (source
verified); the live capture coincided with the still-running agent child (same uid/env code path).

### Allowlist review (SDK asked: any secret-bearing var in the inherited set?)
`PATH, LANG, LC_ALL, TZ, TMPDIR, SSL_CERT_FILE, SSL_CERT_DIR, NODE_EXTRA_CA_CERTS` — **none is
secret-bearing** in a standard deployment: the SSL/CA three are file PATHS, the rest are locale/PATH.
`HTTP(S)_PROXY` are correctly NOT on the list (a proxy URL can embed credentials), so an operator-set
proxy credential cannot ride into an untrusted child — good. Recommendation: keep it a hard-coded
`const`, never pattern-derived, and route any future addition past red-team. No change required.

## Detector-correction note (why the original PoCs over-report on a FIXED build)
`run_env_inheritance.sh` and `run_env_exploit.sh` sweep every PID in `/proc` (skipping only PID 1) for
the secret. On a fixed build the ONLY process that still matches is their own `docker exec` probe shell:
`docker exec` injects the container's configured `-e` environment into the process it starts (PPid==0),
so the scanner finds the secret in **its own** environ. That is a docker artifact of a root-privileged
exec we control — NOT anything an agent inside the sandbox can reach. The real agent child (PPid==1,
uid `agent`) is unreadable by the root probe (no CAP_SYS_PTRACE) and, read as its own uid, is clean.
Correct method (in `verify_env_fix.sh`): target PPid==1 children only, and read their environ as the
child's own uid. The original scripts are kept for the historical live-leak repro on the VULNERABLE
build; they are annotated not to be used as a fix gate.
