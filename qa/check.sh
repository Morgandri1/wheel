#!/usr/bin/env bash
# make check — the merge gate. QA owns this.
#
# Runs every gate that CAN run right now and LOUDLY skips the ones that can't,
# so a green check never overstates how much of the tree is actually covered.
# Exit non-zero if any gate fails.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# APPENDED, not prepended: these are a fallback for finding cargo/pnpm when they are not
# on PATH, never an override of the toolchain the caller chose. Prepending them meant a
# developer who selected node 22 to match CI silently got homebrew's node anyway, and the
# gate reported a verdict about a runtime nobody asked for.
export PATH="$PATH:$HOME/.cargo/bin:/opt/homebrew/bin"
export CARGO_TERM_COLOR=always

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  B=$'\033[1m'; R=$'\033[31m'; G=$'\033[32m'; Y=$'\033[33m'; C=$'\033[36m'; Z=$'\033[0m'
else
  B=""; R=""; G=""; Y=""; C=""; Z=""
fi

# Two kinds of not-run, and the difference decides whether CI may go green:
#   ABSENT  — the area does not exist yet (no web/package.json). Nobody can fix that by
#             trying harder, so strict mode tolerates it and simply reports it.
#   UNAVAIL — the gate itself could not run (no jsonschema, no cargo-llvm-cov, no docker).
#             In CI that is a broken pipeline pretending to be a passing one, so strict
#             mode FAILS on it. This is the distinction that keeps `check-strict` honest
#             without making CI red for a reason no one can act on.
PASS=(); FAIL=(); ABSENT=(); UNAVAIL=(); CONTENDED=()
STRICT="${CHECK_STRICT:-0}"   # CI sets 1: UNAVAIL becomes a failure
COVERAGE="${COVERAGE:-0}"     # coverage is opt-in locally (slow, memory-hungry); CI sets 1
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
  elif [ $rc -eq 75 ]; then
    CONTENDED+=("$name")
    printf '%s  ⊘ %s did not run — another worktree held the cargo lock%s\n' "$Y" "$name" "$Z"
  elif [ $rc -eq 77 ]; then
    UNAVAIL+=("$name — the gate reported it could not run")
    printf '%s  ⊘ %s skipped (could not run)%s\n' "$Y" "$name" "$Z"
  else
    FAIL+=("$name")
    printf '%s  ✗ %s FAILED%s (%ss, exit %d)\n' "$R" "$name" "$Z" "$t1" "$rc"
  fi
}

skip() { # skip <name> <why> — the gate could not run (fails under CHECK_STRICT)
  if [ -n "$ONLY" ] && [[ "$1" != $ONLY* ]]; then return 0; fi
  UNAVAIL+=("$1 — $2")
  printf '%s  ⊘ %s skipped — %s%s\n' "$Y" "$1" "$2" "$Z"
}

skip_absent() { # skip_absent <name> <why> — the area does not exist yet (tolerated in strict)
  if [ -n "$ONLY" ] && [[ "$1" != $ONLY* ]]; then return 0; fi
  ABSENT+=("$1 — $2")
  printf '%s  ⊘ %s not applicable — %s%s\n' "$Y" "$1" "$2" "$Z"
}

have() { command -v "$1" >/dev/null 2>&1; }

CARGO_LOCK="${WHEEL_CARGO_LOCK:-/tmp/wheel-cargo.lock}"
cargo_locked() { python3 qa/tools/with_lock.py "$CARGO_LOCK" "$@"; }

# Prefer the QA venv (jsonschema, requests, pytest) when it exists; `make bootstrap` creates it.
PY=python3
[ -x qa/.venv/bin/python ] && PY=qa/.venv/bin/python

# ----------------------------------------------------------------- rust
RUST_CRATES=$(ls -d crates/*/Cargo.toml 2>/dev/null | wc -l | tr -d ' ')
if [ "$RUST_CRATES" = "0" ]; then
  skip_absent "rust:fmt"    "no crates yet (crates/*/Cargo.toml)"
  skip_absent "rust:clippy" "no crates yet"
  skip_absent "rust:test"   "no crates yet"
elif ! have cargo; then
  skip "rust:fmt"    "cargo not installed — run 'make bootstrap'"
  skip "rust:clippy" "cargo not installed"
  skip "rust:test"   "cargo not installed"
else
  step "rust:fmt"    cargo_locked cargo fmt --all -- --check
  step "rust:clippy" cargo_locked cargo clippy --workspace --all-targets -- -D warnings
  step "rust:test"   cargo_locked "$PY" qa/tools/cargo_test_gate.py cargo test --workspace
  # ARCHITECTURE.md §0b: >=90% lines PER CRATE (PM ruling 2026-09-05 — a workspace
  # average hides a 0%-covered crate behind a well-tested one). Exemptions are declared
  # in qa/tools/coverage_gate.py, each naming its crate, reason and expiry event.
  if [ "$COVERAGE" = "1" ]; then
    step "rust:coverage" cargo_locked "$PY" qa/tools/coverage_gate.py
  else
    skip_absent "rust:coverage" "opt-in locally: run 'make coverage' (CI enforces it via check-strict)"
  fi
