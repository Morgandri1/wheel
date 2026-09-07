# Cloud-board handoff — the cutover model, and the self-redeploy gate (operator + PM, 2026-09-07)

## The model (operator's, confirmed)
- Agents own their OWN repo contents: each clones into its own worktree, pulls before working, commits,
  pushes. No central repo-manager. (Proven in the first wake.)
- Handoff is a CLEAN CUTOVER, single source of truth:
  1. The laptop swarm finishes and STOPS committing.
  2. The cloud board pulls origin/latest once.
  3. From then on the cloud board is the SOLE developer.
- Post-cutover: origin is always current (all pushes originate on the board); each agent pulls before working
  to pick up peers' commits; nothing syncs back to the laptop. Correct — conditional on the cutover being
  clean. If both sides commit, two heads diverge.

## The gate the model surfaces: a self-developing board restarts its own agents
Any merge touching wheel-engine/core/host/cli is in wheel-host's watchPatterns -> Railway redeploys wheel-host
-> container swap -> the board's agents die mid-turn and reconcile. So a board developing its OWN RUNTIME
kicks itself over on every engine merge.
- Docs / web / api development on the board is fine today (those do not redeploy the engine).
- ENGINE development on the board needs DEPLOY-DRAIN (agents finish their turn before the swap) OR DECOUPLING
  (the board runs a stable engine while developing; engine updates are deliberate, not every-merge). Hard
  prerequisite, not a nicety — without it, continuous engine self-development is self-sabotaging.

## How far
- Core PROVEN: an agent does the full clone->edit->commit->push loop on the board (first wake).
- To cut over to continuous cloud-board dev, still needed:
  1. Full wake — all six agents, not one.
  2. Multi-agent concurrent dev confirmed — pull-before-work + conflict handling with several agents pushing
     (untested; the wake was one agent, no conflict).
  3. Deploy-drain or engine-deploy decoupling — the long pole, real engine work (ties to deploy-resume-and-drain).
  4. The clean cutover flip (laptop stops, cloud pulls, cloud takes over).
- Distance: docs/web/api dev could move to the board sooner (no self-redeploy); FULL engine self-development
  waits on deploy-drain/decoupling (days).
