//! How much the host's load slowed a command down, so a test's duration
//! says how slow the test is, not how busy the host was (ADR 0003).
//!
//! A probe thread spins for as long as the command runs. On an idle core its
//! CPU time keeps up with the wall clock; on a host with more runnable
//! threads than cores, it gets its share of a core like every other
//! CPU-bound thread, and its wall time stretches as theirs does. Wall time
//! over CPU time is that stretch. The probe never sleeps: schedulers favour
//! a thread that just woke, and a probe that slept saw a third of the
//! slowdown a busy test thread did.
//!
//! The probe samples its CPU time as it goes, so the stretch is known over
//! any part of the run: a test is divided by the stretch while it ran, not
//! by one averaged over the build before it (slimfit#27).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the probe samples its CPU time.
const SAMPLE: Duration = Duration::from_millis(10);
/// A window shorter than this is widened to it, about its middle: over a
/// few samples one missed time slice would read as a large stretch.
const MIN_WINDOW: f64 = 1.0;

/// What the probe saw: its CPU time, sampled against the wall clock.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// When the probe started, in seconds since the Unix epoch.
    start: f64,
    /// (wall, CPU) seconds since the start, both rising; empty for a
    /// probe that never ran.
    samples: Vec<(f64, f64)>,
}

impl Probe {
    /// The stretch over the whole run, at least 1; 1 for a probe that never ran.
    pub fn slowdown(&self) -> f64 {
        match (self.samples.first(), self.samples.last()) {
            (Some(a), Some(b)) => stretch(b.0 - a.0, b.1 - a.1),
            _ => 1.0,
        }
    }

    /// The stretch while something ran from `from` to `to` (seconds since
    /// the Unix epoch), the window widened to at least `MIN_WINDOW` and
    /// kept within the run. `None` when the window is not within the run
    /// (a report written before it): the probe did not see that time.
    pub fn slowdown_over(&self, from: f64, to: f64) -> Option<f64> {
        let end = self.samples.last()?.0;
        // Report timestamps are rounded to the millisecond.
        let (from, to) = (from - self.start, to - self.start);
        if from < -0.01 || to > end + 0.01 || to < from {
            return None;
        }
        let width = (to - from).max(MIN_WINDOW).min(end);
        let lo = ((from + to - width) / 2.0).clamp(0.0, end - width);
        Some(stretch(width, self.cpu_at(lo + width) - self.cpu_at(lo)))
    }

    /// A probe that saw `slowdown` throughout `seconds`, from `start`.
    #[cfg(test)]
    pub fn steady(start: f64, seconds: f64, slowdown: f64) -> Probe {
        Probe { start, samples: vec![(0.0, 0.0), (seconds, seconds / slowdown)] }
    }

    /// The probe's CPU time at `wall` seconds since the start, between samples by line.
    fn cpu_at(&self, wall: f64) -> f64 {
        let i = self.samples.partition_point(|s| s.0 < wall);
        match (i.checked_sub(1).map(|j| self.samples[j]), self.samples.get(i)) {
            (Some(a), Some(b)) if b.0 > a.0 => a.1 + (b.1 - a.1) * (wall - a.0) / (b.0 - a.0),
            (_, Some(b)) => b.1,
            (Some(a), None) => a.1,
            (None, None) => 0.0,
        }
    }
}

/// Runs `f`, and returns what it returned with what the probe saw meanwhile.
pub fn probe_during<T>(f: impl FnOnce() -> T) -> (T, Probe) {
    let running = Arc::new(AtomicBool::new(true));
    let probe = {
        let running = running.clone();
        std::thread::spawn(move || {
            let start = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
            let (wall, cpu) = (Instant::now(), thread_cpu());
            let sample = || (wall.elapsed().as_secs_f64(), (thread_cpu() - cpu).as_secs_f64());
            let mut samples = vec![(0.0, 0.0)];
            let mut next = SAMPLE;
            while running.load(Ordering::Relaxed) {
                std::hint::spin_loop();
                if wall.elapsed() >= next {
                    samples.push(sample());
                    next += SAMPLE;
                }
            }
            samples.push(sample());
            Probe { start, samples }
        })
    };
    let out = f();
    running.store(false, Ordering::Relaxed);
    (out, probe.join().unwrap_or_default())
}

/// Runs `f`, and returns what it returned with the slowdown the host's load
/// put on a CPU-bound thread meanwhile: wall time over CPU time, at least 1.
#[cfg(test)]
pub fn slowdown_during<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let (out, probe) = probe_during(f);
    (out, probe.slowdown())
}

