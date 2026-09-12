#!/bin/sh
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# A config value is not a control until the running binary is proven to enforce it. WHEEL_SIGNUP
# is validated as a string by preflight.sh before wheeld even starts, but that only checks the
# ENVIRONMENT is well-formed — it says nothing about whether the container actually running as
# "wheeld" is the wheeld binary at all. Someone working around a broken build by pointing
# compose.yml's wheeld service at a different image (docker/Dockerfile.api, say) would sail
# straight past that check: the string is still "closed", and nothing else would notice.
#
# In tunnel mode there is no Caddy in front to fall back on, so wheeld's own gate is the only
# control. This makes one real HTTP call and checks wheeld's own documented response shape
# (docs/API.md: closed → 403 {"error":{"code":"forbidden"}}). `web` and `caddy` both depend on
# this succeeding, so a wheeld that does not actually enforce its signup policy never gets a
# public-facing peer started in front of it.
set -eu

target="http://wheeld:8080/v1/auth/signup"
body='{"email":"verify-signup-gate@wheel.invalid","password":"verify-signup-gate-probe-not-a-real-account"}'

response="$(curl -sS -m 10 -o /tmp/body -w '%{http_code}' -X POST "$target" -H 'content-type: application/json' -d "$body")" || {
    echo "verify-signup-gate: could not reach $target at all (rc=$?) — wheeld is not answering as itself" >&2
    exit 1
}
body_text="$(cat /tmp/body)"

case "${WHEEL_SIGNUP:-closed}" in
    open)
        if [ "$response" = 403 ] && printf '%s' "$body_text" | grep -q '"forbidden"'; then
            echo "verify-signup-gate: WHEEL_SIGNUP=open but wheeld still answers 403 forbidden — signup is not actually open" >&2
            exit 1
        fi
        echo "verify-signup-gate: WHEEL_SIGNUP=open — signup answered $response (not the closed-signup 403), as expected"
        ;;
    *)
        if [ "$response" != 403 ] || ! printf '%s' "$body_text" | grep -q '"forbidden"'; then
            echo "verify-signup-gate: WHEEL_SIGNUP=closed but POST $target answered $response ${body_text:+with body $body_text}, not the documented 403 {\"error\":{\"code\":\"forbidden\"}}" >&2
            echo "verify-signup-gate: refusing to start web or Caddy in front of a wheeld that does not enforce its own signup gate" >&2
            exit 1
        fi
        echo "verify-signup-gate: WHEEL_SIGNUP=closed — wheeld answered 403 forbidden, as documented"
        ;;
esac
