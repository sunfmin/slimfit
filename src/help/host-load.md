On a busy host everything runs slower, and slimfit corrects only `test_seconds` for it.

`test_seconds` is the sum of the tests' own durations, each divided by how much the host's load slowed a CPU-bound thread meanwhile: the slowdown slimfit measured, `load_slowdown` per suite in `.slimfit/out/measurement.json`. So a busy host does not fail `slimfit check` on its own. A gate's or a mutants run's wall time is not corrected.

## When the host is busy

The 1-minute load average (`uptime`) is above the core count (`sysctl -n hw.ncpu` on macOS, `nproc` on Linux), or `load_slowdown` is well above 1. Then don't set a baseline's `test_seconds` from that run: re-measure with `--fresh` when the host is quiet.
