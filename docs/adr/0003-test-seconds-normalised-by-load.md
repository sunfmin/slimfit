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
already at load ~30). The probe measured the same factor to within 3%. The
test holds the normalised time within the tolerance both of the unloaded
run's and of the work's own CPU time, the time it takes on an idle core.
The test spawns one `yes` per core, not two: it runs once per mutant, and
the host it loads is the one everyone shares. In
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
- Nor does it see what a load costs a test that spawns processes and waits
  for them: each child, and the parent when it wakes, queues for a core.
  Beside 36 busy processes on 18 cores, 100 `git rev-parse` took 3.7× as
  long while a spin took 2.0× (2.25× and 1.4× beside busy loops that make
  no syscalls). Such tests stay slower under load once normalised, and
  only `time_slack` absorbs it, so a suite keeps its spawns few. slimfit
  reads its working tree with three git commands, not nine (slimfit#16).
- A test that loads the host itself, like `load.rs`'s CPU-hog test, slows
  the tests that run beside it by more than the probe corrects: with it in
  slimfit's suite, an unchanged tree measured 2.6-3.0s quiet, 1.5s without.
  It runs alone (`.config/nextest.toml`). The Rust preset passes its junit
  setting with `--tool-config-file`, so a repository's own nextest config
  still applies.
- Baselines written before this change hold wall time, which is higher. The
  baseline is re-measured once, with the change, under `--loosen
  test_seconds`.

## Why one tree still swung (slimfit#27)

On one commit at load ~20 the watch measured 5.46 and 7.51, then 10.19 and
14.12. Measured on c01e62c plus #27's cuts, 18 cores (6 performance, 12
efficiency):

- **The slowdown is averaged over the build.** `cargo llvm-cov nextest`
  builds inside the probed command. After a build that relinked (command
  73-87s) the probe saw 4.5-5.1 and test_seconds read 5.8-13.3; on a cached
  build (25-39s) it saw 1.2-1.6 and the same tree read 13-20 (both under
  background QoS, below). How much of the command is build, and how busy
  the build kept the cores, set the number. slimfit rebuilds more often
  than it seems: `build.rs` reruns when the git index changes, and every
  `git status` touches it.
- **Priority.** factorium runs heavy commands as `taskpolicy -b nice -n 10`:
  background QoS, efficiency cores and throttled I/O. The same tree read
  5.46 at normal priority (load 13.5, slowdown 1.006) and 13-20 under it
  (load 14-25); the CPU-hog test took 0.79s and 6.58s. The probe runs at
  the same priority and sees the CPU share, but not throttled I/O or the
  wait to start a process. `factorium worker gate` runs `slimfit check` at
  normal priority (its run: command 7.0s, slowdown 1.008, 5.516).
- **A fresh binary's first run.** macOS checks a newly linked binary on its
  first exec: 0.17-0.34s, and every process that execs it meanwhile waits
  (6 at once: 0.17s each). Each stand-in test, in its own nextest process,
  counted that wait; they now run in one test.

Tried: dividing each test by the slowdown while it ran, its window from
the report's timestamps (2d305b0). The build no longer set the number
(14.7, 14.9, 18.6 at command 76/39/30s, background QoS), but the level
doubled, since the build-inflated slowdown had set the 7.5 baseline. It
needs the baseline measured again, which #27 rules out, so it is not on.
Kept from it: a test whose report started before the command (a sample's
report a test copies) is not divided by a slowdown it never ran under.

What the gate does: it holds test_seconds as before, at normal priority,
and the suite keeps a margin under its bound instead of relying on the
correction. Clean clones at normal priority, load 9-14: c01e62c read 9.72
and 9.38 (67 tests), #27's tip 5.34 and 5.29 (62), against a bound of 10
on the 7.5 baseline.

## Where the correction stops holding (slimfit#41)

The host sat at 1-minute load 20-30 for over an hour (a background
cargo-mutants run, builds). Proving's own tree, unchanged, read 9.84 at
load 5 but 10.25-10.58 at load 15-17 against a bound of 10.475, and
slimfit#41's first measure of it at load 25-29 read 11.585 while the probe
saw a slowdown of 1.012. Most of that load runs at background QoS on the
12 efficiency cores; a gate runs at normal priority and the spinning probe
keeps a fast core, so it sees almost nothing, while the tests, many threads
that start processes, spill onto busy and slower cores. No probe of one
thread's CPU share sees that.

Decision: the correction is trusted up to a 1-minute load of
`load::BUSY` = 0.5 per core, read as the test command starts (before the
run adds its own) and recorded per suite as `host_load`. Above it:

- A `test_seconds` within its bound passes, as before. Load only slows
  tests, so a busy run that fits would fit on a quiet host too.
- One over its bound is neither violated nor passed: `check` lists the
  suite in `time_unchecked`, as for a test run inside `--mutants`
  (slimfit#39). The ratchet keeps the baseline's `test_seconds` and refuses
  `--loosen test_seconds`.
- `slimfit doctor`'s `host_busy` uses the same threshold.

Not taken:

- **A better probe** (per-core load, Darwin's QoS, a probe that starts
  processes). Each needs a model of the scheduler, and a factor tuned on
  this host for this suite; the cost of a miss is a false red that stops
  every landing.
- **Proving's tree measured back to back in the same run.** It doubles the
  gate (a second build and test run) and needs the target's tree checked
  out beside the change.

Cost: a test that really got slower lands unchecked while the host is busy,
and the next quiet gate (or proving's) fails on it. It is reported, in
`time_unchecked`, never passed as checked.
