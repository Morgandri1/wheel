#!/usr/bin/env bash
# PM's traced S1: a vault that only DECLARES a credential key (config.keys) with NO stored value makes
# GET /v1/agents/:id/auth report authenticated:true mode:env. This maps the discrepancy: auth-status uses
# offered_keys (declared UNION stored) while the child env uses stored-only — so an agent reports authed
# but gets no credential. Enumerate what a declared-but-empty key does and does NOT unlock. Throwaway project.
set -u
NAME="wheel-adv-declempty-$$"; PORT=7060; SECRET="$(openssl rand -hex 24)"; VK="$(head -c32 /dev/urandom|base64)"; PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"; ES=(-H "authorization: Bearer ${SECRET}" -H 'content-type: application/json')
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1 || true; }; trap cleanup EXIT
docker run -d --name "$NAME" -p ${PORT}:7000 -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" \
  -e WHEEL_ENGINE_SECRET="$SECRET" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json wheel-engine:dev >/dev/null || { echo "run failed"; exit 2; }
for i in $(seq 1 40); do c=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$c" = 200 ]&&break; sleep 0.5; done
[ "${c:-}" = 200 ] || { echo "unhealthy ($c)"; docker logs "$NAME" 2>&1|tail -20; exit 2; }
echo "healthy (image $(docker image inspect wheel-engine:dev --format '{{.Created}}'))"
mk(){ curl -s "${ES[@]}" -X POST "$URL/v1/nodes" -d "$1" >/dev/null; }
idof(){ curl -s "${ES[@]}" "$URL/v1/board"|python3 -c "import sys,json;print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$1'))"; }

# vault v DECLARES the credential key but NO value is ever PUT. agent a wired read->v.
mk '{"name":"v","type":"vault","config":{"keys":["CLAUDE_CODE_OAUTH_TOKEN"]}}'
mk '{"name":"a","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
VID=$(idof v); AID=$(idof a)
curl -s "${ES[@]}" -X POST "$URL/v1/wires" -d "{\"from\":\"$AID\",\"to\":\"$VID\",\"type\":\"read\"}" >/dev/null
echo "v=$VID a=$AID (v declares CLAUDE_CODE_OAUTH_TOKEN, NO value stored)"

echo; echo "== 1) THE S1: GET /v1/agents/a/auth with a declared-but-empty credential =="
curl -s "${ES[@]}" "$URL/v1/agents/$AID/auth"; echo

echo; echo "== 2) GET /v1/board — does the vault show the declared key, any value? =="
curl -s "${ES[@]}" "$URL/v1/board" | python3 -c 'import sys,json
b=json.load(sys.stdin)
for n in b["nodes"]:
    if n["name"]=="v": print("   vault config:",json.dumps(n["config"]))'

echo; echo "== 3) secret get on the declared-but-empty key (agent token) — value or not-found? =="
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$AID/start" >/dev/null; sleep 2
TOK=$(docker exec "$NAME" sh -lc "cat /data/run/$AID/token 2>/dev/null")
echo "   agent status after start: $(curl -s "${ES[@]}" "$URL/v1/board"|python3 -c 'import sys,json;print(next(n.get("state",{}).get("status") for n in json.load(sys.stdin)["nodes"] if n["name"]=="a"))' 2>/dev/null)"
echo "   secret get v/CLAUDE_CODE_OAUTH_TOKEN: $(curl -s -H "authorization: Bearer $TOK" "$URL/v1/cli/secret?addr=v/CLAUDE_CODE_OAUTH_TOKEN")"

echo; echo "== 4) child env: is CLAUDE_CODE_OAUTH_TOKEN exported (empty?) or absent? (read as child uid) =="
CPID=$(docker exec "$NAME" sh -lc 'for p in $(ls /proc|grep -E "^[0-9]+$"); do [ "$p" = 1 ]&&continue; ppid=$(grep -m1 "^PPid:" /proc/$p/status 2>/dev/null|tr -dc 0-9); [ "$ppid" = 1 ]||continue; c=$(cat /proc/$p/comm 2>/dev/null); case "$c" in claude|node|codex) echo "$p $(grep -m1 ^Uid: /proc/$p/status|awk "{print \$2}")"; break;; esac; done')
if [ -n "$CPID" ]; then set -- $CPID; echo "   child pid=$1 uid=$2"; docker exec -u "$2" "$NAME" sh -lc "tr '\0' '\n' </proc/$1/environ 2>/dev/null | grep -iE 'CLAUDE_CODE_OAUTH_TOKEN|ANTHROPIC' || echo '   (no CLAUDE_CODE_OAUTH_TOKEN / ANTHROPIC in child env)'"; else echo "   (no child caught)"; fi

echo; echo "== 5) FACE #5 (PM overruled 409->warning): does an EMPTY declaration in v 409-block a REAL cred in v2? =="
curl -s "${ES[@]}" -X POST "$URL/v1/nodes" -d '{"name":"v2","type":"vault","config":{"keys":["CLAUDE_CODE_OAUTH_TOKEN"]}}' >/dev/null
V2=$(idof v2)
curl -s "${ES[@]}" -X PUT "$URL/v1/vault/$V2/CLAUDE_CODE_OAUTH_TOKEN" -d '{"value":"sk-ant-oat01-real"}' >/dev/null
body="{\"from\":\"$AID\",\"to\":\"$V2\",\"type\":\"read\"}"
resp=$(curl -s -w ' [HTTP %{http_code}]' "${ES[@]}" -X POST "$URL/v1/wires" -d "$body")
echo "   wire a->v2 (v2 has the REAL value; v is declared-empty): $resp"
echo "   ACCEPTANCE: PM overruled SDK -> this should be a WARNING (wire created), NOT a 409. A 409 = not yet implemented."
echo "== done =="
