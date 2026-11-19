# 9. test_seconds is the tests' CPU time

Supersedes `0003-test-seconds-normalised-by-load.md`.

## Context

`test_seconds` was the tests' wall time, divided by how much a probe thread
saw the host's load slow a CPU-bound thread (ADR 0003). On a shared host
the probe missed most of what load did to the tests (many threads, process
spawns, slower cores), so slimfit#41 stopped holding `test_seconds` over its
bound once the 1-minute load passed 0.5 per core. On an 18-core host shared
with agents that load is the usual state: most of the day no check held
test time, while rule 1 puts fast tests first.

CPU time (user + sys) is the work the tests do. Waiting for a core does not
add to it, so it is the same on a quiet host and a busy one: factorium's
suite spent 194-200s of CPU at load 90-134 on the host where it spent
158-195s of wall time loaded and 24s idle (factorium's SPEC §6).

Invariant: `test_seconds` holds on a busy host, and counts the tests, not
the build.

## Decision

`test_seconds` is the CPU time of the tests' own processes, summed; each
test's `cpu` is what slow-test and redundant-test tasks rank by.

- **Per test, where the test command records it.** The Rust preset's runner
  (`target.runner`, ADR 0007) already wraps each test's process. It runs
  the test as a child of its shell instead of `exec`, then writes the
  shell's `times`: its children's CPU time, the test process and all it
  waited for. `times` is a builtin, and the record's redirection forks
  nothing, so the record counts only the test. One file per test process
  in `$SLIMFIT_OUT/test-cpu/` (`tools::TEST_CPU`), its first line the test's
  id as the JUnit report gives it; a retried test spent the sum. The build
  runs in cargo, outside the runner, so it is not counted.
- **Otherwise, the test command's process tree, after a separate build.**
  slimfit takes `getrusage(RUSAGE_CHILDREN)` around the test command: every
  process the command started and waited for. A preset whose test command
  builds first gets a `build_command` that slimfit runs before it, so the
  test command finds the build done: Go builds the test binaries with
  coverage (`go test -cover -run '^$'`) and runs with `-count=1` (a cached
  result spent no CPU); Swift `swift build --build-tests`; Kotlin
  `gradlew testClasses`, both without the daemon, so Gradle's test workers
  are in the tree (a daemon is no child of ours). Vitest has no separate
  build: it transforms as it runs. The tree's CPU time is shared out to
  the tests by their reported durations, so a slow test still ranks first.
- What is gone: the probe, `load_slowdown`, `host_load`, `time_unchecked`
  for a busy host and the ratchet's refusal to loosen on one, and the
  CPU-hog test that needed its own nextest and cargo-mutants settings.
  `slimfit doctor`'s `host_busy` stays, as advice: a gate's and a mutants
  run's wall times still run high on a busy host.

A test's `seconds` stays its reported wall time: a mutant's timeout per
test (ADR 0007) is wall time, so it is set from wall time.

## Options not taken

- **Keep the probe and raise the busy threshold.** The probe's miss grows
  with load; no threshold makes it hold at the load this host runs at.
- **Tree CPU with the build in it** (ADR 0003's objection). How much of the
  build a run repeats depends on the cache: a false red from a cold cache
  instead of from load. Hence the separate build, and per-test records
  where they can be had.

## Consequences

- A test that waits (sleeps, network, a child it does not wait for) costs
  no CPU and is not held by `test_seconds`. Waiting is not the shared
  host's problem this bound is for; the slow-test steps still name sleeps.
- On a host with fast and slow cores the same work takes more CPU on a
  slow core. A run at background priority runs on the slow ones, so a test
  run inside `--mutants` stays `untimed` (slimfit#39).
- Where the tree is measured, a test's CPU is its share, not its own:
  the sum is right, the split follows wall time.
- Baselines written before hold corrected wall time. They are re-set once
  from a CPU measurement.
