#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Tests infra/vps/lib/version.sh and the pins in infra/vps/toolchain.env. Pure shell, no network,
# no docker, sub-second — so it runs in `make check` on every machine and has no "could not run"
# state to hide in, the same reasoning as infra/tests/prune-probe-projects.test.sh.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=/dev/null
. "$root/infra/vps/lib/version.sh"
# shellcheck source=/dev/null
. "$root/infra/vps/toolchain.env"

pass=0
fail=0
ok() { pass=$((pass + 1)); }
bad() {
    fail=$((fail + 1))
    echo "  FAIL  $*" >&2
}

ge() { # ge <a> <b> <expected: yes|no> <why>
    local want=$3
    if wheel_version_ge "$1" "$2"; then got=yes; else got=no; fi
    if [ "$got" = "$want" ]; then ok; else bad "version_ge $1 $2 = $got, want $want — $4"; fi
}

echo "== wheel_version_ge =="

# THE PAIR THIS LIBRARY EXISTS FOR. The claude floor for headless OAuth refresh is 2.1.269, and
# "2.1.269" < "2.1.27" as strings while 269 > 27 as numbers. A lexical compare silently accepts a
# build hundreds of releases too old, and the symptom is an OAuth refresh that stops working a week
# later on a box nobody is looking at.
ge 2.1.269 2.1.27  yes "269 > 27 numerically; a string compare gets this backwards"
ge 2.1.27  2.1.269 no  "and the other direction must be false"
ge 2.1.269 2.1.269 yes "equal is >="

ge 2.2.0   2.1.269 yes "a higher minor beats any patch"
ge 2.1.999 2.2.0   no  "a lower minor loses however high the patch"
ge 3.0.0   2.9.9   yes "major dominates"
ge 2.0.0   10.0.0  no  "10 > 2 numerically, not lexically"
ge 2.1.270 2.1.269 yes "one past the floor"
ge 2.1.268 2.1.269 no  "one below the floor is refused"

# Padding: a two-component version must compare equal to its three-component spelling, not be
# treated as unordered or as less-than.
ge 2.1     2.1.0   yes "2.1 == 2.1.0"
ge 2.1.0   2.1     yes "and symmetrically"
ge 2.1.1   2.1     yes "2.1.1 > 2.1"
ge 2.1     2.1.1   no  "2.1 < 2.1.1"

# A prerelease tail compares as its base version. Forgiving on purpose (see version.sh), but it
# must not crash or compare as zero.
ge 2.1.269-beta.1 2.1.269 yes "a prerelease of the floor counts as the floor"
ge 2.1.268-beta.1 2.1.269 no  "a prerelease below the floor is still below it"

# Garbage must not become a silent zero-vs-zero pass in the dangerous direction.
ge ""      2.1.269 no  "an empty version never satisfies a floor"
ge abc     2.1.269 no  "an unparseable version never satisfies a floor"

echo "== wheel_version_of =="
probe_dir="$(mktemp -d)"
trap 'rm -rf "$probe_dir"' EXIT
mk() { printf '#!/bin/sh\nprintf "%%s\\n" %q\n' "$2" > "$probe_dir/$1"; chmod +x "$probe_dir/$1"; }
# The real output shapes of the three CLIs this has to read, so a change in any of their banners
# fails here rather than on a server.
mk claude-like "2.1.269 (Claude Code)"
mk codex-like  "codex-cli 0.154.0"
mk node-like   "v22.11.0"
mk silent-like ""
for probe in "claude-like 2.1.269" "codex-like 0.154.0" "node-like 22.11.0"; do
    read -r bin want <<<"$probe"
    got="$(wheel_version_of "$probe_dir/$bin")"
    if [ "$got" = "$want" ]; then ok; else bad "version_of $bin = '$got', want '$want'"; fi
done
got="$(wheel_version_of "$probe_dir/silent-like")"
if [ -z "$got" ]; then ok; else bad "a tool printing no version must yield empty, got '$got'"; fi

echo "== the pins themselves =="

# The whole point of having a floor: a careless bump of the pin must not drop a box under the
# version PR #64's headless OAuth refresh needs.
if wheel_version_ge "$WHEEL_CLAUDE_VERSION" "$WHEEL_CLAUDE_MIN"; then ok
else bad "toolchain.env pins claude $WHEEL_CLAUDE_VERSION, below its own floor $WHEEL_CLAUDE_MIN"; fi

# PR #64's requirement, pinned as a literal so that lowering WHEEL_CLAUDE_MIN is a deliberate edit
# to a test rather than a quiet change to a config value.
if [ "$WHEEL_CLAUDE_MIN" = "2.1.269" ]; then ok
else bad "WHEEL_CLAUDE_MIN is $WHEEL_CLAUDE_MIN, not the 2.1.269 that PR #64's headless OAuth refresh requires"; fi

# The installer building the board with a different pnpm than the repo declares is how a lockfile
# resolves differently on a server than it does in CI.
declared="$(node -e 'process.stdout.write(require("'"$root"'/web/package.json").packageManager||"")' 2>/dev/null || echo "")"
if [ -z "$declared" ]; then
    echo "  note  could not read web/package.json packageManager (no node) — pnpm pin not cross-checked"
elif [ "$declared" = "pnpm@$WHEEL_PNPM_VERSION" ]; then ok
else bad "toolchain.env pins pnpm $WHEEL_PNPM_VERSION but web/package.json declares $declared"; fi

engines="$(node -e 'process.stdout.write((require("'"$root"'/web/package.json").engines||{}).node||"")' 2>/dev/null || echo "")"
if [ -z "$engines" ]; then
    echo "  note  could not read web/package.json engines.node (no node) — node pin not cross-checked"
elif [ "$engines" = "${WHEEL_NODE_MAJOR}.x" ]; then ok
else bad "toolchain.env pins node major $WHEEL_NODE_MAJOR but web/package.json engines.node is $engines"; fi

echo
echo "native-toolchain: $pass passed, $fail failed"
[ "$fail" = 0 ]
