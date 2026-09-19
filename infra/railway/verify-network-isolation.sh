#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Assert the §5b network segmentation actually holds for the deployed wheel-host, by MEASURING it —
# redteam/findings/048-s5b-network-isolation-not-deployed.md was exactly a claim nobody measured.
# Run after the migration in docs/proposals/network-isolation-048.md and periodically after, since
# Railway project membership is dashboard state this repo's CI cannot see change.
#
# Exit 0  isolated, and every probe demonstrably ran (controls passed).
# Exit 1  wheel-host can resolve or connect to Postgres / wheel-api: finding 048 is open.
# Exit 2  could not measure (not logged in, wrong project, service down, tool missing, control
#         failed). NEVER reported as isolated: a probe that did not run proves nothing.
#
# Probes run twice, as root and as an unprivileged uid from the sandbox range, because the property
# is "an agent cannot reach", not "root cannot".
#
#   railway link -p wheel-host -s wheel-host
#   ./infra/railway/verify-network-isolation.sh          # WHEEL_PROBE_UID=20000 by default
set -uo pipefail

PROBE_UID=${WHEEL_PROBE_UID:-20000}
TARGETS=("postgres:postgres.railway.internal:5432" "wheel-api:wheel-api.railway.internal:8080")
# Positive control: a public name that MUST resolve and accept a connection, so a "refused" above
# means the network said no, not that getent or /dev/tcp is missing from the image.
CONTROL="control:one.one.one.one:443"

# Runs on the host. One line per probe: `PROBE <mode> <name> <RESOLVED|UNRESOLVED> <CONNECTED|REFUSED>`.
remote_script() {
  local mode=$1 t
  local body='probe() { n=$1; h=$2; p=$3; if getent hosts "$h" >/dev/null 2>&1; then r=RESOLVED; else r=UNRESOLVED; fi; if timeout 3 bash -c "</dev/tcp/$h/$p" 2>/dev/null; then c=CONNECTED; else c=REFUSED; fi; echo "PROBE '"$mode"' $n $r $c"; };'
  for t in "$CONTROL" "${TARGETS[@]}"; do
    IFS=: read -r n h p <<<"$t"
    body+=" probe $n $h $p;"
  done
  if [ "$mode" = uid ]; then
    printf "setpriv --reuid=%s --regid=%s --clear-groups bash -c '%s'" "$PROBE_UID" "$PROBE_UID" "$body"
  else
    printf "bash -c '%s'" "$body"
  fi
}

status=0
for mode in root uid; do
  out=$(railway ssh --service wheel-host "$(remote_script "$mode")" 2>/dev/null | tr -d '\r') || out=""
  for t in "$CONTROL" "${TARGETS[@]}"; do
    IFS=: read -r n h p <<<"$t"
    line=$(grep -x "PROBE $mode $n [A-Z]* [A-Z]*" <<<"$out" | head -1)
    if [ -z "$line" ]; then
      echo "ERROR: the $mode probe for $n did not run (not logged in, wrong project linked, or the service is down?)" >&2
      exit 2
    fi
    read -r _ _ _ resolved connected <<<"$line"
    if [ "$n" = control ]; then
      if [ "$resolved" != RESOLVED ] || [ "$connected" != CONNECTED ]; then
        echo "ERROR: the $mode positive control failed ($resolved/$connected): this probe cannot tell isolation from a missing tool" >&2
        exit 2
      fi
      continue
    fi
    if [ "$resolved" = RESOLVED ] || [ "$connected" = CONNECTED ]; then
      echo "FAIL: as $mode, wheel-host $resolved/$connected $h:$p — it is not network-isolated from that project" >&2
      status=1
    else
      echo "ok: as $mode, $h:$p is unreachable from wheel-host"
    fi
  done
done
exit $status
