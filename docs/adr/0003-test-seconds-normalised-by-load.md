# 3. test_seconds is normalised by the host's load, measured by a probe

## Context

`slimfit check` holds `test_seconds` (the sum of the tests' durations as the
test runner timed them) to baseline × (1 + time_tolerance) + time_slack. Those
durations are wall time. On a host shared with other agents' builds and
cargo-mutants (load average 45-60 on 2026-11-18) the same factorium suite
measured 449s at baseline, then 460s, 708s and 996s. A red slow gate said how
busy the host was, not that tests got slower.

Invariant: `test_seconds` flags tests that got slower, not a busy host.

## Decision

While the test command runs, a probe thread in slimfit spins without pause
and compares its wall time with its own CPU time (`CLOCK_THREAD_CPUTIME_ID`).
The ratio, at least 1, is how much the host's load stretched a CPU-bound
thread over that time. Each test's duration is divided by it, so
`test_seconds` and the slow-test tasks use the normalised durations.
The measurement records the ratio as `load_slowdown`.

In `load.rs`'s test, a fixed CPU workload ran 2.3-2.5× slower by the wall
clock beside 2 × cores `yes` processes (18 cores, 6 of them faster, on a host
already at load ~30). The probe measured the same factor to within 3%. In
release builds with a hog of threads, it held to within 20% in 6 of 6 runs,
while the "unloaded" run itself was slowed up to 4× by the host. The hog
has to be other processes: threads in the test binary share llvm-cov's
counters with the workload and fight for their cache lines. Under coverage
the normalised time grew 27× that way, a contention no host load causes.

## Options not taken

- **CPU time of the test command's process tree** (`RUSAGE_CHILDREN`). Every
  preset's test command builds before it tests (`cargo llvm-cov nextest`,
  `go test`, `swift test`, gradle, vitest), so the CPU time would count the
  build. How much that is depends on what the build cache already held, and
  on a large workspace it can be more than the tests. That swaps a false red
  from load for a false red from a cold cache. Gradle also runs tests in its
  daemon, which is not our child, so their CPU time would not be counted.
- **A probe that sleeps between short slices** (a tenth of a core). Tried
  first: under the same hog it measured 1.3-1.5× while the workload slowed
  4-6×. Schedulers raise the priority of a thread that has just woken, so a
  probe like that does not wait as a busy test thread does.
- **Re-measure once when over the bound, keep the lower.** Under sustained
  load the second run is as slow as the first, so the invariant fails.
- **Normalise by the load average.** It counts runnable threads, not the
  share of a core each one gets, and needs the core count and a model of the
  scheduler to turn into a factor. The probe measures that share directly.

## Consequences

- The probe uses one core for the length of the test command. On a host the
  suite has to itself, the tests and the build already use every core, so
  the probe sees that load too: about (cores + 1) / cores. It is in the
  baseline the same way, so it cancels out.
- The factor is an average over the whole command, build included. A burst
  of load during the build but not during the tests over-corrects the tests'
  durations, and the other way round. A long-running load from other jobs on
  the host, the case this ADR is about, is the same in both phases.
- Memory bandwidth and cache contention are not CPU share, and the probe
  does not see them. Time lost to them still counts against the tests.
- Baselines written before this change hold wall time, which is higher. The
  baseline is re-measured once, with the change, under `--loosen
  test_seconds`.
