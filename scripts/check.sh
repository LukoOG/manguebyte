#!/usr/bin/env bash
# Runs every check CI runs, and stops at the first one that fails.
# Use it before each commit:  ./scripts/check.sh
set -euo pipefail
cd "$(dirname "$0")/.."

export RUSTFLAGS="${RUSTFLAGS:--D warnings}"
export RUSTDOCFLAGS="${RUSTDOCFLAGS:--D warnings}"

echo "==> format"
cargo fmt --all --check

echo "==> lint"
cargo clippy --workspace --all-targets

echo "==> test"
cargo test --workspace --quiet

echo "==> test without default features"
cargo test -p manguebyte --no-default-features --quiet

echo "==> docs"
cargo doc --workspace --no-deps --quiet

echo "All checks passed."
