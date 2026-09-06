#!/usr/bin/env bash
# Allowed-path e2e via a HOSTNAME allowlist entry (PM's insight): a hostname passes create-time
# host_is_denied (a literal/suffix check; "echo.test" is neither), is allowlisted by that literal, and
# resolves via /etc/hosts to a loopback witness server. So we exercise send()'s allowed path — header-CRLF
# (:328), redirect no-replay/per-hop, 5 MiB cap, 30s timeout — TODAY, RoE-safe (all loopback), without the
# 027 fix (which is about a loopback LITERAL base_url; a hostname alias sidesteps it — noted in 027).
set -u
NAME="wheel-adv-hn-$$"; PORT=7060; SECRET="$(openssl rand -hex 24)"; VK="$(head -c32 /dev/urandom|base64)"; PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"; ES=(-H "authorization: Bearer ${SECRET}" -H 'content-type: application/json')
A=18080; B=18081
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1 || true; }; trap cleanup EXIT
docker run -d --name "$NAME" -p ${PORT}:7000 -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" \
  -e WHEEL_ENGINE_SECRET="$SECRET" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json -e WHEEL_ENV=dev \
  -e WHEEL_TOOL_ALLOW_HOST="echo.test:$A,echo2.test:$B" wheel-engine:dev >/dev/null || { echo "run failed"; exit 2; }
for i in $(seq 1 40); do c=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$c" = 200 ]&&break; sleep 0.5; done
[ "${c:-}" = 200 ] || { echo "unhealthy"; docker logs "$NAME" 2>&1|tail -20; exit 2; }
echo "healthy (image $(docker image inspect wheel-engine:dev --format '{{.Created}}'))"
docker cp "$(dirname "$0")/srv.py" "$NAME:/tmp/srv.py" >/dev/null
docker exec -d "$NAME" sh -lc "python3 /tmp/srv.py $A $B >/tmp/A.log 2>&1"
docker exec -d "$NAME" sh -lc "python3 /tmp/srv.py $B $B >/tmp/B.log 2>&1"
docker exec "$NAME" sh -lc 'cat >> /etc/hosts <<H
127.0.0.1 echo.test
127.0.0.1 echo2.test
H'
sleep 1
docker exec "$NAME" sh -lc "curl -s -o /dev/null -w 'echo up: %{http_code}\n' http://echo.test:$A/echo" 2>/dev/null

SRC='{"format":"manual","raw":"","imported_at":"2026-09-06T00:00:00Z"}'
mknode(){ curl -s "${ES[@]}" -X POST "$URL/v1/nodes" -d "$1"; }
idof(){ curl -s "${ES[@]}" "$URL/v1/board"|python3 -c "import sys,json
try: print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$1'))
except: print('')"; }
call(){ curl -s "${ES[@]}" -X POST "$URL/v1/tools/$1/call" -d "$2"; }

T=$(cat <<JSON
{"name":"al","type":"tool","config":{"kind":"http","source":$SRC,"base_url":"http://echo.test:$A","operations":[
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
CR=$(mknode "$T"); AL=$(idof al)
echo "create al (hostname base_url): ${CR:0:70}  id=${AL:-NONE}"
[ -n "$AL" ] || { echo "FATAL: hostname base_url rejected at create too? detail: $CR"; exit 1; }

echo; echo "===== 1) HEADER CRLF AT SEND (live via :328) ====="
R=$(call "$AL" '{"op":"echoh","args":{"X-Try":"a\r\nX-Injected: pwned"}}')
echo "  result: ${R:0:200}"
if echo "$R"|grep -qi "X-Injected"; then echo "  FAIL(FINDING): X-Injected reached the server -> CRLF injection"
elif echo "$R"|grep -qiE "line break|null|another header|invalid"; then echo "  PASS: refused before send (:328) with a clear reason"
else echo "  PASS/INSPECT: no X-Injected in witness (reqwest/:328 dropped it)  ${R:0:80}"; fi
echo "  control (valid header) — server should echo X-Try:"; call "$AL" '{"op":"echoh","args":{"X-Try":"plainval"}}' | python3 -c 'import sys,json;d=json.load(sys.stdin);h=d.get("headers",{});print("   X-Try seen =",h.get("X-Try") or h.get("x-try"))' 2>/dev/null || echo "   (parse failed)"

echo; echo "===== 5) redirect no-replay / per-hop / limit ====="
echo "  redir->metadata (hop2 must REFUSE): $(call "$AL" '{"op":"redirmeta","args":{}}' | head -c 140)"
echo "  redir->unallowlisted 127.0.0.1:19999 (must REFUSE): $(call "$AL" '{"op":"redirbad","args":{}}' | head -c 140)"
echo "  3-hop chain (redirect LIMIT): $(call "$AL" '{"op":"chain","args":{}}' | head -c 140)"
R=$(call "$AL" '{"op":"redir2nd","args":{"secret":"BODYSECRET"}}')
echo "  body-not-replayed: hop2 echo body = $(echo "$R" | python3 -c 'import sys,json;print(repr(json.load(sys.stdin).get("body","")))' 2>/dev/null || echo "${R:0:80}")"
echo "    (BODYSECRET must be ABSENT)"

echo; echo "===== 6) 5 MiB cap + 30s timeout ====="
echo "  big(6MiB) -> cap: $(call "$AL" '{"op":"big","args":{}}' | head -c 140)"
t0=$(date +%s); OUT=$(call "$AL" '{"op":"slow","args":{}}'); t1=$(date +%s)
echo "  slow(35s) -> $(echo "$OUT" | head -c 100)  [elapsed $((t1-t0))s, expect ~30]"
echo "== done =="
