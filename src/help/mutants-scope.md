Which mutants `--mutants` runs, which survivors it holds, and what the gate holds without running any. Mutants run only on a change: no command, flag or setting mutates the whole codebase, so a run's cost follows the change, not the codebase's size.

- **The change** is what HEAD adds since its merge base with **the target**, the branch it lands on, untracked files included: `target` under `[check]` in `slimfit.toml`, or `origin/HEAD` (the remote's default branch) when unset. Code already on the target is never charged to a change; once a branch merges the target in, what it brought is out of the change. This page is where the term is defined; every other page means exactly this by it.
- **The changed lines** are its added source lines (neither blank nor only a `//` comment, outside inline tests), in the suites whose tool can be limited to a change, that no ratcheted mutants run measured yet. `--mutants` runs their mutants.
- The suite's tool runs as few mutants as it can be limited to: {const:DIFF_LIMITED}'s tool only those of the change's lines in the files that hold a changed line (through the suite's `mutants_diff_flag`: `--in-diff` for cargo-mutants); a tool with only a file flag (`mutants_file_flag`), those of those files. For a tool with neither, `--mutants` refuses rather than run every mutant. With no target found, `--mutants` refuses, saying why.
- {const:REACHED}: each mutant runs only the tests the test run saw run its function, each under a timeout of its own (see [costs](help:costs)). A test that never runs the mutated function cannot fail on it, so the verdicts are those of running every test. The test run records which functions each test ran (`reach_report`); a mutant whose function no test ran runs no test and, once it builds, is missed. When that is not known (no `reach_report` written, a `const fn`, a line in no listed function, a test whose profile could not be read), the mutant runs every test. `mutants_tests = "all"` in the suite runs every test on every mutant.
- Either way only the survivors in the change's lines are held, at 0: each is the change's own. Survivors elsewhere are surviving-mutant tasks, not violations. No baseline keeps a mutant count.
- One that changes nothing observable is accepted with `slimfit ratchet --loosen survived_mutants --reason "<why>"`.
- Surviving-mutant tasks come only from these runs: survivors of the change, never of the whole suite.

A change that only deletes code or tests adds no line, so it runs no mutant: what a deleted test killed in unchanged code is not seen. A test changed outside source files likewise.

The output's `mutated` says, per suite, the source files that hold changed lines (`files`) and the changed lines' mutants by outcome (`change`).

## What the gate holds

No source line reaches the target without a passing mutants measurement of it. Mutants never run in a gate; the gate checks only that a measurement covers the change:

- A `slimfit ratchet` of a measurement that ran mutants (and passed, or loosened its survivors with a reason) writes `.slimfit/mutants/<hash>.json`: the lines that run measured, each by a hash of its file and text. Nothing else writes one. Each run's record is a file of its own, named by its contents, so two branches that each measured their own change merge without a conflict, and after the merge both changes' lines are measured.
- Every `slimfit check`, plain ones included, fails with `unmeasured_mutants` while the change adds a source line whose text no record holds for its file. A line edited after it was measured is a new text: unmeasured until measured again. Its `fix` is the rule: `factorium worker exclusive --background -- slimfit check --mutants` (outside factorium, `nice -n 19 slimfit check --mutants`), then `slimfit ratchet`, then commit `.slimfit/mutants/` with the change.
- `unmeasured_mutants` is never loosened: a survivor that changes nothing observable is the only way out, through `--loosen survived_mutants`, and its run is recorded like any other.
- With no target found, `check` holds no line to a measurement (its `landing` is `null`) and `slimfit doctor` finds `no_target`.

## Who runs it, and when

The agent that made a change runs `slimfit check --mutants` on it, at background priority (`nice -n 19`; in factorium, `factorium worker exclusive --background -- slimfit check --mutants`), then `slimfit ratchet`, then commits `.slimfit/` with the change. Never in a gate: it would hold up every landing. Not as idle work either: idle work commits nothing, so what it measures is never recorded, and a red run is one nobody there can fix.
