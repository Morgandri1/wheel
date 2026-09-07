# Wheel-on-wheel push — runbook

Scaling from the one proven single-agent wake to the cloud board doing real wheel-on-wheel development.
Safe order: land the prerequisites in **one** host deploy (a watched-crate change restarts every running
agent, so we batch and never pay the restart twice), then broaden the wake. The quiet window is now — only
the single proven agent has run, so there is no broad wake to interrupt.

## The one batch (all watched-crate, ship together)

- **(a) Paired workspaces fix — config half.** The engine provisions each agent a git worktree off the
  shared object store (A8/A9) from the node's `workspaces` config, and exports its path into the child env as
  `WHEEL_WORKSPACE`. `infra/bootstrap-board.sh` PATCHes the `workspaces` array onto every agent node so the
  worktree is actually provisioned. Doc half already landed (`main 8700ae5`): `WHEEL-ON-WHEEL.md` tells agents
  to work in `$WHEEL_WORKSPACE`, not clone. Open question for SDK: is worktree provisioning-from-`workspaces`
  live today, or does the batch also need to wire that, not just name the var?
- **(b) Idle-parking (§3c#14).** The engine reads the idle-park config field and runs an Idle→Parked timer so
  a woken agent releases its ~162MB claude process after its turn. Without this, six woken agents hold ≈1GB
  idle. Non-negotiable before the six-agent wake.
- **(c) Agent-sender transcript test.** Covers the multi-agent send producer path a broadened wake runs on.
  Cheap; the wake is when it starts mattering.
- **(d) Ctx-clear one-arm fix.** Trivial, already queued, rides along free.

## Not in this batch

- Script execution — correctly gated on F007 storage isolation.
- Deploy-drain — decouple-via-promotion makes it M2 hardening, not a blocker for the push.
- Finding 049 #1 — API's crate, returned to API.

## Sequence

1. Freeze watched-crate merges; confirm `main` HEAD is exactly the batch and nothing stray (a deploy ships
   `main` HEAD, not just the four items — anything unreviewed on a watched crate would ride it).
2. Fire the one host deploy.
3. SDK confirms worktree provisioning + `WHEEL_WORKSPACE` are live.
4. PM runs `bootstrap-board.sh` to set the `workspaces` array, then PATCHes the live `workflow` ctx node on the
   cloud board (it still carries the old "Clone into `$HOME/wheel`") to match the doc.
5. Verify: an agent's env has `WHEEL_WORKSPACE`, the worktree exists, idle-parking parks after a turn.
6. Broaden the wake beyond the single proven agent.

## Decouple-via-promotion (resolved, out of batch)

Board-dev promotes a build target rather than redeploying the board's own host, so an engine merge on the
board never restarts the board mid-turn. Drain is the long-term hardening (M2).
