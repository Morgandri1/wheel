#!/bin/sh
# Fake `claude` for the stale-resume PoC. Speaks just enough stream-json.
# First start (no --resume): emit init with a fixed session id, then idle (stay alive).
# Resume start (--resume present): behave per /tmp/fakemode:
#   sleep -> never init, sleep long (so the driver can stop() it PRE-INIT: run_id-guard case)
#   exit  -> never init, exit immediately (child exits PRE-INIT on its own: clear case)
is_resume=0
for a in "$@"; do [ "$a" = "--resume" ] && is_resume=1; done
mode=$(cat /tmp/fakemode 2>/dev/null || echo init)
if [ "$is_resume" = "1" ]; then
  case "$mode" in
    sleep) sleep 120; exit 0 ;;
    exit)  exit 0 ;;
  esac
fi
printf '{"type":"system","subtype":"init","session_id":"SESS-POC-1","model":"fake"}\n'
cat >/dev/null
