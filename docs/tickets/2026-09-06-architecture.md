# Architecture tickets — 2026-09-06

Written after an 80-minute production outage whose true cause was a full volume. Each ticket names
the evidence that produced it; none is speculative.

## A1 — A project can fill the volume and take down every other project · **S1** · SDK/Engine

One tenant's build artifacts consumed 4.6 GB and every project on the host went dark. There is no
per-project disk limit anywhere: the volume is shared, first-come, and a single agent running
`cargo build` is enough. This is the noisy-neighbour failure the sandbox design exists to prevent,
and it is the only defect today that let one board's activity destroy every other board's
availability.

Wanted: a per-project disk budget enforced by the host (quota, or a periodic accounting that stops
the project and reports it). A project that exceeds its budget must fail alone and say so.

## A2 — Build artifacts must not live on the durable volume · **S2** · SDK/Engine

`target/` is regenerable by definition — API proved it mid-incident by deleting 1.9 GB to recover
the host. Measured here: 1.7 GB for this workspace, of which 1.4 GB is debug info in `debug/deps`
and 567 MB is incremental state. A shared dev machine reached **73 GB**.

Wanted: `CARGO_TARGET_DIR` for an agent points at container-ephemeral storage, not `/data`. Losing
it on restart costs a rebuild; keeping it costs the grid. Pair with the workspace profile now on
`main` (`debug = "line-tables-only"`, `incremental = false`).

## A3 — An agent's working copy still lands in its credentials directory · **S2** · SDK/Engine

QA's gate is RED and correct. One agent cloned Wheel into `creds/<node>/` and built there, putting
1.9 GB of build output next to that agent's secrets. §3e says the workspace is
`/data/projects/<id>/ws/<name>`. A credentials directory is not a build root, and the disk problem
is the smaller half of this.

## A4 — Disk pressure is invisible to everything that could act on it · **S1** · API

`/healthz` answered `{"status":"ok"}` throughout an outage caused by a full disk, and the platform's
own dashboard reported 127 MB while `df` reported zero bytes free. We repeated the control plane's
lie with our own.

Wanted: free space in the host healthcheck; refusal to start a project below a floor, with the
reason; a `df` line at boot. QA's finding stands that the ENGINE already reports ENOSPC correctly —
this is about the layers that decide, not the one that fails.

## A5 — The host is one process for every tenant, and reconcile is serial · **S2** · SDK/Engine + API

ADVERSARY 032: `numReplicas: 1`, ten retries, a 300 s health window, and reconcile at ~30 s per
project in sequence. Twenty stuck projects is ten minutes of 503 for everyone, and a host that is
merely slow is indistinguishable from a host that is dead — which made every health verdict today
ambiguous. Bound the concurrency, cap total reconcile time, and open the routes before the last
project finishes.

## A6 — A project engine cannot start on a volume the host survives · **S1** · SDK/Engine

QA BUG-022. `store.rs` takes the database exclusively where the volume cannot host a WAL index, so
the host boots; the engine's `set_journal_mode` returns on a read-back before reaching the drain, so
a project engine exits 1 on the same volume. The grid comes up and every board on it stays down.

## A7 — Nothing measures the machine · **S3** · QA

Two of today's wrong turns were resource claims taken from a control plane rather than the running
container, and one test read an unwritable directory as a full disk. Wanted: a standing check that
reports real `df`, real memory and the real toolchain paths from inside a running sandbox, so the
first answer to "is it full?" is measured rather than quoted.

## A8 — Agents share one checkout, not one each · **S2** · SDK/Engine

Operator: "in a large number of workflows, only a single repository and/or monorepo will be open,
meaning one clone per agent is… bloated." Measured today: a checkout plus its build tree cost 1.9 GB
per agent, and three agents filled a 4.6 GB volume. Six agents on one repo is six copies of the same
bytes, six fetches, and six chances for one of them to hold a stale tree.

Wanted: a node type (or a workspace node's shared mode) that materialises ONE checkout per project,
mounted into every agent wired to it. Writes need a policy — a shared working tree with six agents
editing it is a merge conflict machine, so the likely shape is one shared clone plus a cheap
per-agent worktree over it (`git worktree`), which shares the object store and costs kilobytes per
agent rather than gigabytes.

## A9 — There is no clone mechanism, and that is why the token leaked · **S1** · SDK/Engine

QA established it and it changes A8 and the token fix both: **the engine contains no git code**, and
`workspaces` materialisation is unimplemented. Legs 1-3 of the operator's goal work today because
agents IMPROVISE — each one runs `git clone https://<token>@github.com/...` in its own way, which is
exactly how a live GitHub PAT ended up in plaintext in a `.git/config` on the volume.

So the fix is not a repair. Wanted: the engine materialises a workspace, and supplies credentials
through a helper reading the child's environment (or a per-invocation `http.extraheader`) — never in
the remote URL, never in argv. QA's `WOW-token-not-on-disk` gate already exists and passes against a
correct implementation, proven by planting a credential in the production shape and requiring the
detector to catch it. It is waiting for the mechanism.

Until this lands, every agent that clones is one improvisation away from writing the operator's
credential to disk, and the loop that makes Wheel self-developing is the thing doing it.
