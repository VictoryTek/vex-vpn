#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "--- Running clippy ---"
nix develop --command cargo clippy -- -D warnings

echo "--- Running debug build ---"
nix develop --command cargo build

echo "--- Running tests ---"
nix develop --command cargo test

echo "--- Running release build ---"
nix develop --command cargo build --release

echo "--- Running nix build ---"
nix build

echo "--- Preflight passed ---"
