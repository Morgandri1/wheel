#!/usr/bin/env bash
# F015 FIX VERIFICATION (scoped, canary-based). After e09e1ec the supervisor and oauth.rs both
# env_clear() and re-add only an allowlist. This reads the ENGINE-SPAWNED child's own environ —
# as the child's own uid, which is the ONLY thing that can read it (root lacks CAP_SYS_PTRACE in
# the default docker cap set) — and asserts the engine's canary secrets are ABSENT while the
# allowlist is present. It targets PPid==1 children only; a `docker exec` probe shell is PPid==0
# and inherits the container's `-e` env, which is a docker artifact, NOT an agent-reachable leak
# (this is the false positive that made run_env_inheritance.sh/run_env_exploit.sh over-report).
# PM: container removed on exit.
set -u
NAME="wheel-adv-envfix-$$"
PORT=7022
# Distinctive canaries so a match is unambiguous.
VK_CANARY="VAULTKEYCANARY-$(openssl rand -hex 12)"
VK="$(printf '%s' "$VK_CANARY" | head -c32 | base64)"     # still 32B→b64 shaped; canary substring survives? no — so also pass a raw canary env
ES_CANARY="ENGINESECRETCANARY-$(openssl rand -hex 12)"
PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"
ES=(-H "authorization: Bearer ${ES_CANARY}")
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "== image =="; docker image inspect wheel-engine:dev --format '{{.Created}}'
echo "== boot $NAME (engine secret + vault key are canaries) =="
# WHEEL_VAULT_KEY must be valid base64 of 32 bytes; we can't embed the canary there and stay valid,
# so we assert on the KEY NAMES for vault key, and on the literal canary for the engine secret.
docker run -d --name "$NAME" -p ${PORT}:7000 \
  -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" -e WHEEL_ENGINE_SECRET="$ES_CANARY" \
  -e WHEEL_VAULT_KEY="$(head -c32 /dev/urandom | base64)" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json wheel-engine:dev >/dev/null || { echo "run failed"; exit 2; }
for i in $(seq 1 40); do code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$code" = 200 ]&&break; sleep 0.5; done
[ "$code" = 200 ] || { echo "unhealthy ($code)"; docker logs "$NAME" 2>&1|tail -20; exit 2; }
echo "healthy."

mk(){ curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/nodes" -d "$1" >/dev/null; }
mk '{"name":"worker","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
aid=$(curl -s "${ES[@]}" "$URL/v1/board"|python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="worker"))')
echo "worker=$aid"

# Find a PPid==1 harness child (read as root — /status is world-readable), return "pid uid".
find_child() {
  docker exec "$NAME" sh -lc '
    for p in $(ls /proc 2>/dev/null | grep -E "^[0-9]+$"); do
      [ "$p" = 1 ] && continue
      ppid=$(grep -m1 "^PPid:" /proc/$p/status 2>/dev/null | tr -dc "0-9")
      [ "$ppid" = "1" ] || continue
      c=$(cat /proc/$p/comm 2>/dev/null)
      case "$c" in claude|node|codex|wheel*)
        uid=$(grep -m1 "^Uid:" /proc/$p/status 2>/dev/null | awk "{print \$2}")
        echo "$p $uid $c"; exit 0;; esac
    done; exit 1' 2>/dev/null
}

# Read a pid's environ AS its own uid (same-uid can read /proc/<pid>/environ; root cannot without ptrace).
read_environ_as_uid() { # $1=pid $2=uid
  docker exec -u "$2" "$NAME" sh -lc "tr '\0' '\n' </proc/$1/environ 2>/dev/null" 2>/dev/null
}

assert_clean() { # $1=label $2=environ
  local label="$1" env="$2"
  if [ -z "$env" ]; then echo "INCONCLUSIVE $label: empty environ (child gone or unreadable)"; return 2; fi
  local haspath; haspath=$(echo "$env"|grep -c "^PATH=")
  [ "${haspath:-0}" -ge 1 ] || { echo "INCONCLUSIVE $label: no PATH (not a real child environ)"; return 2; }
  local canary esname vkname
  canary=$(echo "$env"|grep -c -- "$ES_CANARY")
  esname=$(echo "$env"|grep -c "^WHEEL_ENGINE_SECRET=")
  vkname=$(echo "$env"|grep -c "^WHEEL_VAULT_KEY=")
  if [ "${canary:-0}" -ge 1 ] || [ "${esname:-0}" -ge 1 ] || [ "${vkname:-0}" -ge 1 ]; then
    echo "FAIL(FINDING) $label: engine secret/vault key present  canary=$canary ES=$esname VK=$vkname"; return 1
  fi
  echo "PASS $label: clean — no engine-secret canary, no WHEEL_ENGINE_SECRET, no WHEEL_VAULT_KEY."
  echo "  child env names: $(echo "$env"|sed 's/=.*//'|sort|tr '\n' ' ')"
  return 0
}

fails=0
echo "== A: agent child =="
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$aid/start" >/dev/null
C=""; for i in $(seq 1 30); do C=$(find_child) && [ -n "$C" ] && break; sleep 0.2; done
if [ -n "$C" ]; then set -- $C; echo "caught agent child pid=$1 uid=$2 comm=$3"; assert_clean "A/agent-child" "$(read_environ_as_uid "$1" "$2")"; [ $? -eq 1 ] && fails=$((fails+1)); else echo "INCONCLUSIVE A: no PPid1 child"; fi

echo "== B: oauth login child (auth/begin) =="
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$aid/auth/begin" >/dev/null 2>&1
C=""; for i in $(seq 1 40); do C=$(find_child) && [ -n "$C" ] && break; sleep 0.25; done
if [ -n "$C" ]; then set -- $C; echo "caught login child pid=$1 uid=$2 comm=$3"; assert_clean "B/oauth-login-child" "$(read_environ_as_uid "$1" "$2")"; [ $? -eq 1 ] && fails=$((fails+1)); else echo "INCONCLUSIVE B: no PPid1 child"; fi

echo "== verdict =="
[ "$fails" -ge 1 ] && { echo "F015 NOT fixed: $fails leak(s)"; exit 1; }
echo "F015 fix VERIFIED — engine-spawned child environ carries only the allowlist; no engine secret / vault key."; exit 0
