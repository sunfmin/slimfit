#!/bin/sh
# The slow gate: the fast one and slimfit's own mutants. Every preset is
# already tested end to end in the fast gate, against stand-ins for its tools
# (tests/standins.rs). The real toolchains run only when asked for:
#   E2E_LANGS="rust go" ./slow.sh   those languages, with their mutants
#   E2E_LANGS=all ./slow.sh         every language whose toolchain is present
set -eu
cd "$(dirname "$0")"
./gate.sh
mkdir -p .slimfit/out
./target/release/slimfit check --mutants > .slimfit/out/slow.json || { cat .slimfit/out/slow.json; exit 1; }
[ -n "${E2E_LANGS:-}" ] || exit 0
langs=$E2E_LANGS
[ "$langs" = all ] && langs=""
# shellcheck disable=SC2086 # word-split the list into arguments
E2E_MUTANTS=1 ./e2e.sh $langs
