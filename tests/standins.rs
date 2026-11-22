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
    /// The build command's steps, run before the test command's.
    build_steps: &'static [&'static str],
    test_steps: &'static [&'static str],
    /// The test step that runs the tests: failing, it still writes reports.
    runs_tests: &'static str,
    /// Steps after the test run that read each test's own profile, sorted:
    /// their report is optional (without it every mutant runs every test).
    reach_steps: &'static [&'static str],
    mutants_tool: &'static str,
    mutants_step: &'static str,
    source: &'static str,
    /// Where its mutants tool lists a change's mutants and runs them by
    /// name: an edit of `source` that the pick, one of its mutants, is on.
    mutates: Option<(Edit, &'static str)>,
}

/// An edit of a file's text.
type Edit = fn(&str) -> String;

const RUST: Lang = Lang {
    name: "rust",
    test_tools: &["cargo"],
    build_steps: &[],
    // The archive, then cargo's `nextest` subcommand runs `cargo-nextest`.
    test_steps: &["cargo llvm-cov", "cargo nextest", "cargo-nextest nextest"],
    runs_tests: "cargo-nextest nextest",
    // The whole run's coverage, then each test's.
    reach_steps: &[
        "llvm-cov export",
        "llvm-cov export",
        "llvm-cov export",
        "llvm-cov export",
        "llvm-cov export",
        "llvm-cov export",
        "llvm-cov export",
        "llvm-profdata merge",
        "llvm-profdata merge",
        "llvm-profdata merge",
        "llvm-profdata merge",
        "llvm-profdata merge",
        "llvm-profdata merge",
        "llvm-profdata merge",
    ],
    mutants_tool: "cargo",
    mutants_step: "cargo mutants",
    source: "src/lib.rs",
    mutates: Some((|s| s.to_string() + "pub fn a_change() {}\n", PICK_KILLED)),
};
const GO: Lang = Lang {
    name: "go",
    test_tools: &["go"],
    build_steps: &["go test"],
    test_steps: &["go test"],
    runs_tests: "go test",
    // Its package listed and its test binary built, which lists the tests
    // and runs each on its own.
    reach_steps: &[
        "1.test -test.list",
        "1.test -test.run",
        "1.test -test.run",
        "1.test -test.run",
        "1.test -test.run",
        "1.test -test.run",
        "1.test -test.run",
        "go list",
        "go test -c",
    ],
    mutants_tool: "gremlins",
    mutants_step: "gremlins unleash",
    source: "calc.go",
    mutates: None,
};
const TYPESCRIPT: Lang = Lang {
    name: "typescript",
    test_tools: &["npx"],
    build_steps: &[],
    test_steps: &["npx vitest"],
    runs_tests: "npx vitest",
    reach_steps: &[],
    mutants_tool: "npx",
    mutants_step: "npx stryker",
    source: "src/calc.ts",
    mutates: None,
};
const KOTLIN: Lang = Lang {
    name: "kotlin",
    test_tools: &["gradlew"],
    build_steps: &["gradlew test"],
    test_steps: &["gradlew test"],
    runs_tests: "gradlew test",
    reach_steps: &[],
    mutants_tool: "gradlew",
    mutants_step: "gradlew pitest",
    source: "src/main/kotlin/com/example/Calc.kt",
    mutates: None,
};
const SWIFT: Lang = Lang {
    name: "swift",
    test_tools: &["swift", "xcrun"],
    build_steps: &["swift build"],
    test_steps: &["swift test", "xcrun llvm-cov"],
    runs_tests: "swift test",
    reach_steps: &[],
    mutants_tool: "muter",
    mutants_step: "muter run",
    source: "Sources/Calc/Calc.swift",
    mutates: None,
};
const PYTHON: Lang = Lang {
    name: "python",
    test_tools: &["uv"],
    build_steps: &["uv true"],
    test_steps: &["uv pytest"],
    runs_tests: "uv pytest",
    // coverage.py's data, read for what each test ran.
    reach_steps: &["uv python -"],
    mutants_tool: "uv",
    mutants_step: "uv python python-mutants.py",
    source: "src/calc.py",
    mutates: Some((|s| s.replace("return a + b", "return a + b  # a change"), PYTHON_PICK)),
};

/// The tools the stand-in binary answers to, and what slimfit and the
/// presets' shell lines need from the host.
const STANDINS: &[&str] =
    &["cargo", "cargo-nextest", "llvm-profdata", "llvm-cov", "go", "gremlins", "swift", "xcrun", "muter", "npx", "uv"];
const HOST: &[&str] = &[
    "sh", "git", "find", "head", "basename", "mv", "cat", "sed", "grep", "xargs", "getconf", "cut", "sort", "mkdir",
    "nice",
];

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

/// The directories the tools write their builds and reports in, by name
/// (Stryker's `reports`; a sample keeps its real reports there too).
const BUILT: &[&str] = &["reports", "node_modules", ".build", "build", "target", ".gradle", ".out"];

