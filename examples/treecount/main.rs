//! `treecount <out.tsv> <command> [args]`: runs the command with the hook
//! (`hook.c`, which it builds beside itself) in its processes, then appends to
//! `out.tsv` one line for the command's whole process tree: its id,
//! instructions retired, cycles, the CPU time of the processes counted, the
//! CPU time of the tree as the kernel sums it at reaping, and how many
//! processes were counted. Where those two CPU times differ, the tree had
//! processes the hook did not reach (see `hook.c`).
//!
//! It is nextest's `target.runner`: the id is `NEXTEST_BINARY_ID::NEXTEST_TEST_NAME`,
//! or `TREECOUNT_ID`; with neither (nextest listing the tests) it only runs
//! the command. For slimfit#66, which asks whether instructions hold under
//! load where CPU time does not.

mod record;

use record::Counts;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn id() -> Option<String> {
    if let Ok(id) = std::env::var("TREECOUNT_ID") {
        return Some(id);
    }
    let (b, t) = (std::env::var("NEXTEST_BINARY_ID").ok()?, std::env::var("NEXTEST_TEST_NAME").ok()?);
    Some(format!("{b}::{t}"))
}

const HOOK: &str = include_str!("hook.c");

/// The system binaries that run others, run as their copies (git apart).
const COPIED: &[&str] = &[
    "/bin/sh",
    "/bin/bash",
    "/bin/zsh",
    "/usr/bin/env",
    "/usr/bin/xargs",
    "/usr/bin/find",
    "/usr/bin/nice",
    "/usr/bin/nohup",
];

fn run(c: &mut Command) -> bool {
    c.status().is_ok_and(|s| s.success())
}

