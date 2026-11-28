# slimfit

Keep a codebase small and its tests few, fast and strong.

slimfit is made for AI coding agents: the agent changes code, slimfit measures each change and only lets the numbers get better.

The manual is `slimfit --help`, and it is the only copy: read it before adopting slimfit on a repo. It says how mutants are picked and recorded, and how growth is held.

## Install

```sh
brew install sunfmin/tap/slimfit
```

The formula's binary is universal (Apple silicon and Intel), Developer ID signed and notarized (ADR 0013). Or build it:

```sh
cargo install --locked --git https://github.com/sunfmin/slimfit
```

## Languages

Each language is a preset (`src/config.rs`): the tools its community uses, each run once to write a standard report. `slimfit doctor` names each tool a repo's presets need and lack, with how to install it.

| Language | Tests and coverage | Mutants |
|---|---|---|
| Rust | cargo-nextest under cargo-llvm-cov | cargo-mutants (ADR 0007) |
| Go | `go test` | gremlins |
| TypeScript | vitest with v8 coverage | Stryker |
| Kotlin | Gradle + Kover | PIT |
| Swift | `swift test` + `llvm-cov` | muter |
| Python | pytest through uv | mutmut (ADR 0017) |

## Develop

- `./gate.sh`: the fast gate. Its tests run every preset end to end against stand-ins for the tools (`tests/standins.rs`, `examples/standin/`), so they need no toolchain but Rust.
- `./e2e.sh [rust|go|typescript|kotlin|swift|python]...`: the real toolchains end to end, in no gate; `E2E_MUTANTS=1` adds Rust's mutants.

The design decisions are in `docs/adr/`.
