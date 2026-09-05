#!/usr/bin/env bash
# Install everything `make check` needs. Must work on a dev laptop (macOS) AND on a CI
# runner (Ubuntu, no Homebrew, node/pnpm already provided by the workflow).
#
# Everything here is idempotent and conditional: bootstrap must never fail because a tool
# is already present, or because a tool this platform gets from somewhere else is missing.
set -uo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
have() { command -v "$1" >/dev/null 2>&1; }
note() { printf '  %s\n' "$*"; }

echo "▸ rust"
if have cargo; then note "cargo $(cargo --version | awk '{print $2}') already installed"
else
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default \
    || { echo "rustup install failed"; exit 1; }
  export PATH="$HOME/.cargo/bin:$PATH"
fi

echo "▸ cargo-llvm-cov (§0b coverage gate)"
if have cargo-llvm-cov; then note "already installed"
elif [ "${SKIP_LLVM_COV:-0}" = "1" ]; then note "skipped (SKIP_LLVM_COV=1)"
else
  # Prefer the prebuilt binary: building it from source takes minutes and is the slowest
  # thing in a cold CI run.
  cargo install cargo-llvm-cov --locked 2>&1 | tail -2 || note "install failed — coverage gate will report as unavailable"
fi

echo "▸ node + pnpm"
if have pnpm; then note "pnpm $(pnpm --version) already installed"
elif have brew; then brew install node pnpm
elif have corepack; then corepack enable && corepack prepare pnpm@9 --activate
else
  # CI supplies these via pnpm/action-setup and setup-node, and does so only once web/
  # exists. Not having them here is not a bootstrap failure.
  note "pnpm not present and no installer available — web gates will report as unavailable"
fi

echo "▸ QA python venv"
if [ -x qa/.venv/bin/python ]; then note "qa/.venv already present"
else
  python3 -m venv qa/.venv || { echo "venv creation failed"; exit 1; }
fi
qa/.venv/bin/pip install -q --disable-pip-version-check -r qa/requirements.txt \
  || { echo "pip install failed"; exit 1; }
note "$(qa/.venv/bin/python -c 'import importlib.metadata as m; print("jsonschema " + m.version("jsonschema"))')"

echo "▸ bootstrap complete"
