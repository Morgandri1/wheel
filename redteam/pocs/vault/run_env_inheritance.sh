#!/usr/bin/env bash
# ⚠️  DETECTOR NOTE (F015): this scans all of /proc and matches its OWN `docker exec` probe
#     shell, which inherits the container `-e` env — so it OVER-REPORTS on a FIXED build.
#     It reproduces the leak on the VULNERABLE build only. To GATE a fix use verify_env_fix.sh
#     (targets PPid==1 children, reads environ as the child's own uid). See findings/015.
# F-VAULT-ENV: the engine spawns agent children with NO env_clear (supervisor/mod.rs:203),
# so every child inherits the engine's WHEEL_VAULT_KEY (project-wide AES-256 master key) and
# WHEEL_ENGINE_SECRET (the /v1/* control-plane bearer) in its OWN /proc/self/environ — readable
# by the process regardless of per-node uid isolation. This boots wheel-engine:dev, starts an
# agent, and snapshots the child environ to confirm the leak live. PM: container removed on exit.
set -u
NAME="wheel-adv-vaultenv-$$"
PORT=7021
VK="$(head -c32 /dev/urandom | base64)"          # the "project-wide" vault master key
SECRET="$(openssl rand -hex 24)"                  # WHEEL_ENGINE_SECRET
PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"
ES=(-H "authorization: Bearer ${SECRET}")
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "== boot $NAME (engine, :$PORT) with a known vault key =="
docker run -d --name "$NAME" -p ${PORT}:7000 \
  -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" -e WHEEL_ENGINE_SECRET="$SECRET" \
  -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json wheel-engine:dev >/dev/null \
  || { echo "docker run failed"; exit 2; }
for i in $(seq 1 40); do code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$code" = 200 ]&&break; sleep 0.5; done
[ "$code" = 200 ] || { echo "engine unhealthy ($code)"; docker logs "$NAME" 2>&1|tail -20; exit 2; }
echo "healthy."

mk(){ curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/nodes" -d "$1" >/dev/null; }
# a vault the agent is NOT wired to, plus an agent
mk '{"name":"othervault","type":"vault","config":{"keys":["ANTHROPIC_API_KEY"]}}'
mk '{"name":"worker","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
board="$(curl -s "${ES[@]}" "$URL/v1/board")"
vid=$(echo "$board"|python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="othervault"))')
aid=$(echo "$board"|python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="worker"))')
echo "othervault=$vid worker=$aid (worker has NO wire to othervault)"
# write a secret into the vault the agent cannot reach through wires
curl -s "${ES[@]}" -H 'content-type: application/json' -X PUT "$URL/v1/vault/$vid/ANTHROPIC_API_KEY" -d '{"value":"sk-ant-SECRET-not-wired-to-worker"}' >/dev/null

echo "== engine's own environ (what every child inherits) =="
docker exec "$NAME" sh -lc 'tr "\0" "\n" </proc/1/environ 2>/dev/null | grep -E "WHEEL_VAULT_KEY|WHEEL_ENGINE_SECRET" | sed "s/=.*/=<present>/"'

echo "== start agent, race a snapshot of the child environ =="
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$aid/start" >/dev/null
HIT=""
for i in $(seq 1 30); do
  HIT=$(docker exec "$NAME" sh -lc '
    for p in $(ls /proc 2>/dev/null | grep -E "^[0-9]+$"); do
      [ "$p" = 1 ] && continue
      if tr "\0" "\n" </proc/$p/environ 2>/dev/null | grep -q "^WHEEL_VAULT_KEY="; then
        echo "PID $p ($(cat /proc/$p/comm 2>/dev/null)):"
        tr "\0" "\n" </proc/$p/environ 2>/dev/null | grep -E "WHEEL_VAULT_KEY|WHEEL_ENGINE_SECRET|WHEEL_TOKEN"
        exit 0
      fi
    done; exit 1' 2>/dev/null) && break
  sleep 0.2
done

echo "----- child environ snapshot -----"
if [ -n "$HIT" ]; then echo "$HIT"; else echo "(no live child caught; harness may have exited — see source-confirmed note)"; fi
echo "----------------------------------"

FOUND_VK=$(echo "$HIT"|grep -c "^WHEEL_VAULT_KEY=" ||true)
FOUND_ES=$(echo "$HIT"|grep -c "^WHEEL_ENGINE_SECRET=" ||true)
FOUND_TOK=$(echo "$HIT"|grep -c "^WHEEL_TOKEN=" ||true)

echo "== verdict =="
if [ "${FOUND_VK:-0}" -ge 1 ]; then
  echo "FAIL(FINDING): child inherited WHEEL_VAULT_KEY — it can decrypt EVERY vault in the project (incl. othervault it has no wire to)."
  [ "${FOUND_ES:-0}" -ge 1 ] && echo "FAIL(FINDING): child inherited WHEEL_ENGINE_SECRET — full /v1/* control-plane bypass of all wire enforcement."
  [ "${FOUND_TOK:-0}" -ge 1 ] && echo "NOTE: WHEEL_TOKEN present in env (should be file-only)."
  rc=1
else
  echo "INCONCLUSIVE-LIVE: no child environ captured. Source is unambiguous (no env_clear at supervisor/mod.rs:203); rerun or lengthen the race."
  rc=2
fi
echo "exit=$rc"; exit $rc
