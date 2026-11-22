What each command costs, and how to measure that before placing slimfit in a repo's gates.

| Command | Cost |
|---|---|
| `slimfit check`, `slimfit next`, `slimfit ratchet`, `slimfit report` | one instrumented test run, about 1.5× a plain one (Go's, which also runs each top-level test on its own, more: see below); none while the working tree is unchanged since the last measurement (`--fresh` measures again) |
| `slimfit keep` | the measurement of this tree, cached or one instrumented test run, as above; no mutant |
| `slimfit report --lines-only` | no test run and no build: it reads HEAD's files, and those of the history's points, from git |
| `slimfit doctor` | no test run and no build: it reads `slimfit.toml`, the baseline and factorium's gates, and finds the tools by name |
| `slimfit init` | one instrumented test run, always; no mutant |
| `slimfit check --mutants` (no name) | no test run and no build: the mutation tool lists the change's mutants from its diff (under a second on slimfit) |
| `slimfit next --sample <N>` | `next`'s measurement, plus the mutation tool's listing of the changes since the last sample, its first build, and a build and a test run per drawn mutant it builds, N viable ones of {const:SAMPLE_DRAWN} × N drawn, in one run: on the idle tasks line only (see [the sample](help:sample)); on slimfit, {const:SAMPLE_COST} |
| `slimfit check --mutants <name>...` | the test run (none when this tree's test run is cached), plus the mutation tool's first build, then a build and a test run per pick, at most {const:PICKS_MAX} ({const:DIFF_LIMITED}); it refuses for a tool that cannot run a mutant by name (see [mutants scope](help:mutants-scope)). {const:REACHED}: the test run is only the tests that ran the mutant's function, none when no test did, and the mutation tool runs no unmutated baseline first. The first build is the one cost that does not follow the number of picks (see below) |

`next` (without `--sample`), `ratchet`, `report` and `keep` run no mutant: they read the cached `check --mutants <name>...` of the same tree, which is the one way mutants run. `next` lists the change's mutants for a pick-mutants task, as `check --mutants` with no name does.

No command mutates the whole codebase or every mutant of a change: what a mutants run costs grows with the picks, never with the size of the codebase or of the change. A test run stops at a mutant's first failing test, so a caught mutant costs less than a missed one.

## Each test's own run

Redundant-test tasks need the lines each test ran. Rust's test run records them as it runs (nextest runs each test in a process of its own). Go's runs every package once, as `go test` does, then each top-level test again on its own, from its package's test binary built with coverage, packages in parallel at background priority: its coverprofile is the test's lines, and its CPU time the test's own (so a package's setup, `TestMain` and `init`, counts in each of its tests). Measured at host load 20-25 on 18 cores, wall seconds of the plain run (`go test -count=1 -json -coverprofile ./...`) against the preset's whole test command:

| Repo | Tests | Plain | Plain and each test | CPU: the plain run's `go test` → the tests' own runs, summed |
|---|---|---|---|---|
| slimfit's Go sample | 4 | 0.80-1.27s | 1.72-1.92s | 0.33 → 0.01s |
| cobra 1.8.0 | 259 | 0.33-0.36s | 4.17-4.60s | 0.58 → 1.52s |
| excelize 2.10.1 | 424 | 23.3-32.3s | 84.2-94.6s | 25.5 → 46.8s |

Excelize's tests are all in one package, so its runs of each test are one after another. Time `slimfit init` on the repo before placing `slimfit check` in a gate.

## The first build

A mutation tool builds its mutants in a copy of the tree. Its first build compiles whatever that copy cannot reuse; each later mutant recompiles only what it changed (on factorium 1-5s; about 20s for the first that compiles, which links the test binaries).

- Rust: cargo-mutants' copy has no `target`, so its first build compiled every dependency: 94-115s on factorium, half of a 226s run of 17 mutants. The preset builds each mutant as the test run built (`cargo llvm-cov show-env`) into the test run's own target directory, so only the mutated crates compile: 9-11s, and the run took 126-136s. Copying `target` into the copy (`--copy-target`) gains nothing, since a target directory at a new path recompiles every crate with a build script; mutating in place (`--in-place`) builds cold too, and leaves the shim no unmutated tree to find the mutant by, so every mutant runs every test (335s). The next test run then recompiles the workspace's own crates once (35s on factorium), as it would after any edit.
- The other presets' first builds are not measured: time one `slimfit check --mutants <name>` on a small change.

## A mutant that hangs

A mutant can make a test loop forever (`+=` to `*=` in a loop). Such a mutant counts as caught, but it costs whatever ends the test:

- Without a timeout per test, the mutation tool's own timeout for the whole test run: cargo-mutants sets it from its unmutated baseline run (5× that run; 71s on factorium, so 3 hanging mutants were 213s of a 274s run), or 300s when that run is skipped.
- {const:REACHED}: each test has its own timeout, {const:TIMEOUT_FLOOR}s plus {const:TIMEOUT_FACTOR}× its duration in the test run (`seconds` in the tests report), and the hanging test is ended then: on factorium 20-24s (tests of about 3.4s) instead of 71s, for the same verdicts. Only the tests that ran the mutant's function run at all, so a hang in one costs that test's timeout, not the suite's.

## Before adopting slimfit on a repo

Time `slimfit init` (one instrumented test run; it runs no mutant). Then place slimfit from that number: `slimfit check` in the gate in place of the project's own test run; `slimfit check --mutants <name>...` run by the agent on its picks of its own change, at background priority, before it ratchets; never in a gate, not as idle work (see [mutants scope](help:mutants-scope)). A change's picks cost the first build plus their count (at most {const:PICKS_MAX}) × one build and one test run (a test run is at most `test_seconds`, and only the tests that ran the mutant's function). [Setting up in factorium](help:factorium) shows one such placement.

## Don't compete with other heavy work

Measure on a quiet host, or at background priority (`nice -n 19 slimfit ...`; in factorium, `factorium worker exclusive --background -- slimfit ...`) so builds, gates and other projects' mutants go first. See [host load](help:host-load).
