#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# The isolation check must not be able to lie in either direction. It runs against a stub `railway`
# that plays back what a host would print, in the cases that matter — including the two an earlier
# version got wrong (`UNRESOLVED` matched `RESOLVED`, and a probe that never ran reported "isolated").
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
script="$here/../railway/verify-network-isolation.sh"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/railway" <<'STUB'
#!/usr/bin/env bash
# Plays back a canned host. CASE picks the story; the command string says which mode is asking.
cmd="${*: -1}"
mode=root; [[ "$cmd" == *setpriv* ]] && mode=uid
[ "$CASE" = broke ] && exit 1
[ "$CASE" = silent ] && exit 0
[ "$CASE" = no-uid ] && [ "$mode" = uid ] && exit 0
line() { echo "PROBE $mode $1 $2 $3"; }
if [ "$CASE" = no-control ]; then line control UNRESOLVED REFUSED; else line control RESOLVED CONNECTED; fi
pg="UNRESOLVED REFUSED"; api="UNRESOLVED REFUSED"
[ "$CASE" = resolves ] && pg="RESOLVED REFUSED"
[ "$CASE" = connects ] && api="UNRESOLVED CONNECTED"
[ "$CASE" = uid-reaches ] && [ "$mode" = uid ] && pg="RESOLVED CONNECTED"
line postgres $pg; line wheel-api $api
STUB
chmod +x "$tmp/railway"

fail=0
expect() { # case, expected exit
  CASE=$1 PATH="$tmp:$PATH" bash "$script" >/dev/null 2>&1; got=$?
  if [ "$got" = "$2" ]; then echo "ok   $1 -> $got"; else echo "FAIL $1: expected exit $2, got $got"; fail=1; fi
}
expect isolated 0
expect resolves 1
expect connects 1
expect uid-reaches 1
expect broke 2
expect silent 2
expect no-uid 2
expect no-control 2
exit $fail
