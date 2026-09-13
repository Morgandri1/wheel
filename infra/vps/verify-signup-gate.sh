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
# control. `web` and `caddy` both depend on this succeeding, so a wheeld that does not actually
# enforce its signup policy never gets a public-facing peer started in front of it — new starts
# only: docker compose depends_on gates STARTING a service, and never stops one already running.
# On an upgrade where wheeld/web are already Up, this is a loud alarm, not containment; deploy.sh
# reacts to this service's failure by stopping them itself, which is the actual containment.
#
# Two properties this earns by construction, not by care taken writing it:
#
#   1. NO ACCOUNT IS EVER CREATED, on either branch. The probed password is 3 characters, under
#      wheel-api's own MIN_PASSWORD_LEN (10, crates/wheel-api/src/auth/local.rs). Signup closed
#      answers 403 before the body is even looked at (the closed-check runs first in
#      routes/auth.rs::signup); signup open reaches create_user, which rejects the password before
#      insert_user is ever called. Neither path plants a real, persistent, credentialed account —
#      unlike an earlier version of this script, which posted a fixed, published password and
#      really did create `verify-signup-gate@wheel.invalid` whenever signup was actually open,
#      including the open setting this script is supposed to also validate.
#
#   2. THE CLOSED-SIGNUP MATCH IS SPECIFIC TO SIGNUP, not any 403. wheeld's own Host-guard
#      (crates/wheeld/src/guard.rs) ALSO answers 403 with "code":"forbidden" — the same top-level
#      shape wheel-api's generic ApiError::Forbidden uses — but a different message text. Matching
#      on the code alone would pass this gate against a container that blanket-403s everything
#      (say, WHEEL_ALLOWED_HOSTS misconfigured so it rejects the Host this container sends), which
#      is a worse failure than the one this script exists to catch: it would report "wheeld
#      enforces its signup gate" while never having reached the signup gate at all. So: a control
#      request first, to something signup has no say over, and the closed-branch match requires
#      wheel-api's own generic message text, which the Host-guard does not produce.
#
# Known limitation, recorded rather than fixed: in the open branch, this probe counts against
# wheeld's own global signup rate limit (50/hour). If a stranger has already spent that bucket —
# reachable only because WHEEL_SIGNUP=open means anyone who reaches wheeld can sign up in the first
# place — wheeld answers 429 instead of the 400 this script expects, and the deploy fails safe
# (nothing starts) but for a reason that has nothing to do with whether wheeld enforces its own
# gate. This is a discouraged-mode-only annoyance, not a security gap: WHEEL_SIGNUP=open itself is
# already documented as unsafe on a reachable server (README.md, "Signup"), and this just means a
# stranger can also make deploys fail while it's set that way.
set -eu

# Where wheeld is. The Docker path leaves this unset and gets the compose service name; the native
# path (wheel-signup-gate.service) sets it to wheeld's loopback address. ONE implementation of "is
# this really wheeld, and does it really enforce its own signup gate", exercised by both
# deployments — so neither can drift into being the only tested one, which is exactly what happened
# while the native check was a block of shell inside install.sh that ran once at install time.
# Resolution order, most explicit first. The native unit deliberately does NOT pass a URL: an
# earlier version had `Environment=WHEEL_GATE_BASE=http://${BIND_ADDR}` in the unit file, and
# systemd does NOT expand variables inside Environment= (only ExecStart= and friends get that), so
# the gate probed the literal host `${BIND_ADDR}` and curl answered "URL rejected: Bad hostname".
# The gate then correctly refused to let the board start -- a real failure, honestly reported, for
# a reason that had nothing to do with signup. Deriving it here keeps one source of truth and takes
# systemd's expansion rules out of the picture entirely.
if [ -n "${WHEEL_GATE_BASE:-}" ]; then
    base="$WHEEL_GATE_BASE"
