#!/usr/bin/env bash
# (b) PM's S1: a vault that only DECLARES CLAUDE_CODE_OAUTH_TOKEN (no value stored). Does GET agent/auth
# report authenticated:true mode:env? What does declared-but-empty unlock — child env, the start gate?
# Throwaway project (my own container), not wheel-dev.
set -u
NAME="wheel-adv-decl-$$"; PORT=7062; SECRET="$(openssl rand -hex 24)"; VK="$(head -c32 /dev/urandom|base64)"; PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"; ES=(-H "authorization: Bearer ${SECRET}" -H 'content-type: application/json')
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1 || true; }; trap cleanup EXIT
docker run -d --name "$NAME" -p ${PORT}:7000 -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" \
  -e WHEEL_ENGINE_SECRET="$SECRET" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json -e WHEEL_ENV=dev wheel-engine:dev >/dev/null || { echo run failed; exit 2; }
for i in $(seq 1 40); do c=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$c" = 200 ]&&break; sleep 0.5; done
[ "${c:-}" = 200 ] || { echo unhealthy; docker logs "$NAME" 2>&1|tail; exit 2; }
echo "healthy (image $(docker image inspect wheel-engine:dev --format '{{.Created}}'))"
mk(){ curl -s "${ES[@]}" -X POST "$URL/v1/nodes" -d "$1" >/dev/null; }
# vault DECLARES the credential key but NO value is ever PUT
mk '{"name":"vempty","type":"vault","config":{"keys":["CLAUDE_CODE_OAUTH_TOKEN"]}}'
mk '{"name":"a","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
board="$(curl -s "${ES[@]}" "$URL/v1/board")"
VID=$(echo "$board"|python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="vempty"))')
AID=$(echo "$board"|python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="a"))')
curl -s "${ES[@]}" -X POST "$URL/v1/wires" -d "{\"from\":\"$AID\",\"to\":\"$VID\",\"type\":\"read\"}" >/dev/null
echo "vempty=$VID a=$AID (declared CLAUDE_CODE_OAUTH_TOKEN, NO value stored; a reads vempty)"

echo; echo "== 1) GET agent/auth for a declared-but-EMPTY credential =="
echo "  $(curl -s "${ES[@]}" "$URL/v1/agents/$AID/auth")"
echo "  vault key listing (should be names only; is the DECLARED key listed with no value?):"
echo "  $(curl -s "${ES[@]}" "$URL/v1/vault/$VID")"

echo; echo "== 2) start the agent — does declared-but-empty pass the start/lapsed gate? =="
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$AID/start" >/dev/null; sleep 2
echo "  agent state: $(curl -s "${ES[@]}" "$URL/v1/board"|python3 -c 'import sys,json
for n in json.load(sys.stdin)["nodes"]:
  if n["name"]=="a": print(n.get("state"))')"

echo; echo "== 3) child env — does the child actually GET a token, or is it empty/absent? =="
TOK=$(docker exec "$NAME" sh -lc "cat /data/run/$AID/token 2>/dev/null")
if [ -n "$TOK" ]; then
  pid=$(docker exec "$NAME" sh -lc 'for p in $(ls /proc|grep -E "^[0-9]+$"); do [ "$p" = 1 ]&&continue; pp=$(grep -m1 ^PPid: /proc/$p/status 2>/dev/null|tr -dc 0-9); [ "$pp" = 1 ]||continue; c=$(cat /proc/$p/comm 2>/dev/null); case "$c" in claude|node|codex) echo "$p $(grep -m1 ^Uid: /proc/$p/status|awk "{print \$2}")"; exit 0;; esac; done')
  set -- $pid
  if [ -n "${1:-}" ]; then
    echo "  child pid=$1 uid=$2; CLAUDE_CODE_OAUTH_TOKEN in its env:"
    docker exec -u "${2:-0}" "$NAME" sh -lc "tr '\0' '\n' </proc/$1/environ 2>/dev/null | grep -E 'CLAUDE_CODE_OAUTH_TOKEN|ANTHROPIC' || echo '   (NOT present in child env)'"
  else echo "  (no live PPid1 child caught — claude likely exited)"; fi
else echo "  (no token file — agent not running)"; fi
echo "== done =="
