How to place slimfit in a repo's gates, with factorium as the example.

1. `slimfit init` detects the suites, writes `slimfit.toml`, measures and writes `.slimfit/baseline.json`. Commit both. It runs no mutant.
2. In the gate, `slimfit check` runs in place of the project's own test run, never beside it. In factorium: `factorium.toml` has `fast = "slimfit check"`, or a gate script that calls `slimfit check`, with `cargo test` (or the language's test command) taken out of it.
3. Mutants go in no gate (`fast`, `slow`) and in no `[idle]` step: idle work runs in a throwaway clone and commits nothing, so what it measures is never recorded. The worker that made a change runs them on it, at background priority: `factorium worker exclusive --background -- slimfit check --mutants`, on the changed lines (see [mutants scope](help:mutants-scope)).
4. When that passes, the worker runs `slimfit ratchet`, which records the lines measured in `.slimfit/mutants/`, and commits it with the change. The gate's plain `slimfit check` fails on a change whose source lines no such record covers, so a landing cannot skip this step (see [what the gate holds](help:mutants-scope)). Workers' records are files of their own: merging proving into a branch brings them along, without a conflict.
5. The change is taken against `origin/HEAD`, the default branch proving is promoted to; set `[check] target` in `slimfit.toml` when changes land elsewhere.

Time it first: [costs](help:costs) says how. Then `slimfit doctor` names whatever in this placement is off, each with its fix.
