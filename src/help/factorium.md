How to place slimfit in a repo's gates, with factorium as the example.

1. `slimfit init` detects the suites, writes `slimfit.toml`, measures and writes `.slimfit/baseline.json`. Commit both. It runs no mutant.
2. In the gate, `slimfit check` runs in place of the project's own test run, never beside it. In factorium: `factorium.toml` has `fast = "slimfit check"`, or a gate script that calls `slimfit check`, with `cargo test` (or the language's test command) taken out of it.
3. Mutants never go in `fast` or `slow`. In factorium they go in idle work, which runs only while the host is idle: `[idle] steps = ["slimfit check --mutants"]` holds the changes since the baseline's `mutants_at`. Nothing mutates the whole codebase, so no idle step runs every mutant.
4. A change accepted with `slimfit check --mutants` is ratcheted (`slimfit ratchet`) and committed with its baseline, so the next run measures only what changed after it (see [mutants scope](help:mutants-scope)).

Time it first: [costs](help:costs) says how. Then `slimfit doctor` names whatever in this placement is off, each with its fix.