fi

# ----------------------------------------------------------------- web
web_script() { # does web/package.json define this script?
  [ -f web/package.json ] && node -e "process.exit(require('./web/package.json').scripts?.['$1']?0:1)" 2>/dev/null
}
if [ ! -f web/package.json ]; then
  skip_absent "web:lint"      "no web/package.json yet"
  skip_absent "web:typecheck" "no web/package.json yet"
  skip_absent "web:test"      "no web/package.json yet"
elif ! have pnpm; then
  skip "web:lint"      "pnpm not installed — run 'make bootstrap'"
  skip "web:typecheck" "pnpm not installed"
  skip "web:test"      "pnpm not installed"
else
  if [ ! -d web/node_modules ]; then
    step "web:install" pnpm -C web install --frozen-lockfile
  fi
  # CI pins node 22 (.github/workflows/ci.yml). A different major here can make the web
  # gates disagree with CI for reasons that have nothing to do with the code — node >= 22.4
  # defines its own experimental `localStorage` global that shadows jsdom's, so every
  # browser-storage test fails locally and passes in CI. Say so up front rather than let
  # the next person debug their runtime instead of the product.
  node_major="$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo '')"
  if [ -n "$node_major" ] && [ "$node_major" != "22" ]; then
    printf '%s  ! node v%s — CI pins node 22. If a web gate fails here and is green on CI, suspect the runtime first.%s\n' \
      "$Y" "$node_major" "$Z"
  fi
  for s in lint typecheck test; do
    if web_script "$s"; then step "web:$s" pnpm -C web run "$s"
    else skip "web:$s" "no '$s' script in web/package.json"; fi
  done
  if web_script "coverage"; then step "web:coverage" pnpm -C web run coverage
  else skip_absent "web:coverage" "no 'coverage' script in web/package.json (§0b: vitest --coverage, lines: $COV_MIN)"; fi
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
# Pure stdlib — deliberately OUTSIDE the jsonschema guard. It was briefly nested inside it,
# which meant the gate silently vanished (not even reported as skipped) on a machine without
# the venv. A gate that disappears is worse than one that fails.
step "qa:wire-conformance" "$PY" qa/contract/wire_matrix_conformance.py

# Engine routes: ARCHITECTURE.md §4 vs docs/PROTOCOL.md (and a live engine when
# WHEEL_ENGINE_URL is set, for 404-vs-405).
step "qa:route-parity" "$PY" qa/contract/route_parity.py

# The plan is the contract and the suites are the evidence; when they drift, both keep
# looking healthy — a suite reports `ok SEC-vault-env-scope` under a name the plan has
# never heard of, so the criterion cannot be traced, reported on, or reviewed. One
# direction only: a planned ID with no test yet is normal (half the plan is M2/M3).
step "qa:id-traceability" "$PY" qa/contract/id_traceability.py

step "qa:suite-isolation" "$PY" qa/contract/suite_isolation.py

# A broken workflow file means CI never runs, which reads as "no red" not "no verdict".
step "qa:ci-lint"      "$PY" qa/contract/ci_workflow_lint.py

# Every data-testid the E2E suite selects must exist in web/src. Playwright can
# only report this by launching a browser and failing 30s in; the same drift is
# detectable statically in under a second, so it is caught here instead.
step "qa:testid-parity" "$PY" qa/contract/testid_parity.py

# A suite that steers the fake harness through the ENGINE's environment steers
# nothing since F015, and reports the resulting silence as an engine fault. Static,
# instant, and it caught three suites I had already broken.
step "qa:fake-steering" "$PY" qa/contract/fake_steering.py

# The F015 boundary. ADVERSARY asked to review changes to INHERITED_ENV; this is what
# makes that review durable rather than dependent on somebody remembering to mention
# it. Static and instant — it reads the constant and compares it to a pinned list.
step "qa:env-allowlist" "$PY" qa/contract/env_allowlist.py

# API's own tests for the probe-project pruner. Wired here because the subject is a
# DELETION tool that runs against production data, and because it costs nothing: plain
# bash, no deps, no network, sub-second. It has no prerequisites, so exit 0/1 is honest
# — there is no "could not run" state for it to hide in.
#
# Not mine, deliberately left where its owner keeps it. A gate does not have to live in
# qa/ to be worth running before a merge.
if [ -x infra/tests/prune-probe-projects.test.sh ]; then
  step "infra:prune-probe-projects" bash infra/tests/prune-probe-projects.test.sh
else
  skip_absent "infra:prune-probe-projects" "infra/tests/prune-probe-projects.test.sh not present"
fi

