#!/bin/sh
# The fast gate: run before every commit.
set -eu
cd "$(dirname "$0")"
[ -d "$HOME/.cargo/bin" ] && PATH="$HOME/.cargo/bin:$PATH"

cargo fmt --check
cargo clippy --all-targets --quiet -- -D warnings
cargo build --release --quiet
mkdir -p .slimfit/out
# slimfit checks itself: tests pass and the change carries its mutants record; coverage and test time are answered as trends.
./target/release/slimfit check --fresh > .slimfit/out/gate.json || { cat .slimfit/out/gate.json; exit 1; }
# The documents: each in its one home, within its budget, citing no local tracker (`docfit --help`).
docfit check > .slimfit/out/docfit.json || { cat .slimfit/out/docfit.json; exit 1; }
# The release script, as far as it goes without publishing.
sh -n packaging/release.sh
for good in v0.1.0 v1.22.333; do packaging/release.sh check-tag "$good"; done
for bad in "" v1 v1.2 1.2.3 v1.2.3.4 v1..3 v1.2.x "v1.2.3;x"; do
    if packaging/release.sh check-tag "$bad" 2>/dev/null; then echo "gate: release.sh takes \"$bad\""; exit 1; fi
done
sh packaging/release-test.sh
DIST=.slimfit/out/dist packaging/release.sh dry
# Without tags (factorium's promotion clone), a build still stamps the
# development form: Cargo.toml's version + the commit count + the hash.
tagless=$PWD/.slimfit/out/tagless
rm -rf "$tagless"
git clone --quiet --no-tags . "$tagless"
CARGO_TARGET_DIR=$PWD/.slimfit/out/tagless-target cargo build --release --quiet --manifest-path "$tagless/Cargo.toml"
want="slimfit $(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)+$(git -C "$tagless" rev-list --count HEAD).g$(git -C "$tagless" rev-parse --short HEAD)"
said=$(.slimfit/out/tagless-target/release/slimfit --version)
[ "$said" = "$want" ] || { echo "gate: a tagless clone says '$said', not '$want'"; exit 1; }
echo "gate: ok"
