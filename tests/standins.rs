//! The real slimfit binary on a git copy of each sample in `testdata/`,
//! its tools replaced by the stand-ins of `examples/standin/` placed first
//! on PATH (nothing else is: a tool left out is missing). Each preset's
//! command lines must reach its tools as they take them, and its reports
//! must come back as measurements; a tool that is missing, fails, or
//! writes no report must reach the agent as an error that names it.

#[allow(dead_code)]
#[path = "../examples/standin/state.rs"]
mod state;

use serde_json::Value;
use state::{Fault, State};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What a sample's preset runs: the stand-ins its test command and its
/// mutants command call, and the steps (tool and subcommand) they record.
struct Lang {
    name: &'static str,
    test_tools: &'static [&'static str],
    test_steps: &'static [&'static str],
    mutants_tool: &'static str,
    mutants_step: &'static str,
    source: &'static str,
}

const RUST: Lang = Lang {
    name: "rust",
    test_tools: &["cargo"],
    test_steps: &["cargo llvm-cov"],
    mutants_tool: "cargo",
    mutants_step: "cargo mutants",
    source: "src/lib.rs",
};
const GO: Lang = Lang {
    name: "go",
    test_tools: &["go"],
    test_steps: &["go test"],
    mutants_tool: "gremlins",
    mutants_step: "gremlins unleash",
    source: "calc.go",
};
const TYPESCRIPT: Lang = Lang {
    name: "typescript",
    test_tools: &["npx"],
    test_steps: &["npx vitest"],
    mutants_tool: "npx",
    mutants_step: "npx stryker",
    source: "src/calc.ts",
};
const KOTLIN: Lang = Lang {
    name: "kotlin",
    test_tools: &["gradlew"],
    test_steps: &["gradlew test"],
    mutants_tool: "gradlew",
    mutants_step: "gradlew pitest",
    source: "src/main/kotlin/com/example/Calc.kt",
};
const SWIFT: Lang = Lang {
    name: "swift",
    test_tools: &["swift", "xcrun"],
    test_steps: &["swift test", "xcrun llvm-cov"],
    mutants_tool: "muter",
    mutants_step: "muter run",
    source: "Sources/Calc/Calc.swift",
};

/// The tools the stand-in binary answers to, and what slimfit and the
/// presets' shell lines need from the host.
const STANDINS: &[&str] = &["cargo", "go", "gremlins", "swift", "xcrun", "muter", "npx"];
const HOST: &[&str] = &["sh", "git", "find", "head", "basename", "mv"];

