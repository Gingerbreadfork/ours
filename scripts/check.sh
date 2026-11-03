#!/usr/bin/env bash
# Runs the checks CONTRIBUTING.md requires across the workspace.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
