#!/usr/bin/env bash
# F003/F007 LIVE — boot the combined image as WHEEL_ROLE=host + SANDBOX_BACKEND=process (the prod
# shape: every tenant's engine as its own uid in ONE container on ONE kernel), create two tenant
# projects A and B through the real host API, then run the cross-tenant isolation probe AS project
# A's uid against project B's data/socket/proc/secrets. PM: remove containers after.
#
# Requires wheel-engine:dev built from main >= cd1305a (starts as root; entrypoint keeps host root
# for process mode and setuids each engine). Build with `make engine-image` if stale.
set -u
IMG="${IMG:-wheel-engine:dev}"
NAME="wheel-adv-host-$$"
PORT="${PORT:-7110}"
VOL="wheel-adv-host-$$-data"
HSEC="$(openssl rand -hex 24)"          # >= 16 chars
URL="http://127.0.0.1:${PORT}"
HB=(-H "authorization: Bearer ${HSEC}")
PID_A="$(uuidgen | tr 'A-Z' 'a-z')"
PID_B="$(uuidgen | tr 'A-Z' 'a-z')"

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; docker volume rm "$VOL" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "== boot $NAME (host role, process backend, :$PORT) =="
docker volume create "$VOL" >/dev/null || { echo "volume create failed"; exit 2; }
docker run -d --name "$NAME" -p ${PORT}:7100 -v "$VOL":/data \
  -e WHEEL_ROLE=host -e SANDBOX_BACKEND=process -e WHEEL_HOST_SECRET="$HSEC" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json \
  "$IMG" >/dev/null || { echo "docker run failed"; exit 2; }

for i in $(seq 1 40); do
  code=$(curl -s -o /dev/null -w '%{http_code}' "${HB[@]}" "$URL/host/v1/healthz" 2>/dev/null || true)
  [ "$code" = "200" ] && break
  # host may exit early if it refuses the config — surface that fast
  docker ps -q -f name="$NAME" | grep -q . || { echo "host exited during startup:"; docker logs "$NAME" 2>&1 | tail -20; exit 2; }
  sleep 0.5
done
[ "$code" = "200" ] || { echo "host not healthy (code=$code); logs:"; docker logs "$NAME" 2>&1 | tail -25; exit 2; }
echo "host healthy."

put_start() {  # $1 = project id
  local id="$1"
  local esec vkey
  esec="$(openssl rand -hex 24)"
  vkey="$(openssl rand -base64 32)"
  curl -s "${HB[@]}" -H 'content-type: application/json' -X PUT "$URL/host/v1/projects/$id" \
    -d "{\"engine_secret\":\"$esec\",\"vault_key\":\"$vkey\"}" >/dev/null
  local out; out="$(curl -s "${HB[@]}" -X POST "$URL/host/v1/projects/$id/start")"
  echo "  start $id -> $out"
}

echo "== create + start two tenants =="
put_start "$PID_A"
put_start "$PID_B"

echo "== layout (as root, for orientation only) =="
docker exec "$NAME" sh -lc 'ls -la /data/projects 2>/dev/null; echo ---; ls -la /run/wheel 2>/dev/null'
echo "== engine processes (pid uid comm) =="
PS="$(docker exec "$NAME" sh -lc 'ps -eo pid,uid,comm 2>/dev/null | grep "wheel-engine"')"
echo "$PS"

# The engine args are just "wheel-engine" (project id travels in env, not argv), so map pid→project
# by UID: A got the first slot (UID_RANGE_START), B the next (+UID_STRIDE). Ownership of
# /data/projects/<id> (shown above) is the ground truth for which uid is which project.
UID_A="$(docker exec "$NAME" sh -lc "stat -c '%u' /data/projects/$PID_A")"
UID_B="$(docker exec "$NAME" sh -lc "stat -c '%u' /data/projects/$PID_B")"
OSPID_A="$(echo "$PS" | awk -v u="$UID_A" '$2==u{print $1}' | head -1)"
OSPID_B="$(echo "$PS" | awk -v u="$UID_B" '$2==u{print $1}' | head -1)"
echo "UID_A=$UID_A OSPID_A=$OSPID_A   UID_B=$UID_B OSPID_B=$OSPID_B"

echo "== probe (as uid A, victim = project B) =="
WHEEL_HOST_CONTAINER="$NAME" WHEEL_UID_A="$UID_A" WHEEL_PID_A="$PID_A" WHEEL_PID_B="$PID_B" \
  WHEEL_OSPID_A="$OSPID_A" WHEEL_OSPID_B="$OSPID_B" \
  python3 "$(dirname "$0")/t_process_backend_isolation.py"
rc=$?
echo "== probe exit=$rc =="
exit $rc