/// Every file under `dir` by path, with its bytes (a symlink, its target),
/// except git's and the tools' builds (`BUILT`).
fn snapshot(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let (name, path, kind) = (e.file_name(), e.path(), e.file_type().unwrap());
        if name == ".git" || BUILT.contains(&&*name.to_string_lossy()) {
            continue;
        }
        if kind.is_symlink() {
            files.insert(path.clone(), std::fs::read_link(&path).unwrap().into_os_string().into_encoded_bytes());
        } else if kind.is_dir() {
            files.extend(snapshot(&path));
        } else {
            files.insert(path.clone(), std::fs::read(&path).unwrap());
        }
    }
    files
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let name = e.file_name();
        if BUILT.contains(&&*name.to_string_lossy()) {
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
        // The branch changes land on: what HEAD adds since is the change.
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
        Sample { _tmp: tmp, repo, bin, state, links }
    }

    /// slimfit's exit status, its JSON, and its stderr.
    fn slimfit(&self, args: &[&str]) -> (i32, Value, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_slimfit"))
            .args(args)
            .current_dir(&self.repo)
            .env("PATH", &self.bin)
            // cargo's own bin directory, where doctor looks for its subcommands: empty.
            .env("CARGO_HOME", self.bin.with_file_name("cargo-home"))
            // Where the Rust preset finds llvm-profdata and llvm-cov, as cargo llvm-cov does.
            .env("LLVM_TOOLS", &self.bin)
            .env(state::ENV, &self.state)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("slimfit {args:?}: {e}; stdout {:?}; stderr {stderr}", out.stdout));
        (out.status.code().unwrap(), json, stderr)
    }

    /// A dry run: it answers, as `would_write`, the files named in `files`
    /// and no others, and leaves the tree as it was, byte for byte.
    fn dry_run(&self, args: &[&str], files: &[&str]) -> Value {
        let before = snapshot(&self.repo);
        let answer = self.ok(args);
        let after = snapshot(&self.repo);
        let changed: Vec<_> = before.keys().chain(after.keys()).filter(|p| before.get(*p) != after.get(*p)).collect();
        assert!(changed.is_empty(), "slimfit {args:?} wrote {changed:?}");
        let would = answer["would_write"].as_object().unwrap_or_else(|| panic!("{answer:#}"));
        assert_eq!(would.keys().collect::<Vec<_>>(), files, "slimfit {args:?}");
        answer
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.repo.join(path)).unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, json, stderr) = self.slimfit(args);
        assert_eq!(code, 0, "slimfit {args:?}: {json:#}\n{stderr}");
        json
    }

    /// slimfit's error, its faults one per line, which must name each word
    /// in `names`; each fault carries a fix.
    fn error(&self, args: &[&str], names: &[&str]) -> String {
        let (code, json, stderr) = self.slimfit(args);
        assert_eq!(code, 2, "slimfit {args:?}: {json:#}\n{stderr}");
        let e = faults(&json);
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

/// Every sample in turn, in one test: one process. A freshly built binary's
/// first run waits while macOS checks it (0.17-0.34s at load ~20), and every
/// process that runs it meanwhile waits too; a test per sample, all started
/// at once, each counted that wait (slimfit#27). The Rust sample also serves
/// `doctor` before `init`, and every command's printed keys.
#[test]
fn every_preset_through_its_standins() {
    let s = Sample::new(&RUST);
    let doctor = doctor_names_each_fix_and_changes_nothing(&s);
    let mut printed = measures_through_its_tools(&s, &RUST, true);
    printed.push(("doctor", doctor));
    rust(&s, &mut printed);
    every_printed_key_is_on_its_page(&printed);
    faults_reach_the_agent(&s, &RUST);
    rust_test_args();
    rust_runs_every_test_whatever_fails();

    go();
    python();
    for lang in [&TYPESCRIPT, &KOTLIN, &SWIFT] {
        let s = Sample::new(lang);
        measures_through_its_tools(&s, lang, false);
        faults_reach_the_agent(&s, lang);
    }
}

/// `doctor` reads and writes nothing: on a fresh sample it says to run
/// `init` and names the tools the detected suite needs that are not on
/// PATH (the stand-ins are `cargo` and `cargo-nextest`), and exits 0. The load is the
/// host's own, so a `host_busy` finding is left out.
fn doctor_names_each_fix_and_changes_nothing(s: &Sample) -> Value {
    let status = || {
        let out =
            Command::new("git").args(["status", "--porcelain", "--ignored"]).current_dir(&s.repo).output().unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    let before = status();
    let found = |doctor: &Value| -> Vec<(String, String)> {
        doctor["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["kind"] != "host_busy")
            .map(|f| {
                (f["kind"].as_str().unwrap().into(), f["what"].as_str().unwrap().split(' ').next().unwrap().into())
            })
            .collect()
    };
    let doctor = s.ok(&["doctor"]);
    let tool = |exe: &str| ("tool_missing".to_string(), exe.to_string());
    let want = vec![("no_config".to_string(), "no".to_string()), tool("cargo-llvm-cov"), tool("cargo-mutants")];
    assert_eq!(found(&doctor), want, "{doctor:#}");
    assert!(doctor["next"].as_str().unwrap().contains("slimfit init"), "{doctor:#}");
    assert!(doctor["host"]["cores"].as_u64() >= Some(1), "{doctor:#}");
    assert_eq!(status(), before, "doctor changed the working tree");
    assert_eq!(s.steps(), Vec::<String>::new(), "doctor ran a tool");
    doctor
}

/// `init` runs the preset's test tools once each, and no mutant; `next`
/// and `check` then work from that measurement. A preset whose mutants tool
/// cannot list a change's mutants and run them by name refuses `--mutants` and runs nothing
/// (Rust's, given the change's diff, is driven in `rust`). With
/// `dry`, `init` and `ratchet` are each asked first with `--dry-run`, which
/// writes nothing and answers what the real run then writes (on one
/// sample: what a dry run writes does not depend on the language). What
/// each command printed, by command.
fn measures_through_its_tools(s: &Sample, lang: &Lang, dry: bool) -> Vec<(&'static str, Value)> {
    let mut printed = Vec::new();
    if dry {
        let files = [".gitignore", ".slimfit/baseline.json", "slimfit.toml"];
        printed.push(("init", s.dry_run(&["init", "--dry-run"], &files)));
        s.take();
    }
    let init = s.ok(&["init"]);
    // The test run's steps in order; its tests' profiles are read in parallel, in any order.
    let mut steps = s.steps();
    steps[lang.build_steps.len() + lang.test_steps.len()..].sort();
    assert_eq!(steps, [lang.build_steps, lang.test_steps, lang.reach_steps].concat(), "{}", lang.name);
    // Rust's runner and Go's runs of each test record each test's CPU
    // time, as many as ran; the others' test commands record none.
    let records = std::fs::read_dir(s.repo.join(".slimfit/out").join(lang.name).join("test-cpu")).unwrap().count();
    assert_eq!(records, if ["rust", "go"].contains(&lang.name) { 6 } else { 0 }, "{}", lang.name);
    let base = s.read(".slimfit/baseline.json");
    if let Some((_, dry)) = printed.first() {
        // What the dry run said it would write, but for `test_seconds` (CPU
        // time differs from run to run).
        let would = &dry["would_write"];
        for f in [".gitignore", "slimfit.toml"] {
            assert_eq!(s.read(f), would[f], "{}: {f}", lang.name);
        }
        let held = |text: &str| {
            let mut b: Value = serde_json::from_str(text).unwrap();
            b["suites"][lang.name]["test_seconds"] = Value::Null;
            b["measured"][lang.name]["test_seconds"] = Value::Null;
            b
        };
        assert_eq!(held(&base), held(would[".slimfit/baseline.json"].as_str().unwrap()), "{}", lang.name);
    }
    let b = &init["baseline"][lang.name];
    assert!(b["tests"].as_u64() >= Some(4) && b["failures"] == 0, "{}: {b:#}", lang.name);
    assert!(b["uncovered_lines"].as_u64() > Some(0) && b.get("mutants").is_none(), "{}: {b:#}", lang.name);

    let next = s.ok(&["next", "--all"]);
    let tasks = next["tasks"].as_array().unwrap();
    assert!(tasks.iter().any(|t| t["kind"] == "uncovered" && t["files"][0] == lang.source), "{}: {next:#}", lang.name);
    if lang.mutates.is_none() {
        s.error(&["check", "--mutants"], &["cannot list a change's mutants", "never mutates the whole codebase"]);
    }
    let check = s.ok(&["check"]);
    if dry {
        printed.extend(dry_ratchet(s, lang, &base));
    }
    assert_eq!(s.steps(), Vec::<String>::new(), "{}: the commands after init reuse its measurement", lang.name);
    printed.extend([("init", init), ("next", next), ("check", check)]);
    printed
}

/// A bound with room to tighten: `ratchet --dry-run` answers exactly the
/// baseline `ratchet` then writes, and nothing once it is written.
fn dry_ratchet(s: &Sample, lang: &Lang, base: &str) -> Vec<(&'static str, Value)> {
    let b: Value = serde_json::from_str(base).unwrap();
    let mut looser = b.clone();
    looser["suites"][lang.name]["uncovered_lines"] =
        (b["suites"][lang.name]["uncovered_lines"].as_u64().unwrap() + 5).into();
    std::fs::write(s.repo.join(".slimfit/baseline.json"), serde_json::to_string_pretty(&looser).unwrap()).unwrap();
    let would = s.dry_run(&["ratchet", "--dry-run"], &[".slimfit/baseline.json"]);
    let ratchet = s.ok(&["ratchet"]);
    assert_eq!(s.read(".slimfit/baseline.json"), would["would_write"][".slimfit/baseline.json"], "{}", lang.name);
    assert_eq!(s.read(".slimfit/baseline.json"), base, "{}", lang.name);
    let none = s.dry_run(&["ratchet", "--dry-run"], &[]);
    vec![("ratchet", would), ("ratchet", ratchet), ("ratchet", none)]
}

/// An error's faults, one per line, each followed by its fix; a fault
/// with no fix fails.
fn faults(json: &Value) -> String {
    let faults = json["error"].as_array().unwrap_or_else(|| panic!("no list of faults: {json:#}"));
    faults
        .iter()
        .map(|f| {
            let (fault, fix) = (f["fault"].as_str().unwrap(), f["fix"].as_str().unwrap_or_default());
            assert!(!fix.is_empty(), "a fault with no fix: {f:#}");
            format!("{fault}\nfix: {fix}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A test or mutants tool that is missing, exits non-zero, or writes no
/// report. A failing exit alone is no error: failing tests still write
/// their reports (a mutants tool's exit is covered in `rust`). No report
/// is, and the error names the report and ends with the tool's own words.
/// The mutants tool runs only on a change, and only where it can list its
/// mutants and run them by name (Rust's and Python's); its listing runs
/// first, and fails only with a failing exit.
fn faults_reach_the_agent(s: &Sample, lang: &Lang) {
    let mutates = lang.mutates.is_some();
    let mut tools = lang.test_tools.to_vec();
    let args: &[&str] = if let Some((change, pick)) = lang.mutates {
        let source = s.repo.join(lang.source);
        std::fs::write(&source, change(&std::fs::read_to_string(&source).unwrap())).unwrap();
        // A mutants tool that also runs the tests is missing for them first.
        if !tools.contains(&lang.mutants_tool) {
            tools.push(lang.mutants_tool);
        }
        &["check", "--mutants", pick, "--why", "w", "--fresh"]
    } else {
        &["next", "--fresh"]
    };
    let missing = if mutates { "listed no mutants of the change" } else { "_report at" };
    for tool in tools {
        let link = s.unlink(tool);
        s.error(args, &[missing, "fix: fix the command's failure in its log", tool]);
        s.relink(&link);
    }
    let no_report = Some(Fault { exit: 0, no_report: true, ..Fault::default() });
    let (first, mutants) = (lang.runs_tests, lang.mutants_step);
    s.fault(first, Some(Fault { exit: 3, ..Fault::default() }));
    if mutates {
        // A listing with no report and exit 0 is cargo-mutants' empty one
        // (`rust`): the listing that fails also exits non-zero.
        s.fault(mutants, Some(Fault { exit: 1, no_report: true, ..Fault::default() }));
    }
    let (code, json, stderr) = s.slimfit(args);
    if mutates {
        // The listing runs first: the tool's own words reach the agent, and nothing is measured.
        assert_eq!(code, 2, "{}: {json:#}\n{stderr}", lang.name);
        assert!(faults(&json).contains(&format!("standin {mutants}: writing no report")), "{json:#}");
        s.fault(mutants, None);
    }
    let (code, json, stderr) = s.slimfit(args);
    assert!(stderr.contains("test command exited exit status: 3"), "{}: {first}: {stderr}", lang.name);
    assert_eq!(code, 0, "{}: {json:#}\n{stderr}", lang.name);
    s.fault(first, None);
    s.fault(mutants, None);
    // cargo's `nextest` writes nothing: `cargo-nextest` does.
    for step in lang.test_steps.iter().filter(|s| **s != "cargo nextest") {
        s.fault(step, no_report);
        s.error(
            args,
            &["_report at", "fix: fix the command's failure in its log", &format!("standin {step}: writing no report")],
        );
        s.fault(step, None);
    }
}

/// Three picks of the Rust sample's real list, one per function: caught,
/// missed, caught.
const PICK_KILLED: &str = "src/lib.rs:2:7: replace + with - in add";
const PICK_MISSED: &str = "src/lib.rs:6:5: replace unused -> i64 with 0";
const PICK_ADULT: &str = "src/lib.rs:12:9: replace >= with < in is_adult";

/// The Rust sample, measured, in the states that make each command print
/// its optional keys: violations, improvements, loosenings, the change's
/// candidates, its picks and their record. A changed line gives
/// cargo-mutants the change's diff; `--all-mutants` is refused.
fn rust(s: &Sample, printed: &mut Vec<(&'static str, Value)>) {
    let mut run = |cmd: &'static str, args: &[&str], code: i32| -> Value {
        let mut all = vec![cmd];
        all.extend(args);
        let (got, json, stderr) = s.slimfit(&all);
        assert_eq!(got, code, "slimfit {all:?}: {json:#}\n{stderr}");
        printed.push((cmd, json.clone()));
        json
    };
    let baseline = s.repo.join(".slimfit/baseline.json");
    let edit = |f: &dyn Fn(&mut Value)| {
        let mut b: Value = serde_json::from_str(&std::fs::read_to_string(&baseline).unwrap()).unwrap();
        f(&mut b["suites"]["rust"]);
        std::fs::write(&baseline, serde_json::to_string(&b).unwrap()).unwrap();
    };

    run("next", &[], 0);
    // Redundant tests are proposed from each test's lines: `adds`,
    // `adds_again` and `slow` run the same lines, so each is proposed but
    // the one that measured least CPU, which is noise: which one that is,
    // the test does not ask. Kept, with a reason, it is not proposed again.
    let proposed = |next: &Value| -> Vec<String> {
        let tasks = next["tasks"].as_array().unwrap().iter();
        let candidates = tasks.flat_map(|t| t["evidence"]["candidates"].as_array().unwrap().iter());
        candidates.map(|c| c["test"].as_str().unwrap().to_string()).collect()
    };
    let all = run("next", &["--all", "--kind", "redundant-test"], 0);
    let before = proposed(&all);
    // One file holds them: one task, its candidates counted.
    assert_eq!((all["counts"]["redundant-test"].clone(), all["candidates"].clone()), (1.into(), 2.into()), "{all:#}");
    let twins = ["calc::tests::adds", "calc::tests::adds_again", "calc::tests::slow"];
    assert!(before.len() == 2 && before.iter().all(|t| twins.contains(&t.as_str())), "{before:?}");
    let twin = before[0].as_str();
    s.error(&["keep", "calc::tests::slow"], &["keep without --reason"]);
    s.error(
        &["keep", "calc::tests::gone", "--reason", "r"],
        &["no suite's test run recorded a test calc::tests::gone"],
    );
    let kept = run("keep", &[twin, "--reason", "it checks its own case"], 0);
    assert_eq!(kept["kept"][0]["test"], twin, "{kept:#}");
    let after = proposed(&run("next", &["--all", "--kind", "redundant-test"], 0));
    assert_eq!(after, before.iter().filter(|t| *t != twin).cloned().collect::<Vec<_>>());
    // A source file is kept as a deepen verdict, with its size and churn, measuring nothing.
    let file = run("keep", &["src/lib.rs", "--reason", "deep enough"], 0);
    assert_eq!(file["kept_files"][0]["file"], "src/lib.rs", "{file:#}");
    assert_eq!(file["kept"][0]["test"], twin, "{file:#}");
    assert_eq!(s.steps(), Vec::<String>::new(), "keep and next reuse the measurement");
    let gone = s.error(&["next", "--all-mutants"], &["`--all-mutants` is gone"]);
    assert!(gone.contains("fix: measure the change's mutants instead: `slimfit check --mutants`"), "{gone}");
    // One way to run mutants: `next` and `ratchet` reuse `check --mutants`.
    for cmd in ["next", "ratchet"] {
        s.error(&[cmd, "--mutants"], &["takes no `--mutants`", "fix: run `slimfit check --mutants`"]);
    }
    run("report", &["--points", "1"], 0);
    // Better than the baseline: improved.
    edit(&|b| b["uncovered_lines"] = (b["uncovered_lines"].as_u64().unwrap() + 100).into());
    run("check", &[], 0);
    // Worse: a trend, not a violation; the ratchet keeps the lower count.
    edit(&|b| b["uncovered_lines"] = 0.into());
    let worse = run("check", &[], 0);
    assert_eq!(worse["verdict"]["uncovered_lines"][0]["baseline"], 0.0, "{worse:#}");
    s.error(&["ratchet", "--loosen", "uncovered_lines", "--reason", "test"], &["never loosened"]);
    run("ratchet", &[], 0);
    assert_eq!(
        serde_json::from_str::<Value>(&s.read(".slimfit/baseline.json")).unwrap()["suites"]["rust"]["uncovered_lines"],
        0
    );
    // A changed line whose mutants survive: cargo-mutants is given the
    // change's diff; the survivors are accepted with a reason. Each mutant
    // runs the tests that ran its function: cargo-mutants lists the
    // mutants, and with a real nextest to hand on to (the stand-in, as
    // `cargo-nextest`), slimfit writes the plan and its shim.
    // Lines of `add`, `unused` and `is_adult` changed: cargo-mutants lists
    // only the mutants whose span touches one.
    // A nested crate's example grows: the preset counts it as a test, not
    // source, so the change adds no source line, no growth, no candidate.
    let nested = |path: &str| {
        let f = s.repo.join(path);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "fn main() {\n    if 1 == 1 {}\n}\n").unwrap();
    };
    nested("crates/e/examples/e.rs");
    let example = run("check", &[], 0);
    assert_eq!((&example["landing"]["lines"], &example["landing"]["growth"]), (&0.into(), &0.into()), "{example:#}");
    s.take();
    assert_eq!(run("check", &["--mutants"], 0)["candidates"], serde_json::json!({"rust": []}));
    assert!(!s.take().iter().any(|c| c.step == "cargo mutants"), "no source line: cargo-mutants not run");
    // A nested crate's source that cargo-mutants lists no mutant of ("No
    // mutants to filter", exit 0): nothing to pick, not a fault.
    std::fs::remove_dir_all(s.repo.join("crates")).unwrap();
    nested("crates/e/src/main.rs");
    let none = run("check", &["--mutants"], 0);
    assert_eq!(none["candidates"], serde_json::json!({"rust": []}), "{none:#}");
    std::fs::remove_dir_all(s.repo.join("crates")).unwrap();
    let lib = s.repo.join("src/lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap();
    let edit = |(i, l): (usize, &str)| {
        if [2, 6, 12].contains(&(i + 1)) { format!("{l} // changed\n") } else { format!("{l}\n") }
    };
    std::fs::write(&lib, text.lines().enumerate().map(edit).collect::<String>()).unwrap();
    // No record of its mutants: the plain check fails.
    let held = run("check", &[], 1);
    let v = &held["verdict"]["violations"][0];
    assert_eq!((&v["metric"], &held["landing"]["lines"]), (&"unrecorded_mutants".into(), &3.into()), "{held:#}");
    // The candidates: cargo-mutants lists the change's diff, and nothing builds.
    s.take();
    let listed = run("check", &["--mutants"], 0);
    assert_eq!(listed["candidates"]["rust"].as_array().unwrap().len(), 13, "{listed:#}");
    let calls = s.take();
    let built = |c: &&state::Call| c.step.contains("nextest") || c.step == "cargo llvm-cov" && c.args[1] != "show-env";
    assert!(!calls.iter().any(|c| built(&c)), "{calls:#?}");
    let mutants = calls.iter().filter(|c| c.step == "cargo mutants");
    assert!(mutants.clone().count() == 1 && mutants.clone().all(|c| c.args.contains(&"--list".into())), "{calls:#?}");
    let picked = ["--mutants", PICK_KILLED, PICK_MISSED, PICK_ADULT, "--why", "the sum", "the result", "the bound"];
    // No nextest to hand on to: every mutant runs every test, and stderr says why.
    let nextest = s.unlink("cargo-nextest");
    let (_, _, stderr) = s.slimfit(&[&["check"], &picked[..]].concat());
    assert!(
        stderr.contains("no cargo-nextest on PATH or in cargo's bin directory: every mutant runs every test"),
        "{stderr}"
    );
    let lists = |calls: &[state::Call]| calls.iter().filter(|c| c.args.contains(&"--list".to_string())).count();
    assert_eq!(lists(&s.take()), 1, "listed for a plan with no nextest");
    s.relink(&nextest);
    let check = run("check", &[&picked[..], &["--fresh"]].concat(), 1);
    let plan: Value = serde_json::from_str(&s.read(".slimfit/out/rust/shim/plan.json")).unwrap();
    let tests = |f: &Value| -> Vec<String> {
        let names =
            f["tests"].as_array().unwrap().iter().map(|i| plan["tests"][i.as_u64().unwrap() as usize]["name"].clone());
        names.map(|n| n.as_str().unwrap().to_string()).collect()
    };
    let functions = plan["functions"].as_array().unwrap();
    let mut add = tests(&functions[0]);
    add.sort();
    assert_eq!(
        (add, tests(&functions[1]), tests(&functions[2])),
        (
            vec!["tests::adds".to_string(), "tests::adds_again".into(), "tests::slow".into()],
            vec![],
            vec!["tests::adult".to_string()]
        ),
        "{plan:#}"
    );
    assert!(s.repo.join(".slimfit/out/rust/shim/cargo-nextest").exists());
    // `next` reads the survivors from that cached `check --mutants`: no mutant runs again.
    let next = run("next", &["--all"], 0);
    assert!(next["tasks"].as_array().unwrap().iter().any(|t| t["kind"] == "surviving-mutant"), "{next:#}");
    assert_eq!(check["verdict"]["violations"][0]["metric"], "survived_mutants", "{check:#}");
    let outcomes: Vec<&str> =
        check["mutated"]["rust"]["picks"].as_array().unwrap().iter().map(|p| p["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes, ["killed", "survived", "killed"], "{check:#}");
    let calls = s.take();
    // The candidates, then the plan's listing of the picks alone.
    assert_eq!(lists(&calls), 2, "{calls:?}");
    // cargo-mutants' build of each of the 3 picks reaches the real nextest
    // through the shim as it came; its test run, narrowed: `add`'s with its
    // 3 tests, `is_adult`'s with its one, each under its timeout; `unused`'s
    // runs none.
    // The test run's own nextest runs from its archive.
    let calls: Vec<_> = calls.into_iter().filter(|c| !c.args.contains(&"--archive-file".into())).collect();
    let runs = calls.iter().filter(|c| c.step == "cargo nextest").count() / 2;
    let builds = calls.iter().filter(|c| c.step == "cargo-nextest nextest" && c.args.contains(&"--no-run".into()));
    assert!(builds.clone().count() == 3 && builds.clone().all(|c| c.args.len() == 5), "{calls:#?}");
    let filters: Vec<&str> = calls
        .iter()
        .filter(|c| c.step == "cargo-nextest nextest" && !c.args.contains(&"--no-run".into()))
        .map(|c| {
            assert!(c.args.iter().any(|a| a.starts_with("slimfit:") && a.ends_with(".toml")), "{c:?}");
            c.args.iter().skip_while(|a| *a != "-E").nth(1).map(String::as_str).unwrap_or("")
        })
        .collect();
    // In the order the tests ran, as the reach report lists them.
    let adds = "(binary_id(=calc) & test(=tests::adds_again)) | (binary_id(=calc) & test(=tests::slow)) | \
                (binary_id(=calc) & test(=tests::adds))";
    let adult = "(binary_id(=calc) & test(=tests::adult))";
    assert_eq!((runs, filters), (3, vec![adds, adult]), "{calls:#?}");
    let mutants = calls.iter().rfind(|c| c.step == "cargo mutants").unwrap();
    // Each mutant builds as the test run built, into its target directory:
    // no cold first build (ADR 0007).
    assert!(calls.iter().any(|c| c.step == "cargo llvm-cov" && c.args[1] == "show-env"), "{calls:#?}");
    assert_eq!(mutants.target_dir, Some(mutants.dir.join("target/llvm-cov-target")), "{mutants:?}");
    let diff = mutants.args.iter().position(|a| a == "--in-diff").map(|i| &mutants.args[i + 1]);
    assert!(diff.is_some_and(|d| d.ends_with("change.diff")), "{:?}", mutants.args);
    assert_eq!(mutants.args.iter().filter(|a| *a == "-F").count(), 3, "{:?}", mutants.args);
    // The run's diff holds one line of each pick, the line it changes.
    let diff = std::fs::read_to_string(s.repo.join(".slimfit/out/rust/change.diff")).unwrap();
    let added: Vec<&str> = diff.lines().filter(|l| l.starts_with("@@ ")).collect();
    assert_eq!(added, ["@@ -1,0 +2,1 @@", "@@ -4,0 +6,1 @@", "@@ -9,0 +12,1 @@"], "{diff}");
    let ratchet =
        run("ratchet", &["--loosen", "survived_mutants", "--reason", "test", "--picker", "p", "--author", "a"], 0);
    let record = ratchet["record"].as_str().unwrap();
    assert!(record.starts_with(".slimfit/mutants/") && s.repo.join(record).exists(), "{ratchet:#}");
    run("report", &["--points", "1"], 0);
    sampled(s);
    // A second `fn slow` its path cannot tell apart: no text to keep it by.
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap() + "fn slow() {}\n").unwrap();
    s.error(&["keep", "calc::tests::slow", "--reason", "r"], &["calc::tests::slow's own function was not found"]);
}

/// A suite's `test_args` reach nextest in the test run and in each
/// mutant's, expanded as the commands run (a filterset from a file, which
/// a change to the file changes): only the tests it leaves run, under its
/// profile, whose JUnit report is read; and they run from copies of the
/// test binaries outside the build directory (which holds none).
fn rust_test_args() {
    let s = Sample::new(&RUST);
    let toml = "[[suite]]\nname = \"rust\"\npreset = \"rust\"\n\
                test_args = '--profile fast -E \"not ($(cat .config/slow-tests))\"'\n";
    std::fs::write(s.repo.join("slimfit.toml"), toml).unwrap();
    std::fs::create_dir_all(s.repo.join(".config")).unwrap();
    std::fs::write(s.repo.join(".config/nextest.toml"), "[profile.fast]\nslow-timeout = \"30s\"\n").unwrap();
    let slow = |filter: &str| std::fs::write(s.repo.join(".config/slow-tests"), filter).unwrap();
    slow("test(=tests::slow)");
    git(&s.repo, &["add", "-A"]);
    git(&s.repo, &["commit", "-qm", "fast tests"]);
    let ran = |s: &Sample| -> Vec<String> {
        let run = s.take().into_iter().find(|c| c.step == "cargo-nextest nextest").unwrap();
        let profile = run.args.iter().skip_while(|a| *a != "--profile").nth(1).cloned();
        assert_eq!(profile.as_deref(), Some("fast"), "{run:?}");
        let map = s.read(".slimfit/out/rust/tests.map");
        let copies = s.repo.join(".slimfit/out/rust/bin/target/debug/deps/calc-standin");
        assert!(map.lines().all(|l| l.ends_with(&*copies.to_string_lossy())), "{map}");
        map.lines().map(|l| l.split('\t').nth(2).unwrap().to_string()).collect()
    };
    let init = s.ok(&["init"]);
    assert_eq!(init["baseline"]["rust"]["tests"], 5, "{init:#}");
    assert_eq!(ran(&s), ["tests::negates", "tests::triples", "tests::adds_again", "tests::adult", "tests::adds"]);
    assert!(s.repo.join("target/nextest/fast/junit.xml").exists() && !s.repo.join("target/nextest/default").exists());
    assert!(!s.repo.join("target/debug").exists(), "the build directory holds a test binary");
    slow("test(=tests::adult)");
    let check = s.ok(&["check"]);
    assert_eq!(check["metrics"]["rust"]["tests"], 5, "{check:#}");
    assert_eq!(ran(&s), ["tests::negates", "tests::triples", "tests::adds_again", "tests::slow", "tests::adds"]);
    // A mutant of `add`, which `adds`, `adds_again` and `slow` run: only
    // those `test_args` leave run it, under its profile.
    slow("test(=tests::slow)");
    let lib = s.repo.join("src/lib.rs");
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap().replacen("a + b", "a + b // changed", 1)).unwrap();
    s.ok(&["check", "--mutants", PICK_KILLED, "--why", "the sum"]);
    let calls = s.take();
    let mutants = calls.iter().rfind(|c| c.step == "cargo mutants").unwrap();
    let given: Vec<&str> = mutants.args.iter().filter_map(|a| a.strip_prefix("--cargo-test-arg=")).collect();
    assert_eq!(given, ["--profile", "fast", "-E", "not (test(=tests::slow))"], "{mutants:?}");
    let run = calls.iter().rfind(|c| c.step == "cargo-nextest nextest" && !c.args.contains(&"--no-run".into()));
    let run = run.unwrap();
    assert_eq!(run.args.iter().filter(|a| *a == "-E").count(), 1, "{run:?}");
    let log = s.read(".slimfit/out/rust/mutants.log");
    let runs: Vec<&str> = log.lines().filter_map(|l| l.strip_prefix("standin nextest: runs calc ")).collect();
    assert_eq!(runs, ["tests::adds_again", "tests::adds"], "{log}");
}

/// A failing test stops none of the Rust test run (slimfit#79): every test
/// is measured, and what each ran, while a mutant's tests still stop at
/// its first failure. A suite's own `--no-fail-fast` is passed once; a
/// `--fail-fast` of its own leaves tests out, and that run is refused,
/// naming them.
fn rust_runs_every_test_whatever_fails() {
    let fails = Some(Fault { first_test_fails: true, ..Fault::default() });
    let measured = |test_args: &str| {
        let s = Sample::new(&RUST);
        let toml = format!("[[suite]]\nname = \"rust\"\npreset = \"rust\"\ntest_args = {test_args:?}\n");
        std::fs::write(s.repo.join("slimfit.toml"), toml).unwrap();
        git(&s.repo, &["add", "-A"]);
        git(&s.repo, &["commit", "-qm", "test_args"]);
        s.fault("cargo-nextest nextest", fails);
        s
    };
    let every = |s: &Sample, init: &Value| {
        assert_eq!(init["baseline"]["rust"]["tests"], 6, "{init:#}");
        let reach = s.read(".slimfit/out/rust/reach.txt");
        assert_eq!(reach.lines().filter(|l| l.starts_with("test ")).count(), 6, "{reach}");
        let run = s.take().into_iter().find(|c| c.step == "cargo-nextest nextest").unwrap();
        assert_eq!(run.args.iter().filter(|a| *a == "--no-fail-fast").count(), 1, "{run:?}");
    };
    let s = measured("");
    every(&s, &s.ok(&["init"]));
    // A mutant of `add`, which `adds_again`, `adds` and `slow` run: the
    // first fails, and the rest are not run.
    let lib = s.repo.join("src/lib.rs");
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap().replacen("a + b", "a + b // changed", 1)).unwrap();
    s.ok(&["check", "--mutants", PICK_KILLED, "--why", "the sum"]);
    let calls = s.take();
    let run = calls.iter().rfind(|c| c.step == "cargo-nextest nextest" && !c.args.contains(&"--no-run".into()));
    assert!(!run.unwrap().args.iter().any(|a| a == "--no-fail-fast"), "{run:?}");
    let log = s.read(".slimfit/out/rust/mutants.log");
    let runs: Vec<&str> = log.lines().filter_map(|l| l.strip_prefix("standin nextest: runs calc ")).collect();
    assert_eq!(runs, ["tests::adds_again"], "{log}");
    assert!(log.contains("warning: 2/3 tests were not run due to test failure"), "{log}");

    let s = measured("--no-fail-fast");
    every(&s, &s.ok(&["init"]));
    let s = measured("--fail-fast");
    s.error(&["init"], &["not run: calc tests::triples, calc tests::adds_again, calc tests::slow, calc tests::adult, calc tests::adds", "--fail-fast"]);
}

/// A sample of 2 draws 4 and runs them in one cargo-mutants run, which
/// builds none once 2 drawn before it have built; the sample is the
/// first 2 of the draw that built.
fn sampled(s: &Sample) {
    s.take();
    let (code, next, stderr) = s.slimfit(&["next", "--all", "--sample", "2"]);
    assert_eq!(code, 0, "{next:#}\n{stderr}");
    let calls = s.take();
    let runs: Vec<&state::Call> =
        calls.iter().filter(|c| c.step == "cargo mutants" && !c.args.contains(&"--list".into())).collect();
    assert_eq!(runs.len(), 1, "{calls:#?}");
    let drawn: Vec<&str> =
        runs[0].args.iter().skip(1).zip(&runs[0].args).filter(|(_, f)| *f == "-F").map(|(n, _)| n.as_str()).collect();
    assert_eq!(drawn.len(), 4, "{:?}", runs[0].args);
    // The shim hands a build on to the real nextest, or says why not; its log is cargo-mutants'.
    let built = calls.iter().filter(|c| c.step == "cargo-nextest nextest" && c.args.contains(&"--no-run".into()));
    let log = s.read(".slimfit/out/sample/rust/mutants.log");
    let not_built = log.matches("not built, and not in the sample").count();
    assert_eq!(log.matches("slimfit: drawn mutant ").count(), 4, "each build is found by its mutant: {log}");
    assert!((2..=4).contains(&built.clone().count()) && built.count() + not_built == 4, "{log}");
    let record = next["tasks"].as_array().unwrap().iter().find(|t| t["kind"] == "record-sample").unwrap();
    let sample: Value = serde_json::from_str(record["evidence"]["text"].as_str().unwrap()).unwrap();
    let kept: Vec<String> = sample["drawn"]["rust"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| format!("^{}$", d["name"].as_str().unwrap()))
        .collect();
    let escaped = |n: &&str| n.replace('\\', "");
    assert_eq!(kept, drawn[..2].iter().map(escaped).collect::<Vec<_>>(), "{sample:#}");
}

/// slimfit run as `cargo-nextest` outside a mutants run (no plan, no real
/// nextest named): it says so and fails, rather than pass a mutant untested.
#[test]
fn the_shim_without_its_plan_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let shim = tmp.path().join("cargo-nextest");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_slimfit"), &shim).unwrap();
    let out = Command::new(&shim)
        .args(["nextest", "run"])
        .env_remove("SLIMFIT_SELECT")
        .env_remove("SLIMFIT_NEXTEST")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(101), "{stderr}");
    assert!(stderr.contains("SLIMFIT_SELECT is not set: every test runs"), "{stderr}");
    assert!(stderr.contains("SLIMFIT_NEXTEST is not set: the real cargo-nextest is unknown"), "{stderr}");
    // A plan, but no file differs (cargo-mutants' unmutated baseline): every
    // test runs, each under its timeout, and nextest's exit is the shim's.
    let plan = tmp.path().join("plan.json");
    let test = r#"{"binary": "calc", "name": "t", "timeout": 6}"#;
    std::fs::write(&plan, format!(r#"{{"root": "/", "tests": [{test}], "functions": []}}"#)).unwrap();
    let out = Command::new(&shim)
        .args(["nextest", "run"])
        .env("SLIMFIT_SELECT", &plan)
        .env("SLIMFIT_NEXTEST", on_path("false"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("no file differs from the suite's own: every test runs"), "{stderr}");
    let configs: Vec<String> = std::fs::read_dir(tmp.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("nextest-"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert!(configs.len() == 1 && configs[0].contains("period = \"6s\""), "{configs:?}");
}

/// Every key a command prints is named on its own page (`slimfit <command>
/// --help`), as `` `key` ``: the agent reads the page, never the source.
/// Keys that are names (a suite's, a file's) are not keys to name.
fn every_printed_key_is_on_its_page(printed: &[(&'static str, Value)]) {
    fn keys(v: &Value, out: &mut std::collections::BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    if k != "rust" && !k.contains('/') && !k.contains('.') {
                        out.insert(k.clone());
                    }
                    keys(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
            _ => {}
        }
    }
    let mut by_cmd: std::collections::BTreeMap<&str, std::collections::BTreeSet<String>> = Default::default();
    for (cmd, json) in printed {
        keys(json, by_cmd.entry(cmd).or_default());
    }
    let mut missing = Vec::new();
    for (cmd, ks) in &by_cmd {
        let page = Command::new(env!("CARGO_BIN_EXE_slimfit")).args([cmd, "--help"]).output().unwrap();
        let page = String::from_utf8(page.stdout).unwrap();
        let reached: &[&str] = match *cmd {
            "init" => &["mutants_diff_flag", "mutants_pick_flag"],
            "next" => &["survivors", "start", "counts", "remaining", "notes"],
            "check" => &[
                "fix",
                "improved",
                "mutated",
                "change",
                "unviable",
                "landing",
                "records",
                "candidates",
                "replacement",
                "picks",
                "why",
                "outcome",
            ],
            "ratchet" => &["from", "reason", "record"],
            "doctor" => &["findings", "kind", "suite", "what", "fix", "host", "load", "cores", "next"],
            "keep" => {
                &["kept", "test", "hash", "reason", "kept_files", "file", "prod_lines", "commits_90d", "commits_total"]
            }
            _ => &["history", "commit", "from", "mutants_records", "with_picks"],
        };
        for k in reached {
            assert!(ks.contains(*k), "{cmd} printed no {k:?}: the states above no longer reach it");
        }
        for k in ks.iter().filter(|k| !page.contains(&format!("`{k}`"))) {
            missing.push(format!("{cmd}: {k}"));
        }
    }
    assert_eq!(by_cmd.len(), 7, "{:?}", by_cmd.keys());
    assert!(missing.is_empty(), "printed but not named on the command's page:\n{}", missing.join("\n"));
}

/// Python's sample: each test's lines come from coverage.py's contexts,
/// so the copies of the add test are proposed, in the module their `def`s
/// are in, and one kept is not proposed again. A change's mutant, picked,
/// runs through the mutmut driver, and its outcome is read.
fn python() {
    let s = Sample::new(&PYTHON);
    measures_through_its_tools(&s, &PYTHON, false);
    let candidates = |next: &Value| -> Vec<String> {
        let tasks = next["tasks"].as_array().unwrap().iter();
        let c = tasks.flat_map(|t| t["evidence"]["candidates"].as_array().unwrap().iter());
        c.map(|c| c["test"].as_str().unwrap().to_string()).collect()
    };
    let next = s.ok(&["next", "--all", "--kind", "redundant-test"]);
    let before = candidates(&next);
    let twins = ["tests.test_calc::test_adds", "tests.test_calc::test_adds_again", "tests.test_calc::test_slow"];
    assert!(before.len() == 2 && before.iter().all(|t| twins.contains(&t.as_str())), "{next:#}");
    assert_eq!(next["tasks"][0]["files"], serde_json::json!(["tests/test_calc.py"]), "{next:#}");
    s.ok(&["keep", &before[0], "--reason", "it checks its own case"]);
    assert_eq!(candidates(&s.ok(&["next", "--all", "--kind", "redundant-test"])), before[1..]);
    git(&s.repo, &["commit", "-qam", "kept"]);
    git(&s.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    s.take();
    faults_reach_the_agent(&s, &PYTHON);
    let check = s.ok(&["check", "--mutants", PYTHON_PICK, "--why", "the sum", "--fresh"]);
    assert_eq!(check["mutated"]["python"]["picks"][0]["outcome"], "killed", "{check:#}");
    // The pick alone, by its name: one `-F`.
    let run = s.take().into_iter().rfind(|c| c.step == PYTHON.mutants_step).unwrap();
    assert_eq!(run.args.iter().filter(|a| *a == "-F").count(), 1, "{:?}", run.args);
}

/// A change's mutant of the Python sample's real list, which its tests catch.
const PYTHON_PICK: &str = "src/calc.py:2:14: replace + with - in add";

/// Go's sample also: a command line the tool would not take is refused by
/// its stand-in, so a preset that drifts from its tool fails here, not on
/// a user's host.
fn go() {
    let s = Sample::new(&GO);
    measures_through_its_tools(&s, &GO, false);
    // Each test's own run gives its lines: `TestAdds`, `TestAddsAgain` and
    // `TestSlow` run the same ones, so two of them are proposed, in one task
    // of the test file their funcs are in; one kept is not proposed again.
    let candidates = |next: &Value| -> Vec<String> {
        let tasks = next["tasks"].as_array().unwrap().iter();
        let c = tasks.flat_map(|t| t["evidence"]["candidates"].as_array().unwrap().iter());
        c.map(|c| c["test"].as_str().unwrap().to_string()).collect()
    };
    let next = s.ok(&["next", "--all", "--kind", "redundant-test"]);
    let before = candidates(&next);
    let twins = ["example.com/calc::TestAdds", "example.com/calc::TestAddsAgain", "example.com/calc::TestSlow"];
    assert!(before.len() == 2 && before.iter().all(|t| twins.contains(&t.as_str())), "{next:#}");
    assert_eq!(next["tasks"][0]["files"], serde_json::json!(["calc_test.go"]), "{next:#}");
    s.ok(&["keep", &before[0], "--reason", "it checks its own case"]);
    assert_eq!(candidates(&s.ok(&["next", "--all", "--kind", "redundant-test"])), before[1..]);
    s.take();
    faults_reach_the_agent(&s, &GO);
    let cfg = std::fs::read_to_string(s.repo.join("slimfit.toml")).unwrap();
    let cfg = cfg.replace(
        "preset = \"go\"",
        "preset = \"go\"\ntest_command = \"go test ./... > \\\"$SLIMFIT_OUT/test.json\\\"\"",
    );
    std::fs::write(s.repo.join("slimfit.toml"), cfg).unwrap();
    s.error(
        &["check", "--fresh"],
        &["_report at", "fix: fix the command's failure in its log", "standin go test: no -json"],
    );
}
