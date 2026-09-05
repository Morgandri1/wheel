#!/bin/sh
# One image, two roles (§4b). The docker sandbox backend runs WHEEL_ROLE=engine;
# the sandbox supervisor runs WHEEL_ROLE=host.
#
# The roles need different privilege, which is why the drop happens here rather than via a single
# `USER` line in the Dockerfile:
#
#   engine — must be NON-ROOT. `--permission-mode bypassPermissions` is refused as root with exit 1
#            and empty stdout, which is indistinguishable from "not logged in", so an engine running
#            as root would make every agent look permanently unauthenticated.
#   host   — must be ROOT when SANDBOX_BACKEND=process. It chowns each project's 0700 tree and
#            setuids every engine to that project's own uid; unprivileged it can do neither, and
#            would leave every project's data owned by one shared user.
#
# Written to work either way round: if the image already starts unprivileged, there is nothing to
# drop and we exec directly, so this is safe before and after the `USER` line is removed.
set -eu

AGENT_UID=10001
AGENT_GID=10001

exec_as_agent() {
    if [ "$(id -u)" = "0" ]; then
        # --clear-groups matters: supplementary groups survive a uid change and would carry the
        # host's memberships into the tenant.
        exec setpriv --reuid="$AGENT_UID" --regid="$AGENT_GID" --clear-groups "$@"
    fi
    exec "$@"
}

case "${WHEEL_ROLE:-engine}" in
  engine) exec_as_agent /usr/local/bin/wheel-engine "$@" ;;
  host)
    if [ ! -x /usr/local/bin/wheel-host ]; then
      echo "wheel: WHEEL_ROLE=host but wheel-host is not in this image" >&2
      exit 2
    fi
    # Deliberately not dropped: see above. The host drops privilege per child instead.
    if [ "${SANDBOX_BACKEND:-docker}" = "process" ] && [ "$(id -u)" != "0" ]; then
      echo "wheel: SANDBOX_BACKEND=process requires root (it setuids each project's engine)," >&2
      echo "       but this container is running as uid $(id -u)." >&2
      exit 2
    fi
    exec /usr/local/bin/wheel-host "$@"
    ;;
  *)
    echo "wheel: WHEEL_ROLE must be 'engine' or 'host', got '${WHEEL_ROLE}'" >&2
    exit 2
    ;;
esac