struct Sample {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    bin: PathBuf,
    state: PathBuf,
    /// Where a tool's stand-in is linked: on PATH, or in the repository for `./gradlew`.
    links: Vec<(&'static str, PathBuf)>,
}

fn standin() -> PathBuf {
    let p = Path::new(env!("CARGO_BIN_EXE_slimfit")).parent().unwrap().join("examples/standin");
    assert!(p.exists(), "no stand-in at {}: `cargo build --examples`", p.display());
    p
}

fn on_path(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} is not on PATH"))
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let name = e.file_name();
        if ["reports", "node_modules", ".build", "build", "target", ".gradle", ".out"]
            .contains(&&*name.to_string_lossy())
        {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst.join(&name));
        } else {
            std::fs::copy(e.path(), dst.join(&name)).unwrap();
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@slimfit", "-c", "user.name=t"])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

impl Sample {
    /// A committed copy of `testdata/<lang>`, every stand-in linked.
    fn new(lang: &Lang) -> Sample {
        let testdata = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata").join(lang.name);
        let tmp = tempfile::tempdir().unwrap();
        let (repo, bin, state) = (tmp.path().join("repo"), tmp.path().join("bin"), tmp.path().join("state.json"));
        copy_tree(&testdata, &repo);
        std::fs::create_dir_all(&bin).unwrap();
        for h in HOST {
            std::os::unix::fs::symlink(on_path(h), bin.join(h)).unwrap();
        }
        let mut links: Vec<_> = STANDINS.iter().map(|t| (*t, bin.join(t))).collect();
        links.push(("gradlew", repo.join("gradlew")));
        for (_, l) in &links {
            let _ = std::fs::remove_file(l);
            std::os::unix::fs::symlink(standin(), l).unwrap();
        }
        State { reports: testdata.join("reports"), ..State::default() }.save(&state);
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "sample"]);
        Sample { _tmp: tmp, repo, bin, state, links }
    }

    /// slimfit's exit status, its JSON, and its stderr.
    fn slimfit(&self, args: &[&str]) -> (i32, Value, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_slimfit"))
            .args(args)
            .current_dir(&self.repo)
            .env("PATH", &self.bin)
            .env(state::ENV, &self.state)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("slimfit {args:?}: {e}; stdout {:?}; stderr {stderr}", out.stdout));
        (out.status.code().unwrap(), json, stderr)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, json, stderr) = self.slimfit(args);
        assert_eq!(code, 0, "slimfit {args:?}: {json:#}\n{stderr}");
        json
    }

    /// slimfit's error, which must name each word in `names`.
    fn error(&self, args: &[&str], names: &[&str]) -> String {
        let (code, json, stderr) = self.slimfit(args);
        assert_eq!(code, 2, "slimfit {args:?}: {json:#}\n{stderr}");
        let e = json["error"].as_str().unwrap().to_string();
        for n in names {
            assert!(e.contains(n), "slimfit {args:?}: the error does not name {n:?}: {e}");
        }
        e
    }

    fn state(&self) -> State {
        State::load(&self.state)
    }

    /// Recorded calls since the last `take`, by step.
    fn take(&self) -> Vec<state::Call> {
        let mut s = self.state();
        let calls = std::mem::take(&mut s.calls);
        s.save(&self.state);
        calls
    }

    fn steps(&self) -> Vec<String> {
        self.take().into_iter().map(|c| c.step).collect()
    }

    fn fault(&self, step: &str, f: Option<Fault>) {
        let mut s = self.state();
        match f {
            Some(f) => s.faults.insert(step.into(), f),
            None => s.faults.remove(step),
        };
        s.save(&self.state);
    }

    fn unlink(&self, tool: &str) -> PathBuf {
        let l = self.links.iter().find(|(t, _)| *t == tool).unwrap().1.clone();
        std::fs::remove_file(&l).unwrap();
        l
    }

    fn relink(&self, link: &Path) {
        std::os::unix::fs::symlink(standin(), link).unwrap();
    }
}

/// `init --mutants` runs the preset's tools once each and measures what
/// they wrote; `next` and `check` then work from that measurement.
fn measures_through_its_tools(lang: &Lang) -> Sample {
    let s = Sample::new(lang);
    let init = s.ok(&["init", "--mutants"]);
    let mut want: Vec<&str> = lang.test_steps.to_vec();
    want.push(lang.mutants_step);
    assert_eq!(s.steps(), want, "{}", lang.name);
    let b = &init["baseline"][lang.name];
    assert!(b["tests"].as_u64() >= Some(4) && b["failures"] == 0, "{}: {b:#}", lang.name);
    assert!(b["uncovered_lines"].as_u64() > Some(0), "{}: {b:#}", lang.name);
    assert!(b["mutants"]["survived"].as_u64() > Some(0), "{}: {b:#}", lang.name);

    let next = s.ok(&["next", "--all", "--mutants"]);
    let tasks = next["tasks"].as_array().unwrap();
    let of = |k: &'static str| tasks.iter().filter(move |t| t["kind"] == k);
    assert!(of("uncovered").any(|t| t["files"][0] == lang.source), "{}: {next:#}", lang.name);
    assert!(of("surviving-mutant").next().is_some(), "{}: {next:#}", lang.name);
    s.ok(&["check"]);
    assert_eq!(s.steps(), Vec::<String>::new(), "{}: next and check reuse the measurement", lang.name);
    s
}

