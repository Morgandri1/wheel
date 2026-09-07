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

# The build SHA, so a deploy can be CONFIRMED rather than inferred — and a marker saying which
# KIND of fact it is, because the two are not equally strong.
#
# `docker/Dockerfile.host` takes it as `ARG GIT_SHA` and `make engine-image` passes
# `--build-arg GIT_SHA=$(git rev-parse HEAD)`. Railway does not: it builds the Dockerfile directly
# from `dockerfilePath`, passes no build args, and the ARG keeps its `unknown` default. Measured on
# the running host: WHEEL_BUILD_SHA=unknown while RAILWAY_GIT_COMMIT_SHA held the real commit.
#
# A build-arg SHA is baked in at BUILD time, so it names the source the binaries were compiled
# from. The platform variable is injected at DEPLOY time, so it names the commit that TRIGGERED the
# deploy. They diverge whenever a deploy does not rebuild — a restart, or a variable change — and
# then the platform value is a FALSE CONFIRM: it reports code the running binaries are not. That is
# worse than `unknown`, because `unknown` sends an operator to check and a wrong SHA stops them.
#
# So the platform value is taken only as a fallback, and never silently: WHEEL_BUILD_SHA_SOURCE
# always says which kind of fact WHEEL_BUILD_SHA is, so nothing downstream has to guess and nothing
# can report a triggering commit as though it were a compiled one.
if [ -n "${WHEEL_BUILD_SHA:-}" ] && [ "${WHEEL_BUILD_SHA}" != "unknown" ]; then
    WHEEL_BUILD_SHA_SOURCE=build-arg
elif [ -n "${RAILWAY_GIT_COMMIT_SHA:-}" ]; then
    WHEEL_BUILD_SHA="$RAILWAY_GIT_COMMIT_SHA"
    WHEEL_BUILD_SHA_SOURCE=deploy-trigger
else
    WHEEL_BUILD_SHA="${WHEEL_BUILD_SHA:-unknown}"
    WHEEL_BUILD_SHA_SOURCE=none
fi
export WHEEL_BUILD_SHA WHEEL_BUILD_SHA_SOURCE

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
