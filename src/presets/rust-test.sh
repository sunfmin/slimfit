# The Rust preset's test command. nextest runs every test through
# runner.sh, which gives the test (and each process it starts) a profile of
# its own beside cargo llvm-cov's, so the merged coverage is unchanged; then
# llvm-cov says, of each test's profiles, which functions and lines it ran
# (ADR 0007), as many tests at once as there are cores. runner.sh also
# records the CPU time of each test's process and everything it started
# (ADR 0009): the shell runs the test as its child, then `times` says what
# its children spent. It is a builtin and the group's redirection forks
# nothing, so the record counts only the test.
printf '[profile.default.junit]\npath = "{NEXTEST_JUNIT}"\n' > "$SLIMFIT_OUT/nextest.toml" &&
cat > "$SLIMFIT_OUT/runner.sh" <<'RUNNER' &&
if [ -n "${NEXTEST_TEST_NAME:-}" ] && [ -n "${LLVM_PROFILE_FILE:-}" ]; then
  key="${LLVM_PROFILE_FILE%/*}/slimfit-$$"
  printf '%s\t%s\t%s\t%s\n' "$key" "$NEXTEST_BINARY_ID" "$NEXTEST_TEST_NAME" "$1" >> "$SLIMFIT_OUT/tests.map"
  LLVM_PROFILE_FILE="$key-%p-%m.profraw"
  export LLVM_PROFILE_FILE
fi
if [ -n "${NEXTEST_TEST_NAME:-}" ]; then
  "$@"
  rc=$?
  { printf '%s::%s\n' "$NEXTEST_BINARY_ID" "$NEXTEST_TEST_NAME"; times; } > "$SLIMFIT_OUT/{TEST_CPU}/$$"
  exit $rc
fi
exec "$@"
RUNNER
cat > "$SLIMFIT_OUT/lines.sh" <<'LINES' &&
# Line $1 of tests.map: its test, then the LCOV of what its profiles ran
# (only the lines and functions that ran), or `unreadable`. The objects are
# the test's binary and the binaries the build put one directory below the
# profiles, which the test may have started.
i=$1
IFS='	' read -r key binary name object <<EOF
$(sed -n "${i}p" "$SLIMFIT_OUT/tests.map")
EOF
printf 'test %s\t%s\n' "$binary" "$name"
set -- -object "$object"
for b in "${key%/*}"/*/*; do
  [ -f "$b" ] && [ -x "$b" ] && set -- "$@" -object "$b"
done
out="$SLIMFIT_OUT/reach-$i"
if "$TOOLS/llvm-profdata" merge -sparse "$key"-*.profraw -o "$out.profdata" 2>/dev/null &&
  "$TOOLS/llvm-cov" export -format=lcov -instr-profile="$out.profdata" "$@" \
    -ignore-filename-regex='/rustc/|/\.cargo/(registry|git)/|/\.rustup/' > "$out.info" 2>/dev/null; then
  grep -E '^(SF:|DA:[0-9]+,[1-9]|FNDA:[1-9]|end_of_record)' "$out.info" || :
else
  printf 'unreadable %s\n' "$key"
fi
LINES
cargo llvm-cov nextest --workspace --tool-config-file "slimfit:$SLIMFIT_OUT/nextest.toml" \
  --config "target.'cfg(all())'.runner = ['sh', '$SLIMFIT_OUT/runner.sh']" \
  --lcov --output-path "$SLIMFIT_OUT/lcov.info"
rc=$?
if [ -s "$SLIMFIT_OUT/tests.map" ]; then
  TOOLS=${LLVM_TOOLS:-$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin}
  export TOOLS SLIMFIT_OUT
  each() { i=0; while read -r _; do i=$((i + 1)); echo "$i"; done < "$SLIMFIT_OUT/tests.map"; }
  each | xargs -P "$(getconf _NPROCESSORS_ONLN)" -I{} sh -c 'sh "$SLIMFIT_OUT/lines.sh" {} > "$SLIMFIT_OUT/reach-{}.txt"'
  each | while read -r i; do cat "$SLIMFIT_OUT/reach-$i.txt"; done > "$SLIMFIT_OUT/{REACH}"
fi
exit $rc
