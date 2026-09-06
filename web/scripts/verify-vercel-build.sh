#!/usr/bin/env bash
# Reproduce a Vercel build locally: a clean copy of web/, NODE_ENV=production, and the
# commands vercel.json actually configures -- not the ones we remember configuring.
#
# Vercel builds with NODE_ENV=production, which makes pnpm skip devDependencies. Without
# typescript installed Next never reads tsconfig.json, the "@/*" alias goes unregistered, and
# every internal import fails as "Module not found" -- a source error for a config cause.
set -u -o pipefail

ref="${1:-HEAD}"
repo="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
sim="$(mktemp -d "${TMPDIR:-/tmp}/wheel-vercel-sim.XXXXXX")"
trap 'rm -rf "$sim"' EXIT

git -C "$repo" archive "$ref" web | tar -x -C "$sim" --strip-components=1
cd "$sim"

install_cmd=$(node -p "require('./vercel.json').installCommand")
build_cmd=$(node -p "require('./vercel.json').buildCommand")

export NODE_ENV=production
export NEXT_PUBLIC_AUTH_MODE="${NEXT_PUBLIC_AUTH_MODE:-local}"
export NEXT_PUBLIC_API_URL="${NEXT_PUBLIC_API_URL:-https://wheel-api-production.up.railway.app}"

echo "ref=$ref  node=$(node -v)"
echo "install: $install_cmd"
eval "$install_cmd" || { echo "FAIL: install"; exit 1; }
echo "build:   $build_cmd"
eval "$build_cmd" || { echo "FAIL: build"; exit 1; }

# A build can print errors and still exit 0; BUILD_ID only exists on a real success.
[ -f .next/BUILD_ID ] || { echo "FAIL: build produced no .next/BUILD_ID"; exit 1; }
echo "PASS: BUILD_ID=$(cat .next/BUILD_ID)"
