# Disk reclaim — staged state and evidence (2026-09-07)

The six per-agent repo clones under `creds/<uuid>/wheel` (2.4G of a 4.6G volume, 62%) are orphaned by A8
(agents now materialise into `ws/`, verified: engine diff between deployed `d47c4a4` and verified `d844e9e`
is empty; SDK's planted-tree test confirmed the engine does not touch `creds/<uuid>/wheel`). This records the
reclaim's staged state so it completes correctly without reconstruction.

## THE CHECK IS RUN WITH safe.directory, EXIT CODE VERIFIED — the false-clean lesson

The trees are owned by uid 21088; a `git status` run as root over ssh is REFUSED with "dubious ownership"
(rc=128) and prints NOTHING to stdout — which reads as "0 lines = clean" if you check output instead of the
exit code. That false-clean nearly deleted an agent's uncommitted work. Every check MUST:
- run with `git -C <tree> -c safe.directory=<tree> ...` (or as uid 21088), AND
- be believed only when it EXITED ZERO, not merely printed nothing (rc=0 AND 0 lines = clean; rc=128 = refused).

## Per-tree disposition (evidence captured post-restart, verified live)

| tree (uuid)        | agent    | size | status (rc=0) | HEAD      | on origin ref                              | verdict |
|--------------------|----------|------|---------------|-----------|--------------------------------------------|---------|
| 0be41bbb           | adversary| 13M  | clean         | 0334041   | origin/HEAD (main)                         | SAFE    |
| 3dfb551a           | sdk      | 16M  | clean         | 9706f2e   | origin/sdk/shm-blocked-regression-test     | SAFE    |
| d7c37993           | api      | 12M  | clean         | 9c793c3   | origin/HEAD (main)                         | SAFE    |
| fb66d07b           | web      | 839M | clean         | 013d75b   | origin/web/handoff-coverage-ruling         | SAFE    |
| b969c042           | qa       | 882M | **DIRTY** ` M qa/BUGS.md` | c9f0f6f | **NONE (unpushed)**             | HOLD — rescue first |
| 456f630e           | pm(cloud)| 13M  | clean         | ccd0a90   | **NONE (unpushed)**                        | HOLD — SDK ccd0a90 ruling |

- SAFE = checkout under creds/<uuid>/, clean by rc=0 check, HEAD reachable from an origin ref → deletable;
  re-materialised by A8 on next start; deleting loses nothing (commit on origin, checkout re-creatable).
- b969c042: uncommitted `qa/BUGS.md` (QA's scars) + unpushed HEAD. Rescue (commit+push, or extract off volume)
  BEFORE deletion. "Orphaned from the engine is not empty of value."
- ccd0a90 (pm): a 16:53 merge "origin/main into sdk/028-face5... (resolve messages.rs conflict)"; branch and
  base both on origin, conflict resolution superseded by tonight's main. PM read: throwaway. SDK's branch →
  SDK's ruling.

## Completion procedure (when not 6am; volume 62% stable, no urgency)

1. QA rescues b969c042 (commit+push, or PM extract on QA's word).
2. SDK rules ccd0a90 keep-or-throwaway.
3. For EACH tree to delete: re-run `git -C <tree> -c safe.directory=<tree> status --porcelain`, confirm rc=0
   AND 0 lines, IMMEDIATELY before its `rm` — state can change (the host restarted mid-window once; prior
   evidence is not trusted at delete-time).
4. `rm -rf creds/<uuid>/wheel` for confirmed trees, one at a time, printed, preserving the rest of
   creds/<uuid>/ (the agent home: .rustup, sessions, policy-limits.json, session-env, shell-snapshots,
   backups, projects, wheel-src, .profile).
5. Re-measure, report before/after `df` to the operator. Expected ~2.8G → ~1.1-1.2G if all six reclaim;
   ~1.9G → ~1.0G if qa held.
