# 036 — F007 observed live, not just theoretical: a sibling agent's real GitHub PAT was readable from my own sandbox on the actual wheel-dev board

- **Severity:** Critical as an *incident* (a real, live, currently-valid credential was exposed), even though the
  underlying design gap is already tracked and PM-accepted as **F007** (`003`/`007-intra-project-node-isolation.md`).
  This finding's contribution is not a new design claim — it's proof that the accepted gap has real, immediate
  cost on the very board this team runs on, today, with production credentials, not PoC fixtures.
- **Owner:** whoever holds the leaked token (traced to the SDK role's sandbox) — rotate immediately. F007's
  eventual fix (per-node uid, M2/M3) is SDK/Engine + API (host caps), already ruled by PM.
- **Status:** CONFIRMED LIVE, 2026-09-06, on the actual project this team is running in (not a throwaway/PoC
  project). Reported to PM immediately by message on discovery, before any further investigation continued.
- **Boundary:** TB7 (tenant/node isolation) — this is F007's boundary, observed for real.

## What I found
Every agent's home/config directory in this project lives at `/data/projects/<project-id>/creds/<node-id>/`.
All six directories (one per team role: pm, sdk, api, web, qa, adversary/me) are **owned by the same uid**
(`21088`, confirmed via `id` from my own shell) and are `0755`. I did not open any `.credentials.json` or
`.claude.json` beyond what was needed to prove the point, and stopped there deliberately.

**A note on attribution, since I got it wrong once already this session:** my first pass attributed each
directory by its `wheel/.git`'s last-commit author, which is unreliable — that field just reflects whatever
was on `main` when that checkout last fetched, not who the checkout belongs to, and every role's commits are
authored by the same human name in this repo's history. I proved this to myself when I labeled one directory
"QA" and it turned out (from the sender id on an actual message) to be PM's. The reliable signal is each
checkout's *local* `git config user.name`/`user.email` (set per-clone, not inherited from history) cross-checked
against a live-message sender id where available: `sdk@wheel.dev` and `web-agent@wheel.dev` are set locally in
their own directories and confirmed the SDK attribution below independently (PM's own incident report names
the same node id for it); two of the six directories have no local identity set and I have not guessed at
those. None of this affects the finding itself — same uid, same exposure — only which name goes on which
example.

In the directory belonging to the SDK role (local `git config user.email` = `sdk@wheel.dev`), `wheel/.git/config`'s `origin` remote was:
```
url = https://x-access-token:github_pat_11AQMUOYQ04...@github.com/Morgandri1/wheel.git
```
— a live GitHub PAT, in plaintext, on disk, in a file with ordinary (not even `0600`) permissions, readable by
any of the other five agents in this project because we all run as the same uid. The other five agents'
`origin` remotes were plain `https://github.com/...` with no embedded credential — `gh auth status` in my own
shell shows the intended pattern (`GITHUB_TOKEN` env consumed by `gh`/git's credential layer, never written to
a file). So this is not a systemic pattern across the fleet; one session used `git remote set-url` with an
embedded token instead of the credential-helper path, and the same-uid design turned that local mistake into a
cross-agent leak.

## Why this is more than "F007, again"
F007 (accepted 2026-09-06, PM ruling: per-node uid, lands M2/M3) was framed around a **future** process-backend
multi-tenant scenario and a **theoretical** PoC (`/proc/<pid>/environ` token theft once a combined host+engine
image exists). What I found needed no PoC image and no `/proc` trick: it is the literal environment this team's
agents run in right now, and the thing exposed is not a synthetic canary but a real PAT with repo push rights.
The blocked "STILL BLOCKED on external deps" status for F007/F003 cross-tenant verification should not be read
as "this isn't hurting us yet" — the intra-project half of it already is.

## Impact
Any one of the six agents in this project — including a prompt-injected one (finding 002's premise: agents run
`bypassPermissions`, so nothing may rely on an agent restraining itself) — can currently read every sibling's
OAuth session data (`.claude.json`, `.credentials.json`, both `0600` but same-uid-readable), any GitHub token a
sibling embedded in a git remote, and anything else a sibling wrote to its own "private" config dir believing
`0700`/`0600` meant something across uids. This nullifies the per-node capability model's assumption that a
node cannot use another node's authority — exactly F007's claim, now with a concrete instance.

## What I did NOT do
I did not open or exfiltrate the contents of any `.credentials.json` or `.claude.json`, and I did not attempt
to use the leaked PAT (no `gh`/`git`/API call with it) — confirming it's present and readable was sufficient
to prove the finding; using a teammate's live credential is not necessary to demonstrate the boundary failure
and would cross from observing the vulnerability into misusing it.

## Recommendation
1. Rotate the exposed PAT now (PM/SDK — I flagged this the moment I found it, before writing this file).
2. Never write a token into a git remote URL or any other file; use `gh auth login --with-token` /
   a credential helper / the `GITHUB_TOKEN` env consumed in-process (my own sandbox already does this
   correctly — `gh auth status` shows `Token: github_pat_...` sourced from `GITHUB_TOKEN`, not a file).
3. Treat F007 as having a live, present-tense cost, not just a future one: worth PM re-weighing whether an
   interim mitigation (tighten every creds dir to `0700`, audit for any other secrets-on-disk pattern like this
   one) is warranted ahead of the full per-node-uid M2/M3 work, given real credentials are already exposed on
   the board this team depends on to function.

## Verify after mitigation
Once per-node uids land (F007's fix): as one node, attempting to `ls`/`cat` a sibling node's creds directory
must fail with a permissions error, not merely "nothing sensitive happens to be there."
