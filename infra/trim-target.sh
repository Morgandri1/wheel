#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Bound the shared cargo target dir.
#
# ~/.cargo/config.toml points every worktree at one target-dir, so six agents compile each
# dependency once instead of six times. Nothing prunes it: cargo has no GC, debug artifacts
# accumulate per test binary per worktree, and the directory reached 150 GB on a full disk.
#
#   trim-target.sh          report size, exit 1 over the ceiling
#   trim-target.sh --apply  prune first, then report
#
# Pruned: incremental state and fingerprints, which cargo rebuilds on demand. Never `release`,
# which is what the size gate measures.
set -euo pipefail

TARGET_DIR="${CARGO_TARGET_DIR:-$(awk -F'"' '/target-dir/{print $2}' "${HOME}/.cargo/config.toml" 2>/dev/null)}"
TARGET_DIR="${TARGET_DIR:-${HOME}/wheel-target}"
CEILING_GB="${TARGET_CEILING_GB:-20}"

[ -d "$TARGET_DIR" ] || { echo "target dir absent: $TARGET_DIR"; exit 0; }

size_gb() { echo $(( $(du -sk "$TARGET_DIR" 2>/dev/null | cut -f1) / 1024 / 1024 )); }

before=$(size_gb)

if [ "${1:-}" = "--apply" ]; then
  rm -rf "$TARGET_DIR"/debug/incremental "$TARGET_DIR"/*/incremental 2>/dev/null || true
  rm -rf "$TARGET_DIR"/llvm-cov-target 2>/dev/null || true
  after=$(size_gb)
  echo "trim-target: ${before}G -> ${after}G (ceiling ${CEILING_GB}G)"
else
  after=$before
  echo "trim-target: ${after}G of ${CEILING_GB}G ceiling"
fi

if [ "$after" -gt "$CEILING_GB" ]; then
  echo "TARGET-DIR-SIZE: $TARGET_DIR is ${after}G, ceiling ${CEILING_GB}G." >&2
  echo "  One shared target dir serves every worktree and cargo never collects it." >&2
  echo "  Run: infra/trim-target.sh --apply   (or rm -rf $TARGET_DIR/debug to reclaim most of it)" >&2
  exit 1
fi
