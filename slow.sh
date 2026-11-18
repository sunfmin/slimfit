#!/bin/sh
# The slow gate: the fast one, plus the Rust sample's real toolchain end to
# end. No mutation testing: mutants are never in a gate (`slimfit --help`,
# rule 2); slimfit's own run as idle work (factorium.toml `[idle]`).
#   E2E_LANGS="go typescript" ./slow.sh   those languages
#   E2E_LANGS=all ./slow.sh               every language (as ./e2e.sh does)
#   E2E_MUTANTS=1 ./slow.sh               the samples' mutants e2e too, on request
set -eu
cd "$(dirname "$0")"
./gate.sh
langs=${E2E_LANGS:-rust}
[ "$langs" = all ] && langs=""
# shellcheck disable=SC2086 # word-split the list into arguments
./e2e.sh $langs
