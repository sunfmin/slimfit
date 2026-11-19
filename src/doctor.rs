//! `slimfit doctor`: how slimfit is set up in a repository and on this
//! host. It reads `slimfit.toml`, the baseline, factorium's gates and the
//! host, changes nothing, and answers each finding with its fix.

use crate::config::{self, Suite};
use crate::ratchet;
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
    /// A gate or an `[idle]` step runs mutation testing (`--mutants` or a mutation tool): in a gate it holds up every landing; idle work commits nothing, so its measurement is never recorded. Fix: take it out; the agent runs `slimfit check --mutants` on its own change at background priority, then `slimfit ratchet`, and commits; the gate's plain `slimfit check` holds that it did (see [what the gate holds](help:mutants-scope)).
    MutantsInGate,
    /// A command in `factorium.toml` (a gate or idle work) mutates the whole codebase: `--all-mutants`, which slimfit refuses, or a mutation tool run without slimfit and without a diff. Mutants run only on a change. Fix: take it out; the agent runs `slimfit check --mutants`, the mutants of its changed lines, as for `mutants_in_gate`.
    WholeCodebaseMutants,
    /// There is no branch changes land on: `slimfit.toml` sets no `[check] target` and the repository has no `origin/HEAD`. `check` then cannot hold a change's lines to a mutants measurement, and `--mutants` refuses (see [what the gate holds](help:mutants-scope)). Fix: set `target` under `[check]` to that branch, or `git remote set-head origin --auto`.
    NoTarget,
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
        let path = tool_path();
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

/// Where tools are looked for: PATH, then cargo's own bin directory, where
/// cargo finds its subcommands too.
pub fn tool_path() -> Vec<PathBuf> {
    let mut path: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")));
    path.extend(cargo_home.map(|h| h.join("bin")));
    path
}

