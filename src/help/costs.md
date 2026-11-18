What each command costs, and how to measure that before placing slimfit in a repo's gates.

| Command | Cost |
|---|---|
| `slimfit check`, `slimfit next`, `slimfit ratchet`, `slimfit report` | one instrumented test run, about 1.5× a plain one; none while the working tree is unchanged since the last measurement (`--fresh` measures again) |
| `slimfit init` | one instrumented test run, always; no mutant |
| `--mutants` | that (none when this tree's test run is cached), plus one test run per mutant of the changed lines ({const:DIFF_LIMITED}), or of the files that hold them where the tool takes only files; it refuses for a tool that takes neither (see [mutants scope](help:mutants-scope)) |

No command mutates the whole codebase: what a mutants run costs grows with the change, never with the size of the codebase. A test run stops at a mutant's first failing test, so a caught mutant costs less than a missed one.

## Before adopting slimfit on a repo

Time `slimfit init` (one instrumented test run; it runs no mutant). Then place slimfit from that number: `slimfit check` in the gate in place of the project's own test run; `slimfit check --mutants` run by the agent on its own change, at background priority, before it ratchets; never in a gate, not as idle work (see [mutants scope](help:mutants-scope)). A change's mutants cost their count × one test run (Rust: `cargo mutants --list --in-diff <diff> | wc -l`; one run is `test_seconds` plus the build). [Setting up in factorium](help:factorium) shows one such placement.

## Don't compete with other heavy work

Measure on a quiet host, or at background priority (`nice -n 19 slimfit ...`; in factorium, `factorium worker exclusive --background -- slimfit ...`) so builds, gates and other projects' mutants go first. See [host load](help:host-load).
