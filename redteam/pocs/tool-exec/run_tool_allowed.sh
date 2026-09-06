#!/usr/bin/env bash
# Allowed-path tool-executor campaign (needs WHEEL_TOOL_ALLOW_HOST + a loopback witness server).
# Priorities: (1) HEADER CRLF AT SEND with a witness, (2) 026 metadata-deny via all 5 v6 spellings through
# the REAL lookup_host path (the seam), (3) allowlist can't widen, (4) prod-boot-refusal, (5) redirect
# no-replay / per-hop / limit, (6) 5 MiB cap + 30s timeout. RoE: only loopback + refused-before-connect
# internal targets; the not-over-denied public case is left to SDK's unit test (would touch a real IP).
set -u
NAME="wheel-adv-allow-$$"; PORT=7040; SECRET="$(openssl rand -hex 24)"; VK="$(head -c32 /dev/urandom|base64)"; PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"; ES=(-H "authorization: Bearer ${SECRET}" -H 'content-type: application/json')
A=18080; B=18081
cleanup(){ docker rm -f "$NAME" "${NAME}-prod" >/dev/null 2>&1 || true; }; trap cleanup EXIT

echo "== boot engine (dev) with allowlist 127.0.0.1:$A,127.0.0.1:$B =="
docker run -d --name "$NAME" -p ${PORT}:7000 -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" \
  -e WHEEL_ENGINE_SECRET="$SECRET" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json -e WHEEL_ENV=dev \
  -e WHEEL_TOOL_ALLOW_HOST="127.0.0.1:$A,127.0.0.1:$B" wheel-engine:dev >/dev/null || { echo "run failed"; exit 2; }
for i in $(seq 1 40); do code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$code" = 200 ]&&break; sleep 0.5; done
[ "${code:-}" = 200 ] || { echo "unhealthy ($code)"; docker logs "$NAME" 2>&1|tail -30; exit 2; }
echo "healthy (image $(docker image inspect wheel-engine:dev --format '{{.Created}}'))"
echo "-- boot WARN should name the allowlist:"; docker logs "$NAME" 2>&1 | grep -iE "allow|WHEEL_TOOL" | head -3

# witness server on 127.0.0.1:$A (redirects target $B) and a plain echo on $B
docker cp "$(dirname "$0")/srv.py" "$NAME:/tmp/srv.py" >/dev/null
docker exec -d "$NAME" sh -lc "python3 /tmp/srv.py $A $B >/tmp/srvA.log 2>&1"
docker exec -d "$NAME" sh -lc "python3 /tmp/srv.py $B $B >/tmp/srvB.log 2>&1"
sleep 1
docker exec "$NAME" sh -lc "curl -s -o /dev/null -w 'echo server $A: %{http_code}\n' http://127.0.0.1:$A/echo" 2>/dev/null || echo "echo server not up?"

# 026 metadata spellings via /etc/hosts (169.254.169.254 = a9fe:a9fe)
docker exec "$NAME" sh -lc 'cat >> /etc/hosts <<HOSTS
::ffff:a9fe:a9fe   mappedmeta
::a9fe:a9fe        compatmeta
2002:a9fe:a9fe::   sixto4meta
64:ff9b::a9fe:a9fe nat64meta
2001:0:a9fe:a9fe:: teredosrvmeta
2001::5601:5601    teredocltmeta
HOSTS'
echo "-- /etc/hosts resolution witness (what lookup_host will see):"
for n in mappedmeta compatmeta sixto4meta nat64meta teredosrvmeta teredocltmeta; do
  echo "   $n -> $(docker exec "$NAME" sh -lc "getent hosts $n 2>/dev/null | awk '{print \$1}' | head -1")"
done

