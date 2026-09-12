#!/usr/bin/env python3
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
"""The invariants of the native systemd units, checked statically.

The real proof that these units work is infra/vps/rehearsal/native/ -- a container running actual
systemd, with an actual agent workload under the actual directives. That needs Docker, takes
minutes, and nobody runs it on every commit.

This is the other half: the handful of properties whose violation is catastrophic, silent, and
detectable by reading the files. It is pure stdlib, needs no Docker and no network, runs in
milliseconds, and therefore has no "could not run" state to hide in -- the same argument
infra/tests/prune-probe-projects.test.sh makes for living in `make check`.

Each check names what breaks if it fails. A unit-file assertion with no stated consequence becomes
cargo cult the first time someone needs to change it.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VPS = ROOT / "infra" / "vps"
SYSTEMD = VPS / "systemd"

failures: list[str] = []
checks = 0


def check(ok: bool, what: str, why: str) -> None:
    """Record one assertion. `why` is what breaks in production when it is false."""
    global checks
    checks += 1
    if not ok:
        failures.append(f"{what}\n        → {why}")


def directives(path: Path, section: str = "Service") -> dict[str, list[str]]:
    """Every KEY=VALUE in a section, comments and blanks dropped.

    Values accumulate into a list because systemd genuinely allows repeats for list-valued
    directives (ReadWritePaths=), and collapsing them would hide a second line that widens the
    first.
    """
    out: dict[str, list[str]] = {}
    current = None
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current = line[1:-1]
            continue
        if current != section or "=" not in line:
            continue
        key, _, value = line.partition("=")
        out.setdefault(key.strip(), []).append(value.strip())
    return out


def one(d: dict[str, list[str]], key: str) -> str | None:
    values = d.get(key)
    return values[-1] if values else None


# ---------------------------------------------------------------- the files exist at all
for name in ("wheeld.service", "wheel-web.service", "wheel-signup-gate.service"):
    check((SYSTEMD / name).is_file(), f"{name} exists", "install.sh installs it by name and would fail on the server")

DROPIN = SYSTEMD / "wheeld.service.d" / "10-resources.conf"
check(DROPIN.is_file(), "wheeld.service.d/10-resources.conf exists",
      "every resource limit, including OOMPolicy, lives in it")

if failures:
    print("native-units: a unit file is missing; nothing else can be checked", file=sys.stderr)
    for f in failures:
        print(f"  FAIL  {f}", file=sys.stderr)
    raise SystemExit(1)

wheeld = directives(SYSTEMD / "wheeld.service")
web = directives(SYSTEMD / "wheel-web.service")
gate = directives(SYSTEMD / "wheel-signup-gate.service")
gate_unit = directives(SYSTEMD / "wheel-signup-gate.service", "Unit")
web_unit = directives(SYSTEMD / "wheel-web.service", "Unit")
res = directives(DROPIN)

# ---------------------------------------------------------------- shutdown: the drain
#
# docs/proposals/headless-first.md §4. KillMode=mixed signals wheeld ALONE so its own drain runs;
# systemd's default (control-group) SIGTERMs every agent at the same instant, turning a turn in
# flight into a turn killed mid-flight.
check(one(wheeld, "KillMode") == "mixed", "wheeld KillMode=mixed",
      "control-group (the default) signals every agent at the same instant as wheeld, so the ~28s "
      "drain never runs and in-flight turns die mid-turn")

stop = one(wheeld, "TimeoutStopSec")
check(stop is not None and int(re.sub(r"\D", "", stop) or 0) >= 35, "wheeld TimeoutStopSec >= 35",
      "the drain is ~28s and the last seconds are exactly where a slow shutdown gets SIGKILLed; "
      "35 is margin, not the bare minimum")

# ---------------------------------------------------------------- THE OOM INVERSION
#
# The single most consequential line in this kit, and the one nobody would think to write.
check(one(res, "OOMPolicy") == "continue", "10-resources.conf sets OOMPolicy=continue",
      "systemd's DefaultOOMPolicy is `stop`: without this, ONE agent hitting MemoryMax makes "
      "systemd stop wheeld and every other project's agents with it. Measured in "
      "infra/vps/rehearsal/native/oom-containment.sh — the limit escalates the failure instead of "
      "containing it")
# VALUES, not presence. `MemoryMax=infinity` and `TasksMax=infinity` are both legal, both look
# like the limit is configured, and both mean there is no limit at all — which is exactly the shape
# a careless "let's stop the OOM kills" edit takes.
def bounded(key: str) -> bool:
    v = one(res, key)
    return bool(v) and v.lower() not in {"infinity", "0", ""}

check(bounded("MemoryMax"), "10-resources.conf sets a FINITE MemoryMax",
      "without a cgroup ceiling the HOST OOM killer fires instead, and it can take sshd — leaving "
      "a box you cannot log in to in order to fix it. `MemoryMax=infinity` would satisfy a "
      "presence check while meaning exactly no limit")
check(bounded("MemoryHigh"), "10-resources.conf sets a finite MemoryHigh",
      "MemoryHigh is what turns a memory-hungry agent into a slow one instead of a dead one; "
      "without it every overage goes straight to the OOM killer")
check(bounded("TasksMax"), "10-resources.conf sets a finite TasksMax",
      "a fork bomb in one agent otherwise reaches the host's pid limit")
check(bounded("LimitNOFILE"), "10-resources.conf sets LimitNOFILE",
      "systemd's 1024 soft default is below what Node, pnpm and watchers need; EMFILE out of a "
      "bundler is a famously unattributable failure")
# MemoryHigh must be BELOW MemoryMax or the soft limit never bites before the hard one.
def as_bytes(v: str | None) -> int | None:
    if not v:
        return None
    m = re.fullmatch(r"(\d+(?:\.\d+)?)\s*([KMGT]?)", v.strip())
    if not m:
        return None
    return int(float(m.group(1)) * {"": 1, "K": 1024, "M": 1024**2, "G": 1024**3, "T": 1024**4}[m.group(2)])

high, hard = as_bytes(one(res, "MemoryHigh")), as_bytes(one(res, "MemoryMax"))
check(high is not None and hard is not None and high < hard,
      "MemoryHigh < MemoryMax",
      f"MemoryHigh={one(res, 'MemoryHigh')} MemoryMax={one(res, 'MemoryMax')}: if the soft limit is "
      "not below the hard one, reclaim never gets a chance to work and every overage becomes an "
      "OOM kill — the throttle-first behaviour the drop-in claims simply does not happen")

# ---------------------------------------------------------------- hardening that must NOT tighten
#
# These four are the ones a well-meaning security review would "fix". Each was measured to break a
# real agent capability (docs/proposals/wheeld-native-production.md §4a), so each gets a test whose
# failure message says what it costs — because otherwise the next person adds it back.
check(one(wheeld, "ProcSubset") == "all", "wheeld keeps ProcSubset=all",
      "ProcSubset=pid also hides /proc/meminfo and /proc/cpuinfo (measured), which build tools size "
      "their parallelism from. The breakage is silent and gets blamed on the model, not this file")
check("SystemCallFilter" not in wheeld, "wheeld has NO SystemCallFilter",
      "measured: @system-service breaks `unshare -Urm` + mount, which is what bwrap and Chromium's "
      "sandbox do. An agent doing browser QA is a first-class Wheel workload. wheel-web.service "
      "takes the filter instead — it runs one known program that spawns nothing")
check("RestrictNamespaces" not in wheeld, "wheeld has NO RestrictNamespaces",
      "measured: it breaks `unshare -Ur` outright. Ubuntu 24.04's "
      "kernel.apparmor_restrict_unprivileged_userns=1 addresses this at the host level, where it "
      "belongs")
families = one(wheeld, "RestrictAddressFamilies") or ""
check("AF_NETLINK" in families, "wheeld keeps AF_NETLINK",
      "measured: dropping it does NOT break DNS (that is folklore) but DOES break `ss` — and "
      "libexec/wheeld-ready uses ss to prove this daemon binds loopback only. Dropping it turns "
      "the strongest guarantee in proposal §3 into an unverifiable claim")
check("AF_PACKET" not in families, "wheeld excludes AF_PACKET",
      "raw packet capture on the VPS's own interfaces, available to any agent")

# ---------------------------------------------------------------- hardening that must stay ON
for key, value, why in [
    ("ProtectSystem", "strict", "the whole filesystem outside the data dir becomes writable to every agent"),
    ("ProtectHome", "yes", "agents can read the operator's SSH keys, ~/.aws and shell history"),
    ("ProtectProc", "invisible", "agents can read sshd's, caddy's and root's /proc — cmdlines and environments included"),
    ("NoNewPrivileges", "yes", "a setuid binary becomes an escalation path out of the service account"),
    ("PrivateTmp", "yes", "/tmp becomes a channel between agents and everything else on the box"),
    ("LimitCORE", "0", "a core dump of wheeld writes master.key, the operator token and every "
                       "in-flight vault value to /var/lib/systemd/coredump"),
]:
    check(one(wheeld, key) == value, f"wheeld {key}={value}", why)

check(one(wheeld, "StateDirectory") == "wheel" and one(wheeld, "StateDirectoryMode") == "0700",
      "wheeld StateDirectory=wheel mode 0700",
      "the data directory's owner and mode would be a fact about one install moment rather than "
      "about every boot; master.key lives there")

# ---------------------------------------------------------------- the auto-update contract
#
# sdk/auto-update restarts wheeld by exiting 75. Its lane froze this contract; breaking it here
# would be an infra change that silently disables another lane's product feature.
check(one(wheeld, "Restart") == "on-failure", "wheeld Restart=on-failure",
      "sdk/auto-update's WHEEL_UPDATE_RESTART=exit swaps the binary and exits 75, and relies on a "
      "nonzero exit being restarted. `always` would also restart clean exits, losing that "
      "distinction")
check("SuccessExitStatus" not in wheeld, "wheeld has NO SuccessExitStatus",
      "SuccessExitStatus=75 would make the auto-update exit look clean, so systemd would not "
      "restart and the update would never take effect")

# ---------------------------------------------------------------- one source of truth for the bind
#
# config.rs resolves the bind as flag, THEN BIND_ADDR, then its default. A literal address in
# ExecStart would therefore beat an operator's BIND_ADDR while wheel-preflight and wheeld-ready
# both read BIND_ADDR -- preflight would validate an address the daemon ignored, and wheeld-ready
# would probe the wrong port and fail a healthy start.
exec_start = one(wheeld, "ExecStart") or ""
check("${BIND_ADDR}" in exec_start, "wheeld ExecStart interpolates ${BIND_ADDR}",
      "a literal --bind silently overrides the operator's BIND_ADDR, and then preflight and "
      "wheeld-ready are checking a value the daemon never used")
# ---------------------------------------------------------------- running vs serving
check("ExecStartPre" in wheeld, "wheeld has an ExecStartPre",
      "nothing would refuse a non-loopback bind, a wrong data-dir mode, or a claude below the "
      "OAuth floor before the daemon starts serving")
check("ExecStartPost" in wheeld, "wheeld has an ExecStartPost",
      "Type=simple reports active the instant execve succeeds, so `systemctl status` would claim "
      "healthy for a daemon that never bound. This is also what measures the loopback promise")

# ---------------------------------------------------------------- the signup gate ordering
#
# The native equivalent of compose's `depends_on: service_completed_successfully`. Previously this
# check was a block of shell inside install.sh, so it ran once at install time and never again --
# a reboot brought the board up with nothing having checked.
check(one(gate, "Type") == "oneshot", "wheel-signup-gate is Type=oneshot",
      "it must run to completion and report a verdict, not linger as a service")
# Ordering, and DELIBERATELY NOT Requires=. With Requires=wheeld.service, every `systemctl restart
# wheeld` -- every upgrade -- stopped this oneshot, which stopped wheel-web (it Requires= the gate),
# and the board did not come back. Measured in rehearse-native.sh, after a stand-in experiment with
# toy units had wrongly suggested restart does not propagate.
#
# Both directions are asserted, because re-adding Requires= would look like a tightening.
check(any("wheeld.service" in v for v in gate_unit.get("After", [])),
      "wheel-signup-gate After=wheeld.service",
      "the gate would probe wheeld before it is up and fail for a reason that has nothing to do "
      "with the signup policy")
check(not any("wheeld.service" in v for v in gate_unit.get("Requires", [])),
      "wheel-signup-gate does NOT Requires=wheeld.service",
      "Requires= propagates wheeld's stop to this oneshot and from there to wheel-web, so every "
      "upgrade restart drops the board and it does not return. The gate does not need the "
      "dependency: it makes a real HTTP call and fails on its own if wheeld is not answering")
check(any("wheeld.service" in v for v in gate_unit.get("Wants", [])),
      "wheel-signup-gate Wants=wheeld.service",
      "without it nothing pulls wheeld in when the gate is started on its own at boot")
check(any("wheel-signup-gate.service" in v for v in web_unit.get("Requires", [])),
      "wheel-web Requires=wheel-signup-gate.service",
      "the board would start in front of a wheeld nobody proved enforces its own signup gate — on "
      "every boot, which is exactly what the compose path guards against")
check(any("wheel-signup-gate.service" in v for v in web_unit.get("After", [])),
      "wheel-web After=wheel-signup-gate.service",
      "Requires= without After= lets both start at once, so the board can be serving before the "
      "gate has a verdict")
# The gate must resolve WHEEL_SIGNUP exactly as wheeld does, or it reports on a different config.
check(any("wheeld.env" in v for v in gate.get("EnvironmentFile", [])),
      "wheel-signup-gate reads wheeld.env",
      "it would check its own idea of WHEEL_SIGNUP rather than the one wheeld resolved")
check(any("wheeld.local.env" in v for v in gate.get("EnvironmentFile", [])),
      "wheel-signup-gate reads wheeld.local.env too",
      "the operator's override is read second by wheeld and would be invisible to the gate, so an "
      "operator who opened signup would get a gate that disagrees with the daemon")

# ---------------------------------------------------------------- systemd does NOT expand Environment=
#
# `Environment=FOO=http://${BAR}` is passed through LITERALLY -- systemd performs variable
# expansion in ExecStart= and friends, not in Environment= values. It reads like it would work,
# which is why it shipped: the signup gate ended up probing a host named "${BIND_ADDR}", curl
# answered "URL rejected: Bad hostname", and because wheel-web Requires= the gate the board would
# not start at all. Caught by the install rehearsal; pinned here so it costs milliseconds next time.
for unit_name, parsed in (("wheeld.service", wheeld), ("wheel-web.service", web),
                          ("wheel-signup-gate.service", gate)):
    for value in parsed.get("Environment", []):
        check("${" not in value, f"{unit_name} Environment= has no ${{...}} to expand",
              f"systemd passes Environment= values through verbatim, so {value!r} reaches the "
              "process with the braces intact rather than the variable's value")

# ---------------------------------------------------------------- wheel-web is the tight one
check("SystemCallFilter" in web, "wheel-web HAS a SystemCallFilter",
      "it runs one known Node program that spawns nothing, so the filter wheeld had to refuse is "
      "free here. If it is absent, the asymmetry that justifies wheeld's permissiveness is gone")
check(one(web, "IPAddressDeny") == "any" and "localhost" in (one(web, "IPAddressAllow") or ""),
      "wheel-web is confined to loopback",
      "the board dials exactly 127.0.0.1:8080 and serves 127.0.0.1:3000; without this an RCE in "
      "Next.js can exfiltrate")

# ---------------------------------------------------------------- the scripts the units invoke
for key in ("ExecStartPre", "ExecStartPost", "ExecStart"):
    for value in wheeld.get(key, []) + gate.get(key, []):
        path = value.lstrip("+-!@").split()[0]
        if not path.startswith("/opt/wheel/libexec/"):
            continue
        source = VPS / "libexec" / Path(path).name
        if Path(path).name == "verify-signup-gate.sh":
            source = VPS / "verify-signup-gate.sh"
        check(source.is_file(), f"{key}={path} has a source at {source.relative_to(ROOT)}",
              "the unit references a file install.sh would have to install, and it does not exist")

# ---------------------------------------------------------------- install.sh keeps the rollback artefact
install_sh = (VPS / "install.sh").read_text()
# rm, find -delete, or an mv that consumes it. A presence check for `rm` alone would miss the two
# other ways a tidy-up removes the other lane's rollback artefact.
destroys_prev = [
    line for line in install_sh.splitlines()
    if not line.lstrip().startswith("#")
    and re.search(r"bin/[^\s\"']*\.prev", line)
    and re.search(r"\brm\b|-delete\b", line)
]
check(not destroys_prev,
      "install.sh never deletes /opt/wheel/bin/*.prev",
      "*.prev is sdk/auto-update's rollback artefact as well as this script's; deleting it removes "
      "the daemon's ability to undo its own failed update")
check(".prev" in install_sh and "--rollback" in install_sh,
      "install.sh writes a .prev generation and offers --rollback",
      "an upgrade with no way back leaves a failed build as the only build")
check(re.search(r"^\s*echo \"BIND_ADDR=", install_sh, re.M) is not None,
      "install.sh writes BIND_ADDR into wheeld.env",
      "wheeld.service interpolates ${BIND_ADDR} into --bind, so an unset one makes --bind take an "
      "empty argument and the daemon fails to start")
check("libexec/wheel-preflight" in install_sh and "libexec/wheeld-ready" in install_sh,
      "install.sh installs the libexec scripts the units call",
      "the units would reference files that are not on the server and wheeld would fail to start")
check("wheel-signup-gate.service" in install_sh,
      "install.sh installs wheel-signup-gate.service",
      "wheel-web Requires= it, so the board would never start")
# An ASSIGNMENT, not a mention: install.sh's own header explains that neither lane may set this,
# and a substring check would fire on the sentence that documents the rule it is enforcing.
sets_auto_update = [
    line for line in install_sh.splitlines()
    if "WHEEL_AUTO_UPDATE=" in line and not line.lstrip().startswith("#")
]
check(not sets_auto_update,
      "install.sh never writes WHEEL_AUTO_UPDATE",
      "the update POLICY is the operator's, in wheeld.local.env, default off — sdk/auto-update's "
      "contract. An installer that sets it takes that decision away silently")

print(f"native-units: {checks - len(failures)}/{checks} checks passed")
if failures:
    print()
    for f in failures:
        print(f"  FAIL  {f}", file=sys.stderr)
    raise SystemExit(1)
