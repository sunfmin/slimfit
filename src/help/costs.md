What each command costs, and how to measure that before placing slimfit in a repo's gates.

| Command | Cost |
|---|---|
| `slimfit check`, `slimfit next`, `slimfit ratchet`, `slimfit report` | one instrumented test run, about 1.5× a plain one; none while the working tree is unchanged since the last measurement (`--fresh` measures again) |
| `slimfit init` | one instrumented test run, always |
| `--mutants` | one test run per mutant of the files changed since the baseline's mutants were measured |
| `--all-mutants`, `init --mutants` | one test run per mutant of every file: hours on a large workspace |
| `ratchet --pending-seconds <s>` | about `<s>` seconds in all, the test run included: one bounded piece of the mutants baseline |

## Before adopting slimfit on a repo

Time `slimfit init` (one instrumented test run) and count the mutants: their count × one test run is what mutating every file costs (Rust: `cargo mutants --list | wc -l`; one run is `test_seconds` plus the build). Then place slimfit from those numbers: `slimfit check` in the gate in place of the project's own test run; mutants, if they fit at all, in idle work, built in pieces on a large workspace (see [the piecewise baseline](help:piecewise-baseline)). [Setting up in factorium](help:factorium) shows one such placement.

## Don't compete with other heavy work

Measure on an idle host, or at background priority (`nice -n 19 slimfit ...`) so builds, gates and other projects' mutants go first. See [host load](help:host-load).
