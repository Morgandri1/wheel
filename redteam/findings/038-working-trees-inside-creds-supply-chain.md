# 038 — Working trees (node_modules, git objects) live INSIDE the credentials directory: supply-chain × single-uid = credential theft by any dependency

- **Severity:** High (process/Railway backend, live). Owner: SDK/Engine (workspace layout) + API/host (per-node
  uid, F007/§2). Boundary TB4b (child ↔ child) and supply-chain. Companion to 037 (single-uid blast radius) and
  036 (the PAT that was found in one of these very trees). PM measured the trigger: six repo clones in
  `creds/`, 2.5 GB, two holding 827 MB of `node_modules` each; `ws/` holds 9.8 MB but the `creds/` clones are
  still in use.
- **PM framing:** "What is in there that should not be?" — this answers it and names the NEW class the location
  creates, beyond 037's generic uid gap.

## The specific defect: executable third-party code co-located with credentials, under one uid
Each agent's working checkout lives at `/data/projects/<id>/creds/<node-id>/wheel/…` — INSIDE the credentials
directory. That directory also holds the harness credential files (`.claude/.credentials.json`, `CODEX_HOME`)
and, until 036's repair, the git remote with the embedded PAT. Two facts combine into a class 037 does not
state on its own:
1. `node_modules` (and a `.cargo` build cache, and git packfiles) is THOUSANDS of files nobody reviewed —
   including package **install/build scripts** (`postinstall`, `prepare`, cargo `build.rs`, native `.node`
   addons) that **execute automatically** on `npm install` / `cargo build`.
2. Every agent runs as the SAME uid (037; PM measured uid 21088). So an install/build script runs as that uid
   with **read access to every sibling agent's `creds/` directory** — every other account's OAuth tokens and
   API keys.

## Why this is worse than "037, again": it widens the ATTACKER SET
037's threat actor is "a compromised or prompt-injected agent" — someone has to subvert an agent's reasoning.
This defect adds a far cheaper actor: **any transitive dependency**. To steal every agent's credentials on the
board, an attacker does not need to prompt-inject anything — they need one agent to `npm install` (or
`cargo build`) a tree that contains one malicious or typo-squatted package. Its `postinstall` runs as the
shared uid and reads `../..//<other-node>/.claude/.credentials.json` for all five siblings, plus any PAT still
in a `creds/.../.git/config`. The agents on the wheel-dev board install real dependency trees to build Wheel
itself (M1.6), so this is not hypothetical surface — it is the daily workflow. The PAT in 036 proves the
credential material is really there to be read.

## What is in `creds/` that should not be (the inventory)
- **`node_modules` / build outputs** — unreviewed third-party executable code + install scripts, sibling-readable
  and (in the installing agent) sibling-EXECUTED as the shared uid. The credential-theft primitive above.
- **Sibling harness credentials** (`.claude/.credentials.json`, `CODEX_HOME`) — 037 item 5; the thing the
  above reads.
- **git objects / packfiles / other branches** an agent fetched — may contain secrets from history or from
  branches the agent had no business holding; all sibling-readable.
- **The `.git/config` PAT** — 036 (now repaired by `sanitise_remotes`, but the finding is that the *location*
  keeps inviting this).
- **2.5 GB of it** — also the A10/efficiency-P1 cost: a working tree in `creds/` is backed up, permission-swept
  (git_creds walks it depth-6 on every start), and volume-accounted as if it were credentials.

## Interaction with the fixes already landing
- 036's `sanitise_remotes` now walks the workspace on every start to depth 6. With `node_modules` (wide, and
  some npm deps carry their own `.git`), that is a real per-start `read_dir` cost over thousands of dirs, and it
  will read/rewrite `.git/config` inside third-party packages. Bounded (good — they thought about depth), but
  another reason the working tree does not belong under a path the engine security-sweeps.
- Per-node uids (037's fix) REDUCE this but do not fully remove it: a node's own `node_modules` postinstall
  runs as that node's own uid and can still read that node's own `creds/` — so even post-uids, a working tree
  must not be a sibling of the credential files inside the same directory.

## Recommendation (radius + the structural fix; PM asked radius, this is both)
1. **Finish the migration SDK started:** move the existing `creds/.../wheel` clones into `ws/` and DELETE the
   `creds/` copies. `ws/` is already the intended home (9.8 MB there vs 2.5 GB in `creds/`); the old clones
   being "still in use" is the operational gap.
2. **Structural rule: `creds/` holds ONLY credentials, never a working tree.** Executable third-party code and
   secrets must never be co-located in one directory — so that even a dependency install script cannot reach a
   credential by a relative path within its own tree.
3. **Per-node uids (037)** for the cross-agent half — the only thing that stops sibling A's install script from
   reading sibling B's creds at all.
4. Consider `--ignore-scripts` / vendored, reviewed dependencies for the trees the wheel-dev agents build, so
   automatic install-script execution is not the default on an untrusted tree.

## Verify after mitigation
- No `node_modules`, build cache, or working checkout under any `creds/<node>/` path (only credential files).
- Post per-node-uids: as node A, `cat`/`ls` of node B's `creds/` fails with EACCES, and a postinstall run in
  A's tree cannot read B's credentials.
