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

## OUTCOME — executed 2026-09-07 ~01:25 MDT (operator go; SDK scope = pm tree only)

All six `creds/<uuid>/wheel` checkouts deleted, one at a time, each guarded by a live
delete-time re-check (`git -C <tree> -c safe.directory=<tree> status --porcelain`, rc=0
AND 0 lines; b969c042's expected dirty ` M qa/BUGS.md` gated instead on its diff sha256
`5832e81b…` matching the rescued snapshot, which itself is on `origin/main@3d5ea1d`).
Every agent home preserved; zero deleted-but-open fds; namespace clean afterward.

Authority for the record: PM's own rc/safe.directory/origin-reachability checks on the five
+ QA's own-tree declaration and PM's content-verified rescue at 3d5ea1d + operator's go.
SDK signed off the **pm tree (ccd0a90) only**; the four-tree evidence never reached SDK
intact, so SDK did not review the other five. Not a second pair of eyes on the five.

### Space recovered: ~0.2G, not the projected ~1.7G — projection was wrong, here is why
`df /data`: 2.8G→2.6G used (62%→58%). `du creds`: 2.4G→2.2G.

The ~1.7G projection double-counted shared content. Web's and QA's checkouts (pnpm apps)
materialise `node_modules` as **hardlinks into each home's `.local/share/pnpm` store**
(882M each). Per-dir `du wheel/` counted those blocks under the checkout, but they are
shared with the LIVE agent home; deleting the checkout drops one hardlink, not the blocks.
Only the checkouts' unique content (`.git` + source, ~0.2G total) was actually freed.
(The post-delete `find -links +1` count is low precisely because the second link — the
checkout side — is now gone; it does not disprove the pre-delete sharing.)

### The real space consumer, for any future reclaim
The remaining 2.2G in `creds/` is agent homes; two `.local/share/pnpm` stores (~882M each,
~1.76G) dominate. These are re-fetchable caches inside LIVE homes — a separate, more careful
decision than orphaned-checkout cleanup, and not taken here. Volume at 58% is healthy; no urgency.

## TICKET (M2, not tonight — engine env work, under deploy freeze): per-project pnpm store-dir

SDK cross-checked the hardlink finding against A8's own premise and both survived:
- A8's git saving is REAL and structural, not hardlink-luck. Measured with `git clone --no-hardlinks`
  (which is what cloning from GitHub over https actually is — a remote cannot hardlink):
  A8 (1 bare store + 2 worktrees) 58840 KB vs old (2 independent clones) 75344 KB.
  A8 collapses the `.git` objects (8.1M of an 18M clone), NOT the checked-out working files (10M each).
  SDK's FIRST control was confounded by exactly our effect — local `git clone` hardlinks objects by
  default, so "two independent clones" shared blocks and was artificially cheap. The control was not a
  control until `--no-hardlinks`. (This is the reclaim's lesson restated: a control that shares blocks
  measures nothing.)
- The same shape answers the pnpm 1.76G: six agents each with their own `.local/share/pnpm` store is the
  duplication A8 removed for git objects. One `store-dir` (via `PNPM_HOME`/`store-dir`) per PROJECT rather
  than per agent collapses ~1.76G toward one copy. The engine already sets per-project `CARGO_HOME` for
  exactly this reason, so the mechanism exists. Unlike deleting a cache it loses nothing: the store is
  re-fetchable by definition and shared by design.
- NOT now: engine env work under the deploy freeze, and it wants measuring before building. Recorded so
  the second (bigger) half of the finding is not lost. Volume at 58% is a fine place to stop.
