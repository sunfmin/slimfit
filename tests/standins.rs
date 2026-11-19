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
    /// Steps after the test run that read each test's own profile, sorted:
    /// their report is optional (without it every mutant runs every test).
    reach_steps: &'static [&'static str],
    mutants_tool: &'static str,
    mutants_step: &'static str,
    source: &'static str,
}

const RUST: Lang = Lang {
    name: "rust",
    test_tools: &["cargo"],
    test_steps: &["cargo llvm-cov"],
    reach_steps: &[
        "llvm-cov export",
        "llvm-cov export",
        "llvm-cov export",
        "llvm-cov export",
        "llvm-profdata merge",
        "llvm-profdata merge",
        "llvm-profdata merge",
        "llvm-profdata merge",
    ],
    mutants_tool: "cargo",
    mutants_step: "cargo mutants",
    source: "src/lib.rs",
};
const GO: Lang = Lang {
    name: "go",
    test_tools: &["go"],
    test_steps: &["go test"],
    reach_steps: &[],
    mutants_tool: "gremlins",
    mutants_step: "gremlins unleash",
    source: "calc.go",
};
const TYPESCRIPT: Lang = Lang {
    name: "typescript",
    test_tools: &["npx"],
    test_steps: &["npx vitest"],
    reach_steps: &[],
    mutants_tool: "npx",
    mutants_step: "npx stryker",
    source: "src/calc.ts",
};
const KOTLIN: Lang = Lang {
    name: "kotlin",
    test_tools: &["gradlew"],
    test_steps: &["gradlew test"],
    reach_steps: &[],
    mutants_tool: "gradlew",
    mutants_step: "gradlew pitest",
    source: "src/main/kotlin/com/example/Calc.kt",
};
const SWIFT: Lang = Lang {
    name: "swift",
    test_tools: &["swift", "xcrun"],
    test_steps: &["swift test", "xcrun llvm-cov"],
    reach_steps: &[],
    mutants_tool: "muter",
    mutants_step: "muter run",
    source: "Sources/Calc/Calc.swift",
};

/// The tools the stand-in binary answers to, and what slimfit and the
/// presets' shell lines need from the host.
const STANDINS: &[&str] = &["cargo", "llvm-profdata", "llvm-cov", "go", "gremlins", "swift", "xcrun", "muter", "npx"];
const HOST: &[&str] = &["sh", "git", "find", "head", "basename", "mv", "cat", "sed", "grep", "xargs", "getconf"];

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

    go();
    for lang in [&TYPESCRIPT, &KOTLIN, &SWIFT] {
        let s = Sample::new(lang);
        measures_through_its_tools(&s, lang, false);
        faults_reach_the_agent(&s, lang);
    }
}

/// `doctor` reads and writes nothing: on a fresh sample it says to run
/// `init` and names the tools the detected suite needs that are not on
/// PATH (the stand-in is `cargo` alone), and exits 0. The load is the
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
    let want = vec![
        ("no_config".to_string(), "no".to_string()),
        tool("cargo-llvm-cov"),
        tool("cargo-nextest"),
        tool("cargo-mutants"),
    ];
    assert_eq!(found(&doctor), want, "{doctor:#}");
    assert!(doctor["next"].as_str().unwrap().contains("slimfit init"), "{doctor:#}");
    assert!(doctor["host"]["cores"].as_u64() >= Some(1), "{doctor:#}");
    assert_eq!(status(), before, "doctor changed the working tree");
    assert_eq!(s.steps(), Vec::<String>::new(), "doctor ran a tool");
    doctor
}

