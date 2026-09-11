#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Watch the rehearsal's gates fail. Breaks one layer on purpose, runs rehearse.sh against it, and
# exits 0 only if every check that guards that layer came back rc=1. Anything else that went red
# is listed, not hidden.
#
#   infra/vps/rehearsal/mutate.sh edge   [--ref <git ref>] [--print]   the Caddyfile, a published port
#   infra/vps/rehearsal/mutate.sh config [--ref <git ref>]             wheeld's signup flag, the web origin
#
# The broken Caddyfile is derived from the one in the ref under test, and every edit must apply
# exactly once, so a mutation cannot silently miss the line it was meant to break. --print shows
# it and stops.

set -uo pipefail

layer="${1:-}"
shift || true
ref=HEAD
print=0
while [ $# -gt 0 ]; do
    case "$1" in
        --ref) ref="${2:?--ref needs a git ref}"; shift 2 ;;
        --print) print=1; shift ;;
        *) echo "mutate: unknown argument $1" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)" || exit 2
work="$(mktemp -d /tmp/wheel-mutate.XXXXXX)"
trap 'rm -rf "$work"' EXIT
caddyfile=""

case "$layer" in
    edge)
        git -C "$repo" show "$ref:infra/vps/Caddyfile" >"$work/Caddyfile" || exit 2
        git -C "$repo" show "$ref:infra/vps/rehearsal/mutations/publish-wheeld.yml" >"$work/extra.yml" || exit 2
        python3 - "$work/Caddyfile" <<'PY' || exit 2
import sys
path = sys.argv[1]
text = open(path).read()
edits = [
    ("\tadmin off\n", "\tadmin localhost:2019\n\tservers {\n\t\ttrusted_proxies static 0.0.0.0/0\n\t}\n"),
    ('\theader @tls Strict-Transport-Security "max-age=31536000"\n', ""),
    ("\t\tmax_size 256KiB\n", "\t\tmax_size 10MiB\n"),
    ("\t\t\theader_up -Cookie\n", ""),
    ("\t\t\tflush_interval -1\n", "\t\t\tflush_interval -1\n\t\t\tresponse_buffers 1MiB\n"),
    ('\t\texpression `"{$WHEEL_SIGNUP:closed}" != "open"`\n', "\t\texpression false\n"),
]
for old, new in edits:
    count = text.count(old)
    if count != 1:
        sys.exit(f"mutate: {old!r} occurs {count} times, so this mutation would not apply")
    text = text.replace(old, new)
open(path, "w").write(text)
PY
        caddyfile="$work/Caddyfile"
        red="edge-headers signup-closed-at-edge web-sse forwarded-headers-overwritten body-limits ingress-rate-limit-ignores-xff not-published cookie-stripped-to-wheeld caddy-admin-off"
        ;;
    config)
        git -C "$repo" show "$ref:infra/vps/rehearsal/mutations/config.yml" >"$work/extra.yml" || exit 2
        red="signup-closed-at-wheeld sign-in"
        ;;
    *)
        echo "mutate: say which layer to break: edge or config" >&2
        exit 2
        ;;
esac
if [ "$print" = 1 ]; then
    if [ -n "$caddyfile" ]; then cat "$caddyfile"; else cat "$work/extra.yml"; fi
    exit 0
fi

REHEARSE_CADDYFILE="$caddyfile" REHEARSE_COMPOSE_EXTRA="$work/extra.yml" REHEARSE_RESULTS="$work/results" \
    "$repo/infra/vps/rehearse.sh" --ref "$ref"
rehearse_rc=$?
if [ ! -s "$work/results" ]; then
    echo "mutate: the rehearsal produced no results (rc=$rehearse_rc), so nothing was measured" >&2
    exit 2
fi

echo
echo "mutation '$layer': rehearse.sh rc=$rehearse_rc"
missed=0
for name in $red; do
    rc="$(awk -v n="$name" '$1 == n { print $2 }' "$work/results")"
    if [ "$rc" = 1 ]; then
        echo "  red as it must be   rc=1  $name"
    else
        echo "  STILL NOT RED       rc=${rc:-missing}  $name"
        missed=1
    fi
done
while read -r name rc; do
    case " $red " in *" $name "*) continue ;; esac
    [ "$rc" = 0 ] || echo "  collateral          rc=$rc  $name"
done <"$work/results"
[ "$missed" = 0 ]
