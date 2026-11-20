//! One process's counts, as the kernel keeps them for it alive or as a
//! zombie, and as a line of the log `hook.c` writes.

/// A process's counts. `start` (mach time) and `pid` make it one process
/// across an `exec`, which keeps both and the counts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Counts {
    pub pid: i32,
    pub start: u64,
    pub instructions: u64,
    pub cycles: u64,
    /// user + system, in nanoseconds.
    pub cpu_ns: u64,
    /// user + system of the descendants it reaped, in nanoseconds.
    pub children_cpu_ns: u64,
}

#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(tb: *mut Timebase) -> i32;
}

/// Mach time as nanoseconds.
fn ns(ticks: u64) -> u64 {
    let mut tb = Timebase { numer: 0, denom: 0 };
    // SAFETY: `tb` is a valid timebase for the call to write.
    unsafe { mach_timebase_info(&mut tb) };
    (ticks as u128 * tb.numer as u128 / tb.denom.max(1) as u128) as u64
}

/// The counts of `pid`, alive or a zombie not yet reaped.
pub fn of(pid: i32) -> Option<Counts> {
    // SAFETY: an all-zero rusage_info_v4 is valid, and the call only writes it.
    let mut ri: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `ri` is a rusage_info_v4, the size flavour V4 writes.
    let r = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, &mut ri as *mut _ as *mut libc::rusage_info_t) };
    (r == 0).then(|| Counts {
        pid,
        start: ri.ri_proc_start_abstime,
        instructions: ri.ri_instructions,
        cycles: ri.ri_cycles,
        cpu_ns: ns(ri.ri_user_time + ri.ri_system_time),
        children_cpu_ns: ns(ri.ri_child_user_time + ri.ri_child_system_time),
    })
}

impl Counts {
    pub fn line(&self) -> String {
        let c = self;
        format!("{} {} {} {} {} {}\n", c.pid, c.start, c.instructions, c.cycles, c.cpu_ns, c.children_cpu_ns)
    }

    pub fn parse(line: &str) -> Option<Counts> {
        let v: Vec<u64> = line.split(' ').map(|f| f.parse().ok()).collect::<Option<_>>()?;
        let [pid, start, instructions, cycles, cpu_ns, children_cpu_ns] = v[..] else { return None };
        Some(Counts { pid: pid as i32, start, instructions, cycles, cpu_ns, children_cpu_ns })
    }
}
