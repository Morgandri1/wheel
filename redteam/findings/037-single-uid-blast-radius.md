# 037 — Blast radius of the single-uid gap: one compromised agent ⇒ full project compromise + every linked account

> Companion to **036** (my cloud twin's LIVE incident: the actual PAT observed on the wheel-dev board — uid
> **21088** confirmed shared across all six agents via `id`, creds dirs `0755`). 036 = the observation; 037 =
> the full radius PM asked for. The twin's live facts UPGRADE items below from REASONED to VERIFIED-LIVE: the
> shared uid is measured (21088), and item 5 (sibling creds dirs) is the incident itself. Note the dirs are
> `0755`, not `0700` — worse than the code intends; item 5's `.git/config` PAT sat in an ordinary-perms file.

- **Severity:** Critical (on the `process`/Railway backend, where it is live). This is the concrete blast-radius
  enumeration PM asked for around the plaintext-PAT exposure (a vault token embedded in a workspace
  `.git/config` remote URL). The PAT is ONE LEAF. Owner: SDK/Engine + API/host (the per-node-uid gap, §2 / F007).
  Boundary TB4 (engine ↔ child) and TB4b (child ↔ child). Companion to F007 (accepted "node is the boundary" —
  this shows what "not yet" costs today).
- **Framing PM set:** blast radius, NOT the fix. What else on the volume is readable by a child uid that should
  not be, given every agent currently runs as the SAME uid. Below, each item is marked VERIFIED (traced in
  source + Linux semantics) or REASONED/STAGEABLE (needs the combined process-backend image to demo live).

## The single fact that makes everything below true
`§2`/F007: per-node uids are not implemented. On the **process backend (production, Railway)** the host
`setuid`s to the project uid and execs the engine as that uid (§4b: "the engine runs as the project uid the
host has already dropped to"), and every child (agent, script, MCP server) is spawned by that engine and runs
as **the same project uid**. So the `0700` dirs and `0600` files the engine carefully creates
(`auth.rs`, `db/tokens.rs`, `lib.rs`) protect against OTHER tenants' uids — but NOT against a sibling under the
same project uid. **Under one uid, `0700`/`0600` is a no-op between siblings**, and same-uid `/proc/<pid>/environ`
is readable. This is the `/data/projects/<id>/…` volume where the PAT was found — same backend, same uid.

## Blast radius, ranked by impact

### 1. CROWN JEWELS — the engine's own `/proc/<pid>/environ` ⇒ total project compromise. VERIFIED (mechanism); live demo STAGEABLE.
The host launches the engine with `WHEEL_ENGINE_SECRET` and `WHEEL_VAULT_KEY` in its environment (spawn
contract §4b). The engine reads them with `std::env::var` (config.rs:86 for the vault key) and **never removes
them from its own environ** — I checked: the only `remove_var` calls are in the config.rs **test** helper
`with_env` (lines 177/184), not in startup. So for the engine's whole lifetime both secrets sit in
`/proc/<engine-pid>/environ`, mode 0400 owned by the project uid. A same-uid child reads it directly. Then:
  - `WHEEL_ENGINE_SECRET` → the child speaks to the engine control plane as the HOST, not as a node. In docker
    mode that is `:7000` with the bearer; in process mode it is the unix socket. This **bypasses the entire wire
    matrix** — every `POST /v1/nodes`, `/v1/wires`, `/v1/agents/:id/*`, `PUT /v1/vault/:id/:key`, board read —
    because wire enforcement keys on the per-NODE token, and this is the engine-secret path that sits ABOVE it.
  - `WHEEL_VAULT_KEY` (base64 AES-256, vault.rs:33-45) + read access to `/data/.../wheel.db` (item 4) →
    **decrypt every vault in the project**: every account's `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY`/
    `CODEX_API_KEY`, every endpoint bearer secret, every tool `{mode:vault}` fill, the git PATs. The M1.6
    multi-account design (one vault per account, wire-gated) collapses: the key that opens ALL of them is one
    `/proc` read away, regardless of which vault the agent is wired to.
  This is the escalation that turns F007 from "a sibling can read my creds dir" into "any one prompt-injected
  agent owns the whole project and every cloud account attached to it." *Verified:* env-not-scrubbed (source),
  same-uid on the process backend (§4b), `/proc/<pid>/environ` same-uid readability (Linux, engine is dumpable —
  no setuid transition or `PR_SET_DUMPABLE=0`). *Stageable:* the live read awaits the combined host+engine
  process-backend image (the same image the pending cross-project probe needs).

### 2. Every sibling agent's vault-exported secrets, WITHOUT a wire — via `/proc/<sibling>/environ`. VERIFIED (mechanism).
`supervisor/mod.rs:531-533` exports a wired agent's vault values into THAT CHILD'S env (`cmd.env(k, v)`). Under
one uid, agent A reads `/proc/<B-pid>/environ` and lifts B's vault secrets **even with no wire from A to B's
vault**. This is a direct wire-matrix bypass for secrets, needing neither the engine secret nor the vault key —
just a running sibling. (The `secrets` redaction at 531-556 keeps values out of logs/transcripts; as §M1.6 and
F007 say, redaction is not containment — this is the proof.)

### 3. Every node's capability TOKEN — impersonate any node, use any wire. VERIFIED.
Each node's token reaches its child as a `0600` file (`token_file`, `write_secret_file`, mod.rs:456-460;
`db/tokens.rs` "0600 FILE, never an env var"). `0600` under a shared uid = readable by every sibling. Agent A
reads B's token file (path is `run_dir/token`, siblings enumerate `/proc/*/environ` for `WHEEL_TOKEN_FILE` or
walk the run dirs) → calls `POST /v1/cli/*` AS B → uses **every wire B holds**. The MCP config (mod.rs:167-186)
points the token env at that file, so the path is discoverable, not secret.

