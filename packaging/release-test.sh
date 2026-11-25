#!/bin/sh
# Checks `release.sh cut` without building or publishing (gate.sh runs it):
# cargo, lipo, rustup and gh are faked, origin is a bare repo here. The cut
# dispatches the tap's workflow, waits for that run and fails with its log
# when it fails; a tag it pushed is gone from origin when it ends, and a tag
# origin already had on HEAD stays.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
mkdir -p "$t/bin" "$t/repo/packaging"
cp "$here/release.sh" "$t/repo/packaging/"
cat >"$t/bin/cargo" <<'EOF'
#!/bin/sh
for a; do case "$prev" in --target) target=$a ;; esac; prev=$a; done
mkdir -p "target/$target/release"
printf '#!/bin/sh\necho "slimfit %s"\n' "$SLIMFIT_RELEASE" >"target/$target/release/slimfit"
chmod +x "target/$target/release/slimfit"
EOF
cat >"$t/bin/lipo" <<'EOF'
#!/bin/sh
case "$1" in -archs) echo "x86_64 arm64" ;; -create) cp "$4" "$3" ;; esac
EOF
printf '#!/bin/sh\n' >"$t/bin/rustup"
# gh: `workflow run` makes run <n+1>, which `run view` reports as $RUN_ENDS.
# `run view` fails with HTTP 500 as many times as $s.fail-view says, and
# `run list` after a dispatch as many as $s.fail-list says; `run cancel` records whether origin still has the tag.
cat >"$t/bin/gh" <<'EOF'
#!/bin/sh
s=$FAKE_GH
flaky() {
    n=$(cat "$s.fail-$1" 2>/dev/null || echo 0)
    [ "$n" -gt 0 ] || return 0
    echo $((n - 1)) >"$s.fail-$1"
    echo "HTTP 500: Internal Server Error" >&2
    exit 1
}
case "$1 $2" in
"workflow run") echo "$*" >>"$s.calls"; echo $(( $(cat "$s.n") + 1 )) >"$s.n"; touch "$s.sent" ;;
"run list") [ ! -f "$s.sent" ] || flaky list; n=$(cat "$s.n"); [ "$n" = 0 ] || echo "$n" ;;
"run view") case "$*" in *--log-failed*) echo "the run's failed log" ;; *) flaky view; echo "$RUN_ENDS" ;; esac ;;
"run cancel") echo "cancel $3 tag:$(git ls-remote "$ORIGIN" refs/tags/v1.2.3 | wc -l | tr -d ' ')" >>"$s.calls" ;;
esac
EOF
# sleep: the cut's polls go by fast here.
printf '#!/bin/sh\nexec /bin/sleep 0.1\n' >"$t/bin/sleep"
chmod +x "$t/bin/"*
git init -q --bare "$t/origin.git"
cd "$t/repo"
git init -q && git config user.email t@t && git config user.name t
git add . && git commit -q -m one
git remote add origin "$t/origin.git"
head=$(git rev-parse HEAD)

run_cut() {
    PATH="$t/bin:$PATH" FAKE_GH="$t/gh" ORIGIN="$t/origin.git" \
        RELEASE_WAIT_LIMIT="${LIMIT:-20}" RUN_ENDS="$1" \
        packaging/release.sh cut v1.2.3 >"$t/out" 2>&1
}
fail() { echo "release-test: $*" >&2; cat "$t/out" >&2; exit 1; }
tagged() { git ls-remote "$t/origin.git" refs/tags/v1.2.3 | cut -f1; }

echo 0 >"$t/gh.n"
run_cut "completed success" || fail "a run that succeeds fails the cut"
grep -qx "workflow run release.yml -R sunfmin/homebrew-tap -f repo=slimfit -f tag=v1.2.3" "$t/gh.calls" \
    || fail "the cut ran '$(cat "$t/gh.calls")'"
[ -z "$(tagged)" ] || fail "the tag the cut pushed is still on origin"
[ -z "$(git tag)" ] || fail "the cut left a tag here: factorium's own tag step would fail"

if run_cut "completed failure"; then fail "a run that fails passes the cut"; fi
grep -q "the run's failed log" "$t/out" || fail "a failed run's log is not printed"
[ -z "$(tagged)" ] || fail "the tag of a failed cut is still on origin"
grep -q "^cancel" "$t/gh.calls" && fail "the cut cancelled a run that had ended"

# One HTTP 500 is no verdict on the run: gh is asked again.
rm -f "$t/gh.sent"
echo 1 >"$t/gh.fail-list"
echo 2 >"$t/gh.fail-view"
run_cut "completed success" || fail "two failing gh run views fail the cut"
[ "$(cat "$t/gh.fail-list") $(cat "$t/gh.fail-view")" = "0 0" ] || fail "gh was not asked again"
[ -z "$(tagged)" ] || fail "the tag the cut pushed is still on origin"

# The limit passes with the run queued: it is cancelled while the tag is
# still there, then the tag goes.
if LIMIT=1 run_cut "queued "; then fail "a run still queued at the limit passes the cut"; fi
grep -q "still 'queued '" "$t/out" || fail "the cut does not say the run is still queued"
grep -qx "cancel 4 tag:1" "$t/gh.calls" || fail "the queued run 4 was not cancelled before the tag went: $(grep cancel "$t/gh.calls")"
[ -z "$(tagged)" ] || fail "the tag of a timed-out cut is still on origin"

# factorium's tag step, after the cut: it tags HEAD and pushes the tag. Then a
# cut run again finds it there, and leaves it.
git tag -a v1.2.3 -m v1.2.3
git push -q origin refs/tags/v1.2.3
git tag -d v1.2.3 >/dev/null
run_cut "completed success" || fail "a tag origin has on HEAD fails the cut"
[ -n "$(tagged)" ] || fail "the cut took away a tag it did not push"

git commit -q --allow-empty -m two
if run_cut "completed success"; then fail "a tag origin has on another commit passes the cut"; fi
grep -q "not HEAD" "$t/out" || fail "the cut does not say the tag is on $head"
echo "release-test: ok"
