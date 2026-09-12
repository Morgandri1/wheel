# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
# shellcheck shell=bash

# Sourced by rehearse.sh and deploy.sh. Computes the settings that follow from WHEEL_DOMAIN,
# rather than asking the operator to keep them consistent by hand, and appends them to a `.env`
# file — never the operator's own, always a working copy.
#
# Why appended lines and not compose-file `${WHEEL_DOMAIN:+...}` interpolation: Compose always
# resolves `${VAR:+x}` to a literal, possibly empty, string — there is no way to make the KEY
# disappear from that expression alone. An empty `PUBLIC_BASE_URL` is not the same as an unset
# one: wheeld's own default (derived from its bind address) applies only when the variable is
# fully absent from its environment, and `WHEEL_API_URL`/`WHEEL_PUBLIC_ORIGIN`'s "unset" defaults
# only apply the same way. compose.yml therefore uses the bare passthrough form (`- KEY`, no `=`)
# for every value this function may omit, which Compose passes through from ITS OWN process
# environment (or, as here, an `--env-file`) and drops entirely when that key is not present —
# confirmed against a running container, not assumed.
wheel_derive_env() {
    target="$1"
    if [ -n "${WHEEL_DOMAIN:-}" ]; then
        {
            echo "PUBLIC_BASE_URL=https://$WHEEL_DOMAIN"
            echo "WHEEL_PUBLIC_ORIGIN=https://$WHEEL_DOMAIN"
            echo "WHEEL_ALLOWED_HOSTS=wheeld,$WHEEL_DOMAIN"
            echo "WHEEL_TRUSTED_PROXIES=${WHEEL_CADDY_IP:-10.231.8.10}"
            echo "WHEEL_TRUST_PROXY=1"
            echo "COMPOSE_PROFILES=tls"
        } >>"$target"
    else
        {
            echo "WHEEL_ALLOWED_HOSTS=wheeld"
            echo "WHEEL_PUBLIC_ORIGIN=http://localhost:${WHEEL_WEB_PORT:-3000}"
        } >>"$target"
        # PUBLIC_BASE_URL, WHEEL_TRUSTED_PROXIES, WHEEL_TRUST_PROXY and COMPOSE_PROFILES stay
        # unwritten in tunnel mode: wheeld computes its own PUBLIC_BASE_URL from its bind address
        # (http://localhost:8080), nothing proxies to wheeld or web so there is no forwarded
        # header to trust, and no `tls` profile means Caddy never starts.
    fi
}
