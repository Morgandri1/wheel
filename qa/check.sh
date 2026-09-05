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
COV_MIN="${COV_MIN:-90}"      # ARCHITECTURE.md §0b
ONLY="${CHECK_ONLY:-}"        # e.g. CHECK_ONLY=rust

# A step exiting 77 means "I could not run" and is recorded as a SKIP, never a pass.
# A gate that cannot run must not look like a gate that passed.
step() { # step <name> <cmd...>
  local name="$1"; shift
  if [ -n "$ONLY" ] && [[ "$name" != $ONLY* ]]; then return 0; fi
  printf '%s▸ %s%s\n' "$C" "$name" "$Z"
  local t0 t1 rc; t0=$SECONDS
  "$@"; rc=$?
  t1=$((SECONDS-t0))
  if [ $rc -eq 0 ]; then
    PASS+=("$name (${t1}s)")
    printf '%s  ✓ %s%s (%ss)\n' "$G" "$name" "$Z" "$t1"
  elif [ $rc -eq 77 ]; then
    SKIP+=("$name — gate reported it could not run")
    printf '%s  ⊘ %s skipped%s\n' "$Y" "$name" "$Z"
  else
    FAIL+=("$name")
    printf '%s  ✗ %s FAILED%s (%ss, exit %d)\n' "$R" "$name" "$Z" "$t1" "$rc"
  fi
}

skip() { # skip <name> <why>
  if [ -n "$ONLY" ] && [[ "$1" != $ONLY* ]]; then return 0; fi
  SKIP+=("$1 — $2")
  printf '%s  ⊘ %s skipped — %s%s\n' "$Y" "$1" "$2" "$Z"
}

have() { command -v "$1" >/dev/null 2>&1; }

# Prefer the QA venv (jsonschema, requests, pytest) when it exists; `make bootstrap` creates it.
PY=python3
[ -x qa/.venv/bin/python ] && PY=qa/.venv/bin/python

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
  # ARCHITECTURE.md §0b: >=90% lines per crate. A failing check, not a warning.
  if cargo llvm-cov --version >/dev/null 2>&1; then
    step "rust:coverage" cargo llvm-cov --workspace --fail-under-lines "$COV_MIN"
  else
    skip "rust:coverage" "cargo-llvm-cov not installed — run 'make bootstrap'"
  fi
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
  if web_script "coverage"; then step "web:coverage" pnpm -C web run coverage
  else skip "web:coverage" "no 'coverage' script in web/package.json (§0b: vitest --coverage, lines: $COV_MIN)"; fi
fi

# ----------------------------------------------------------------- qa's own
if have python3; then
  step "qa:harness-selftest" "$PY" qa/harness/selftest.py
else
  skip "qa:harness-selftest" "python3 not installed"
fi

step "qa:wire-matrix" "$PY" qa/tools/gen_wire_matrix.py --check

# The contract gates need `jsonschema` from qa/requirements.txt. A MISSING DEPENDENCY and a
# BROKEN SCHEMA must never look the same: skip loudly here, and let CI (which bootstraps first,
# and runs CHECK_STRICT=1) be the place where a skip is a hard failure.
if "$PY" -c "import jsonschema" >/dev/null 2>&1; then
  # Proves the schema contract test can actually fail, using scratch schemas. Runs today.
  step "qa:contract-selftest" "$PY" qa/contract/selftest_schema.py

# wheel-core's exported matrix vs the ARCHITECTURE.md §3 matrix QA derives independently.
step "qa:wire-conformance" "$PY" qa/contract/wire_matrix_conformance.py
  # Runs today and self-skips until SDK exports the schema, so it goes green on its own.
  step "qa:contract-schema" "$PY" qa/contract/schema_fixtures.py
else
  skip "qa:contract-selftest" "jsonschema missing — run 'make bootstrap' (creates qa/.venv)"
  skip "qa:contract-schema"   "jsonschema missing — run 'make bootstrap'"
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
