Which mutants `--mutants` runs, and which survivors it holds. Mutants run only on a change: no command, flag or setting mutates the whole codebase, so a run's cost follows the change, not the codebase's size.

- `--mutants` measures **the changed lines**: the source lines added or rewritten since the tree the baseline's `mutants_at` names, untracked files included. This page is where the term is defined; every other page means exactly this by it. `slimfit init` sets `mutants_at`; a ratchet of a measurement with mutants moves it to the tree measured, so committing what was measured leaves no change behind.
- The suite's tool runs as few mutants as it can be limited to: {const:DIFF_LIMITED}'s tool only those of the changed lines (through the suite's `mutants_diff_flag`: `--in-diff` for cargo-mutants); a tool with only a file flag (`mutants_file_flag`), those of the files that hold changed lines. For a tool with neither, `--mutants` refuses rather than run every mutant.
- {const:REACHED}: each mutant runs only the tests the test run saw run its function, each under a timeout of its own (see [costs](help:costs)). A test that never runs the mutated function cannot fail on it, so the verdicts are those of running every test. The test run records which functions each test ran (`reach_report`); a mutant whose function no test ran runs no test and, once it builds, is missed. When that is not known (no `reach_report` written, a `const fn`, a line in no listed function, a test whose profile could not be read), the mutant runs every test. `mutants_tests = "all"` in the suite runs every test on every mutant.
- Either way only the survivors in the changed lines are held, at 0: each is the change's own. Survivors elsewhere are surviving-mutant tasks, not violations. No baseline keeps a mutant count.
- One that changes nothing observable is accepted with `slimfit ratchet --loosen survived_mutants --reason "<why>"`.
- Surviving-mutant tasks come only from these runs: survivors of the change, never of the whole suite.

A change that only deletes code or tests adds no line, so it runs no mutant: what a deleted test killed in unchanged code is not seen. Likewise a change to a test file alone: it adds no source line, so it runs no mutant, and what the changed test now catches or misses is not measured.

When the tree `mutants_at` names is gone (another clone, or `git gc` pruned it), the change is taken from its commit. When the commit is gone too, or none is recorded, it is taken from the merge base with the branch's upstream; with no upstream, `--mutants` refuses, saying why. stderr says which. `slimfit init`, and the first `slimfit ratchet` of a baseline without one, record the tree as `mutants_at`.

The output's `mutated` says, per suite, the source files that hold changed lines (`files`) and the changed lines' mutants by outcome (`change`).

## Who runs it, and when

The agent that made a change runs `slimfit check --mutants` on it, at background priority (`nice -n 19`; in factorium, `factorium worker exclusive --background -- slimfit check --mutants`), then `slimfit ratchet`, which moves `mutants_at` to the tree measured, then commits the baseline with the change. Never in a gate: it would hold up every landing. Not as idle work either: idle work commits nothing, so `mutants_at` never moves, each run measures a change that only grows, and a red run is one nobody there can fix.
