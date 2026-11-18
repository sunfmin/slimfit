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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Runs `f`, and returns what it returned with the slowdown the host's load
/// put on a CPU-bound thread meanwhile: wall time over CPU time, at least 1.
pub fn slowdown_during<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let running = Arc::new(AtomicBool::new(true));
    let probe = {
        let running = running.clone();
        std::thread::spawn(move || {
            let (wall, cpu) = (Instant::now(), thread_cpu());
            while running.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
            let cpu = (thread_cpu() - cpu).as_secs_f64();
            if cpu > 0.0 { (wall.elapsed().as_secs_f64() / cpu).max(1.0) } else { 1.0 }
        })
    };
    let out = f();
    running.store(false, Ordering::Relaxed);
    (out, probe.join().unwrap_or(1.0))
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

    /// A stand-in test: a fixed amount of CPU work, timed by the wall clock
    /// as a test runner times a test, then normalised by the slowdown.
    fn normalised_seconds() -> f64 {
        let (wall, slowdown) = slowdown_during(|| {
            let t = Instant::now();
            let mut x = 0u64;
            for i in 0..100_000_000u64 {
                x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
            }
            t.elapsed().as_secs_f64()
        });
        wall / slowdown
    }

    /// The hog is other processes, as a busy host's load is: threads of
    /// this one would share its coverage counters with the stand-in, and
    /// contend for their cache lines, which no host load does.
    #[test]
    fn a_cpu_hog_leaves_the_normalised_time_within_tolerance() {
        let unloaded = normalised_seconds();
        let mut hogs: Vec<_> = (0..2 * std::thread::available_parallelism().map_or(4, |n| n.get()))
            .map(|_| std::process::Command::new("yes").stdout(std::process::Stdio::null()).spawn().unwrap())
            .collect();
        let loaded = normalised_seconds();
        for h in &mut hogs {
            h.kill().unwrap();
            h.wait().unwrap();
        }
        let tolerance = crate::config::CheckConfig::default().time_tolerance;
        assert!(
            (loaded - unloaded).abs() <= unloaded * tolerance,
            "unloaded {unloaded:.3}s, under a CPU hog {loaded:.3}s"
        );
    }
}
