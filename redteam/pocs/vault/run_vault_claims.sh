#!/usr/bin/env bash
# Vault structural claims (PM's campaign, minus F015 which is filed): encryption-at-rest, write-only
# API, ambiguity at wire-creation AND PUT (409 ambiguous_credential), auth-source correctness.
# Driven on the engine control plane (/v1/* with the engine secret). PM: container removed on exit.
set -u
NAME="wheel-adv-vc-$$"; PORT=7025; VK="$(head -c32 /dev/urandom|base64)"; SECRET="$(openssl rand -hex 24)"
URL="http://127.0.0.1:${PORT}"; ES=(-H "authorization: Bearer ${SECRET}"); SEC="sk-ant-PLAINTEXT-marker-9f3a2b"
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1||true; }; trap cleanup EXIT
docker run -d --name "$NAME" -p ${PORT}:7000 -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$(uuidgen)" \
  -e WHEEL_ENGINE_SECRET="$SECRET" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json wheel-engine:dev >/dev/null||{ echo "run failed";exit 2;}
for i in $(seq 1 40);do code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true);[ "$code" = 200 ]&&break;sleep 0.5;done
[ "$code" = 200 ]||{ echo "unhealthy $code";exit 2;}
J='-H content-type:application/json'
mkj(){ curl -s "${ES[@]}" $J -X POST "$URL/v1/nodes" -d "$1" >/dev/null;}
mkj '{"name":"v1","type":"vault","config":{"keys":["ANTHROPIC_API_KEY"]}}'
mkj '{"name":"v2","type":"vault","config":{"keys":["ANTHROPIC_API_KEY"]}}'
mkj '{"name":"v3","type":"vault","config":{"keys":["OTHER_KEY"]}}'
mkj '{"name":"worker","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
b="$(curl -s "${ES[@]}" "$URL/v1/board")"
id(){ echo "$b"|python3 -c "import sys,json;print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$1'))";}
v1=$(id v1); v2=$(id v2); v3=$(id v3); aid=$(id worker)
curl -s "${ES[@]}" $J -X PUT "$URL/v1/vault/$v1/ANTHROPIC_API_KEY" -d "{\"value\":\"$SEC\"}" >/dev/null
F=0; ok(){ echo "PASS $1"; }; bad(){ echo "FAIL(FINDING) $1"; F=$((F+1)); }

echo "== claim: encryption at rest (raw sqlite + WAL/SHM have no plaintext) =="
# grep -o|wc -l avoids grep -c's exit-1-on-zero footgun; scan the main db and both sidecars.
DBHIT=$(docker exec "$NAME" sh -lc "cat /data/wheel.db /data/wheel.db-wal /data/wheel.db-shm 2>/dev/null | grep -a -o '$SEC' | wc -l | tr -d ' '")
[ "${DBHIT:-1}" = 0 ] && ok "plaintext absent from db + wal + shm" || bad "plaintext '$SEC' found on disk (count=$DBHIT)"

echo "== claim: write-only API (no GET route; board carries names only) =="
gs=$(curl -s -o /dev/null -w '%{http_code}' "${ES[@]}" "$URL/v1/vault/$v1/ANTHROPIC_API_KEY")
[ "$gs" = 404 ]||[ "$gs" = 405 ] && ok "GET /v1/vault/:id/:key -> $gs (no read route)" || bad "GET /v1/vault/:id/:key -> $gs (a value-read route exists?)"
b2="$(curl -s "${ES[@]}" "$URL/v1/board")"
echo "$b2"|grep -q "$SEC" && bad "/v1/board leaks the secret value" || ok "/v1/board carries no vault value"
echo "$b2"|grep -q "ANTHROPIC_API_KEY" && ok "/v1/board shows key NAME only" || echo "  note: key name not on board (config-only)"

echo "== claim: ambiguity at WIRE creation (409 ambiguous_credential) =="
curl -s "${ES[@]}" $J -X POST "$URL/v1/wires" -d "{\"from\":\"$aid\",\"to\":\"$v1\",\"type\":\"read\"}" >/dev/null   # ok
w2=$(curl -s -o /dev/null -w '%{http_code}' "${ES[@]}" $J -X POST "$URL/v1/wires" -d "{\"from\":\"$aid\",\"to\":\"$v2\",\"type\":\"read\"}")
[ "$w2" = 409 ] && ok "2nd vault with same key -> 409 at wire creation" || bad "2nd vault with dup key -> $w2 (expected 409)"

echo "== claim: ambiguity at PUT (409 ambiguous_credential) =="
# worker is wired to v1(ANTHROPIC_API_KEY) and v3(OTHER_KEY). PUT ANTHROPIC_API_KEY into v3 -> would
# collide across worker's wired vaults -> must 409.
curl -s "${ES[@]}" $J -X POST "$URL/v1/wires" -d "{\"from\":\"$aid\",\"to\":\"$v3\",\"type\":\"read\"}" >/dev/null   # ok (OTHER_KEY only)
ps=$(curl -s -o /dev/null -w '%{http_code}' "${ES[@]}" $J -X PUT "$URL/v1/vault/$v3/ANTHROPIC_API_KEY" -d "{\"value\":\"x\"}")
[ "$ps" = 409 ] && ok "PUT of a key that collides across an agent's vaults -> 409" || bad "PUT colliding key -> $ps (expected 409 ambiguous_credential)"

echo "== claim: auth source correctness (mode env, source = vault name) =="
au=$(curl -s "${ES[@]}" "$URL/v1/agents/$aid/auth")
echo "  auth: $au"
echo "$au"|grep -q '"v1"' && ok "auth source names the vault supplying ANTHROPIC_API_KEY (v1)" || echo "  note: source not 'v1' — inspect (may report differently)"

echo "== total findings: $F =="; exit $([ "$F" = 0 ]&&echo 0||echo 1)
