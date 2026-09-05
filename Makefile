# Wheel — root Makefile.
# `check` is the merge gate and is owned by QA (qa/check.sh). Teams add their own
# targets under their own area; please don't edit `check` without pinging QA.

SHELL := /bin/bash
.DEFAULT_GOAL := help

export PATH := $(HOME)/.cargo/bin:/opt/homebrew/bin:$(PATH)

.PHONY: help check check-strict fmt clippy test-rust coverage web-lint web-typecheck web-test \
        qa-selftest test-int test-e2e test-live bootstrap clean

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
	@CHECK_ONLY=rust:coverage bash qa/check.sh

qa-selftest: ## test the fake harness itself
	@python3 qa/harness/selftest.py

## ---------------------------------------------------------------- heavier suites
test-int: ## integration suite (docker; needs wheel-engine:test + compose)
	@bash qa/integration/run.sh

test-e2e: ## Playwright end-to-end suite
	@bash qa/e2e/run.sh

test-live: ## OPT-IN: same suites against the REAL claude/codex CLIs. Costs money. Never in CI.
	@WHEEL_LIVE=1 bash qa/integration/run.sh

## ---------------------------------------------------------------- setup
bootstrap: ## install the toolchain (rust, node, pnpm) and the QA python venv
	@command -v cargo >/dev/null || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
	@command -v pnpm  >/dev/null || brew install node pnpm
	@cargo llvm-cov --version >/dev/null 2>&1 || cargo install cargo-llvm-cov
	@python3 -m venv qa/.venv && qa/.venv/bin/pip install -q -r qa/requirements.txt
	@test -x qa/.venv/bin/python || python3 -m venv qa/.venv
	@qa/.venv/bin/pip install -q --disable-pip-version-check -r qa/requirements.txt
	@echo "toolchain ready — you may need to restart your shell for PATH"

clean: ## remove build artefacts
	@rm -rf target web/.next web/node_modules
