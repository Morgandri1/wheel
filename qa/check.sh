#!/usr/bin/env bash
# make check — the merge gate. QA owns this.
#
# Runs every gate that CAN run right now and LOUDLY skips the ones that can't,
# so a green check never overstates how much of the tree is actually covered.
# Exit non-zero if any gate fails.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
export CARGO_TERM_COLOR=always

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  B=$'\033[1m'; R=$'\033[31m'; G=$'\033[32m'; Y=$'\033[33m'; C=$'\033[36m'; Z=$'\033[0m'
else
  B=""; R=""; G=""; Y=""; C=""; Z=""
fi

PASS=(); FAIL=(); SKIP=()
STRICT="${CHECK_STRICT:-0}"   # CI sets 1: skips become failures
ONLY="${CHECK_ONLY:-}"        # e.g. CHECK_ONLY=rust

step() { # step <name> <cmd...>
  local name="$1"; shift
  if [ -n "$ONLY" ] && [[ "$name" != $ONLY* ]]; then return 0; fi
  printf '%s▸ %s%s\n' "$C" "$name" "$Z"
  local t0 t1; t0=$SECONDS
  if "$@"; then
    t1=$((SECONDS-t0)); PASS+=("$name (${t1}s)")
    printf '%s  ✓ %s%s (%ss)\n' "$G" "$name" "$Z" "$t1"
  else
    t1=$((SECONDS-t0)); FAIL+=("$name")
    printf '%s  ✗ %s FAILED%s (%ss)\n' "$R" "$name" "$Z" "$t1"
  fi
}

skip() { # skip <name> <why>
  if [ -n "$ONLY" ] && [[ "$1" != $ONLY* ]]; then return 0; fi
  SKIP+=("$1 — $2")
  printf '%s  ⊘ %s skipped — %s%s\n' "$Y" "$1" "$2" "$Z"
}

have() { command -v "$1" >/dev/null 2>&1; }

# ----------------------------------------------------------------- rust
RUST_CRATES=$(ls -d crates/*/Cargo.toml 2>/dev/null | wc -l | tr -d ' ')
if [ "$RUST_CRATES" = "0" ]; then
  skip "rust:fmt"    "no crates yet (crates/*/Cargo.toml)"
  skip "rust:clippy" "no crates yet"
  skip "rust:test"   "no crates yet"
elif ! have cargo; then
  skip "rust:fmt"    "cargo not installed — run 'make bootstrap'"
  skip "rust:clippy" "cargo not installed"
  skip "rust:test"   "cargo not installed"
else
  step "rust:fmt"    cargo fmt --all -- --check
  step "rust:clippy" cargo clippy --workspace --all-targets -- -D warnings
  step "rust:test"   cargo test --workspace
fi

# ----------------------------------------------------------------- web
web_script() { # does web/package.json define this script?
  [ -f web/package.json ] && node -e "process.exit(require('./web/package.json').scripts?.['$1']?0:1)" 2>/dev/null
}
if [ ! -f web/package.json ]; then
  skip "web:lint"      "no web/package.json yet"
  skip "web:typecheck" "no web/package.json yet"
  skip "web:test"      "no web/package.json yet"
elif ! have pnpm; then
  skip "web:lint"      "pnpm not installed — run 'make bootstrap'"
  skip "web:typecheck" "pnpm not installed"
  skip "web:test"      "pnpm not installed"
else
  if [ ! -d web/node_modules ]; then
    step "web:install" pnpm -C web install --frozen-lockfile
  fi
  for s in lint typecheck test; do
    if web_script "$s"; then step "web:$s" pnpm -C web run "$s"
    else skip "web:$s" "no '$s' script in web/package.json"; fi
  done
fi

# ----------------------------------------------------------------- qa's own
if have python3; then
  step "qa:harness-selftest" python3 qa/harness/selftest.py
else
  skip "qa:harness-selftest" "python3 not installed"
fi

step "qa:wire-matrix" python3 qa/tools/gen_wire_matrix.py --check

if ls docs/schema/*.json >/dev/null 2>&1; then
  step "qa:contract-schema" python3 qa/contract/schema_fixtures.py
else
  skip "qa:contract-schema" "docs/schema/*.json not exported yet (SDK)"
fi

# ----------------------------------------------------------------- summary
echo
printf '%s──────── make check ────────%s\n' "$B" "$Z"
for p in "${PASS[@]:-}"; do [ -n "$p" ] && printf '%s  pass%s  %s\n' "$G" "$Z" "$p"; done
for s in "${SKIP[@]:-}"; do [ -n "$s" ] && printf '%s  SKIP%s  %s\n' "$Y" "$Z" "$s"; done
for f in "${FAIL[@]:-}"; do [ -n "$f" ] && printf '%s  FAIL%s  %s\n' "$R" "$Z" "$f"; done
echo

NF=${#FAIL[@]}; NS=${#SKIP[@]}
if [ "$NF" -gt 0 ]; then
  printf '%s✗ make check FAILED — %d gate(s) red.%s Fix before merging to main.\n' "$R" "$NF" "$Z"
  exit 1
fi
if [ "$NS" -gt 0 ]; then
  if [ "$STRICT" = "1" ]; then
    printf '%s✗ make check FAILED — %d gate(s) skipped and CHECK_STRICT=1.%s\n' "$R" "$NS" "$Z"
    exit 1
  fi
  printf '%s✓ make check passed, but %d gate(s) were SKIPPED%s — coverage is partial.\n' "$Y" "$NS" "$Z"
  printf '  This is expected while the tree is still being built out. It is NOT a\n'
  printf '  statement that those areas are healthy — only that they do not exist yet.\n'
  exit 0
fi
printf '%s✓ make check passed — all gates green.%s\n' "$G" "$Z"
