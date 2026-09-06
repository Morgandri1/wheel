# Wheel — root Makefile.
# `check` is the merge gate and is owned by QA (qa/check.sh). Teams add their own
# targets under their own area; please don't edit `check` without pinging QA.

SHELL := /bin/bash
.DEFAULT_GOAL := help

export PATH := $(HOME)/.cargo/bin:/opt/homebrew/bin:$(PATH)

.PHONY: help check check-strict fmt clippy test-rust coverage web-lint web-typecheck web-test \
        qa-selftest test-int test-e2e test-pkg test-live test-live-ws bootstrap clean

help: ## show this help
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) | awk -F':.*?## ' '{printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

## ---------------------------------------------------------------- merge gate
check: ## run every gate that can run (fmt, clippy, tests, web, qa) — RUN BEFORE MERGING
	@bash qa/check.sh

check-strict: ## CI gate: coverage enforced, and a gate that COULD NOT RUN is a failure
	@CHECK_STRICT=1 COVERAGE=1 bash qa/check.sh

## ---------------------------------------------------------------- granular
fmt: ## rustfmt the workspace (writes)
	@cargo fmt --all

clippy: ## clippy with warnings denied
	@CHECK_ONLY=rust:clippy bash qa/check.sh

test-rust: ## cargo test --workspace
	@CHECK_ONLY=rust:test bash qa/check.sh

web-lint: ## web lint
	@CHECK_ONLY=web:lint bash qa/check.sh

web-typecheck: ## web typecheck
	@CHECK_ONLY=web:typecheck bash qa/check.sh

web-test: ## web unit tests
	@CHECK_ONLY=web:test bash qa/check.sh

coverage: ## coverage gate only (§0b: >=90% lines)
	@CHECK_ONLY=rust:coverage COVERAGE=1 bash qa/check.sh

qa-selftest: ## test the fake harness itself
	@python3 qa/harness/selftest.py

## ---------------------------------------------------------------- heavier suites
test-int: ## integration suite (docker; needs wheel-engine:test + compose)
	@bash qa/integration/run.sh

test-e2e: ## Playwright end-to-end suite
	@bash qa/e2e/run.sh

# Installs before building on purpose: a stale node_modules makes `next build` fail its
# typecheck on a missing @types/*, which is indistinguishable from a broken product build.
# I chased exactly that for several minutes before checking whether the dependency was
# merely uninstalled. A gate must not be able to blame the code for the state of a laptop.
test-pkg: ## E2E against the PACKAGED board (npx wheel-web) — builds it first, minutes not seconds
	@pnpm -C web install --frozen-lockfile
	@pnpm -C web build:pkg
	@pnpm -C web pack:pkg
	@# Free the port first: a stale server on :3300 makes Playwright's new one fail to
	@# bind while its URL check passes against the old one, so the suite silently tests a
	@# server launched with different flags than the config says.
	@lsof -ti :3300 | xargs -r kill -9 2>/dev/null || true
	@lsof -ti :8789 | xargs -r kill -9 2>/dev/null || true
	@bash qa/e2e/run.sh --config packaged.config.ts

test-live-ws: ## WS-vs-DB log stream parity against a running stack (needs infra/docker-compose.yml up)
	@node qa/live/ws_streams_parity.mjs

test-live: ## OPT-IN: same suites against the REAL claude/codex CLIs. Costs money. Never in CI.
	@WHEEL_LIVE=1 bash qa/integration/run.sh

## ---------------------------------------------------------------- setup
bootstrap: ## install the toolchain (rust, node, pnpm, cargo-llvm-cov, QA venv)
	@bash qa/bootstrap.sh

clean: ## remove build artefacts
	@rm -rf target web/.next web/node_modules

# --- SDK: images -----------------------------------------------------------
.PHONY: engine-image engine-image-test image-verify-prod

engine-image: ## build wheel-engine:dev (production layout)
	docker build -f docker/Dockerfile.host -t wheel-engine:dev .

engine-image-test: engine-image ## build wheel-engine:test (QA fake harnesses)
	docker build -f docker/Dockerfile.test --build-arg BASE=wheel-engine:dev -t wheel-engine:test .

image-verify-prod: engine-image ## assert the image ships what it must, and no fakes
	@docker run --rm --entrypoint sh wheel-engine:dev -c '\
	  for b in wheel wheel-engine wheel-host python3; do \
	    command -v $$b >/dev/null || { echo "FAIL: $$b missing from the image"; exit 1; }; \
	  done; \
	  wheel --help >/dev/null 2>&1 || wheel --version >/dev/null 2>&1 || \
	    { echo "FAIL: wheel is present but not runnable"; exit 1; }; \
	  test ! -e /usr/local/bin/claude || { echo "FAIL: something shadows claude in the production image"; exit 1; }; \
	  test ! -e /usr/local/bin/codex  || { echo "FAIL: something shadows codex in the production image"; exit 1; }; \
	  claude --version | grep -qi fake && { echo "FAIL: claude is a fake in the production image"; exit 1; }; \
	  claude --version | grep -q "Claude Code" || { echo "FAIL: real claude missing"; exit 1; }; \
	  su agent -c "env -i PATH=$$PATH HOME=/tmp/wowcheck CARGO_HOME=/tmp/wowcheck/cargo RUSTUP_HOME=$$RUSTUP_HOME cargo --version" >/dev/null 2>&1 || \
	    { echo "FAIL: the agent uid cannot run cargo with the environment the ENGINE actually gives it — a Wheel-on-Wheel agent clones and then cannot build"; exit 1; }; \
	  su agent -c "env -i PATH=$$PATH HOME=/tmp/wowcheck CARGO_HOME=/tmp/wowcheck/cargo cargo --version" >/dev/null 2>&1 && \
	    { echo "FAIL: cargo worked WITHOUT RUSTUP_HOME, so this check cannot detect the regression it exists for"; exit 1; }; \
	  su agent -c "touch /opt/rust/rustup/POISON" >/dev/null 2>&1 && \
	    { echo "FAIL: an agent can WRITE the shared toolchain and poison it for every other project"; exit 1; }; \
	  su agent -c "touch /opt/rust/cargo/bin/POISON" >/dev/null 2>&1 && \
	    { echo "FAIL: an agent can write the shared cargo bin dir"; exit 1; }; \
	  echo "ok: image ships wheel + wheel-engine + wheel-host + python3, the real claude ($$(claude --version)), a toolchain the agent uid can RUN but not WRITE"'