/// Wall time over CPU time, at least 1; 1 for a thread that never ran.
fn stretch(wall: f64, cpu: f64) -> f64 {
    if cpu > 0.0 { (wall / cpu).max(1.0) } else { 1.0 }
}

/// The CPU time the calling thread has used.
fn thread_cpu() -> Duration {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `t` is a valid timespec for the call to write.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window's stretch is the probe's while it ran: a short window is
    /// widened about its middle, and one outside the run has none.
    #[test]
    fn a_window_has_the_stretch_the_probe_saw_while_it_ran() {
        // Idle for a second, then at half a core for two.
        let p = Probe { start: 100.0, samples: vec![(0.0, 0.0), (1.0, 1.0), (2.0, 1.5), (3.0, 2.0)] };
        assert_eq!(p.slowdown(), 1.5);
        assert_eq!(p.slowdown_over(100.0, 101.0), Some(1.0));
        assert_eq!(p.slowdown_over(101.0, 103.0), Some(2.0));
        assert_eq!(p.slowdown_over(102.0, 102.01), Some(2.0));
        // Widened to a second, kept within the run: [0, 1].
        assert_eq!(p.slowdown_over(100.0, 100.01), Some(1.0));
        assert_eq!(p.slowdown_over(99.0, 99.5), None);
        assert_eq!(p.slowdown_over(102.5, 104.0), None);
        assert_eq!(Probe::default().slowdown(), 1.0);
        assert_eq!(Probe::default().slowdown_over(0.0, 1.0), None);
    }

    /// The stretch is wall over CPU, never below 1, and 1 for a probe that never ran.
    #[test]
    fn stretch_is_wall_over_cpu_at_least_one() {
        assert_eq!(stretch(3.0, 1.5), 2.0);
        assert_eq!(stretch(1.0, 2.0), 1.0);
        assert_eq!(stretch(1.0, 0.0), 1.0);
        let before = thread_cpu();
        let mut x = 0u64;
        for i in 0..200_000u64 {
            x = std::hint::black_box(x.wrapping_add(i));
        }
        assert!(thread_cpu() > before, "thread CPU time did not advance");
    }

    /// A stand-in test: a fixed amount of CPU work, timed by the wall clock
    /// as a test runner times a test, then normalised by the slowdown. Also
    /// the work's own CPU time: what it would take on an idle core.
    fn normalised_and_cpu_seconds() -> (f64, f64) {
        let ((wall, cpu), slowdown) = slowdown_during(|| {
            let (t, c) = (Instant::now(), thread_cpu());
            let mut x = 0u64;
            for i in 0..60_000_000u64 {
                x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
            }
            (t.elapsed().as_secs_f64(), (thread_cpu() - c).as_secs_f64())
        });
        (wall / slowdown, cpu)
    }

    /// The hog is other processes, as a busy host's load is: threads of
    /// this one would share its coverage counters with the stand-in, and
    /// contend for their cache lines, which no host load does.
    /// Not run under mutants (.cargo/mutants.toml): its timing verdict on a
    /// busy host would count kills that are not the mutant's.
    /// Each run is held to its own CPU time, not to the other run: on a
    /// host with fast and slow cores the same work took 0.25s or 0.87s of
    /// CPU, by which core it got, which no load probe can correct.
    #[test]
    fn a_cpu_hog_leaves_the_normalised_time_within_tolerance() {
        let tolerance = crate::config::CheckConfig::default().time_tolerance;
        let near = |a: f64, b: f64| (a - b).abs() <= b * tolerance;
        let (unloaded, unloaded_cpu) = normalised_and_cpu_seconds();
        let mut hogs: Vec<_> = (0..std::thread::available_parallelism().map_or(4, |n| n.get()))
            .map(|_| std::process::Command::new("yes").stdout(std::process::Stdio::null()).spawn().unwrap())
            .collect();
        let (loaded, loaded_cpu) = normalised_and_cpu_seconds();
        for h in &mut hogs {
            h.kill().unwrap();
            h.wait().unwrap();
        }
        assert!(
            near(unloaded, unloaded_cpu) && near(loaded, loaded_cpu),
            "normalised (CPU) seconds: unloaded {unloaded:.3} ({unloaded_cpu:.3}), under a CPU hog {loaded:.3} ({loaded_cpu:.3})"
        );
    }
}
