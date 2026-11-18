What each command costs, and how to measure that before placing slimfit in a repo's gates.

| Command | Cost |
|---|---|
| `slimfit check`, `slimfit next`, `slimfit ratchet`, `slimfit report` | one instrumented test run, about 1.5× a plain one; none while the working tree is unchanged since the last measurement (`--fresh` measures again) |
| `slimfit init` | one instrumented test run, always; no mutant |
| `--mutants` | that (none when this tree's test run is cached), plus one test run per mutant of the lines changed since the baseline's `mutants_at` ({const:DIFF_LIMITED}), or of the files they are in where the tool takes only files; it refuses for a tool that takes neither, and never runs every mutant (see [mutants scope](help:mutants-scope)) |
| `next --all-mutants` | one test run per mutant of every file: hours on a large workspace |

## Before adopting slimfit on a repo

Time `slimfit init` (one instrumented test run; it runs no mutant). Then place slimfit from that number: `slimfit check` in the gate in place of the project's own test run; `slimfit check --mutants` on each change, in idle work. A change's mutants cost their count × one test run (Rust: `cargo mutants --list --in-diff <diff> | wc -l`; one run is `test_seconds` plus the build). [Setting up in factorium](help:factorium) shows one such placement.

## Don't compete with other heavy work

Measure on an idle host, or at background priority (`nice -n 19 slimfit ...`) so builds, gates and other projects' mutants go first. See [host load](help:host-load).
