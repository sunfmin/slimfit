#!/bin/sh
# End to end: the real slimfit binary on a fresh git copy of each sample in
# testdata/, with the language's real tools. For each language:
#   init                      → baseline written
#   next --all                → an `uncovered` task names the `unused` function's file;
#                               for rust, go and python, whose test runs record each
#                               test's lines, a `redundant-test` task names a copy of
#                               the add test
#   check                     → passes
#   add an untested function  → check's uncovered_lines trend names its lines as
#                               a new span (a trend: it fails no check)
# For python, and with E2E_MUTANTS=1 for rust (the presets whose mutants
# tool lists a change's mutants and runs them by name; nothing mutates a
# whole sample), after the injection:
#   check --mutants           → lists the injected lines' mutants
#   check --mutants <2 picks> → the tool runs only those two of the untested
#                               function, and their survivors fail the check;
#                               cargo-mutants runs each with only the tests
#                               that ran its function (none)
#   next --all                → a `surviving-mutant` task in the injected
#                               file, from the same measurement (cached; no
#                               mutant runs again)
# A language whose toolchain for this run is incomplete (cargo-mutants too
# under E2E_MUTANTS=1) is skipped, and the skip names what is missing.
# Usage: ./e2e.sh [rust|go|typescript|kotlin|swift|python]...
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
    go) need go ;;
    typescript) need npm ;;
    python) need uv ;;
    kotlin) [ -n "${JAVA_HOME:-}" ] && "$JAVA_HOME/bin/java" -version >/dev/null 2>&1 || m="$m java (JAVA_HOME)" ;;
    swift)
      need swift
      # Command Line Tools ship swift but not XCTest; only Xcode has it.
      { have xcrun && xcrun --find xctest >/dev/null 2>&1; } || m="$m XCTest (Xcode)" ;;
  esac
  echo "${m# }"
}
# An untested function appended to the sample's source, per language.
inject() {
  case "$1" in
    rust) printf '\npub fn untested(x: i64) -> i64 {\n    let y = x * 3;\n    y - 1\n}\n' ;;
    go) printf '\nfunc Untested(x int) int {\n\ty := x * 3\n\treturn y - 1\n}\n' ;;
    typescript) printf '\nexport function untested(x: number): number {\n  const y = x * 3;\n  return y - 1;\n}\n' ;;
    kotlin) printf '\nfun untested(x: Int): Int {\n    val y = x * 3\n    return y - 1\n}\n' ;;
    swift) printf '\npublic func untested(_ x: Int) -> Int {\n    let y = x * 3\n    return y - 1\n}\n' ;;
    python) printf '\n\ndef untested(x):\n    y = x * 3\n    return y - 1\n' ;;
  esac >> "$(source_of "$1")"
}
source_of() {
  case "$1" in
    rust) echo src/lib.rs ;; go) echo calc.go ;; typescript) echo src/calc.ts ;;
    kotlin) echo src/main/kotlin/com/example/Calc.kt ;; swift) echo Sources/Calc/Calc.swift ;;
    python) echo src/calc.py ;;
  esac
}
json() { uv run --no-project python -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

langs=${*:-rust go typescript kotlin swift python}
failed=0
for lang in $langs; do
  lack=$(missing "$lang")
  if [ -n "$lack" ]; then
    echo "e2e $lang: SKIP (missing: $lack)"
    continue
  fi
  dir="$work/$lang"
  # What slimfit answers goes outside the sample: a file in its tree would
  # change the tree, and so miss the measurement cached for it.
  out="$work/$lang.out"
  mkdir "$out"
  cp -R "testdata/$lang" "$dir"
  (
    cd "$dir"
    rm -rf reports .out target build .build node_modules
    [ "$lang" = typescript ] && npm ci --silent >/dev/null 2>&1
    git init -q && git add -A && git -c user.email=e2e@slimfit -c user.name=e2e commit -qm sample
    start=$(date +%s)
    "$bin" init >/dev/null 2>"$out/init.log" || { tail -20 "$out/init.log"; exit 1; }
    # The branch changes land on: what the injection adds is the change.
    perl -0pi -e 's/^\[check\]\n/[check]\ntarget = "base"\n/m' slimfit.toml
    grep -qx 'target = "base"' slimfit.toml || { echo "e2e: no [check] in slimfit.toml to set the target in" >&2; exit 1; }
    git add -A && git -c user.email=e2e@slimfit -c user.name=e2e commit -qm slimfit && git branch base
    "$bin" next --all >"$out/next.json" 2>/dev/null
    json '[t["kind"] for t in d["tasks"]]' <"$out/next.json" | grep -q uncovered
    case $lang in
      rust | go | python)
        json '[c["test"] for t in d["tasks"] if t["kind"] == "redundant-test" for c in t["evidence"]["candidates"]]' <"$out/next.json" \
          | grep -qiE 'adds|slow' || { echo "e2e $lang: no redundant-test task" >&2; cat "$out/next.json" >&2; exit 1; }
        ;;
    esac
    "$bin" check >/dev/null 2>&1
    inject "$lang"
    "$bin" check >"$out/check.json" 2>/dev/null || :
    json '[s["file"] for t in d["verdict"]["uncovered_lines"] for s in t["new_spans"]]' <"$out/check.json" \
      | grep -qF "$(source_of "$lang")" || { echo "e2e $lang: no new uncovered span" >&2; cat "$out/check.json" >&2; exit 1; }
    if [ "$lang" = python ] || { [ "${E2E_MUTANTS:-}" = 1 ] && [ "$lang" = rust ]; }; then
      "$bin" check --mutants >"$out/list.json" 2>"$out/list.log" || { tail -20 "$out/list.log"; exit 1; }
      pick() { json "[c['name'] for c in d['candidates']['$lang'] if c['function'] == 'untested'][$1]" <"$out/list.json"; }
      first=$(pick 0) second=$(pick 1)
      "$bin" check --mutants "$first" "$second" --why "its result" "its result" >"$out/change.json" 2>"$out/change.log" \
        && { echo "check --mutants passed the untested function" >&2; exit 1; }
      json "[p['outcome'] for p in d['mutated']['$lang']['picks']]" <"$out/change.json" \
        | grep -qx "\['survived', 'survived'\]" || { cat "$out/change.json"; tail -20 "$out/change.log"; exit 1; }
      if [ "$lang" = rust ]; then
        grep -qh "^slimfit: src/lib.rs:[0-9]*: 0 of [0-9]* tests run its function" .slimfit/out/rust/mutants.out/log/*.log \
          || { echo "the untested function's mutants ran tests" >&2; tail -20 "$out/change.log"; exit 1; }
      fi
      "$bin" next --all 2>/dev/null | json '[t["files"] for t in d["tasks"] if t["kind"] == "surviving-mutant"]' \
        | grep -qF "$(source_of "$lang")" || { echo "no surviving-mutant task in $(source_of "$lang")" >&2; exit 1; }
    fi
    echo "e2e $lang: ok ($(( $(date +%s) - start ))s)"
  ) || { echo "e2e $lang: FAILED"; failed=1; }
done
exit $failed
