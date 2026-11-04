#!/usr/bin/env bash
# Runs the checks CONTRIBUTING.md requires across the workspace.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# One TLS provider: nothing may enable ring. A cargo failure stops the script.
rustls="$(cargo tree --workspace -e features -i rustls)"
if grep -q 'feature "ring"' <<<"$rustls"; then
    echo "something enables rustls' ring provider; see CONTRIBUTING.md" >&2
    exit 1
fi
