#!/bin/sh
# End to end: the real slimfit binary on a fresh git copy of each sample in
# testdata/, with the language's real tools. For each language:
#   init                      → baseline written
#   next --all                → an `uncovered` task names the `unused` function's file
#   check                     → passes
#   add an untested function  → check exits 1 on uncovered_lines
# With E2E_MUTANTS=1, also (before the injection):
#   next --all --all-mutants  → a `surviving-mutant` task (the is_adult boundary);
#                               for rust, `redundant-test` tasks name `slow` and
#                               one of the two identical `add` tests (adds_again
#                               is the deliberate copy; which one goes is a tie)
# and after it, for rust:
#   check --mutants           → cargo-mutants runs only the injected lines'
#                               mutants, and their survivors fail the check
# A language whose toolchain for this run is incomplete (the mutants tool too
# under E2E_MUTANTS=1) is skipped, and the skip names what is missing.
# Usage: ./e2e.sh [rust|go|typescript|kotlin|swift]...
set -eu
cd "$(dirname "$0")"
[ -d "$HOME/.cargo/bin" ] && PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -q
bin="$PWD/target/release/slimfit"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Gates run without a shell rc, so JAVA_HOME may be unset: ask macOS for a
# JDK 21 (-F: fail rather than fall back to another version). An explicit
# JAVA_HOME wins.
if [ -z "${JAVA_HOME:-}" ] && [ -x /usr/libexec/java_home ]; then
  JAVA_HOME=$(/usr/libexec/java_home -F -v 21 2>/dev/null) && export JAVA_HOME || unset JAVA_HOME
fi

have() { command -v "$1" >/dev/null 2>&1; }
# Prints what the language's toolchain for this run lacks; nothing if whole.
missing() {
  m=""
  need() { have "$1" || m="$m $1"; }
  mut() { [ "${E2E_MUTANTS:-}" = 1 ] && need "$1"; return 0; }
  case "$1" in
    rust) need cargo-llvm-cov; need cargo-nextest; mut cargo-mutants ;;
    go) need go; mut gremlins ;;
    typescript) need npm ;;
    kotlin) [ -n "${JAVA_HOME:-}" ] && "$JAVA_HOME/bin/java" -version >/dev/null 2>&1 || m="$m java (JAVA_HOME)" ;;
    swift)
      need swift
      # Command Line Tools ship swift but not XCTest; only Xcode has it.
      { have xcrun && xcrun --find xctest >/dev/null 2>&1; } || m="$m XCTest (Xcode)"
      mut muter ;;
  esac
  echo "${m# }"
}
# An untested function appended to the sample's source, per language.
inject() {
  case "$1" in
    rust) printf '\npub fn untested(x: i64) -> i64 {\n    let y = x * 3;\n    y - 1\n}\n' >> src/lib.rs ;;
    go) printf '\nfunc Untested(x int) int {\n\ty := x * 3\n\treturn y - 1\n}\n' >> calc.go ;;
    typescript) printf '\nexport function untested(x: number): number {\n  const y = x * 3;\n  return y - 1;\n}\n' >> src/calc.ts ;;
    kotlin) printf '\nfun untested(x: Int): Int {\n    val y = x * 3\n    return y - 1\n}\n' >> src/main/kotlin/com/example/Calc.kt ;;
    swift) printf '\npublic func untested(_ x: Int) -> Int {\n    let y = x * 3\n    return y - 1\n}\n' >> Sources/Calc/Calc.swift ;;
  esac
}
json() { uv run --no-project python -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

langs=${*:-rust go typescript kotlin swift}
failed=0
for lang in $langs; do
  lack=$(missing "$lang")
  if [ -n "$lack" ]; then
    echo "e2e $lang: SKIP (missing: $lack)"
    continue
  fi
  dir="$work/$lang"
  cp -R "testdata/$lang" "$dir"
  (
    cd "$dir"
    rm -rf reports .out target build .build node_modules
    [ "$lang" = typescript ] && npm ci --silent >/dev/null 2>&1
    git init -q && git add -A && git -c user.email=e2e@slimfit -c user.name=e2e commit -qm sample
    start=$(date +%s)
    "$bin" init >/dev/null 2>init.log || { tail -20 init.log; exit 1; }
    git add -A && git -c user.email=e2e@slimfit -c user.name=e2e commit -qm slimfit
    "$bin" next --all 2>/dev/null | json '[t["kind"] for t in d["tasks"]]' | grep -q uncovered
    "$bin" check >/dev/null 2>&1
    if [ "${E2E_MUTANTS:-}" = 1 ]; then
      "$bin" next --all --all-mutants 2>mutants.log | json '[t["kind"] for t in d["tasks"]]' >kinds.txt || { tail -20 mutants.log; exit 1; }
      grep -q surviving-mutant kinds.txt || { echo "no surviving-mutant task: $(cat kinds.txt)" >&2; exit 1; }
      if [ "$lang" = rust ]; then
        "$bin" next --all --all-mutants --kind redundant-test 2>/dev/null | json '" ".join(sorted(t["evidence"]["test"] for t in d["tasks"]))' >redundant.txt
        grep -qE '^calc::tests::adds(_again)? calc::tests::slow$' redundant.txt || { echo "redundant tests: $(cat redundant.txt)" >&2; exit 1; }
      fi
    fi
    inject "$lang"
    if "$bin" check >check.json 2>/dev/null; then
      echo "e2e $lang: check passed with an untested function added" >&2
      exit 1
    fi
    json '[v["metric"] for v in d["verdict"]["violations"]]' <check.json | grep -q uncovered_lines
    if [ "${E2E_MUTANTS:-}" = 1 ] && [ "$lang" = rust ]; then
      "$bin" check --mutants >change.json 2>change.log && { echo "check --mutants passed the untested function" >&2; exit 1; }
      json 'd["mutated"]["rust"]["all"], d["mutated"]["rust"]["files"], d["mutated"]["rust"]["change"]["survived"] > 0' <change.json \
        | grep -qx "False \['src/lib.rs'\] True" || { cat change.json; tail -20 change.log; exit 1; }
    fi
    echo "e2e $lang: ok ($(( $(date +%s) - start ))s)"
  ) || { echo "e2e $lang: FAILED"; failed=1; }
done
exit $failed
