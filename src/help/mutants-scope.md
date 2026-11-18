Which mutants `--mutants` runs, and which survivors it holds.

- `--mutants` measures the change: the lines that differ from the tree the baseline's `mutants_at` names, untracked files included. `slimfit init` sets `mutants_at`; a ratchet of a measurement with mutants moves it to the tree measured, so committing what was measured leaves no change behind.
- The suite's tool runs as few mutants as it can be limited to: {const:DIFF_LIMITED}'s tool only those of the changed lines (through the suite's `mutants_diff_flag`: `--in-diff` for cargo-mutants); a tool with only a file flag (`mutants_file_flag`), those of the files the change touches; a tool with neither, every mutant.
- Either way only the survivors in the lines the change added are held, at 0: each is the change's own. Survivors elsewhere are surviving-mutant tasks, not violations. No baseline keeps a mutant count.
- One that changes nothing observable is accepted with `slimfit ratchet --loosen survived_mutants --reason "<why>"`.
- `slimfit next --all-mutants` runs every mutant, for tasks across the whole suite: its redundant tests too.

A change that only deletes code or tests adds no line, so it runs no mutant: what a deleted test killed in unchanged code is not seen. A test changed outside source files likewise.

When the tree `mutants_at` names is gone (another clone, or `git gc` pruned it), the change is taken from its commit; when the commit is gone too, or none is recorded, from HEAD. stderr says which.

The output's `mutated` says, per suite, whether every mutant ran (`all`), the source files the change touches (`files`) and its mutants by outcome (`change`).
