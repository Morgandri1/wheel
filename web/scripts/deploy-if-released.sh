#!/usr/bin/env bash
# Fire the Vercel deploy hook when a push to main CHANGED web/package.json's version.
#
# Why this exists: Vercel's ignoreCommand can only inspect the commit it is deploying, and on a
# shared main the release commit is usually no longer HEAD by the time Vercel looks — another lane
# has landed on top, that commit's version equals its parent's, and the release is skipped. The
# release then sits on main, deployed to nobody, and the only way to notice is to go looking.
#
# A push range does not have that problem: "did the version change between what main was and what
# main now is" is answerable regardless of which commit ended up on top.
#
#   deploy-if-released.sh <before-sha> <after-sha>
#   DRY_RUN=1  print the decision, fire nothing.
set -euo pipefail

before="${1:?usage: deploy-if-released.sh <before-sha> <after-sha>}"
after="${2:?usage: deploy-if-released.sh <before-sha> <after-sha>}"

version_at() {
  # A ref without the file (a first push, a shallow clone) reads as empty, which compares unequal
  # to any real version and therefore deploys. Deploying once too often is recoverable; silently
  # never deploying is the failure this script exists to remove.
  git show "$1:web/package.json" 2>/dev/null | sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' | head -1
}

from="$(version_at "$before")"
to="$(version_at "$after")"

if [ "$from" = "$to" ]; then
  echo "web: version unchanged at ${to:-none} — no deploy"
  exit 0
fi

echo "web: release ${from:-none} -> ${to:-none}"

if [ -n "${DRY_RUN:-}" ]; then
  echo "DRY_RUN: would fire the deploy hook"
  exit 0
fi

# Fail loudly. A release that silently does not ship is precisely the bug being fixed, so a missing
# hook must break the build rather than pass quietly.
if [ -z "${VERCEL_DEPLOY_HOOK:-}" ]; then
  echo "::error::web/package.json version changed to ${to} but VERCEL_DEPLOY_HOOK is not set — the release will NOT deploy." >&2
  exit 1
fi

code="$(curl -sS -X POST -o /dev/null -w '%{http_code}' --max-time 30 "$VERCEL_DEPLOY_HOOK")"
if [ "$code" != "200" ] && [ "$code" != "201" ]; then
  echo "::error::deploy hook answered ${code}; the release did not start." >&2
  exit 1
fi
echo "web: deploy hook fired for ${to} (${code})"