/// The hook's dylib and the system binaries' re-signed copies, made once beside this
/// binary (a directory named by the hook's source, renamed into place, so
/// runners starting at once make it at most once each and see it whole).
fn prepared() -> PathBuf {
    let me = std::env::current_exe().expect("treecount: no path to itself");
    let hash = HOOK.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let dir = me.with_file_name(format!("treecount-{hash:016x}"));
    if dir.join("ready").exists() {
        return dir;
    }
    let tmp = me.with_file_name(format!("treecount-{hash:016x}.{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("treecount: no directory for the hook");
    std::fs::write(tmp.join("hook.c"), HOOK).expect("treecount: hook.c not written");
    let built = run(Command::new("cc")
        .args(["-O2", "-arch", "arm64", "-arch", "arm64e", "-dynamiclib", "hook.c", "-o", "libtreecount_hook.dylib"])
        .current_dir(&tmp));
    assert!(built, "treecount: cc did not build the hook");
    for bin in COPIED {
        let copy = tmp.join(Path::new(bin).file_name().unwrap());
        let signed = std::fs::copy(bin, &copy).is_ok()
            && run(Command::new("codesign").args(["-f", "-s", "-"]).arg(&copy).stderr(std::process::Stdio::null()));
        assert!(signed, "treecount: {bin} not copied and re-signed");
    }
    copy_git(&tmp.join("git"));
    std::fs::write(tmp.join("ready"), "").expect("treecount: not ready");
    if std::fs::rename(&tmp, &dir).is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    dir
}

/// Xcode's git (what /usr/bin/git runs), re-signed: its bin/git, the
/// programs it runs from libexec/git-core and its share/git-core, which git
/// finds beside its binary.
fn copy_git(to: &Path) {
    let out = Command::new("xcrun").args(["--find", "git"]).output().expect("treecount: no xcrun");
    let git = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let usr = git.parent().and_then(Path::parent).expect("treecount: git is not in <dir>/bin");
    for part in ["bin/git", "libexec/git-core", "share/git-core"] {
        let dest = to.join(part);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        assert!(run(Command::new("cp").arg("-R").arg(usr.join(part)).arg(&dest)), "treecount: {part} not copied");
    }
    let macho = |p: &Path| {
        let head = std::fs::read(p).map(|b| b.get(..4).map(<[u8]>::to_vec)).ok().flatten();
        matches!(head.as_deref(), Some([0xcf, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe]))
    };
    let mut files = vec![to.join("bin/git")];
    for e in std::fs::read_dir(to.join("libexec/git-core")).unwrap().flatten() {
        if e.file_type().is_ok_and(|t| t.is_file()) {
            files.push(e.path());
        }
    }
    for f in files.into_iter().filter(|f| macho(f)) {
        let signed = run(Command::new("codesign").args(["-f", "-s", "-"]).arg(&f).stderr(std::process::Stdio::null()));
        assert!(signed, "treecount: {} not re-signed", f.display());
    }
}

/// A system binary's re-signed copy, as the hook runs it in place of the
/// binary, or `cmd` itself.
fn copy_of(hook: &Path, cmd: &str) -> PathBuf {
    let found = if cmd.contains('/') {
        Some(PathBuf::from(cmd))
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).map(|d| d.join(cmd)).find(|p| p.is_file())
    };
    match found.and_then(|f| f.canonicalize().ok()).as_ref().and_then(|f| f.to_str()) {
        Some("/usr/bin/git") => hook.join("git/bin/git"),
        Some(f) if COPIED.contains(&f) => hook.join(Path::new(f).file_name().unwrap()),
        _ => PathBuf::from(cmd),
    }
}

/// Sums the log's processes, each once: a process logged twice (by itself
/// at exit, by its parent at reaping) counts with its later, larger counts.
pub fn total(log: &str) -> (Counts, usize) {
    let mut each: BTreeMap<(i32, u64), Counts> = BTreeMap::new();
    for c in log.lines().filter_map(Counts::parse) {
        let e = each.entry((c.pid, c.start)).or_insert(c);
        if c.instructions > e.instructions {
            *e = c;
        }
    }
    let mut t = Counts { pid: 0, start: 0, instructions: 0, cycles: 0, cpu_ns: 0, children_cpu_ns: 0 };
    for c in each.values() {
        t.instructions += c.instructions;
        t.cycles += c.cycles;
        t.cpu_ns += c.cpu_ns;
    }
    (t, each.len())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [out, cmd, rest @ ..] = &args[..] else {
        eprintln!("usage: treecount <out.tsv> <command> [args]");
        return ExitCode::from(64);
    };
    let Some(id) = id() else {
        let e = Command::new(cmd).args(rest).exec();
        eprintln!("treecount: {cmd}: {e}");
        return ExitCode::from(127);
    };
    let hook = prepared();
    let log = tempfile::NamedTempFile::new().expect("treecount: no temporary log");
    let mut child = match Command::new(copy_of(&hook, cmd))
        .args(rest)
        .env("DYLD_INSERT_LIBRARIES", hook.join("libtreecount_hook.dylib"))
        .env("TREECOUNT_COPIES", &hook)
        .env("TREECOUNT_LOG", log.path())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("treecount: {cmd}: {e}");
            return ExitCode::from(127);
        }
    };
    let pid = child.id() as i32;
    // SAFETY: an all-zero siginfo_t is valid, and waitid only writes it.
    let mut si: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `si` is valid to write; WNOWAIT leaves the child a zombie to read.
    unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut si, libc::WEXITED | libc::WNOWAIT) };
    let top = record::of(pid);
    let status = child.wait().expect("treecount: the command was not reaped");
    let mut lines = std::fs::read_to_string(log.path()).unwrap_or_default();
    if let Some(t) = top {
        lines.push_str(&t.line());
    }
    let (t, n) = total(&lines);
    let tree = top.map_or(0, |t| t.cpu_ns + t.children_cpu_ns);
    let row = format!("{id}\t{}\t{}\t{}\t{tree}\t{n}\n", t.instructions, t.cycles, t.cpu_ns);
    let f = std::fs::OpenOptions::new().append(true).create(true).open(out);
    if let Err(e) = f.and_then(|mut f| f.write_all(row.as_bytes())) {
        eprintln!("treecount: {out}: {e}");
    }
    ExitCode::from(status.code().unwrap_or(1) as u8)
}
