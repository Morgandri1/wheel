#!/usr/bin/env bash
# Live tool-executor campaign via the engine-secret realm (/v1/tools/:id/call + POST /v1/nodes).
# A) 022 fix e2e: query/path vault+static secrets masked in dry_run curl; cookie value encoded (no injection).
# B) SSRF: per-base_url tool nodes -> classify deny-at-config / deny-at-call / REACHABLE(gap). 6to4/NAT64/Teredo.
# PM: container removed (trap).
set -u
NAME="wheel-adv-toollive-$$"; PORT=7031; SECRET="$(openssl rand -hex 24)"; VK="$(head -c32 /dev/urandom|base64)"; PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"; ES=(-H "authorization: Bearer ${SECRET}" -H 'content-type: application/json')
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1 || true; }; trap cleanup EXIT
docker run -d --name "$NAME" -p ${PORT}:7000 -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" \
  -e WHEEL_ENGINE_SECRET="$SECRET" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json wheel-engine:dev >/dev/null || { echo "run failed"; exit 2; }
for i in $(seq 1 40); do code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$code" = 200 ]&&break; sleep 0.5; done
[ "${code:-}" = 200 ] || { echo "unhealthy ($code)"; docker logs "$NAME" 2>&1|tail -20; exit 2; }
echo "healthy (image $(docker image inspect wheel-engine:dev --format '{{.Created}}'))"
node(){ curl -s "${ES[@]}" -X POST "$URL/v1/nodes" -d "$1"; }   # returns JSON (id or error)
SRC='{"format":"manual","raw":"","imported_at":"2026-09-06T00:00:00Z"}'

# vault with a secret that contains encoded chars, wired to the tool
node "{\"name\":\"vlt\",\"type\":\"vault\",\"config\":{\"keys\":[\"APIKEY\"]}}" >/dev/null
VID=$(curl -s "${ES[@]}" "$URL/v1/board"|python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="vlt"))')
curl -s "${ES[@]}" -X PUT "$URL/v1/vault/$VID/APIKEY" -d '{"value":"sk/secret+val=="}' >/dev/null

# tool node t1 for the mask/cookie tests (base_url is a benign public-looking host; we only dry_run)
T1CFG=$(cat <<JSON
{"name":"t1","type":"tool","config":{"kind":"http","source":$SRC,"base_url":"https://api.example.com","operations":[
 {"id":"q","method":"GET","path":"/data","params":[{"name":"key","location":"query","fill":{"mode":"vault","vault_ref":"vlt/APIKEY"}}]},
 {"id":"p","method":"GET","path":"/x/{tok}","params":[{"name":"tok","location":"path","fill":{"mode":"static","value":"st/atic+="}}]},
 {"id":"c","method":"GET","path":"/c","params":[{"name":"sid","location":"cookie","fill":{"mode":"agent"}}]}
]}}
JSON
)
R=$(node "$T1CFG"); echo "t1 create: ${R:0:80}"
TID=$(curl -s "${ES[@]}" "$URL/v1/board"|python3 -c 'import sys,json
try: print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="t1"))
except: print("")')
[ -n "$TID" ] || { echo "t1 not created; config rejected: $R"; }
# wire t1 -> vlt (read) so the vault fill resolves
curl -s "${ES[@]}" -X POST "$URL/v1/wires" -d "{\"from\":\"$TID\",\"to\":\"$VID\",\"type\":\"read\"}" >/dev/null

echo "== A) mask + cookie via dry_run =="
call(){ curl -s "${ES[@]}" -X POST "$URL/v1/tools/$TID/call" -d "$1"; }
echo "q(query vault):  $(call '{"op":"q","args":{},"dry_run":true}')"
echo "p(path static):  $(call '{"op":"p","args":{},"dry_run":true}')"
echo "c(cookie inject): $(call '{"op":"c","args":{"sid":"x; admin=1"},"dry_run":true}')"

