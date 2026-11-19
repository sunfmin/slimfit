What each command costs, and how to measure that before placing slimfit in a repo's gates.

| Command | Cost |
|---|---|
| `slimfit check`, `slimfit next`, `slimfit ratchet`, `slimfit report` | one instrumented test run, about 1.5× a plain one; none while the working tree is unchanged since the last measurement (`--fresh` measures again) |
| `slimfit keep` | the measurement of this tree, cached or one instrumented test run, as above; no mutant |
| `slimfit doctor` | no test run and no build: it reads `slimfit.toml`, the baseline and factorium's gates, and finds the tools by name |
| `slimfit init` | one instrumented test run, always; no mutant |
| `slimfit check --mutants` | that (none when this tree's test run is cached), plus the mutation tool's first build, then a build and a test run per mutant of the changed lines ({const:DIFF_LIMITED}), or of the files that hold them where the tool takes only files; it refuses for a tool that takes neither (see [mutants scope](help:mutants-scope)). {const:REACHED}: the test run is only the tests that ran the mutant's function, none when no test did, and the mutation tool runs no unmutated baseline first. The first build is the one cost that does not follow the mutant count (see below) |

`next`, `ratchet`, `report` and `keep` run no mutant: they read the cached `check --mutants` of the same tree, which is the one way mutants run.

No command mutates the whole codebase: what a mutants run costs grows with the change, never with the size of the codebase. A test run stops at a mutant's first failing test, so a caught mutant costs less than a missed one.

## The first build

A mutation tool builds its mutants in a copy of the tree. Its first build compiles whatever that copy cannot reuse; each later mutant recompiles only what it changed (on factorium 1-5s; about 20s for the first that compiles, which links the test binaries).

- Rust: cargo-mutants' copy has no `target`, so its first build compiled every dependency: 94-115s on factorium, half of a 226s run of 17 mutants. The preset builds each mutant as the test run built (`cargo llvm-cov show-env`) into the test run's own target directory, so only the mutated crates compile: 9-11s, and the run took 126-136s. Copying `target` into the copy (`--copy-target`) gains nothing, since a target directory at a new path recompiles every crate with a build script; mutating in place (`--in-place`) builds cold too, and leaves the shim no unmutated tree to find the mutant by, so every mutant runs every test (335s). The next test run then recompiles the workspace's own crates once (35s on factorium), as it would after any edit.
- The other presets' first builds are not measured: time one `slimfit check --mutants` on a small change.

## A mutant that hangs

A mutant can make a test loop forever (`+=` to `*=` in a loop). Such a mutant counts as caught, but it costs whatever ends the test:

- Without a timeout per test, the mutation tool's own timeout for the whole test run: cargo-mutants sets it from its unmutated baseline run (5× that run; 71s on factorium, so 3 hanging mutants were 213s of a 274s run), or 300s when that run is skipped.
- {const:REACHED}: each test has its own timeout, {const:TIMEOUT_FLOOR}s plus {const:TIMEOUT_FACTOR}× its duration in the test run (`seconds` in the tests report), and the hanging test is ended then: on factorium 20-24s (tests of about 3.4s) instead of 71s, for the same verdicts. Only the tests that ran the mutant's function run at all, so a hang in one costs that test's timeout, not the suite's.

## Before adopting slimfit on a repo

Time `slimfit init` (one instrumented test run; it runs no mutant). Then place slimfit from that number: `slimfit check` in the gate in place of the project's own test run; `slimfit check --mutants` run by the agent on its own change, at background priority, before it ratchets; never in a gate, not as idle work (see [mutants scope](help:mutants-scope)). A change's mutants cost the first build plus their count × one build and one test run (Rust: `cargo mutants --list --in-diff <diff> | wc -l`; a test run is at most `test_seconds`, and only the tests that ran the mutant's function). [Setting up in factorium](help:factorium) shows one such placement.

## Don't compete with other heavy work

Measure on a quiet host, or at background priority (`nice -n 19 slimfit ...`; in factorium, `factorium worker exclusive --background -- slimfit ...`) so builds, gates and other projects' mutants go first. See [host load](help:host-load).
