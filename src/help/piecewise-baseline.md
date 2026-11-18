On a large workspace, build the mutants baseline in bounded pieces instead of one `--all-mutants` run of hours.

```sh
slimfit ratchet --pending-seconds 1500
```

One piece mutates the files changed since the baseline's mutants were measured (as `--mutants` does), then files with no count yet, one mutation-tool run per file, starting none that would end past the budget (at least one per piece), and ratchets them in. Commit `.slimfit/baseline.json`; repeat until `slimfit check --mutants` lists no `mutants_pending`. `--pending-files <n>` caps the piece at n files instead, or as well.

- A suite with no mutants yet starts with all its source files pending.
- Until then `slimfit check --mutants` holds only the files that have a count; a new file is mutated as a changed one, not left pending.
- A piece is refused only for failing tests, new uncovered lines or more surviving mutants. `test_seconds` over its bound (a loaded host) is listed in `not_recorded`, and the baseline keeps its old `test_seconds`.
