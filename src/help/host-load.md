On a busy host everything runs slower, and slimfit corrects only `test_seconds` for it.

`test_seconds` is the sum of the tests' own durations, each divided by how much the host's load slowed a CPU-bound thread meanwhile: the slowdown slimfit measured, `load_slowdown` per suite in `.slimfit/out/measurement.json`. A test whose report says it started before the test command ran (JUnit `timestamp`, `go test -json` `Time`), as in a report the command did not write, is left as reported. So a busy host does not fail `slimfit check` on its own. A gate's or a mutants run's wall time is not corrected.

## When the host is busy

The 1-minute load average (`uptime`) is above the core count (`sysctl -n hw.ncpu` on macOS, `nproc` on Linux), or `load_slowdown` is well above 1. Then don't set a baseline's `test_seconds` from that run: re-measure with `--fresh` when the host is quiet.

## A test run inside `--mutants`

`slimfit check --mutants` runs at background priority (rule 2), and the host slows such a process without raising its load, so no correction reaches it. When it finds no cached test run of the tree it runs the tests itself, and that run's `test_seconds` is not what a gate measures: it holds the run's coverage, failures and which tests ran which function, but not its time. Its `check` answers the suite in `time_unchecked` and holds no `test_seconds` bound; `slimfit ratchet` keeps the baseline's `test_seconds` as it was, and refuses `--loosen test_seconds` and a suite with no baseline yet. A plain `slimfit check` of the same tree runs the tests again, at its own priority, and keeps the mutants: `test_seconds` is checked, and ratcheted, only from such a run.
