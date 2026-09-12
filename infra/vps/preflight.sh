#!/bin/sh
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Refuses to start the stack on a configuration that would boot and then be wrong. Every service
# waits for this to exit 0.
#
# Tunnel mode (WHEEL_DOMAIN unset) is the default and publishes nothing: wheeld and web are
# loopback-only, and Caddy does not run at all (COMPOSE_PROFILES=tls is what starts it, computed by
# lib/derive-env.sh, never set by hand). There is deliberately no "plain HTTP, publicly" mode here:
# a VPS's published ports are reachable the moment anything upstream of it — a cloud firewall
# misconfigured, ufw not covering Docker's own iptables rules, ufw not installed at all — lets
# traffic through, and by then a password or a wht_ token has already crossed the network in the
# clear. Loopback plus an SSH tunnel, or a real certificate: nothing in between.
set -eu

fail() {
    echo "wheel preflight: $*" >&2
    exit 1
}

if [ -n "${WHEEL_DOMAIN:-}" ]; then
    case "$WHEEL_DOMAIN" in
        *://* | */* | *" "*) fail "WHEEL_DOMAIN must be a bare host name, not '$WHEEL_DOMAIN' (no scheme, no path)" ;;
    esac
    if [ "${COMPOSE_PROFILES:-}" != tls ]; then
        fail "WHEEL_DOMAIN is set but COMPOSE_PROFILES=tls is not — Caddy would never start. Use rehearse.sh or deploy.sh rather than 'docker compose up' directly; both set this from WHEEL_DOMAIN."
    fi
elif [ -n "${ACME_EMAIL:-}" ] || [ "${WHEEL_TLS_INTERNAL:-}" = 1 ] || [ "${COMPOSE_PROFILES:-}" = tls ]; then
    fail "ACME_EMAIL, WHEEL_TLS_INTERNAL and COMPOSE_PROFILES=tls all need WHEEL_DOMAIN — none of them mean anything without a name to serve"
fi
case "${WHEEL_TLS_INTERNAL:-}" in
    "" | 1) ;;
    *) fail "WHEEL_TLS_INTERNAL must be 1 or empty, got '$WHEEL_TLS_INTERNAL'" ;;
esac
case "${WHEEL_SIGNUP:-closed}" in
    closed) ;;
    open) echo "wheel preflight: WARNING: WHEEL_SIGNUP=open lets anyone who reaches this server create an account and run agents on it" >&2 ;;
    *) fail "WHEEL_SIGNUP must be 'closed' or 'open', got '$WHEEL_SIGNUP'" ;;
esac

if [ -n "${WHEEL_DOMAIN:-}" ]; then
    echo "wheel preflight: TLS mode — https://$WHEEL_DOMAIN, Caddy publishing 80 and 443"
else
    echo "wheel preflight: tunnel mode — nothing published; reach it with an SSH tunnel (see infra/vps/README.md)"
fi
echo "wheel preflight: ok"
