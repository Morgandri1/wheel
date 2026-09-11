#!/bin/sh
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Refuses to start the stack on a configuration that would boot and then be wrong: an empty public
# origin, two modes at once, or a typo in a flag. Every service waits for this to exit 0.
set -eu

fail() {
    echo "wheel preflight: $*" >&2
    exit 1
}

if [ -n "$WHEEL_DOMAIN" ] && [ -n "$WHEEL_PUBLIC_HOST" ]; then
    fail "set WHEEL_DOMAIN (HTTPS) or WHEEL_PUBLIC_HOST (plain HTTP), not both"
fi
if [ -z "$WHEEL_DOMAIN" ] && [ -z "$WHEEL_PUBLIC_HOST" ]; then
    fail "set WHEEL_DOMAIN=<your domain> for HTTPS, or WHEEL_PUBLIC_HOST=<server IP> for plain HTTP"
fi
for value in "$WHEEL_DOMAIN" "$WHEEL_PUBLIC_HOST"; do
    case "$value" in
        *://* | */* | *" "*) fail "'$value' must be a bare host name or IP, without a scheme or path" ;;
    esac
done
case "$WHEEL_TLS_INTERNAL" in
    "" | 1) ;;
    *) fail "WHEEL_TLS_INTERNAL must be 1 or empty, got '$WHEEL_TLS_INTERNAL'" ;;
esac
if [ -n "$WHEEL_TLS_INTERNAL" ] && [ -z "$WHEEL_DOMAIN" ]; then
    fail "WHEEL_TLS_INTERNAL needs WHEEL_DOMAIN: plain HTTP has no certificate to issue"
fi
case "$WHEEL_SIGNUP" in
    closed) ;;
    open) echo "wheel preflight: WARNING: WHEEL_SIGNUP=open lets anyone who reaches this server create an account and run agents on it" >&2 ;;
    *) fail "WHEEL_SIGNUP must be 'closed' or 'open', got '$WHEEL_SIGNUP'" ;;
esac
if [ -n "$WHEEL_PUBLIC_HOST" ]; then
    echo "wheel preflight: WARNING: plain HTTP. Passwords, session tokens and wht_ tokens cross the network unencrypted; use a domain or an SSH tunnel for anything real" >&2
fi
echo "wheel preflight: ok"