elif [ -n "${BIND_ADDR:-}" ]; then
    # Always dial loopback, whatever wheeld was told to bind: an operator who set 0.0.0.0 still has
    # a daemon reachable on 127.0.0.1, and this probe has no business leaving the machine.
    base="http://127.0.0.1:${BIND_ADDR##*:}"
else
    # The Docker path: compose's service name, on its own network.
    base="http://wheeld:8080"
fi

# Control: proves this really is wheeld, answering normally, before trusting anything it says
# about signup specifically. /healthz needs no auth and no signup opinion; if THIS is not a plain
# 200, nothing below can be trusted — including a 403 that looks like a correct "closed" answer.
control_status="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$base/healthz")" || {
    echo "verify-signup-gate: could not reach $base/healthz at all (rc=$?) — wheeld is not answering as itself" >&2
    exit 1
}
if [ "$control_status" != 200 ]; then
    echo "verify-signup-gate: GET /healthz answered $control_status, not 200 — this container is not behaving like wheeld at all (a blanket 403, a wrong image, a misconfigured Host guard), so a 403 from the signup probe below would prove nothing" >&2
    exit 1
fi

# A private temporary file, never a fixed /tmp path. In compose this ran as the only process in a
# throwaway container and /tmp/body was harmless; natively it runs on a host where /tmp is shared,
# and a predictable path another local user can pre-create as a symlink is a file this script then
# writes through as whatever it is running as.
body_file="$(mktemp)"
trap 'rm -f "$body_file"' EXIT

# A password under wheel-api's 10-character minimum. See the file header: this is what makes the
# probe side-effect-free on both branches, not merely a coincidence of who lost this particular
# race.
body='{"email":"verify-signup-gate-probe@wheel.invalid","password":"x"}'
response="$(curl -sS -m 10 -o "$body_file" -w '%{http_code}' -X POST "$base/v1/auth/signup" -H 'content-type: application/json' -d "$body")" || {
    echo "verify-signup-gate: could not reach $base/v1/auth/signup at all (rc=$?) — wheeld is not answering as itself" >&2
    exit 1
}
body_text="$(cat "$body_file")"
# wheel-api's ApiError::Forbidden always renders this exact text (error.rs), regardless of which
# internal reason triggered it — signup-closed is the only Forbidden this route can raise, but the
# Host-guard's differently-worded 403 is ruled out explicitly rather than assumed away.
generic_forbidden='This operation is not permitted.'
too_short='at least 10 characters'

case "${WHEEL_SIGNUP:-closed}" in
    open)
        if [ "$response" = 403 ] && printf '%s' "$body_text" | grep -qF "$generic_forbidden"; then
            echo "verify-signup-gate: WHEEL_SIGNUP=open but wheeld still answers signup-closed's 403 — signup is not actually open" >&2
            exit 1
        fi
        if [ "$response" != 400 ] || ! printf '%s' "$body_text" | grep -qF "$too_short"; then
            echo "verify-signup-gate: WHEEL_SIGNUP=open but POST /v1/auth/signup answered $response ${body_text:+with body $body_text}, not the expected 400 naming the password's minimum length — cannot confirm this is really wheeld's own signup route" >&2
            exit 1
        fi
        echo "verify-signup-gate: WHEEL_SIGNUP=open — the too-short-password probe reached real validation (400, no account created), as expected"
        ;;
    *)
        if [ "$response" != 403 ] || ! printf '%s' "$body_text" | grep -qF "$generic_forbidden"; then
            echo "verify-signup-gate: WHEEL_SIGNUP=closed but POST /v1/auth/signup answered $response ${body_text:+with body $body_text}, not wheel-api's own generic-forbidden 403 — refusing to let web or Caddy start against a wheeld that does not enforce its own signup gate" >&2
            exit 1
        fi
        echo "verify-signup-gate: WHEEL_SIGNUP=closed — wheeld answered its own signup-closed 403, as documented"
        ;;
esac
