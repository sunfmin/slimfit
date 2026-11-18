# The Rust preset's test command. nextest runs every test through
# runner.sh, which gives the test (and each process it starts) a profile of
# its own beside cargo llvm-cov's, so the merged coverage is unchanged; then
# llvm-profdata says which functions each test ran (ADR 0007).
printf '[profile.default.junit]\npath = "{NEXTEST_JUNIT}"\n' > "$SLIMFIT_OUT/nextest.toml" &&
cat > "$SLIMFIT_OUT/runner.sh" <<'RUNNER' &&
if [ -n "${NEXTEST_TEST_NAME:-}" ] && [ -n "${LLVM_PROFILE_FILE:-}" ]; then
  key="${LLVM_PROFILE_FILE%/*}/slimfit-$$"
  printf '%s\t%s\t%s\n' "$key" "$NEXTEST_BINARY_ID" "$NEXTEST_TEST_NAME" >> "$SLIMFIT_OUT/tests.map"
  LLVM_PROFILE_FILE="$key-%p-%m.profraw"
  export LLVM_PROFILE_FILE
fi
exec "$@"
RUNNER
cargo llvm-cov nextest --workspace --tool-config-file "slimfit:$SLIMFIT_OUT/nextest.toml" \
  --config "target.'cfg(all())'.runner = ['sh', '$SLIMFIT_OUT/runner.sh']" \
  --lcov --output-path "$SLIMFIT_OUT/lcov.info"
rc=$?
if [ -s "$SLIMFIT_OUT/tests.map" ]; then
  pd=${LLVM_PROFDATA:-$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-profdata}
  while IFS='	' read -r key binary name; do
    printf 'test %s\t%s\n' "$binary" "$name"
    for f in "$key"-*.profraw; do
      [ -e "$f" ] && { "$pd" show --all-functions --counts "$f" || printf 'unreadable %s\n' "$f"; }
    done
  done < "$SLIMFIT_OUT/tests.map" > "$SLIMFIT_OUT/{REACH}"
fi
exit $rc
