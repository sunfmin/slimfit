//! `slimfit doctor`: how slimfit is set up in a repository and on this
//! host. It reads `slimfit.toml`, the baseline, factorium's gates and the
//! host, changes nothing, and answers each finding with its fix.

use crate::config::{self, Suite};
use crate::ratchet::{self, Baseline};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// What `slimfit doctor` can find, each with the fix it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, agent_help::Help)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The repository has no `slimfit.toml`: slimfit is not set up here. Fix: `slimfit init`, timed first (see [costs](help:costs)).
    NoConfig,
    /// `slimfit.toml` cannot be read: every command refuses until it can. Fix: correct it as the finding says.
    BadConfig,
    /// There is no `.slimfit/baseline.json`: `slimfit check` has nothing to hold. Fix: `slimfit init` (it keeps `slimfit.toml`).
    NoBaseline,
    /// A gate runs the project's own tests and also `slimfit check`, which is a test run: the tests run twice. Fix: take the project's test command out of that gate.
    TestsRunTwice,
    /// A gate runs mutation testing (`--mutants`, `--all-mutants`, `--pending-*` or a mutation tool): it holds up every landing for hours. Fix: move it to idle work (in factorium, `[idle] steps`).
    MutantsInGate,
    /// The baseline holds a suite's mutants with no commit they were measured at, or no per-file counts: every `--mutants` run mutates every file. Fix: one `slimfit ratchet --all-mutants` on an idle host, then commit the baseline.
    MutantsUnscoped,
    /// Files of a suite have no mutant count in the baseline yet: `--mutants` does not hold them. Fix: `slimfit ratchet --pending-seconds <s>` pieces on an idle host (see [the piecewise baseline](help:piecewise-baseline)).
    MutantsPending,
    /// The host's 1-minute load is above its core count: what is measured now runs slow and its wall times are skewed (see [host load](help:host-load)). Fix: measure when the host is quiet, or at `nice -n 19`.
    HostBusy,
    /// A tool a suite's commands run is not installed on this host: the command that needs it fails. Fix: install it as the finding says.
    ToolMissing,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Finding {
    pub kind: Kind,
    /// The suite it concerns, when it concerns one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suite: Option<String>,
    /// What was found, and where.
    pub what: String,
    /// What to do about it.
    pub fix: String,
}

fn finding(kind: Kind, suite: Option<&str>, what: String, fix: String) -> Finding {
    Finding { kind, suite: suite.map(String::from), what, fix }
}

/// What doctor reads from the host: its load and where its tools are.
#[derive(Debug, Clone, Serialize)]
pub struct Host {
    /// The 1-minute load average.
    pub load: f64,
    pub cores: usize,
    /// The directories searched for tools: PATH, then cargo's own bin
    /// directory (where `cargo install` puts cargo's subcommands).
    #[serde(skip)]
    pub path: Vec<PathBuf>,
    /// Whether a Java development kit is installed (Gradle needs one).
    #[serde(skip)]
    pub jdk: bool,
}

impl Host {
    pub fn current() -> Host {
        let mut load = [0f64; 3];
        // SAFETY: `load` has room for the 3 samples asked for.
        let n = unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
        let mut path: Vec<PathBuf> =
            std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")));
        path.extend(cargo_home.map(|h| h.join("bin")));
        let jdk = jdk(&path);
        Host {
            load: if n >= 1 { load[0] } else { 0.0 },
            cores: std::thread::available_parallelism().map_or(1, |n| n.get()),
            path,
            jdk,
        }
    }

    fn has(&self, exe: &str) -> bool {
        find(&self.path, exe).is_some()
    }
}

