#!/bin/sh
# End to end: the real slimfit binary on a fresh git copy of each sample in
# testdata/, with the language's real tools. For each language:
#   init                      → baseline written
#   next --all                → an `uncovered` task names the `unused` function's file
#   check                     → passes
#   add an untested function  → check exits 1 on uncovered_lines
# With E2E_MUTANTS=1, also (before the injection):
#   next --all --mutants      → a `surviving-mutant` task (the is_adult boundary)
# A language whose toolchain is missing is skipped, and says so.
# Usage: ./e2e.sh [rust|go|typescript|kotlin|swift]...
set -eu
cd "$(dirname "$0")"
[ -d "$HOME/.cargo/bin" ] && PATH="$HOME/.cargo/bin:$PATH"
cargo build --release -q
bin="$PWD/target/release/slimfit"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

have() { command -v "$1" >/dev/null 2>&1; }
needs() {
  case "$1" in
    rust) have cargo-llvm-cov && have cargo-nextest ;;
    go) have go ;;
    typescript) have npm ;;
    kotlin) [ -n "${JAVA_HOME:-}" ] && "$JAVA_HOME/bin/java" -version >/dev/null 2>&1 ;;
    swift) have swift && have xcrun ;;
  esac
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
  if ! needs "$lang"; then
    echo "e2e $lang: SKIP (toolchain missing)"
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
      "$bin" next --all --mutants 2>mutants.log | json '[t["kind"] for t in d["tasks"]]' >kinds.txt || { tail -20 mutants.log; exit 1; }
      grep -q surviving-mutant kinds.txt || { echo "no surviving-mutant task: $(cat kinds.txt)" >&2; exit 1; }
    fi
    inject "$lang"
    if "$bin" check >check.json 2>/dev/null; then
      echo "e2e $lang: check passed with an untested function added" >&2
      exit 1
    fi
    json '[v["metric"] for v in d["verdict"]["violations"]]' <check.json | grep -q uncovered_lines
    echo "e2e $lang: ok ($(( $(date +%s) - start ))s)"
  ) || { echo "e2e $lang: FAILED"; failed=1; }
done
exit $failed
