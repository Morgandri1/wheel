# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
#
# Written by infra/vps/install.sh and OVERWRITTEN on every run. Yours goes in
# /etc/systemd/system/wheeld.service.d/90-local.conf, which install.sh never touches — drop-ins
# apply in lexical order, so 90- wins. Same managed/.local split as wheeld.env / wheeld.local.env.
#
# Sized for the documented target: a 2 vCPU, 3.8 GB Linode. The goal is NOT "cap the agent". It is:
# when a runaway agent hits a limit, THE AGENT DIES AND THE ENGINE KEEPS SERVING — never the
# reverse — and the box stays reachable over SSH so you can go and look.
#
# Reasoning and the measurements: docs/proposals/wheeld-native-production.md §5.

[Service]
# ---------------------------------------------------------------- THE LINE THAT MAKES THE REST SAFE
# systemd's default is DefaultOOMPolicy=stop: when ANY process in a unit's cgroup is OOM-killed,
# systemd stops THE UNIT. Left at the default, one runaway agent hitting MemoryMax below takes down
# wheeld and every other project's agents with it — the exact inversion this file exists to prevent.
#
# Measured, not assumed (infra/vps/rehearsal/native/oom-containment.sh reproduces it): with the
# default, a child OOM kill left the unit `failed`, `Result=oom-kill`, the parent dead. With
# `continue`, the child was killed (memory.events oom_kill 1) and the parent stayed
# `active (running)`.
#
# `continue` makes systemd ignore the kill. The engine then sees its child die and marks the turn
# interrupted, which is the behaviour the product already has for an agent killed out of band
# (INTERRUPTED_BY_SHUTDOWN, wheel-engine/src/supervisor/mod.rs).
OOMPolicy=continue

# ---------------------------------------------------------------- memory
# Soft. Above this the cgroup is throttled and reclaimed hard; nothing dies. This is what turns
# "memory-hungry agent" into "slow agent" for the common case, which is most of them.
MemoryHigh=2G
# Hard: the kernel OOM-kills a process IN THIS CGROUP. 2.8 of 3.8 GB leaves ~1 GB for the kernel,
# sshd, journald, Caddy and the board, so the HOST's own OOM killer never fires and you never lose
# sshd to an agent. That is what this number buys.
#
# It does NOT protect wheeld from being the victim. The kernel picks by oom_score, roughly
# proportional to RSS — wheeld resident is tens of MB against a runaway build's hundreds or
# thousands, so the agent is overwhelmingly likely to be chosen, but it is not guaranteed. If
# wheeld loses, Restart=on-failure brings it back in 2s having lost its drain. Losing a drain is
# worse than losing a turn and better than losing the box. The real fix is oom_score_adj on agent
# children: follow-up F3, SDK/Engine lane.
MemoryMax=2.8G
# Bounds swap so a thrashing agent cannot hold the box unresponsive for minutes while the OOM
# killer declines to fire. Cost: a build that would have limped through on swap now dies. On 2 vCPU,
# dying in seconds beats thrashing for an hour.
MemorySwapMax=1G

# ---------------------------------------------------------------- processes
# cgroup pids.max. On hit, fork() returns EAGAIN: a fork bomb stops here and the box survives. The
# number matches wheel-host's RLIMIT_NPROC default for the same population (wheel-host/src/config.rs)
# and sits just under Ubuntu's own DefaultTasksMax=15% (~4900) — the value is in it being stated,
# testable, and scoped to the CGROUP rather than the uid.
#
# Cost, and it is real: the budget is shared across every agent in this unit, so one agent leaking
# processes starves its siblings and can stop wheeld spawning a new one. Per-agent accounting needs
# per-node cgroups, which needs per-node uids (M2/M3, redteam 037).
TasksMax=4096

# ---------------------------------------------------------------- file descriptors
# Per process. On hit, EMFILE in the one process that ran out — contained and loud. systemd's 1024
# soft default is below what Node, pnpm, file watchers and rust-analyzer want, and EMFILE out of a
# bundler is a famously unattributable failure.
LimitNOFILE=65536

# ---------------------------------------------------------------- cpu
# Of 200% on 2 vCPU, leaving half a core for sshd, journald and Caddy. This is the difference
# between "the box is slow" and "I cannot ssh in to stop it". Cost: an agent-driven build is ~25%
# slower at saturation.
#
# It does NOT slow the install: install.sh compiles as wheel-build, outside this unit entirely.
# On a bigger box, raise or remove it in 90-local.conf (`CPUQuota=` with no value clears it).
CPUQuota=150%

# IOWeight is deliberately unset: the Linode is SSD-backed and io.latency tuning without measurement
# is guesswork dressed as rigour.
