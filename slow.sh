#!/bin/sh
# The slow gate: the fast one, slimfit's own mutants, and every language's
# real toolchain end to end.
set -eu
cd "$(dirname "$0")"
./gate.sh
mkdir -p .slimfit/out
./target/release/slimfit check --mutants > .slimfit/out/slow.json || { cat .slimfit/out/slow.json; exit 1; }
E2E_MUTANTS=1 ./e2e.sh
