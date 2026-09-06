#!/usr/bin/env bash
# Boot a throwaway wheel-engine:dev, build a vault board, PUT a secret, live-check that the
# plaintext never lands in /data/wheel.db, extract node tokens, run t_vault.py, tear down.
# PM: containers removed after (trap).
set -u
NAME="wheel-adv-vault-$$"
PORT=7020
SECRET="$(openssl rand -hex 24)"
VAULTKEY="$(openssl rand 32 | base64)"          # 32 bytes → base64 for WHEEL_VAULT_KEY
PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"
SECRET1="sk-ant-VAULT-CAMPAIGN-$(openssl rand -hex 8)"   # distinctive plaintext
ES=(-H "authorization: Bearer ${SECRET}")

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "== boot $NAME (engine role, :$PORT) =="
docker run -d --name "$NAME" -p ${PORT}:7000 \
  -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" -e WHEEL_ENGINE_SECRET="$SECRET" \
  -e WHEEL_VAULT_KEY="$VAULTKEY" \
  -e WHEEL_LISTEN="tcp://0.0.0.0:7000" -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json \
  wheel-engine:dev >/dev/null || { echo "docker run failed"; exit 2; }

for i in $(seq 1 40); do
  code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null || true)
  [ "$code" = "200" ] && break; sleep 0.5
done
[ "${code:-}" = "200" ] || { echo "engine not healthy (code=${code:-}); logs:"; docker logs "$NAME" 2>&1 | tail -25; exit 2; }
echo "healthy."

mk() { curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/nodes" -d "$1" >/dev/null; }
mk '{"name":"a","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
mk '{"name":"b","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
mk '{"name":"v1","type":"vault","config":{"keys":["K1"]}}'
mk '{"name":"v2","type":"vault","config":{"keys":["K1"]}}'   # declares K1 too → dup with v1 for agent a
mk '{"name":"v3","type":"vault","config":{"keys":[]}}'

board="$(curl -s "${ES[@]}" "$URL/v1/board")"
id() { echo "$board" | python3 -c "import sys,json;print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$1'))"; }
A_ID=$(id a); B_ID=$(id b); V1=$(id v1); V2=$(id v2); V3=$(id v3)
echo "a=$A_ID b=$B_ID v1=$V1 v2=$V2 v3=$V3"

# PUT the secret + a recognised credential key (for the auth-source check). Both into v1.
curl -s "${ES[@]}" -H 'content-type: application/json' -X PUT "$URL/v1/vault/$V1/K1" -d "{\"value\":\"$SECRET1\"}" >/dev/null
curl -s "${ES[@]}" -H 'content-type: application/json' -X PUT "$URL/v1/vault/$V1/ANTHROPIC_API_KEY" -d "{\"value\":\"$SECRET1-cred\"}" >/dev/null

# TARGET 1 — live encryption-at-rest: the plaintext must NOT appear in the db file.
HITS=$(docker exec "$NAME" sh -lc "grep -a -c -o '$SECRET1' /data/wheel.db 2>/dev/null | head -1" 2>/dev/null || echo 0)
HITS="${HITS:-0}"
echo "db plaintext hits for SECRET1 = $HITS (expect 0)"

# wire a→v1 read (b stays unwired)
curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/wires" -d "{\"from\":\"$A_ID\",\"to\":\"$V1\",\"type\":\"read\"}" >/dev/null

curl -s "${ES[@]}" -X POST "$URL/v1/agents/$A_ID/start" >/dev/null
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$B_ID/start" >/dev/null
sleep 2

TOK_A=$(docker exec "$NAME" sh -lc "cat /data/run/$A_ID/token 2>/dev/null")
TOK_B=$(docker exec "$NAME" sh -lc "cat /data/run/$B_ID/token 2>/dev/null")
echo "tokA len=${#TOK_A} tokB len=${#TOK_B}"

echo "== probe =="
WHEEL_ENGINE_URL="$URL" WHEEL_ENGINE_SECRET="$SECRET" \
  V1="$V1" V2="$V2" V3="$V3" A_ID="$A_ID" B_ID="$B_ID" \
  TOK_A="$TOK_A" TOK_B="$TOK_B" SECRET1="$SECRET1" DB_PLAINTEXT_HITS="$HITS" \
  python3 "$(dirname "$0")/t_vault.py"
rc=$?
echo "== probe exit=$rc =="
exit $rc