mknode(){ curl -s "${ES[@]}" -X POST "$URL/v1/nodes" -d "$1"; }
SRC='{"format":"manual","raw":"","imported_at":"2026-09-06T00:00:00Z"}'
idof(){ curl -s "${ES[@]}" "$URL/v1/board"|python3 -c "import sys,json
try: print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$1'))
except: print('')"; }
call(){ curl -s "${ES[@]}" -X POST "$URL/v1/tools/$1/call" -d "$2"; }

# tool at the allowlisted echo, with agent header + body params
T=$(cat <<JSON
{"name":"al","type":"tool","config":{"kind":"http","source":$SRC,"base_url":"http://127.0.0.1:$A","operations":[
 {"id":"echoh","method":"GET","path":"/echo","params":[{"name":"X-Try","location":"header","fill":{"mode":"agent"}}]},
 {"id":"redirmeta","method":"GET","path":"/redir-meta"},
 {"id":"redirbad","method":"GET","path":"/redir-bad"},
 {"id":"redir2nd","method":"POST","path":"/redir-2nd","params":[{"name":"secret","location":"body","fill":{"mode":"agent"}}]},
 {"id":"chain","method":"GET","path":"/c1"},
 {"id":"big","method":"GET","path":"/big"},
 {"id":"slow","method":"GET","path":"/slow"}
]}}
JSON
)
CR=$(mknode "$T"); echo "== create allowlisted tool: ${CR:0:70}"
AL=$(idof al)
[ -n "$AL" ] || { echo "FATAL: could not create a tool at the allowlisted loopback (create rejected base_url?). Detail: $CR"; }

echo; echo "===== 1) HEADER CRLF AT SEND (priority, with witness) ====="
if [ -n "$AL" ]; then
  R=$(call "$AL" '{"op":"echoh","args":{"X-Try":"a\r\nX-Injected: pwned"}}')
  echo "call result: ${R:0:200}"
  if echo "$R" | grep -q "X-Injected"; then echo "FAIL(FINDING): X-Injected header REACHED the server -> CRLF injection at send"; else echo "PASS: no X-Injected in the witness (reqwest rejected the control chars OR dropped it)"; fi
  echo "control (valid header):"; call "$AL" '{"op":"echoh","args":{"X-Try":"plainvalue"}}' | python3 -c 'import sys,json;d=json.load(sys.stdin);print("  server saw X-Try =",d.get("headers",{}).get("X-Try") or d.get("headers",{}).get("x-try"))' 2>/dev/null || echo "  (echo parse failed)"
fi

echo; echo "===== 2) 026 metadata via 5 v6 spellings (through real lookup_host) — must DENY ====="
i=0
for n in mappedmeta compatmeta sixto4meta nat64meta teredosrvmeta teredocltmeta; do
  i=$((i+1)); nm="m$i"
  mknode "{\"name\":\"$nm\",\"type\":\"tool\",\"config\":{\"kind\":\"http\",\"source\":$SRC,\"base_url\":\"http://$n/\",\"operations\":[{\"id\":\"g\",\"method\":\"GET\",\"path\":\"/\"}]}}" >/dev/null
  mid=$(idof "$nm")
  if [ -z "$mid" ]; then echo "  $n: DENIED-AT-CONFIG (create rejected) -> PASS"; continue; fi
  out=$(call "$mid" '{"op":"g","args":{}}')
  if echo "$out"|grep -qiE 'not reachable|private|loopback|internal|not a reachable'; then echo "  $n: DENIED-AT-GUARD -> PASS  ${out:0:70}"
  elif echo "$out"|grep -qiE '"status":[0-9]'; then echo "  $n: ***REACHED*** -> FINDING  ${out:0:90}"
  else echo "  $n: not guard-denied (resolve/connect err) -> INSPECT  ${out:0:90}"; fi
done

echo; echo "===== 3) allowlist can't widen ====="
for entry in "127.0.0.2:$A|second-host" "127.0.0.1:19090|second-port" "169.254.169.254|metadata"; do
  hp="${entry%%|*}"; lbl="${entry##*|}"; nm="w_${lbl}"
  mknode "{\"name\":\"$nm\",\"type\":\"tool\",\"config\":{\"kind\":\"http\",\"source\":$SRC,\"base_url\":\"http://$hp/\",\"operations\":[{\"id\":\"g\",\"method\":\"GET\",\"path\":\"/\"}]}}" >/dev/null
  wid=$(idof "$nm")
  if [ -z "$wid" ]; then echo "  $lbl ($hp): DENIED-AT-CONFIG -> PASS"; continue; fi
  out=$(call "$wid" '{"op":"g","args":{}}')
  if echo "$out"|grep -qiE 'not reachable|private|loopback|internal'; then echo "  $lbl ($hp): still REFUSED -> PASS  ${out:0:60}"
  else echo "  $lbl ($hp): NOT refused -> FINDING (allowlist widened)  ${out:0:80}"; fi
done

echo; echo "===== 5) redirect no-replay / per-hop / limit ====="
if [ -n "$AL" ]; then
  echo "  redir->metadata (hop2 must refuse): $(call "$AL" '{"op":"redirmeta","args":{}}' | head -c 120)"
  echo "  redir->unallowed-port (must refuse): $(call "$AL" '{"op":"redirbad","args":{}}' | head -c 120)"
  echo "  3-hop chain (must hit redirect limit): $(call "$AL" '{"op":"chain","args":{}}' | head -c 120)"
  echo "  body-not-replayed (POST redir to 2nd allowlisted echo): "
  R=$(call "$AL" '{"op":"redir2nd","args":{"secret":"BODYSECRET"}}'); echo "    hop2 echo body: $(echo "$R" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("body",""))' 2>/dev/null || echo "$R" | head -c 100)"
  echo "    (BODYSECRET must be ABSENT above)"
fi

echo; echo "===== 6) 5 MiB cap + 30s timeout ====="
if [ -n "$AL" ]; then
  echo "  big(6MiB) -> cap: $(call "$AL" '{"op":"big","args":{}}' | head -c 120)"
  echo "  slow(35s) -> timeout (timing):"; t0=$(date +%s); call "$AL" '{"op":"slow","args":{}}' | head -c 80; t1=$(date +%s); echo "  [elapsed $((t1-t0))s, expect ~30]"
fi

echo; echo "===== 4) prod-boot-refusal: WHEEL_ENV=prod + allowlist must NOT boot ====="
docker run -d --name "${NAME}-prod" -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$(uuidgen)" \
  -e WHEEL_ENGINE_SECRET="x" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json -e WHEEL_ENV=prod \
  -e WHEEL_TOOL_ALLOW_HOST="127.0.0.1:$A" wheel-engine:dev >/dev/null 2>&1
sleep 3
st=$(docker inspect -f '{{.State.Running}} exit={{.State.ExitCode}}' "${NAME}-prod" 2>/dev/null)
echo "  prod container: $st"
echo "  reason: $(docker logs "${NAME}-prod" 2>&1 | grep -iE "prod|allow|WHEEL_TOOL" | head -2)"
echo "  (Running=false / non-zero exit + a reason naming the allowlist = PASS)"
echo "== done =="
