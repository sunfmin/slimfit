# slimfit

Keep a codebase small and its tests few, fast and strong.

slimfit is made for AI coding agents. The agent changes code; slimfit measures each change and only lets the numbers get better:

- **failures** stay at 0
- **test time** (the sum of the tests' own durations) does not grow
- **uncovered lines** do not grow: code may grow, untested code may not
- **surviving mutants** do not grow: a test can be deleted only if the suite still catches everything it caught

`slimfit next` hands out one task at a time, cheapest and safest first: fix failing tests, remove uncovered code, remove redundant tests, speed up slow tests, kill surviving mutants, and deepen modules that keep changing. Each task carries its evidence and the command that accepts it.

The full manual is `slimfit --help`.

## Install

```sh
cargo install --locked --git https://github.com/sunfmin/slimfit
```

## Languages

Each language is a preset: the tools its community uses, each run once to write a standard report. Every preset field can be overridden per suite in `slimfit.toml`.

| Language | Tests and coverage | Mutants | Needs |
|---|---|---|---|
| Rust | cargo-nextest under cargo-llvm-cov (JUnit, LCOV) | cargo-mutants running nextest with `--no-fail-fast`, so each mutant's log names every killing test: `--mutants` mutates only the files changed since the baseline's commit (`--file`, whole files); `--all-mutants` mutates every file | `cargo install cargo-nextest cargo-llvm-cov cargo-mutants`, `rustup component add llvm-tools-preview` |
| Go | `go test -json -coverprofile` | gremlins | `go install github.com/go-gremlins/gremlins/cmd/gremlins@v0.5.1` |
| TypeScript | vitest with v8 coverage (JUnit, LCOV) | Stryker (`--disableBail`, so every killing test is recorded) | `vitest`, `@vitest/coverage-v8`, `@stryker-mutator/core`, `@stryker-mutator/vitest-runner` in devDependencies |
| Kotlin | Gradle `test` + Kover (JUnit, JaCoCo XML) | PIT with `fullMutationMatrix` | the Kover and `info.solidsoft.pitest` plugins (see `testdata/kotlin/build.gradle.kts`) and a JDK Gradle can use |
| Swift | `swift test` + `llvm-cov export` (xUnit, LCOV) | muter | `brew install muter-mutation-testing/formulae/muter` |

Mutants are measured in two ways:

- `--all-mutants` mutates every file. `init --mutants` does this for the first baseline.
- `--mutants` mutates only the source files that changed since the commit the baseline's mutants were measured at. Every other file keeps its per-file count from the baseline.

A workspace too large for one `--all-mutants` run builds its baseline in pieces: `slimfit ratchet --pending-seconds 1500` mutates the changed files as `--mutants` does, then files with no count yet, one tool run per file, until the budget is spent (at least one file), and ratchets them in. Commit the baseline and repeat until `check --mutants` lists no `mutants_pending`; until then it holds only the files that have a count. See `docs/adr/0002-mutants-baseline-in-pieces.md`.

`--mutants` falls back to every file when the suite's tool cannot be limited to files. Today that is every language except Rust. A test changed outside source files is seen only by `--all-mutants`; see `docs/adr/0001-mutate-only-changed-files.md`.

Redundant tests can be found only where the mutation tool records which test kills which mutant: Rust (cargo-mutants under nextest, from each mutant's log), TypeScript (Stryker) and Kotlin (PIT). For the other languages, `slimfit next` says so. Recording every killer means running every test on every killed mutant instead of stopping at the first failure; `docs/adr/0003-rust-kill-matrix.md` says what that costs.

## Use

```sh
slimfit init          # detect suites, write slimfit.toml, measure, write .slimfit/baseline.json
slimfit next          # the next task
slimfit check         # exit 1 if a bound got worse; --mutants also mutates the changed files, --all-mutants every file
slimfit ratchet       # lock the gain into the baseline
slimfit report        # metrics, baseline, and line counts over git history
```

In a repo gated by factorium:

```toml
[gate]
fast = "slimfit check"
slow = "slimfit check --mutants"
```

Before that slow gate can run, ratchet once with `slimfit ratchet --all-mutants`, or build the baseline in pieces as above. That records the per-file counts and the commit later runs build on.

## Develop

- `./gate.sh`: the fast gate (fmt, clippy, slimfit checking itself).
- `./slow.sh`: the fast gate, slimfit's own mutants, and the Rust sample end to end. `E2E_LANGS="go swift" ./slow.sh` runs those languages instead; `E2E_LANGS=all ./slow.sh` runs every language whose toolchain is present.
- `./e2e.sh [rust|go|typescript|kotlin|swift]...`: end to end on its own; no arguments means every language.
