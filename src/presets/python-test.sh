# The Python preset's test command. pytest, through uv, writes the JUnit
# report; pytest-cov records each line with the tests that ran it
# (coverage.py's dynamic contexts, `--cov-context=test`), its data file
# under $SLIMFIT_OUT, and writes the merged LCOV. Then coverage.py's data
# gives what each test ran ({REACH}): `test <classname>\t<name>`, as the
# JUnit report names the test, then the LCOV of its lines, setup and
# teardown included. No bytecode is written: the tree stays as it was.
export COVERAGE_FILE="$SLIMFIT_OUT/.coverage" PYTHONDONTWRITEBYTECODE=1
uv run --with pytest-cov pytest -p no:cacheprovider --junitxml="$SLIMFIT_OUT/junit.xml" \
  --cov --cov-context=test --cov-report="lcov:$SLIMFIT_OUT/lcov.info"
rc=$?
uv run --with pytest-cov python - "$COVERAGE_FILE" > "$SLIMFIT_OUT/{REACH}" <<'REACH' || exit $rc
import re, sys
import coverage

data = coverage.CoverageData(sys.argv[1])
data.read()
tests = {}
for file in sorted(data.measured_files()):
    for line, contexts in data.contexts_by_lineno(file).items():
        for c in contexts:
            node = c.rpartition("|")[0]
            if node:
                tests.setdefault(node, {}).setdefault(file, set()).add(line)
for node, files in sorted(tests.items()):
    # pytest's JUnit classname: the module's path, dotted, then its classes.
    parts = node.split("::")
    parts[0] = re.sub(r"\.py$", "", parts[0].replace("/", "."))
    print(f"test {'.'.join(parts[:-1])}\t{parts[-1]}")
    for file, lines in sorted(files.items()):
        print(f"SF:{file}")
        print("".join(f"DA:{n},1\n" for n in sorted(lines)), end="")
        print("end_of_record")
REACH
exit $rc