fn find(path: &[PathBuf], exe: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.iter()
        .map(|d| d.join(exe))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// A JDK: `$JAVA_HOME/bin/java`, one macOS knows of, or a `java` on PATH
/// other than macOS's stub, which only offers to install one.
fn jdk(path: &[PathBuf]) -> bool {
    if std::env::var_os("JAVA_HOME").is_some_and(|h| Path::new(&h).join("bin/java").is_file()) {
        return true;
    }
    let java_home = Path::new("/usr/libexec/java_home");
    if java_home.exists()
        && std::process::Command::new(java_home)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    {
        return true;
    }
    find(path, "java").is_some_and(|j| j != Path::new("/usr/bin/java"))
}

/// A tool a preset's commands run: found by `needle` in a command line.
struct Tool {
    needle: &'static str,
    /// The executable that must be found; `None`: a JDK.
    exe: Option<&'static str>,
    install: &'static str,
}

const TOOLS: &[Tool] = &[
    Tool { needle: "cargo ", exe: Some("cargo"), install: "install Rust: `curl https://sh.rustup.rs -sSf | sh`" },
    Tool { needle: "cargo llvm-cov", exe: Some("cargo-llvm-cov"), install: "`cargo install cargo-llvm-cov --locked`" },
    Tool { needle: "nextest", exe: Some("cargo-nextest"), install: "`cargo install cargo-nextest --locked`" },
    Tool { needle: "cargo mutants", exe: Some("cargo-mutants"), install: "`cargo install cargo-mutants --locked`" },
    Tool { needle: "go test", exe: Some("go"), install: "`brew install go`" },
    Tool {
        needle: "gremlins",
        exe: Some("gremlins"),
        install: "`go install github.com/go-gremlins/gremlins/cmd/gremlins@latest`",
    },
    Tool { needle: "swift test", exe: Some("swift"), install: "install Xcode, or a Swift toolchain from swift.org" },
    Tool { needle: "muter", exe: Some("muter"), install: "`brew install muter-mutation-testing/formulae/muter`" },
    Tool { needle: "npx ", exe: Some("npx"), install: "install Node.js: `brew install node`" },
    Tool { needle: "gradlew", exe: None, install: "`brew install openjdk@21`, then set `JAVA_HOME` to it" },
];

/// Lines of a gate that run a project's tests (other than `slimfit check`).
const TEST_RUNNERS: &[&str] = &[
    "cargo test",
    "cargo nextest",
    "cargo llvm-cov",
    "go test",
    "swift test",
    "gradlew test",
    "gradle test",
    "vitest",
    "jest",
    "npm test",
    "npm run test",
    "pnpm test",
    "yarn test",
    "bun test",
    "pytest",
];

/// What in a gate's line is mutation testing.
const MUTATION: &[&str] = &[
    "--mutants",
    "--all-mutants",
    "--pending-seconds",
    "--pending-files",
    "cargo mutants",
    "stryker",
    "pitest",
    "gremlins",
    "muter run",
];

/// Every finding, in the order to fix them: setup first, then gates, the
/// baseline, the host and its tools.
pub fn diagnose(repo: &Path, host: &Host) -> Vec<Finding> {
    let mut out = Vec::new();
    let suites: Vec<Suite> = if !repo.join(config::FILE).exists() {
        out.push(finding(
            Kind::NoConfig,
            None,
            format!("no {}: slimfit is not set up in this repository", config::FILE),
            "time it first (`slimfit help costs`), then `slimfit init`".into(),
        ));
        let files = crate::git::worktree(repo).map(|w| w.files).unwrap_or_default();
        config::detect(&files).iter().map(|s| s.resolve()).collect()
    } else {
        match config::load(repo) {
            Ok(c) => c.suites.iter().map(|s| s.resolve()).collect(),
            Err(e) => {
                out.push(finding(
                    Kind::BadConfig,
                    None,
                    format!("{e:#}"),
                    format!("correct {} as the error says; `slimfit init --help` shows its fields", config::FILE),
                ));
                Vec::new()
            }
        }
    };
    out.extend(gates(repo));
    if repo.join(config::FILE).exists() {
        match ratchet::load(repo) {
            Ok(b) if repo.join(ratchet::FILE).exists() => out.extend(baseline(&b)),
            Ok(_) => out.push(finding(
                Kind::NoBaseline,
                None,
                format!("no {}: `slimfit check` holds nothing", ratchet::FILE),
                format!("`slimfit init` (it keeps {}), then commit the baseline", config::FILE),
            )),
            Err(e) => out.push(finding(
                Kind::NoBaseline,
                None,
                format!("{e:#}"),
                format!("restore {} from git, or `slimfit init` to write it again", ratchet::FILE),
            )),
        }
    }
    if host.load > host.cores as f64 {
        out.push(finding(
            Kind::HostBusy,
            None,
            format!("the 1-minute load is {:.1} on {} cores: anything measured now runs slow", host.load, host.cores),
            format!(
                "measure when `uptime`'s load is below {}; meanwhile run slimfit at `nice -n 19` and don't ratchet test_seconds from it",
                host.cores
            ),
        ));
    }
    out.extend(tools(&suites, host));
    out
}

/// The commands a factorium gate runs, by where they are written: each
/// `[gate]` entry of `factorium.toml`, and the lines of each repository
/// script it names (not the scripts those name).
fn gates(repo: &Path) -> Vec<Finding> {
    let Ok(text) = std::fs::read_to_string(repo.join("factorium.toml")) else { return Vec::new() };
    let Ok(t) = toml::from_str::<toml::Table>(&text) else { return Vec::new() };
    let Some(toml::Value::Table(gate)) = t.get("gate") else { return Vec::new() };
    let mut out = Vec::new();
    for (key, entry) in gate {
        let Some(entry) = entry.as_str() else { continue };
        let at = format!("factorium.toml [gate] {key}");
        let mut lines = vec![(at.clone(), entry.to_string())];
        for word in entry.split_whitespace() {
            let p = repo.join(word.trim_start_matches("./"));
            if word.contains(['/', '.']) && p.is_file() {
                let script = std::fs::read_to_string(&p).unwrap_or_default();
                let name = word.trim_start_matches("./");
                lines.extend(
                    script
                        .lines()
                        .enumerate()
                        .filter(|(_, l)| !l.trim_start().starts_with('#'))
                        .map(|(i, l)| (format!("{name}:{}", i + 1), l.trim().to_string())),
                );
            }
        }
        for (place, line) in &lines {
            if MUTATION.iter().any(|m| line.contains(m)) {
                out.push(finding(
                    Kind::MutantsInGate,
                    None,
                    format!("{place} runs mutation testing in the `{key}` gate: `{line}`"),
                    format!("take `{line}` out of {place}; run it as idle work: factorium.toml `[idle] steps`"),
                ));
            }
        }
        let checks = lines.iter().any(|(_, l)| l.contains("slimfit check"));
        for (place, line) in lines.iter().filter(|(_, l)| !l.contains("slimfit")) {
            if checks && let Some(r) = TEST_RUNNERS.iter().find(|r| line.contains(*r)) {
                out.push(finding(
                    Kind::TestsRunTwice,
                    None,
                    format!(
                        "the `{key}` gate ({at}) runs `slimfit check` and also `{r}` at {place}: the tests run twice"
                    ),
                    format!("take `{line}` out of {place}: `slimfit check` is that test run"),
                ));
            }
        }
    }
    out
}

fn baseline(b: &Baseline) -> Vec<Finding> {
    let mut out = Vec::new();
    for (suite, m) in &b.suites {
        if m.mutants.is_some() && (b.mutants_at.is_none() || m.mutant_files.is_empty()) {
            let missing = match (b.mutants_at.is_none(), m.mutant_files.is_empty()) {
                (true, true) => "the commit they were measured at and per-file counts",
                (true, false) => "the commit they were measured at",
                _ => "per-file counts",
            };
            out.push(finding(
                Kind::MutantsUnscoped,
                Some(suite),
                format!(
                    "the baseline holds {suite}'s mutants without {missing}: every `--mutants` run mutates every file"
                ),
                "on an idle host, `nice -n 19 slimfit ratchet --all-mutants`, then commit .slimfit/baseline.json"
                    .into(),
            ));
        }
        if !m.mutants_pending.is_empty() {
            let n = m.mutants_pending.len();
            out.push(finding(
                Kind::MutantsPending,
                Some(suite),
                format!(
                    "{n} file(s) of {suite} have no mutant count yet (first: {}): `--mutants` does not hold them",
                    m.mutants_pending[0]
                ),
                "on an idle host, `nice -n 19 slimfit ratchet --pending-seconds 1800`, then commit .slimfit/baseline.json; \
                 again until none is pending"
                    .into(),
            ));
        }
    }
    out
}

fn tools(suites: &[Suite], host: &Host) -> Vec<Finding> {
    let mut out = Vec::new();
    for s in suites {
        let mut seen = Vec::new();
        for (command, what) in [(&s.test_command, "test command"), (&s.mutants_command, "mutants command")] {
            for t in TOOLS.iter().filter(|t| command.contains(t.needle)) {
                let name = t.exe.unwrap_or("a JDK");
                let found = t.exe.map_or(host.jdk, |e| host.has(e));
                if found || seen.contains(&name) {
                    continue;
                }
                seen.push(name);
                out.push(finding(
                    Kind::ToolMissing,
                    Some(&s.name),
                    format!("{name} is not installed: the {} suite's {what} runs it", s.name),
                    t.install.to_string(),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Metrics, MutantCounts, Snapshot};

    /// A repository with a Rust suite, a baseline and `files` as written.
    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut all = vec![(config::FILE, "[[suite]]\nname = \"rust\"\npreset = \"rust\"\n")];
        all.extend_from_slice(files);
        for (f, text) in all {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        if !files.iter().any(|(f, _)| *f == ratchet::FILE) {
            ratchet::save(dir.path(), &Baseline::default()).unwrap();
        }
        dir
    }

    /// A directory holding every tool.
    fn every_tool() -> tempfile::TempDir {
        let bin = tempfile::tempdir().unwrap();
        for t in TOOLS.iter().filter_map(|t| t.exe) {
            install(bin.path(), t);
        }
        bin
    }

    /// An idle host with a JDK and the tools in `bin`.
    fn host(bin: &tempfile::TempDir) -> Host {
        Host { load: 0.5, cores: 8, path: vec![bin.path().into()], jdk: true }
    }

    fn install(bin: &Path, exe: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = bin.join(exe);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn kinds(f: &[Finding]) -> Vec<Kind> {
        f.iter().map(|f| f.kind).collect()
    }

    fn with_mutants(at: Option<&str>, per_file: bool, pending: &[&str]) -> String {
        let c = MutantCounts { killed: 3, survived: 1, unviable: 0 };
        let m = Metrics {
            mutants: Some(c),
            mutant_files: if per_file { [("src/lib.rs".to_string(), c)].into() } else { Default::default() },
            mutants_pending: pending.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let b = Baseline {
            suites: [("rust".to_string(), m)].into(),
            loosened: vec![],
            mutants_at: at.map(|c| Snapshot { commit: c.into(), dirty: vec![] }),
        };
        serde_json::to_string(&b).unwrap()
    }

    /// A set-up repository on an idle host with its tools: nothing to fix.
    /// slimfit's own setup is one such.
    #[test]
    fn a_sound_setup_has_no_findings() {
        let r = repo(&[(ratchet::FILE, &with_mutants(Some("abc"), true, &[]))]);
        assert_eq!(diagnose(r.path(), &host(&every_tool())), vec![]);
        let own = Path::new(env!("CARGO_MANIFEST_DIR"));
        let found = diagnose(own, &host(&every_tool()));
        assert!(!found.iter().any(|f| matches!(f.kind, Kind::TestsRunTwice | Kind::MutantsInGate)), "{found:#?}");
    }

    /// With no `slimfit.toml`, the tools are those of the suites `init` would detect.
    #[test]
    fn no_config_says_init_and_names_the_detected_suites_tools() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module x\n").unwrap();
        crate::git::run(dir.path(), &["init", "-q"]).unwrap();
        let bin = every_tool();
        std::fs::remove_file(bin.path().join("gremlins")).unwrap();
        let f = diagnose(dir.path(), &host(&bin));
        assert_eq!(kinds(&f), vec![Kind::NoConfig, Kind::ToolMissing], "{f:#?}");
        assert!(f[0].fix.contains("slimfit init") && f[1].what.starts_with("gremlins"), "{f:#?}");
    }

    #[test]
    fn a_bad_config_or_no_baseline_is_named() {
        let r = repo(&[(config::FILE, "[[suite]]\nname = 1\n")]);
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::BadConfig]);
        let r = repo(&[]);
        std::fs::remove_file(r.path().join(ratchet::FILE)).unwrap();
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::NoBaseline]);
        assert!(f[0].fix.contains("slimfit init"), "{f:#?}");
    }

    /// The gate's script runs `cargo test` and `slimfit check`: the fix
    /// names that line of the script. Comments and other gates don't count.
    #[test]
    fn a_gate_running_tests_and_check_runs_them_twice() {
        let r = repo(&[
            ("factorium.toml", "[gate]\nfast = \"./gate.sh\"\nfast_limit = 600\nslow = \"cargo test\"\n"),
            (
                "gate.sh",
                "#!/bin/sh\n# cargo test is slimfit check's\ncargo fmt --check\ncargo test --workspace\nslimfit check\n",
            ),
        ]);
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::TestsRunTwice], "{f:#?}");
        assert!(f[0].fix.contains("`cargo test --workspace` out of gate.sh:4"), "{f:#?}");
    }

    /// Mutants in a gate, written in its entry or its script; in `[idle]` they are fine.
    #[test]
    fn mutants_in_a_gate_go_to_idle_work() {
        let r = repo(&[
            (
                "factorium.toml",
                "[gate]\nfast = \"slimfit check --mutants\"\nslow = \"sh slow.sh\"\n[idle]\nsteps = [\"slimfit check --all-mutants\"]\n",
            ),
            ("slow.sh", "slimfit check --all-mutants\n"),
        ]);
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::MutantsInGate, Kind::MutantsInGate], "{f:#?}");
        assert!(f[0].what.contains("[gate] fast") && f[1].what.contains("slow.sh:1"), "{f:#?}");
        assert!(f.iter().all(|f| f.fix.contains("[idle]")), "{f:#?}");
    }

    /// Mutants with no commit or no per-file counts: every `--mutants` is a full run.
    #[test]
    fn mutants_without_a_commit_or_per_file_counts_are_unscoped() {
        for (at, per_file, missing) in [
            (None, true, "without the commit"),
            (Some("abc"), false, "without per-file"),
            (None, false, "without the commit they were measured at and per-file"),
        ] {
            let r = repo(&[(ratchet::FILE, &with_mutants(at, per_file, &[]))]);
            let f = diagnose(r.path(), &host(&every_tool()));
            assert_eq!(kinds(&f), vec![Kind::MutantsUnscoped], "{f:#?}");
            assert!(f[0].what.contains(missing) && f[0].fix.contains("--all-mutants"), "{f:#?}");
            assert_eq!(f[0].suite.as_deref(), Some("rust"));
        }
    }

    #[test]
    fn files_pending_a_mutant_count_are_named() {
        let r = repo(&[(ratchet::FILE, &with_mutants(Some("abc"), true, &["src/a.rs", "src/b.rs"]))]);
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::MutantsPending], "{f:#?}");
        assert!(f[0].what.starts_with("2 file(s)") && f[0].what.contains("src/a.rs"), "{f:#?}");
        assert!(f[0].fix.contains("--pending-seconds"), "{f:#?}");
    }

    /// A load above the core count is busy; at it, not yet.
    #[test]
    fn a_load_above_the_cores_is_a_busy_host() {
        let r = repo(&[]);
        let bin = every_tool();
        let at = |load| kinds(&diagnose(r.path(), &Host { load, ..host(&bin) }));
        assert_eq!(at(8.0), vec![]);
        assert_eq!(at(8.5), vec![Kind::HostBusy]);
    }

    /// Each tool a suite's commands run, once, by the command that needs it;
    /// Kotlin's Gradle needs a JDK.
    #[test]
    fn a_missing_tool_is_named_with_its_install() {
        let r = repo(&[]);
        let bin = tempfile::tempdir().unwrap();
        install(bin.path(), "cargo");
        let h = host(&bin);
        let f = diagnose(r.path(), &h);
        let named: Vec<&str> = f.iter().map(|f| f.what.split(' ').next().unwrap()).collect();
        assert_eq!(named, ["cargo-llvm-cov", "cargo-nextest", "cargo-mutants"], "{f:#?}");
        assert!(f[0].what.contains("test command") && f[2].what.contains("mutants command"), "{f:#?}");
        assert!(f[0].fix.contains("cargo install cargo-llvm-cov"), "{f:#?}");
        for (preset, missing) in [("go", "gremlins"), ("swift", "muter"), ("kotlin", "a JDK")] {
            let r = repo(&[(config::FILE, &format!("[[suite]]\nname = \"s\"\npreset = \"{preset}\"\n"))]);
            let bin = every_tool();
            let mut h = host(&bin);
            h.jdk = preset != "kotlin";
            if preset != "kotlin" {
                std::fs::remove_file(h.path[0].join(missing)).unwrap();
            }
            let f = diagnose(r.path(), &h);
            assert_eq!(kinds(&f), vec![Kind::ToolMissing], "{preset}: {f:#?}");
            assert!(f[0].what.starts_with(missing), "{preset}: {f:#?}");
        }
    }
}
