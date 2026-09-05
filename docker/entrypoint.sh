#!/bin/sh
# One image, two roles (§4b). The docker sandbox backend runs WHEEL_ROLE=engine;
# the sandbox supervisor runs WHEEL_ROLE=host.
set -eu

case "${WHEEL_ROLE:-engine}" in
  engine) exec /usr/local/bin/wheel-engine "$@" ;;
  host)
    if [ ! -x /usr/local/bin/wheel-host ]; then
      echo "wheel: WHEEL_ROLE=host but wheel-host is not in this image" >&2
      exit 2
    fi
    exec /usr/local/bin/wheel-host "$@"
    ;;
  *)
    echo "wheel: WHEEL_ROLE must be 'engine' or 'host', got '${WHEEL_ROLE}'" >&2
    exit 2
    ;;
esac
