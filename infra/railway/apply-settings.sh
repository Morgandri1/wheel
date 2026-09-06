#!/usr/bin/env bash
# Apply infra/railway/settings.json to the Railway services.
#
# Railway deprecated railway.toml (see README.md), so build and deploy settings live only in the
# platform's own database. This script makes that state reproducible: settings.json is the desired
# state in git, and running this asserts it. Idempotent — safe to re-run any time, and the way to
# undo a hand-edit someone made in the dashboard.
#
# Auth comes from the Railway CLI's own session, so `railway login` is the only prerequisite.
set -euo pipefail

cd "$(dirname "$0")"
API=https://backboard.railway.com/graphql/v2
CONFIG=~/.railway/config.json

[ -f "$CONFIG" ] || { echo "run 'railway login' first: no $CONFIG" >&2; exit 1; }
TOKEN=$(python3 -c "import json,os;print(json.load(open(os.path.expanduser('$CONFIG')))['user']['accessToken'])")

python3 - "$API" "$TOKEN" <<'PY'
import json, subprocess, sys

api, token = sys.argv[1], sys.argv[2]
cfg = json.load(open("settings.json"))
env = cfg["environment"]

MUTATION = ("mutation($id:String!,$env:String!,$input:ServiceInstanceUpdateInput!)"
            "{ serviceInstanceUpdate(serviceId:$id, environmentId:$env, input:$input) }")

def call(body):
    out = subprocess.run(
        ["curl", "-sS", api,
         "-H", "Authorization: Bearer " + token,
         "-H", "content-type: application/json",
         "-d", json.dumps(body)],
        capture_output=True, text=True, check=True).stdout
    return json.loads(out)

failed = False
for name, svc in cfg["services"].items():
    sid = svc.pop("serviceId")
    # One field per call: the API rejects a whole input object if any single field is unsupported,
    # which would silently drop the rest and leave a half-applied service.
    for field, value in svc.items():
        r = call({"query": MUTATION, "variables": {"id": sid, "env": env, "input": {field: value}}})
        if "errors" in r:
            failed = True
            print(f"  {name}.{field}: REJECTED — {r['errors'][0]['message']}", file=sys.stderr)
        else:
            print(f"  {name}.{field}: ok")

sys.exit(1 if failed else 0)
PY
