# The Rust preset's test command. cargo llvm-cov builds the tests,
# instrumented, into the target directory the mutants build in too, and
# archives them; nextest runs them from the archive's copy in
# $SLIMFIT_OUT, so a build there cannot take a test binary away mid-run.
# The suite's `test_args` ($SLIMFIT_TEST_ARGS, expanded here) go to that
# run: a profile, a filterset. nextest runs every test through runner.sh,
# which gives the test (and each process it starts) a profile of its own;
# the merged coverage reads them all; then llvm-cov says, of each test's
# profiles, which functions and lines it ran (ADR 0007), as many tests at
# once as there are cores. runner.sh also records the CPU time of each
# test's process and everything it started (ADR 0010): the shell runs the
# test as its child, then `times` says what its children spent. It is a
# builtin and the group's redirection forks nothing, so the record counts
# only the test.
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
# the test's binary and the archive's other binaries, which the test may
# have started.
i=$1
IFS='	' read -r key binary name object <<EOF
$(sed -n "${i}p" "$SLIMFIT_OUT/tests.map")
EOF
printf 'test %s\t%s\n' "$binary" "$name"
set -- -object "$object"
while IFS= read -r b; do set -- "$@" -object "$b"; done < "$SLIMFIT_OUT/bins.txt"
out="$SLIMFIT_OUT/reach-$i"
if "$TOOLS/llvm-profdata" merge -sparse "$key"-*.profraw -o "$out.profdata" 2>/dev/null &&
  "$TOOLS/llvm-cov" export -format=lcov -instr-profile="$out.profdata" "$@" \
    -ignore-filename-regex="$IGNORED" > "$out.info" 2>/dev/null; then
  grep -E '^(SF:|DA:[0-9]+,[1-9]|FNDA:[1-9]|end_of_record)' "$out.info" || :
else
  printf 'unreadable %s\n' "$key"
fi
LINES
eval "set -- ${SLIMFIT_TEST_ARGS:-}" &&
cargo llvm-cov nextest-archive --workspace --archive-file "$SLIMFIT_OUT/tests.tar.zst" &&
mkdir -p "$SLIMFIT_OUT/bin" "$SLIMFIT_OUT/profiles" || exit
LLVM_PROFILE_FILE="$SLIMFIT_OUT/profiles/run-%p-%m.profraw" cargo nextest run \
  --archive-file "$SLIMFIT_OUT/tests.tar.zst" --workspace-remap . --extract-to "$SLIMFIT_OUT/bin" \
  --tool-config-file "slimfit:$SLIMFIT_OUT/nextest.toml" \
  --config "target.'cfg(all())'.runner = ['sh', '$SLIMFIT_OUT/runner.sh']" "$@"
rc=$?
TOOLS=${LLVM_TOOLS:-$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin}
IGNORED='/rustc/|/\.cargo/(registry|git)/|/\.rustup/'
export TOOLS SLIMFIT_OUT IGNORED
# The archive's binaries that are no test's (one directory below its
# target), which a test may start; then the merged coverage of them and of
# every test binary that ran.
for b in "$SLIMFIT_OUT"/bin/target/*/*; do [ -f "$b" ] && [ -x "$b" ] && echo "$b"; done > "$SLIMFIT_OUT/bins.txt"
set --
{ cat "$SLIMFIT_OUT/bins.txt"; cut -f4 "$SLIMFIT_OUT/tests.map" 2>/dev/null | sort -u; } > "$SLIMFIT_OUT/objects.txt"
while IFS= read -r b; do set -- "$@" -object "$b"; done < "$SLIMFIT_OUT/objects.txt"
find "$SLIMFIT_OUT/profiles" -name '*.profraw' > "$SLIMFIT_OUT/profiles.txt"
"$TOOLS/llvm-profdata" merge -sparse -f "$SLIMFIT_OUT/profiles.txt" -o "$SLIMFIT_OUT/run.profdata" &&
  "$TOOLS/llvm-cov" export -format=lcov -instr-profile="$SLIMFIT_OUT/run.profdata" "$@" \
    -ignore-filename-regex="$IGNORED" > "$SLIMFIT_OUT/lcov.info"
if [ -s "$SLIMFIT_OUT/tests.map" ]; then
  each() { i=0; while read -r _; do i=$((i + 1)); echo "$i"; done < "$SLIMFIT_OUT/tests.map"; }
  each | xargs -P "$(getconf _NPROCESSORS_ONLN)" -I{} sh -c 'sh "$SLIMFIT_OUT/lines.sh" {} > "$SLIMFIT_OUT/reach-{}.txt"'
  each | while read -r i; do cat "$SLIMFIT_OUT/reach-$i.txt"; done > "$SLIMFIT_OUT/{REACH}"
fi
exit $rc
