#!/usr/bin/env bash
# Boot wheel-engine:dev, run the e2e tool executor/importer HTTP campaign, tear down. PM: removed after.
set -u
NAME="wheel-adv-toolhttp-$$"; PORT=7027; SECRET="$(openssl rand -hex 24)"; VK="$(head -c32 /dev/urandom|base64)"; PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1 || true; }; trap cleanup EXIT
echo "== boot $NAME (image $(docker image inspect wheel-engine:dev --format '{{.Created}}')) =="
docker run -d --name "$NAME" -p ${PORT}:7000 -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" \
  -e WHEEL_ENGINE_SECRET="$SECRET" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json wheel-engine:dev >/dev/null || { echo "run failed"; exit 2; }
for i in $(seq 1 40); do code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$code" = 200 ]&&break; sleep 0.5; done
[ "${code:-}" = 200 ] || { echo "unhealthy ($code)"; docker logs "$NAME" 2>&1|tail -20; exit 2; }
echo healthy.
URL="$URL" ESEC="$SECRET" NAME="$NAME" python3 "$(dirname "$0")/t_tool_http.py"
rc=$?; echo "== probe exit=$rc =="; exit $rc