/// A test or mutants tool that is missing, exits non-zero, or writes no
/// report. A failing exit alone is no error: failing tests still write
/// their reports. No report is, and the error names the report and ends
/// with the tool's own words.
fn faults_reach_the_agent(s: &Sample, lang: &Lang) {
    const TESTS: &[&str] = &["check", "--fresh"];
    const MUTANTS: &[&str] = &["check", "--all-mutants", "--fresh"];
    let tools = lang.test_tools.iter().map(|t| (*t, TESTS));
    for (tool, args) in tools.chain([(lang.mutants_tool, MUTANTS)]) {
        let link = s.unlink(tool);
        s.error(args, &["no report at", tool]);
        s.relink(&link);
        s.take();
    }
    let steps = lang.test_steps.iter().map(|t| (*t, TESTS));
    for (step, args) in steps.chain([(lang.mutants_step, MUTANTS)]) {
        s.fault(step, Some(Fault { exit: 3, no_report: false }));
        let (code, json, stderr) = s.slimfit(args);
        assert!(code < 2, "{}: {step} exiting 3 with its report: {json:#}\n{stderr}", lang.name);
        // The command's status is its last tool's, or the one it keeps.
        if step == lang.test_steps[0] || step == lang.mutants_step {
            assert!(stderr.contains("exited exit status: 3"), "{}: {step}: {stderr}", lang.name);
        }
        s.fault(step, Some(Fault { exit: 0, no_report: true }));
        s.error(args, &["no report at", &format!("standin {step}: writing no report")]);
        s.fault(step, None);
        s.take();
    }
}

#[test]
fn rust() {
    let s = measures_through_its_tools(&RUST);
    // Killers come from nextest's logs: the redundant tests are found.
    let next = s.ok(&["next", "--all", "--all-mutants", "--kind", "redundant-test"]);
    let mut redundant: Vec<&str> =
        next["tasks"].as_array().unwrap().iter().map(|t| t["evidence"]["test"].as_str().unwrap()).collect();
    redundant.sort();
    assert!(
        redundant == ["calc::tests::adds", "calc::tests::slow"]
            || redundant == ["calc::tests::adds_again", "calc::tests::slow"],
        "{next:#}"
    );
    // A changed file: cargo-mutants is limited to it.
    let lib = s.repo.join("src/lib.rs");
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap() + "// changed\n").unwrap();
    s.ok(&["check", "--mutants"]);
    let calls = s.take();
    let mutants = calls.iter().find(|c| c.step == "cargo mutants").unwrap();
    assert!(mutants.args.ends_with(&["--file".into(), "src/lib.rs".into()]), "{:?}", mutants.args);
    faults_reach_the_agent(&s, &RUST);
}

#[test]
fn go() {
    faults_reach_the_agent(&measures_through_its_tools(&GO), &GO);
}

#[test]
fn typescript() {
    faults_reach_the_agent(&measures_through_its_tools(&TYPESCRIPT), &TYPESCRIPT);
}

#[test]
fn kotlin() {
    faults_reach_the_agent(&measures_through_its_tools(&KOTLIN), &KOTLIN);
}

#[test]
fn swift() {
    faults_reach_the_agent(&measures_through_its_tools(&SWIFT), &SWIFT);
}

/// A command line the tool would not take is refused by its stand-in, so
/// a preset that drifts from its tool fails here, not on a user's host.
#[test]
fn a_command_line_the_tool_would_not_take_is_an_error() {
    let s = Sample::new(&GO);
    std::fs::write(
        s.repo.join("slimfit.toml"),
        "[[suite]]\nname = \"go\"\npreset = \"go\"\ntest_command = \"go test ./... > \\\"$SLIMFIT_OUT/test.json\\\"\"\n",
    )
    .unwrap();
    s.error(&["init"], &["no report at", "standin go test: no -json"]);
}
