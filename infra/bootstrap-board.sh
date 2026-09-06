#!/usr/bin/env bash
# Creates the Wheel-on-Wheel board (docs/WHEEL-ON-WHEEL.md) through the public API. Idempotent by project name.
set -euo pipefail
: "${WHEEL_API:?set WHEEL_API}"; : "${WHEEL_EMAIL:?}"; : "${WHEEL_PASSWORD:?}"
case "$WHEEL_API" in http://localhost*|http://127.0.0.1*) ;; *) [ "${WHEEL_ALLOW_REMOTE:-}" = 1 ] || { echo "refusing non-loopback WHEEL_API=$WHEEL_API without WHEEL_ALLOW_REMOTE=1" >&2; exit 2; } ;; esac
PROJECT_NAME="${PROJECT_NAME:-wheel-dev}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

api() { curl -sS --fail-with-body -H 'content-type: application/json' "$@"; }
TOKEN=$(api -X POST -d "{\"email\":\"$WHEEL_EMAIL\",\"password\":\"$WHEEL_PASSWORD\"}" "$WHEEL_API/v1/auth/login" | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])')
auth=(-H "x-auth-token: $TOKEN")

PID=$(api "${auth[@]}" "$WHEEL_API/v1/projects" | python3 -c "import sys,json;print(next((p['id'] for p in json.load(sys.stdin) if p['name']=='$PROJECT_NAME'),''))")
if [ -z "$PID" ]; then
  PID=$(api "${auth[@]}" -X POST -d "{\"name\":\"$PROJECT_NAME\"}" "$WHEEL_API/v1/projects" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')
fi
proj=("${auth[@]}" -H "x-project-id: $PID")
api "${proj[@]}" -X POST "$WHEEL_API/v1/projects/$PID/start" >/dev/null
ENGINE="$WHEEL_API/v1/projects/$PID/engine/v1"

node() { # name type x y config-json → id (create or reuse)
  local existing; existing=$(api "${proj[@]}" "$ENGINE/board" | python3 -c "import sys,json;print(next((n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$1'),''))")
  if [ -n "$existing" ]; then echo "$existing"; return; fi
  python3 - "$1" "$2" "$3" "$4" "$5" <<'PY' | api "${proj[@]}" -X POST -d @- "$ENGINE/nodes" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])'
import sys,json; n,t,x,y,c=sys.argv[1:6]; print(json.dumps({"name":n,"type":t,"position":{"x":float(x),"y":float(y)},"config":json.loads(c)}))
PY
}
wire() { api "${proj[@]}" -X POST -d "{\"from\":\"$1\",\"to\":\"$2\",\"type\":\"$3\"}" "$ENGINE/wires" >/dev/null 2>&1 || true; }
md() { python3 -c 'import sys,json;print(json.dumps({"markdown":open(sys.argv[1]).read()}))' "$1"; }
prompt() { python3 -c 'import sys,json,os;p=sys.argv[1];s=open(p).read() if os.path.exists(p) else sys.argv[2];print(json.dumps({"harness":"claude","system_prompt":s,"run_on_startup":True,"ephemeral_context":sys.argv[3]=="1"}))' "$1" "$2" "$3"; }

CONTRACT=$(node contract ctx 0 0 "$(md "$ROOT/docs/ARCHITECTURE.md")")
WORKFLOW=$(node workflow ctx 0 200 "$(python3 -c 'import json,re;t=open("docs/WHEEL-ON-WHEEL.md").read();print(json.dumps({"markdown":t[t.index("## Working rules"):t.index("## Sizing")]}))')")
SECRETS=$(node secrets vault 0 400 '{"keys":["GITHUB_TOKEN","CLAUDE_CODE_OAUTH_TOKEN"]}')
REPORTS=$(node reports table 0 600 '{"columns":[{"name":"ts","type":"text"},{"name":"author","type":"text"},{"name":"kind","type":"text"},{"name":"body","type":"json"}]}')
AG_FILE="$(mktemp)"; trap 'rm -f "$AG_FILE"' EXIT
agent_id() { awk -v r="$1" '$1==r {print $2}' "$AG_FILE"; }
y=0; for role in pm sdk api web qa adversary; do
  brief="$ROOT/docs/plans/$role.brief.md"; eph=0; [ "$role" = pm ] && eph=1
  id=$(node "$role" agent 500 "$y" "$(prompt "$brief" "You are the $role agent developing Wheel on Wheel. Follow the contract and workflow contexts." "$eph")")
  echo "$role $id" >> "$AG_FILE"; y=$((y+150))
done
PM_ID=$(agent_id pm)
for role in pm sdk api web qa adversary; do
  id=$(agent_id "$role")
  wire "$CONTRACT" "$id" send; wire "$WORKFLOW" "$id" send
  wire "$id" "$SECRETS" read; wire "$id" "$REPORTS" write
  [ "$role" != pm ] && { wire "$PM_ID" "$id" send; wire "$id" "$PM_ID" send; }
done
wire "$(agent_id sdk)" "$(agent_id api)" send; wire "$(agent_id api)" "$(agent_id sdk)" send
wire "$(agent_id sdk)" "$(agent_id web)" send; wire "$(agent_id web)" "$(agent_id sdk)" send
echo "project $PID ready — set the vault secrets in the web app, then start the agents (run_on_startup starts them on the next project start)."