# The image must contain the binaries the contract depends on. BUG-010: the `wheel`
# CLI was silently absent because the Dockerfile built a bin name that does not exist
# under `|| true` and copied it with an optional glob. Nothing failed; it just was not there.
#
# This gate needs the image, and `make check` deliberately does not build one -- a full
# Rust image build on the job everyone waits for is the wrong trade (API's call, and they
# are right). Under CHECK_STRICT a skip is a failure, correctly, so the gate cannot simply
# skip here: it would be red forever in a job that can never satisfy it.
#
# So it is NOT APPLICABLE where no image exists, and it runs for real in the CI job that
# builds one. That is only honest if something guarantees it still runs SOMEWHERE, which is
# why ci_workflow_lint.py asserts image_contents.py is invoked by a job that also runs
# `make engine-image`. Moving a gate out of check must not be how it quietly stops running.
# PM A10: dependency weight is P1 and it drifts silently. Cheap -- `cargo metadata` only,
# no compile -- so it belongs in the gate everyone runs rather than a job nobody watches.
# Binary size is the other half and needs a real release build, so it is `make size` and CI.
step "qa:deps-budget" "$PY" qa/tools/deps_gate.py

if docker image inspect wheel-engine:dev >/dev/null 2>&1 || \
   docker image inspect wheel-engine:test >/dev/null 2>&1; then
  step "qa:image-contents" "$PY" qa/contract/image_contents.py
else
  skip_absent "qa:image-contents" "no engine image here; runs in the CI job that builds one (ci_workflow_lint asserts it does)"
fi

if "$PY" -c "import jsonschema" >/dev/null 2>&1; then
  # Proves the schema contract test can actually fail, using scratch schemas. Runs today.
  step "qa:contract-selftest" "$PY" qa/contract/selftest_schema.py
  # Runs today and self-skips until SDK exports the schema, so it goes green on its own.
  step "qa:contract-schema" "$PY" qa/contract/schema_fixtures.py
else
  skip "qa:contract-selftest" "jsonschema missing — run 'make bootstrap' (creates qa/.venv)"
  skip "qa:contract-schema"   "jsonschema missing — run 'make bootstrap'"
fi

# E2E is heavy (browser download, two dev servers) so it is not part of `make check`.
# `make test-e2e` runs it; CI has its own job.

# ----------------------------------------------------------------- summary
echo
printf '%s──────── make check ────────%s\n' "$B" "$Z"
for p in "${PASS[@]:-}"; do [ -n "$p" ] && printf '%s  pass%s  %s\n' "$G" "$Z" "$p"; done
for a in "${ABSENT[@]:-}"; do [ -n "$a" ] && printf '%s  n/a %s  %s\n' "$Y" "$Z" "$a"; done
for u in "${UNAVAIL[@]:-}"; do [ -n "$u" ] && printf '%s  SKIP%s  %s\n' "$Y" "$Z" "$u"; done
for c in "${CONTENDED[@]:-}"; do [ -n "$c" ] && printf '%s  LOCK%s  %s — another worktree held the cargo lock\n' "$Y" "$Z" "$c"; done
for f in "${FAIL[@]:-}"; do [ -n "$f" ] && printf '%s  FAIL%s  %s\n' "$R" "$Z" "$f"; done
echo

NF=${#FAIL[@]}; NU=${#UNAVAIL[@]}; NA=${#ABSENT[@]}; NC=${#CONTENDED[@]}
if [ "$NF" -gt 0 ]; then
  printf '%s✗ make check FAILED — %d gate(s) red.%s Fix before merging to main.\n' "$R" "$NF" "$Z"
  exit 1
fi
# Contention is not failure and not a skip: it is "no verdict yet". Six worktrees share one
# cargo lock, so a busy host can turn every Rust gate into a not-run — and a mostly-green
# check with a few grey lines is exactly what a person merges on. The run therefore exits
# non-zero and says the word INCONCLUSIVE, because "I could not check" must never read as
# "the check passed". Re-running is the whole fix; nothing is broken.
if [ "$NC" -gt 0 ]; then
  printf '%s✗ make check INCONCLUSIVE — %d gate(s) never ran: the cargo lock was held\n' "$R" "$NC"
  printf '  by another worktree for longer than WHEEL_LOCK_TIMEOUT (%ss).%s\n' "${WHEEL_LOCK_TIMEOUT:-1800}" "$Z"
  printf '  Nothing is broken and nothing is proven. Run it again when the host is quieter,\n'
  printf '  or raise WHEEL_LOCK_TIMEOUT. Do NOT merge on this result.\n'
  exit 1
fi
if [ "$STRICT" = "1" ] && [ "$NU" -gt 0 ]; then
  printf '%s✗ make check FAILED — %d gate(s) COULD NOT RUN and CHECK_STRICT=1.%s\n' "$R" "$NU" "$Z"
  printf '  In CI a gate that cannot run is a broken pipeline pretending to be a passing\n'
  printf '  one. Install the missing tooling (make bootstrap) rather than lowering the bar.\n'
  exit 1
fi
if [ "$((NU + NA))" -gt 0 ]; then
  printf '%s✓ make check passed, but %d gate(s) did not run%s (%d not applicable, %d unavailable).\n' \
    "$Y" "$((NU + NA))" "$Z" "$NA" "$NU"
  printf '  Expected while the tree is still being built out. It is NOT a statement that\n'
  printf '  those areas are healthy — only that they could not be checked here.\n'
  exit 0
fi
printf '%s✓ make check passed — all gates green.%s\n' "$G" "$Z"