### 4. `/data/.../wheel.db` read directly — messages, table rows, chest index in cleartext; vault ciphertext + key(1) ⇒ everything. VERIFIED.
The db is created under the project data dir (db/mod.rs:119-120, `create_dir_all`, no per-file 0600); the parent
`/data/projects/<id>` is 0700 to the project uid (process.rs:335) — which stops OTHER tenants, not siblings. Any
same-uid child opens `wheel.db` and reads all messages (poison-pill store, cf. 034/035), all table rows, the
chest index, and the vault CIPHERTEXT. Ciphertext alone is inert; combined with the vault key from item 1 it is
plaintext. No wire, no token needed for the cleartext tables.

### 5. Every other agent's harness credential dir — the found PAT's neighbours. VERIFIED.
`creds_dir().join(agent)` is per-agent, 0700, holding the OAuth token / API-key file 0600 (auth.rs:96, 299,
319; mod.rs:441). Under one uid, agent A reads agent B's `CLAUDE_CONFIG_DIR`/`CODEX_HOME` → B's logged-in
account credentials, and the workspace clones under B's tree — including the `.git/config` with the embedded
PAT PM found. So the PAT is not special: it is item 5's most legible instance. Anything a workspace clone,
`codex` login, or `claude setup-token` leaves on disk under one agent is readable by all the others.

### 6. Other agents' `prompt.txt` (system prompt + injected ctx) and on-disk scripts/chest blobs. VERIFIED.
`prompt.txt` (mod.rs:394-395, the composed system prompt + every injected ctx node) is written to the node's
run dir and passed as a file arg — deliberately, so no secret hits argv (§5b). Same-uid siblings read it, so
one agent sees every other agent's system prompt and injected context (operator instructions, references to
secret locations, internal workflow). Script sources (`/data/scripts/<node>/`) and chest blobs
(`/data/chest/<node>/`) are likewise same-uid readable regardless of wires.

## Net
On the **process/Railway backend that is in production now**, the single-uid gap means the node is NOT a
boundary in practice: any one compromised or prompt-injected agent (and once the Telegram→PM ingress lands,
that trigger is reachable from the public internet — see 035 link 6) can, in ascending order of effort,
(a) read every sibling's vault secrets and creds from `/proc` and disk, (b) impersonate any node via its token
file, (c) read the whole `wheel.db`, and (d) lift the engine secret + vault master key from the engine's
environ for control-plane godmode and decryption of every secret in the project. The plaintext PAT is the
first leaf of a tree whose root is item 1. The docker backend is less exposed IF children run as a non-root uid
distinct from the root engine (then `/proc/<engine>/environ` is cross-uid-denied) — worth SDK confirming, but
production is the process backend where engine and child share the uid.

## What closes it (for reference — PM asked for radius, not fix)
Items 1-6 all close with the SAME control: per-node uids (F007 / §2 M2/M3). Interim hardening that shrinks the
top of the tree independently of that: the engine should `std::env::remove_var("WHEEL_VAULT_KEY")` and
`WHEEL_ENGINE_SECRET` immediately after reading them at startup (removes item 1's `/proc` carrier — a one-liner,
does not need per-node uids), and workspace git creds must be delivered out-of-band, never in the remote URL
(the SDK fix already in flight). These reduce blast radius; only per-node uids remove it.