/// `init` runs the preset's test tools once each, and no mutant; `next`
/// and `check` then work from that measurement. A preset whose mutants tool
/// cannot be limited to a change refuses `--mutants` and runs nothing
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
    steps[lang.test_steps.len()..].sort();
    assert_eq!(steps, [lang.test_steps, lang.reach_steps].concat(), "{}", lang.name);
    let base = s.read(".slimfit/baseline.json");
    if let Some((_, dry)) = printed.first() {
        // What the dry run said it would write, but for `test_seconds` (the
        // host's load moves it) and the tree mutants_at names (the dry run's
        // lacks the files it did not write).
        let would = &dry["would_write"];
        for f in [".gitignore", "slimfit.toml"] {
            assert_eq!(s.read(f), would[f], "{}: {f}", lang.name);
        }
        let held = |text: &str| {
            let mut b: Value = serde_json::from_str(text).unwrap();
            b["suites"][lang.name]["test_seconds"] = Value::Null;
            b["mutants_at"]["tree"] = Value::Null;
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
    if lang.name != "rust" {
        s.error(&["check", "--mutants"], &["cannot be limited to a change", "never mutates the whole codebase"]);
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
/// The mutants tool runs only on a change, and only where it can be
/// limited to one (Rust's).
fn faults_reach_the_agent(s: &Sample, lang: &Lang) {
    let mutates = lang.name == "rust";
    let mut tools = lang.test_tools.to_vec();
    let args: &[&str] = if mutates {
        let source = s.repo.join(lang.source);
        std::fs::write(&source, std::fs::read_to_string(&source).unwrap() + "// a change\n").unwrap();
        // A mutants tool that also runs the tests is missing for them first.
        if !tools.contains(&lang.mutants_tool) {
            tools.push(lang.mutants_tool);
        }
        &["check", "--mutants", "--fresh"]
    } else {
        &["next", "--fresh"]
    };
    for tool in tools {
        let link = s.unlink(tool);
        s.error(args, &["_report at", "fix: fix the command's failure in its log", tool]);
        s.relink(&link);
    }
    let no_report = Some(Fault { exit: 0, no_report: true });
    let (first, mutants) = (lang.test_steps[0], lang.mutants_step);
    s.fault(first, Some(Fault { exit: 3, no_report: false }));
    if mutates {
        s.fault(mutants, no_report);
    }
    let (code, json, stderr) = s.slimfit(args);
    assert!(stderr.contains("test command exited exit status: 3"), "{}: {first}: {stderr}", lang.name);
    if mutates {
        assert_eq!(code, 2, "{}: {json:#}\n{stderr}", lang.name);
        assert!(faults(&json).contains(&format!("standin {mutants}: writing no report")), "{json:#}");
    } else {
        assert_eq!(code, 0, "{}: {json:#}\n{stderr}", lang.name);
    }
    s.fault(first, None);
    s.fault(mutants, None);
    for step in lang.test_steps {
        s.fault(step, no_report);
        s.error(
            args,
            &["_report at", "fix: fix the command's failure in its log", &format!("standin {step}: writing no report")],
        );
        s.fault(step, None);
    }
}

/// The Rust sample, measured, in the states that make each command print
/// its optional keys: violations, improvements, loosenings, the change's
/// mutants and `mutants_at`. A changed line gives cargo-mutants the
/// change's diff; `--all-mutants` is refused.
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
    // Redundant tests are proposed from each test's lines: `slow` runs only
    // what `adds` runs. Kept, with a reason, it is not proposed again.
    let proposed = |next: &Value| -> Vec<String> {
        let tasks = next["tasks"].as_array().unwrap().iter();
        tasks.map(|t| t["evidence"]["test"].as_str().unwrap().to_string()).collect()
    };
    let before = proposed(&run("next", &["--all", "--kind", "redundant-test"], 0));
    assert!(before.contains(&"calc::tests::slow".to_string()), "{before:?}");
    s.error(&["keep", "calc::tests::slow"], &["keep without --reason"]);
    s.error(
        &["keep", "calc::tests::gone", "--reason", "r"],
        &["no suite's test run recorded a test calc::tests::gone"],
    );
    let kept = run("keep", &["calc::tests::slow", "--reason", "it checks the sleep"], 0);
    assert_eq!(kept["kept"][0]["test"], "calc::tests::slow", "{kept:#}");
    let after = proposed(&run("next", &["--all", "--kind", "redundant-test"], 0));
    assert_eq!(after, before.into_iter().filter(|t| t != "calc::tests::slow").collect::<Vec<_>>());
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
    // Worse: violations, then a loosening.
    edit(&|b| b["uncovered_lines"] = 0.into());
    run("check", &[], 1);
    run("ratchet", &["--loosen", "uncovered_lines", "--reason", "test"], 0);
    // A changed line whose mutants survive: cargo-mutants is given the
    // change's diff; the survivors are accepted with a reason. Each mutant
    // runs the tests that ran its function: cargo-mutants lists the
    // mutants, and with a real nextest to hand on to (the stand-in, as
    // `cargo-nextest`), slimfit writes the plan and its shim.
    let lib = s.repo.join("src/lib.rs");
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap() + "// changed\n").unwrap();
    // No nextest to hand on to: every mutant runs every test, and stderr says why.
    s.take();
    let (_, _, stderr) = s.slimfit(&["check", "--mutants"]);
    assert!(
        stderr.contains("no cargo-nextest on PATH or in cargo's bin directory: every mutant runs every test"),
        "{stderr}"
    );
    assert!(!s.take().iter().any(|c| c.args.contains(&"--list".to_string())), "listed with no nextest");
    std::os::unix::fs::symlink(standin(), s.bin.join("cargo-nextest")).unwrap();
    let check = run("check", &["--mutants", "--fresh"], 1);
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
    assert_eq!(check["mutated"]["rust"]["files"], serde_json::json!(["src/lib.rs"]), "{check:#}");
    let calls = s.take();
    let listed = calls.iter().filter(|c| c.step == "cargo mutants" && c.args.contains(&"--list".to_string())).count();
    assert_eq!(listed, 1, "{calls:?}");
    // cargo-mutants' build of each of the 17 listed mutants reaches the
    // real nextest through the shim as it came; its test run, narrowed:
    // `add`'s 5 with its 3 tests, `is_adult`'s 3 with its one, each under its
    // timeout; `unused`'s 9 run none.
    let runs = calls.iter().filter(|c| c.step == "cargo nextest").count() / 2;
    let builds = calls.iter().filter(|c| c.step == "cargo-nextest nextest" && c.args.contains(&"--no-run".into()));
    assert!(builds.clone().count() == 17 && builds.clone().all(|c| c.args.len() == 5), "{calls:#?}");
    let filters: Vec<&str> = calls
        .iter()
        .filter(|c| c.step == "cargo-nextest nextest" && !c.args.contains(&"--no-run".into()))
        .map(|c| {
            assert!(c.args.iter().any(|a| a.starts_with("slimfit:") && a.ends_with(".toml")), "{c:?}");
            c.args.iter().skip_while(|a| *a != "-E").nth(1).map(String::as_str).unwrap_or("")
        })
        .collect();
    // In the order the tests ran, as the reach report lists them.
    let adds = "(binary_id(=calc) & test(=tests::adds_again)) | (binary_id(=calc) & test(=tests::adds)) | \
                (binary_id(=calc) & test(=tests::slow))";
    let adult = "(binary_id(=calc) & test(=tests::adult))";
    assert_eq!((runs, filters), (17, [[adds; 5].as_slice(), &[adult; 3]].concat()), "{calls:#?}");
    let mutants = calls.iter().rfind(|c| c.step == "cargo mutants").unwrap();
    // Each mutant builds as the test run built, into its target directory:
    // no cold first build (ADR 0007).
    assert!(calls.iter().any(|c| c.step == "cargo llvm-cov" && c.args[1] == "show-env"), "{calls:#?}");
    assert_eq!(mutants.target_dir, Some(mutants.dir.join("target/llvm-cov-target")), "{mutants:?}");
    let diff = mutants.args.iter().position(|a| a == "--in-diff").map(|i| &mutants.args[i + 1]);
    assert!(diff.is_some_and(|d| d.ends_with("change.diff")), "{:?}", mutants.args);
    run("ratchet", &["--loosen", "survived_mutants", "--reason", "test"], 0);
    run("report", &["--points", "1"], 0);
    // A second `fn slow` its path cannot tell apart: no text to keep it by.
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap() + "fn slow() {}\n").unwrap();
    s.error(&["keep", "calc::tests::slow", "--reason", "r"], &["calc::tests::slow's own function was not found"]);
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
            "init" => &["mutants_diff_flag", "mutants_file_flag"],
            "next" => &["survivors", "start", "counts", "remaining", "notes"],
            "check" => &["fix", "improved", "mutated", "change", "unviable"],
            "ratchet" => &["from", "reason", "mutants_at", "tree"],
            "doctor" => &["findings", "kind", "suite", "what", "fix", "host", "load", "cores", "next"],
            "keep" => &["kept", "test", "hash", "reason"],
            _ => &["history", "commit", "from"],
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

/// Go's sample also: a command line the tool would not take is refused by
/// its stand-in, so a preset that drifts from its tool fails here, not on
/// a user's host.
fn go() {
    let s = Sample::new(&GO);
    measures_through_its_tools(&s, &GO, false);
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