echo "== B) SSRF per base_url (create -> call classify) =="
declare -a URLS=(
 "http://127.0.0.1/|loopback"
 "http://10.0.0.1/|rfc1918"
 "http://169.254.169.254/|metadata-v4"
 "http://2130706433/|decimal-loopback"
 "http://x.railway.internal/|railway-internal"
 "http://[2002:7f00:0001::]/|6to4-loopback"
 "http://[64:ff9b::7f00:1]/|nat64-loopback"
 "http://[2001:0:53aa::]/|teredo"
)
i=0
for entry in "${URLS[@]}"; do
  u="${entry%%|*}"; label="${entry##*|}"; i=$((i+1)); nm="s$i"
  cfg="{\"name\":\"$nm\",\"type\":\"tool\",\"config\":{\"kind\":\"http\",\"source\":$SRC,\"base_url\":\"$u\",\"operations\":[{\"id\":\"g\",\"method\":\"GET\",\"path\":\"/\"}]}}"
  cr=$(node "$cfg")
  if echo "$cr"|grep -qiE '"error"'; then echo "  $label ($u): DENIED-AT-CONFIG (node create rejected) -> PASS"; continue; fi
  sid=$(curl -s "${ES[@]}" "$URL/v1/board"|python3 -c "import sys,json
try: print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$nm'))
except: print('')")
  out=$(curl -s "${ES[@]}" -X POST "$URL/v1/tools/$sid/call" -d '{"op":"g","args":{}}')
  if echo "$out"|grep -qiE 'not reachable|private|loopback|internal|not a reachable'; then
    echo "  $label ($u): DENIED-AT-CALL -> PASS  ${out:0:70}"
  elif echo "$out"|grep -qiE '"status":[0-9]'; then
    echo "  $label ($u): ***REACHED (got an HTTP status)*** -> FINDING  ${out:0:90}"
  else
    echo "  $label ($u): not SSRF-denied (connect/resolve error) -> GAP?  ${out:0:90}"
  fi
done
echo "== C) 6to4/NAT64 via HOSTNAME (the real gap: literal fails on brackets, but a name that RESOLVES bypasses ip_is_denied) =="
# lookup_host reads /etc/hosts; map names to a 6to4/NAT64 address and to loopback, then compare guard behaviour.
docker exec "$NAME" sh -lc 'printf "%s\n" "64:ff9b::7f00:1 natsix.test" "2002:7f00:0001:: sixfour.test" "127.0.0.1 lo.test" >> /etc/hosts' 2>/dev/null
j=0
for entry in "http://natsix.test/|nat64-hostname" "http://sixfour.test/|6to4-hostname" "http://lo.test/|loopback-hostname(control)"; do
  u="${entry%%|*}"; label="${entry##*|}"; j=$((j+1)); nm="h$j"
  cfg="{\"name\":\"$nm\",\"type\":\"tool\",\"config\":{\"kind\":\"http\",\"source\":$SRC,\"base_url\":\"$u\",\"operations\":[{\"id\":\"g\",\"method\":\"GET\",\"path\":\"/\"}]}}"
  cr=$(node "$cfg")
  if echo "$cr"|grep -qiE '"error"'; then echo "  $label: DENIED-AT-CONFIG -> PASS"; continue; fi
  sid=$(curl -s "${ES[@]}" "$URL/v1/board"|python3 -c "import sys,json
try: print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$nm'))
except: print('')")
  out=$(curl -s "${ES[@]}" -X POST "$URL/v1/tools/$sid/call" -d '{"op":"g","args":{}}')
  if echo "$out"|grep -qiE 'not reachable|private|loopback|internal'; then
    echo "  $label: DENIED-BY-GUARD -> PASS  ${out:0:70}"
  else
    echo "  $label: PASSED THE GUARD (reached connect; error is not an SSRF deny) -> GAP  ${out:0:100}"
  fi
done
echo "== done =="
