#!/bin/sh
# The slow gate: the fast one. No mutation testing: mutants are never in a
# gate (`slimfit --help`, rule 2), and run only on a change. Every preset is
# already tested end to end in the fast gate, against stand-ins for its
# tools (tests/standins.rs). The real toolchains run only when asked for:
#   E2E_LANGS="rust go" ./slow.sh   those languages
#   E2E_LANGS=all ./slow.sh         every language whose toolchain is present
#   E2E_MUTANTS=1                   the Rust sample's mutants of a change too
set -eu
cd "$(dirname "$0")"
./gate.sh
[ -n "${E2E_LANGS:-}" ] || exit 0
langs=$E2E_LANGS
[ "$langs" = all ] && langs=""
# shellcheck disable=SC2086 # word-split the list into arguments
./e2e.sh $langs
