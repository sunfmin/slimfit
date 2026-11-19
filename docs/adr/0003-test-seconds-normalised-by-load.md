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

The probe measures the factor a loaded host puts on a busy thread to within
3% (evidence: slimfit#40, comment "Evidence of ADR 0003's probe"). The hog
in `load.rs`'s test has to be other processes, one `yes` per core: threads
in the test binary share llvm-cov's counters with the workload and fight
for their cache lines, a contention no host load causes, and the test runs
once per mutant on a host everyone shares.

## Options not taken

- **CPU time of the test command's process tree** (`RUSAGE_CHILDREN`). Every
  preset's test command builds before it tests (`cargo llvm-cov nextest`,
  `go test`, `swift test`, gradle, vitest), so the CPU time would count the
  build. How much that is depends on what the build cache already held, and
  on a large workspace it can be more than the tests. That swaps a false red
  from load for a false red from a cold cache. Gradle also runs tests in its
  daemon, which is not our child, so their CPU time would not be counted.
- **A probe that sleeps between short slices** (a tenth of a core). Tried
  first: it saw a third of the slowdown a busy thread did. Schedulers raise
  the priority of a thread that has just woken, so a probe like that does
  not wait as a busy test thread does.
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
  for them: each child, and the parent when it wakes, queues for a core
  (a spawn slowed nearly twice as much as a spin). Such tests stay slower under load once normalised, and
  only `time_slack` absorbs it, so a suite keeps its spawns few. slimfit
  reads its working tree with three git commands, not nine (slimfit#16).
- A test that loads the host itself, like `load.rs`'s CPU-hog test, slows
  the tests that run beside it by more than the probe corrects. It runs
  alone (`.config/nextest.toml`). The Rust preset passes its junit
  setting with `--tool-config-file`, so a repository's own nextest config
  still applies.

## The gate runs at normal priority

What sets the number besides the tests (evidence: slimfit#40, comment
"Evidence of slimfit#27"):

- **The slowdown is averaged over the build.** `cargo llvm-cov nextest`
  builds inside the probed command, so how much of it is build, and how
  busy the build kept the cores, moves the factor. slimfit rebuilds more
  often than it seems: `build.rs` reruns when the git index changes, and
  every `git status` touches it.
- **Priority.** factorium runs heavy commands at background QoS
  (efficiency cores, throttled I/O); the probe sees the CPU share there,
  but not throttled I/O or the wait to start a process, and the same tree
  read 2-4× higher. `factorium worker gate` runs `slimfit check` at normal
  priority.
- **A fresh binary's first run.** macOS checks a newly linked binary on its
  first exec, and every process that execs it meanwhile waits; the
  stand-in tests run in one test, not one nextest process each.

So the gate holds `test_seconds` at normal priority, and the suite keeps a
margin under its bound instead of relying on the correction. A test whose
report started before the command (a sample's report a test copies) is not
divided by a slowdown it never ran under. Not taken: dividing each test by
the slowdown while it ran, which needs every baseline measured again.

## Where the correction stops holding

Under a sustained 1-minute load of 20-30 an unchanged tree read over its
bound while the probe saw a slowdown of 1.012 (evidence: slimfit#40,
comment "Evidence of slimfit#41"). Most of that load runs at background
QoS on the efficiency cores; a gate runs at normal priority and the
spinning probe keeps a fast core, so it sees almost nothing, while the
tests, many threads that start processes, spill onto busy and slower
cores. No probe of one thread's CPU share sees that.

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
