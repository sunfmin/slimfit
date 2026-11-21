# The Go preset's test command. One plain run of every package gives the
# tests report and the merged coverage (`-count=1`: a cached result spent
# no CPU). Then each top-level test runs again on its own, from its
# package's test binary built with coverage, in the package's directory as
# `go test` runs it: its coverprofile is the lines it ran ({REACH}, `test
# <package>\t<name>` then the profile's blocks that ran, or `unreadable`),
# and the shell's `times` after it is its CPU time ({TEST_CPU}, ADR 0010),
# so the plain run's CPU is not the tests' twice. Packages run in parallel,
# each one's tests one after another, at background priority.
go test -count=1 -json -coverprofile="$SLIMFIT_OUT/cover.out" ./... > "$SLIMFIT_OUT/test.json"
rc=$?
go list -f '{{if or .TestGoFiles .XTestGoFiles}}{{.ImportPath}}	{{.Dir}}{{end}}' ./... > "$SLIMFIT_OUT/packages.txt" || exit $rc
mkdir -p "$SLIMFIT_OUT/each" || exit
cat > "$SLIMFIT_OUT/each.sh" <<'EACH'
# Line $1 of packages.txt: each top-level test of its package, run alone.
i=$1
IFS='	' read -r pkg dir <<EOF
$(sed -n "${i}p" "$SLIMFIT_OUT/packages.txt")
EOF
bin="$SLIMFIT_OUT/each/$i.test"
if ! go test -c -cover -o "$bin" "$pkg" > "$SLIMFIT_OUT/each/$i.log" 2>&1 || [ ! -x "$bin" ]; then
  exit
fi
cd "$dir" || exit
"$bin" -test.list '.*' | grep -E '^(Test|Fuzz|Example)' | while IFS= read -r name; do
  printf 'test %s\t%s\n' "$pkg" "$name"
  out="$SLIMFIT_OUT/each/$i-$name"
  sh -c '"$@" > /dev/null 2>&1; times' sh "$bin" -test.run "^$name\$" -test.count=1 -test.timeout=10m \
    -test.coverprofile="$out.cover" > "$out.times"
  { printf '%s::%s\n' "$pkg" "$name"; cat "$out.times"; } > "$SLIMFIT_OUT/{TEST_CPU}/$i-$name"
  if [ -s "$out.cover" ]; then grep -v -e '^mode:' -e ' 0$' "$out.cover"; else echo "unreadable $out.cover"; fi
done
EACH
each() { i=0; while read -r _; do i=$((i + 1)); echo "$i"; done < "$SLIMFIT_OUT/packages.txt"; }
each | nice -n 19 xargs -P "$(getconf _NPROCESSORS_ONLN)" -I{} sh -c 'sh "$SLIMFIT_OUT/each.sh" {} > "$SLIMFIT_OUT/each/{}.reach"'
each | while read -r i; do cat "$SLIMFIT_OUT/each/$i.reach"; done > "$SLIMFIT_OUT/{REACH}"
exit $rc
