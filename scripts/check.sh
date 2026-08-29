#!/usr/bin/env bash
# Pre-commit verification: fmt, clippy, test must all pass.
# Run this before every commit.

set -euo pipefail

echo "==> cargo fmt"
cargo fmt

echo "==> cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "==> cargo test"
cargo test

echo
echo "✓ all checks passed"
