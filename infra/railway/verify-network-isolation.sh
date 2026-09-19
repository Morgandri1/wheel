#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Assert the §5b network segmentation actually holds for the deployed wheel-host, rather than
# trusting it once and letting a future topology change re-open it silently
# (redteam/findings/048-s5b-network-isolation-not-deployed.md: the original gap was exactly this —
# nobody measured it, the contract was read instead). Run after the migration in
# docs/proposals/network-isolation-048.md, and periodically after (cron/CI) since Railway project
# membership is dashboard state, not something this repo's CI can see change.
#
# Exit 0: wheel-host cannot resolve or reach Postgres or wheel-api's private names — isolation holds.
# Exit 1: either resolved or connected — the finding is back, treat as a deploy failure, not a warning.
#
# Needs `railway login` (this SSHes into the wheel-host service, same prerequisite as
# apply-settings.sh) and the CLI linked to whichever project wheel-host is in:
#   railway link -p wheel-host -s wheel-host
#   ./infra/railway/verify-network-isolation.sh
set -euo pipefail

cd "$(dirname "$0")"

TARGETS=(
  "postgres.railway.internal:5432"
  "wheel-api.railway.internal:8080"
)

fail=0
for target in "${TARGETS[@]}"; do
  host=${target%%:*}
  port=${target##*:}
  # `getent hosts` resolves without connecting, so a name that merely resolves (but whose port
  # refuses) still fails this check — resolution alone is the private-network boundary breaking,
  # independent of whether anything is listening.
  if railway ssh --service wheel-host \
       "getent hosts $host >/dev/null 2>&1 && echo RESOLVED || echo UNRESOLVED" \
       2>/dev/null | tr -d '\r' | grep -q RESOLVED; then
    echo "FAIL: wheel-host can resolve $host — it is not network-isolated from that project" >&2
    fail=1
    continue
  fi
  # Belt and braces: even if resolution were somehow bypassed (a hardcoded IP), the port must not
  # accept a connection either. /dev/tcp is a bash builtin, no extra tooling needed on the image.
  if railway ssh --service wheel-host \
       "timeout 3 bash -c '</dev/tcp/$host/$port' 2>/dev/null && echo CONNECTED || echo REFUSED" \
       2>/dev/null | tr -d '\r' | grep -q CONNECTED; then
    echo "FAIL: wheel-host can connect to $target — it is not network-isolated from that project" >&2
    fail=1
    continue
  fi
  echo "ok: $target unreachable from wheel-host"
done

exit $fail
