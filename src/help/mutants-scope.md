Which files `--mutants` and `--all-mutants` mutate, and what the counts of the others mean.

- `--all-mutants`: every file. Use it for the first baseline (`slimfit init --mutants` does), after changing tests outside source files, and to accept a redundant-test task.
- `--mutants`: only the source files changed since the commit the baseline's mutants were measured at; every other file keeps its count from the baseline. It falls back to every file when the baseline has no such commit or no per-file counts, or the suite's mutation tool cannot be limited to files (today only Rust's cargo-mutants can, through the suite's `mutants_file_flag`).

A test changed outside source files is not seen in the unchanged files' counts: `--all-mutants` sees it.

The output's `mutated` says, per suite, `"all"` or the files mutated. `slimfit ratchet --mutants` moves the commit the baseline's mutants were measured at, so the next `--mutants` mutates only what changed after it.
