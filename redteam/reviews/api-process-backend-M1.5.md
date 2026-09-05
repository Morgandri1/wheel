# Review — API process-backend design (M1.5), against F003/F007

Reviewer: ADVERSARY. Scope: the `process` `Sandbox` backend (host = root, drops to per-node uid; one
container, one kernel, one `/run` + `/data` tree for all tenants). Answers API's three questions, then
the residual list. Verified against the engine's process-mode socket (main.rs: pathname `UnixListener`,
parent dir `0o700`, socket `0o600` — already correct) and probe `redteam/pocs/child-isolation/
t_process_backend_isolation.py` (covers uid-drop, `/data`, socket, `/proc` cross-uid).

## (a) Is project uid enough? Does same-project /proc defeat the per-node split? Is hidepid needed?
- **Project uid is NOT enough — the ruling is per-NODE uid (F007).** Collapsing to one uid per project
  re-opens exactly what F007 closed: node A reads node B's creds dir and (pre-file-token) its env. Keep
  per-node uid; the project uid is only the engine's base.
- **/proc does NOT defeat per-node uid.** `/proc/<pid>/environ` and `/proc/<pid>/fd` are owned by the
  target process's uid and are `0400`/uid-gated — a *different* uid cannot read them **without hidepid at
  all**. So per-node uid alone closes the token/creds/fd-theft vector between nodes. (Bonus: it also
  closes F008 same-uid fd-injection *between* nodes; the only residual is an agent injecting into its own
  node's CLI fd — same node, same uid, expected, and SDK's session_id binding handles the consequence.)
- **hidepid is defense-in-depth, not load-bearing — you're right not to depend on it.** hidepid=2 hides
  other uids' pid *existence* and cmdline/status fields (pid enumeration, `total_cost` snooping), which is
  nice, but the secrets are already protected by uid ownership + the F007 0600 token-file. Request
  `hidepid=2` (and `mount -o remount,hidepid=2 /proc` in the capability spike) and adopt it if Railway
  grants it; **the design must remain correct if it does not.** Do not put a secret anywhere whose only
  protection is hidepid.

## (b) Anything cross-tenant beyond ":7100 + public internet"? — yes, several; contradiction requested
Your two are correct but the list is incomplete. All tenants share one kernel + one net namespace + one
`/run`+`/data` tree, so uid ownership — not a namespace — is the whole boundary. Also reachable/attackable
across tenants unless explicitly closed:
1. **Other tenants' engine sockets** `/run/wheel/<other>/engine.sock` — closed ONLY by pathname+0600+dir
   0700 (see (c)); list it as a boundary you must keep, not assume.
2. **`/data/projects/<other>`** (must be 0700 to that uid) and **`/data/host.db`** (must not be world-
   readable — a dropped child reading the host's sqlite = every engine secret + vault key).
3. **Abstract unix sockets (`@`-prefixed)** — these IGNORE filesystem perms and are reachable by ANY uid
   in the shared net namespace. The engine uses a *pathname* socket (good); make it a hard rule that NO
   component (engine, host, any helper, any MCP child) ever opens an abstract socket. One abstract socket =
   instant cross-tenant.
4. **Any TCP bind inside the container.** Process mode must be unix-socket-ONLY. If anything binds a TCP
   port (a stray `0.0.0.0`/`127.0.0.1` health/metrics/debug listener, or the docker-mode `:7000` branch
   leaking into process mode), every tenant shares loopback and reaches it. Engine's process branch is
   unix-only (main.rs) — verify the host's healthz and any exporter are too, or bound to an interface no
   sandbox can route to.
5. **`/tmp`, `/dev/shm`, SysV IPC.** World-writable shared tmp → cross-tenant file drop, symlink attacks,
   and DoS (fill the fs); shared POSIX/SysV shm/semaphores are not uid-partitioned the way files are. Give
   each project a private `TMPDIR` under its 0700 data dir; mount `/tmp` and `/dev/shm` per-project or keep
   them off the attack surface; disk quota per project (see rlimits below).
6. **The host `:7100` itself.** Bearer-gated is necessary; also: constant-time bearer compare, a rate
   limit so a sandbox can't brute/replay against it, and confirm the sandbox can reach ONLY `:7100` on the
   host (not other host-internal ports).
7. **ptrace.** Cross-uid ptrace is uid-denied, but set `kernel.yama.ptrace_scope>=1` so same-uid ptrace of
   a node's CLI is constrained too (hardens F008's self-injection).

## (c) Is unix-socket ownership sufficient given one kernel / one /run tree?
**Yes — for a PATHNAME socket at 0600 inside a 0700 per-project dir.** connect(2) needs write permission on
the socket inode plus traverse on every containing dir; a foreign uid fails on the 0700 dir and again on
the 0600 socket. SDK already pins both (main.rs 97/108) instead of trusting umask — correct. Provisos:
1. **Pathname, never abstract** (see (b)#3).
2. **The per-`<id>` dir is created 0700 owned by that project's uid BEFORE the engine drops** — the host
   does this; a 0755 dir here silently defeats the socket perm.
3. **Add SO_PEERCRED on accept:** the engine should verify the connecting peer's uid is one it expects, so
   isolation does not rest on filesystem perms alone (catches a misconfigured dir, and a future abstract-
   socket mistake). None today — recommend it as belt-and-braces. Low effort, high assurance.

## setuid-drop correctness (must-test, already in the probe)
Real+effective+saved uid all dropped; `setgroups([])` clears supplementary groups (a leftover
supplementary gid is the classic drop bug); `no_new_privs` set; cwd not left inside another project's
tree; and no ambient capability survives to the child (the engine keeps only CAP_SETUID/SETGID and must
not pass them to children). The probe asserts uid + `id -G`; add a `no_new_privs`/`CapEff==0` assertion on
the child.

## rlimits (shared-machine DoS — F003)
Per-project (ideally per-uid) `RLIMIT_NPROC` (fork bomb), `RLIMIT_AS`/cgroup memory, `RLIMIT_FSIZE` + disk
quota (fill `/data`), `RLIMIT_NOFILE`, CPU cgroup share. One tenant must not starve the box.

## Bottom line
Design is sound IF: per-node uid (not project); pathname 0600 socket in 0700 dir (done) + SO_PEERCRED
(add); no abstract sockets, no TCP in process mode; `/data/projects/<id>` 0700 and `host.db` non-world-
readable; per-project TMPDIR + rlimits; hidepid/ptrace_scope as available-but-not-load-bearing hardening.
Everything here is checked by `t_process_backend_isolation.py` — wire it into the M1.5 host boot and it
gives a live yes/no the moment the spawn path lands.
