# 015 — Agent children inherit the engine's env: WHEEL_VAULT_KEY + WHEEL_ENGINE_SECRET leak to untrusted code

- **Severity:** CRITICAL (S1). CVSS-ish: AV:L/AC:L/PR:L/UI:N/S:C/C:H/I:H/A:H — an in-sandbox actor
  (a prompt-injected or otherwise compromised agent) gains project-wide secret disclosure AND full
  control-plane authority; scope changes to the whole project.
- **Owner:** SDK/Engine (`crates/wheel-engine/src/supervisor/mod.rs` child spawn, ~line 203).
- **Status:** CONFIRMED LIVE against `wheel-engine:dev` (@ 39cbcb0, docker backend, 2026-09-05).
  PoCs: `redteam/pocs/vault/run_env_inheritance.sh` (leak), `run_env_exploit.sh` (weaponised).
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
