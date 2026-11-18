How to place slimfit in a repo's gates, with factorium as the example.

1. `slimfit init` detects the suites, writes `slimfit.toml`, measures and writes `.slimfit/baseline.json`. Commit both.
2. In the gate, `slimfit check` runs in place of the project's own test run, never beside it. In factorium: `factorium.toml` has `fast = "slimfit check"`, or a gate script that calls `slimfit check`, with `cargo test` (or the language's test command) taken out of it.
3. Mutants never go in `fast` or `slow`. In factorium they go in idle work, which runs only while the host is idle: `[idle] steps = ["slimfit check --mutants"]`.
4. Ratchet the mutants baseline by hand on an idle host: once with `slimfit ratchet --all-mutants`, or in pieces (see [the piecewise baseline](help:piecewise-baseline)).

Time it first: [costs](help:costs) says how. Then `slimfit doctor` names whatever in this placement is off, each with its fix.
