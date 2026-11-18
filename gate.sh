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
# The release script, as far as it goes without publishing.
sh -n packaging/release.sh
for good in v0.1.0 v1.22.333; do packaging/release.sh check-tag "$good"; done
for bad in "" v1 v1.2 1.2.3 v1.2.3.4 v1..3 v1.2.x "v1.2.3;x"; do
    if packaging/release.sh check-tag "$bad" 2>/dev/null; then echo "gate: release.sh takes \"$bad\""; exit 1; fi
done
DIST=.slimfit/out/dist packaging/release.sh dry
echo "gate: ok"
