#!/bin/sh
# The slow gate: the fast one, slimfit's own mutants, and the Rust sample's
# real toolchain end to end. Other languages run when asked for:
#   E2E_LANGS="go typescript" ./slow.sh   those languages
#   E2E_LANGS=all ./slow.sh               every language (as ./e2e.sh does)
set -eu
cd "$(dirname "$0")"
./gate.sh
mkdir -p .slimfit/out
./target/release/slimfit check --mutants > .slimfit/out/slow.json || { cat .slimfit/out/slow.json; exit 1; }
langs=${E2E_LANGS:-rust}
[ "$langs" = all ] && langs=""
# shellcheck disable=SC2086 # word-split the list into arguments
E2E_MUTANTS=1 ./e2e.sh $langs
