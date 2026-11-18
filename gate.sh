#!/bin/sh
# The fast gate: run before every commit.
set -eu
cd "$(dirname "$0")"
[ -d "$HOME/.cargo/bin" ] && PATH="$HOME/.cargo/bin:$PATH"

cargo fmt --check
cargo clippy --all-targets --quiet -- -D warnings
cargo build --release --quiet
mkdir -p .slimfit/out
# slimfit checks itself: tests pass, no new uncovered lines, no slower tests.
./target/release/slimfit check --fresh > .slimfit/out/gate.json || { cat .slimfit/out/gate.json; exit 1; }
echo "gate: ok"