pub fn find(path: &[PathBuf], exe: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.iter()
        .map(|d| d.join(exe))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// A JDK: `$JAVA_HOME/bin/java`, one macOS knows of, or a `java` on PATH
/// other than macOS's stub, which only offers to install one.
fn jdk(path: &[PathBuf]) -> bool {
    let mut dirs: Vec<PathBuf> = std::env::var_os("JAVA_HOME").map(|h| Path::new(&h).join("bin")).into_iter().collect();
    dirs.extend_from_slice(path);
    let java_home = Path::new("/usr/libexec/java_home");
    find(&dirs, "java").is_some_and(|j| j != Path::new("/usr/bin/java"))
        || java_home.exists()
            && std::process::Command::new(java_home)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
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
const MUTATION: &[&str] = &["--mutants", "cargo mutants", "stryker", "pitest", "gremlins", "muter run"];

/// A line that mutates the whole codebase: `--all-mutants`, or a mutation
/// tool run without slimfit and without a diff to limit it.
fn whole_codebase(line: &str) -> bool {
    line.contains("--all-mutants")
        || (!line.contains("slimfit") && !line.contains("--in-diff") && MUTATION[1..].iter().any(|m| line.contains(m)))
}

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
            Ok(c) => {
                out.extend(no_target(repo, &c));
                c.suites.iter().map(|s| s.resolve()).collect()
            }
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
            Ok(_) if repo.join(ratchet::FILE).exists() => {}
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

/// The commands a factorium gate or idle step runs, by where they are
/// written: each `[gate]` entry and `[idle]` step of `factorium.toml`, and the
/// lines of each repository script it names (not the scripts those name).
fn gates(repo: &Path) -> Vec<Finding> {
    let Ok(text) = std::fs::read_to_string(repo.join("factorium.toml")) else { return Vec::new() };
    let Ok(t) = toml::from_str::<toml::Table>(&text) else { return Vec::new() };
    let mut out = Vec::new();
    let steps = t.get("idle").and_then(|i| i.get("steps")).and_then(|s| s.as_array()).cloned().unwrap_or_default();
    for (i, step) in steps.iter().filter_map(|s| s.as_str()).enumerate() {
        let lines = run_by(repo, &format!("factorium.toml [idle] steps[{i}]"), step);
        out.extend(mutation(&lines, "as idle work, which commits nothing, so what it measures is never recorded"));
    }
    let Some(toml::Value::Table(gate)) = t.get("gate") else { return out };
    for (key, entry) in gate {
        let Some(entry) = entry.as_str() else { continue };
        let at = format!("factorium.toml [gate] {key}");
        let lines = run_by(repo, &at, entry);
        out.extend(mutation(&lines, &format!("in the `{key}` gate, holding up every landing")));
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

/// The lines `entry` (written at `at`) runs, each with where it is written:
/// itself, and the lines of each repository script it names (not the
/// scripts those name), comments left out.
fn run_by(repo: &Path, at: &str, entry: &str) -> Vec<(String, String)> {
    let mut lines = vec![(at.to_string(), entry.to_string())];
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
    lines
}

/// Where mutants run instead of a gate or idle work.
const RUN_ON_THE_CHANGE: &str = "the agent that made a change runs `slimfit check --mutants` on its changed lines, at \
     background priority (in factorium, `factorium worker exclusive --background -- slimfit check --mutants`), then \
     `slimfit ratchet`, and commits the baseline with the change";

/// A finding for each of `lines` (run `how`, in a gate or idle work) that
/// runs mutants: one that mutates the whole codebase is named as that.
fn mutation(lines: &[(String, String)], how: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (place, line) in lines {
        let fix = format!("take `{line}` out of {place}; {RUN_ON_THE_CHANGE}");
        if whole_codebase(line) {
            let what = format!("{place} mutates the whole codebase: `{line}`");
            out.push(finding(Kind::WholeCodebaseMutants, None, what, fix));
        } else if MUTATION.iter().any(|m| line.contains(m)) {
            let what = format!("{place} runs mutation testing {how}: `{line}`");
            out.push(finding(Kind::MutantsInGate, None, what, fix));
        }
    }
    out
}

/// A finding when a suite runs mutants but no branch to land on is found.
fn no_target(repo: &Path, cfg: &config::Config) -> Vec<Finding> {
    let mutates = cfg.suites.iter().any(|s| s.resolve().mutates());
    let target = cfg.check.target.as_deref().unwrap_or(crate::landing::DEFAULT_TARGET);
    // Outside git (a bare directory) there is no branch to ask about.
    if !mutates || !repo.join(".git").exists() || crate::git::merge_base(repo, target).is_some() {
        return vec![];
    }
    let what = match &cfg.check.target {
        Some(t) => format!("slimfit.toml's [check] target {t} shares no commit with HEAD"),
        None => format!("slimfit.toml sets no [check] target and there is no {target}"),
    };
    vec![finding(
        Kind::NoTarget,
        None,
        format!("{what}: `check` cannot hold the change's lines to a mutants measurement, and `--mutants` refuses"),
        "set `target` under [check] in slimfit.toml to the branch changes land on (e.g. \"origin/main\"), or \
         `git remote set-head origin --auto`"
            .into(),
    )]
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
    use crate::measure::t::git;
    use crate::model::Metrics;
    use crate::ratchet::Baseline;

    /// A directory with a Rust suite, a baseline and `files` as written.
    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let empty = ratchet::render(&Baseline::default()).unwrap();
        let mut all = vec![(config::FILE, "[[suite]]\nname = \"rust\"\npreset = \"rust\"\n")];
        if !files.iter().any(|(f, _)| *f == ratchet::FILE) {
            all.push((ratchet::FILE, &empty));
        }
        all.extend_from_slice(files);
        for (f, text) in all {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
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

    fn measured() -> String {
        let b = Baseline { suites: [("rust".to_string(), Metrics::default())].into(), ..Baseline::default() };
        serde_json::to_string(&b).unwrap()
    }

    /// A set-up repository on an idle host with its tools: nothing to fix.
    /// slimfit's own setup is one such.
    #[test]
    fn a_sound_setup_has_no_findings() {
        let r = repo(&[(ratchet::FILE, &measured())]);
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
        let r = repo(&[(ratchet::FILE, "not json")]);
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::NoBaseline]);
        assert!(f[0].what.contains(ratchet::FILE) && f[0].fix.contains("restore"), "{f:#?}");
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

    /// Mutants in a gate, written in its entry or its script, or in an
    /// `[idle]` step: each goes to the agent's own change.
    #[test]
    fn mutants_in_a_gate_or_idle_work_go_to_the_change() {
        let r = repo(&[
            (
                "factorium.toml",
                "[gate]\nfast = \"slimfit check --mutants\"\nslow = \"sh slow.sh\"\n[idle]\nsteps = [\"slimfit check --mutants\"]\n",
            ),
            ("slow.sh", "cargo mutants --in-diff d\n"),
        ]);
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::MutantsInGate; 3], "{f:#?}");
        assert!(f[0].what.starts_with("factorium.toml [idle] steps[0] runs mutation testing as idle work"), "{f:#?}");
        assert!(f[1].what.contains("[gate] fast") && f[2].what.contains("slow.sh:1"), "{f:#?}");
        let fix = "--background -- slimfit check --mutants`), then `slimfit ratchet`";
        assert!(f.iter().all(|f| f.fix.contains(fix)), "{f:#?}");
    }

    /// `--all-mutants`, or a mutation tool run on its own without a diff,
    /// mutates the whole codebase: found in idle work and in a gate's script.
    #[test]
    fn a_whole_codebase_mutants_run_is_replaced_by_the_changes() {
        let r = repo(&[
            (
                "factorium.toml",
                "[gate]\nslow = \"sh slow.sh\"\n[idle]\nsteps = [\"slimfit next --all-mutants\", \"slimfit check --mutants\"]\n",
            ),
            ("slow.sh", "cargo mutants --workspace\n"),
        ]);
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::WholeCodebaseMutants, Kind::MutantsInGate, Kind::WholeCodebaseMutants]);
        assert!(f[0].what.starts_with("factorium.toml [idle] steps[0] mutates the whole codebase"), "{f:#?}");
        assert!(f[2].what.starts_with("slow.sh:1") && f[2].fix.contains("`slimfit check --mutants`"), "{f:#?}");
    }

    /// No branch to land on: no change's lines are held to a measurement.
    /// A target set in slimfit.toml is the one looked for.
    #[test]
    fn a_repository_with_no_target_is_found() {
        let r = repo(&[(ratchet::FILE, &measured())]);
        for args in [&["init", "-q"][..], &["add", "-A"], &["commit", "-qm", "setup"]] {
            git(r.path(), args);
        }
        let f = diagnose(r.path(), &host(&every_tool()));
        assert_eq!(kinds(&f), vec![Kind::NoTarget], "{f:#?}");
        assert!(f[0].what.contains("no [check] target and there is no origin/HEAD"), "{f:#?}");
        assert!(f[0].fix.starts_with("set `target` under [check]"), "{f:#?}");
        git(r.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(r.path(), &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
        assert_eq!(diagnose(r.path(), &host(&every_tool())), vec![]);
        let toml = "[[suite]]\nname = \"rust\"\npreset = \"rust\"\n[check]\ntarget = \"HEAD\"\n";
        std::fs::write(r.path().join(config::FILE), toml).unwrap();
        assert_eq!(diagnose(r.path(), &host(&every_tool())), vec![]);
        std::fs::write(r.path().join(config::FILE), toml.replace("HEAD", "nope")).unwrap();
        let f = diagnose(r.path(), &host(&every_tool()));
        assert!(f[0].what.starts_with("slimfit.toml's [check] target nope shares no commit"), "{f:#?}");
    }

    /// This host: its cores, PATH and cargo's bin directory searched; a
    /// `java` on the path is a JDK.
    #[test]
    fn the_host_is_read_from_the_environment() {
        let h = Host::current();
        assert!(h.cores >= 1 && h.load >= 0.0, "{h:?}");
        assert!(h.path.last().is_some_and(|d| d.ends_with("bin")), "{h:?}");
        let bin = tempfile::tempdir().unwrap();
        install(bin.path(), "java");
        assert!(jdk(&[bin.path().into()]));
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
