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
    fn dry_run(&self, args: &[&str], files: &[&str]) -> serde_json::Map<String, Value> {
        let before = snapshot(&self.repo);
        let answer = self.ok(args);
        let after = snapshot(&self.repo);
        let changed: Vec<_> = before.keys().chain(after.keys()).filter(|p| before.get(*p) != after.get(*p)).collect();
        assert!(changed.is_empty(), "slimfit {args:?} wrote {changed:?}");
        let would = answer["would_write"].as_object().unwrap_or_else(|| panic!("{answer:#}")).clone();
        assert_eq!(would.keys().collect::<Vec<_>>(), files, "slimfit {args:?}");
        would
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

/// `init --mutants` runs the preset's tools once each and measures what
/// they wrote; `next` and `check` then work from that measurement.
fn measures_through_its_tools(lang: &Lang) -> Sample {
    let s = Sample::new(lang);
    // cargo-mutants exits 2 on a missed mutant, as the stand-in does; the
    // other tools are made to fail all the same.
    let exit = if lang.name == "rust" { 2 } else { 3 };
    s.fault(lang.mutants_step, Some(Fault { exit, no_report: false }));
    let would =
        s.dry_run(&["init", "--mutants", "--dry-run"], &[".gitignore", ".slimfit/baseline.json", "slimfit.toml"]);
    s.take();
    let (code, init, stderr) = s.slimfit(&["init", "--mutants"]);
    assert_eq!(code, 0, "{}: {init:#}\n{stderr}", lang.name);
    // What the dry run said it would write, but for `test_seconds`: the host's load moves it.
    let baseline = |text: &str| {
        let mut b: Value = serde_json::from_str(text).unwrap();
        b["suites"][lang.name]["test_seconds"] = Value::Null;
        b
    };
    for f in [".gitignore", "slimfit.toml"] {
        assert_eq!(s.read(f), would[f], "{}: {f}", lang.name);
    }
    let base = s.read(".slimfit/baseline.json");
    assert_eq!(baseline(&base), baseline(would[".slimfit/baseline.json"].as_str().unwrap()), "{}", lang.name);
    assert!(stderr.contains(&format!("mutants command exited exit status: {exit}")), "{}: {stderr}", lang.name);
    s.fault(lang.mutants_step, None);
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
    // A bound with room to tighten: ratchet would write exactly the baseline it writes.
    let mut looser: Value = serde_json::from_str(&base).unwrap();
    looser["suites"][lang.name]["uncovered_lines"] = (b["uncovered_lines"].as_u64().unwrap() + 5).into();
    std::fs::write(s.repo.join(".slimfit/baseline.json"), serde_json::to_string_pretty(&looser).unwrap()).unwrap();
    let would = s.dry_run(&["ratchet", "--dry-run"], &[".slimfit/baseline.json"]);
    s.ok(&["ratchet"]);
    assert_eq!(s.read(".slimfit/baseline.json"), would[".slimfit/baseline.json"], "{}", lang.name);
    assert_eq!(s.read(".slimfit/baseline.json"), base, "{}", lang.name);
    s.dry_run(&["ratchet", "--dry-run"], &[]);
    assert_eq!(s.steps(), Vec::<String>::new(), "{}: next, check and ratchet reuse the measurement", lang.name);
    s
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
/// their reports (a mutants tool's exit is covered at `init`). No report
/// is, and the error names the report and ends with the tool's own words.
fn faults_reach_the_agent(s: &Sample, lang: &Lang) {
    const ARGS: &[&str] = &["check", "--all-mutants", "--fresh"];
    let mut tools = lang.test_tools.to_vec();
    // A mutants tool that also runs the tests is missing for them first.
    if !tools.contains(&lang.mutants_tool) {
        tools.push(lang.mutants_tool);
    }
    for tool in tools {
        let link = s.unlink(tool);
        s.error(ARGS, &["_report at", "fix: fix the command's failure in its log", tool]);
        s.relink(&link);
    }
    let no_report = Some(Fault { exit: 0, no_report: true });
    let (first, mutants) = (lang.test_steps[0], lang.mutants_step);
    s.fault(first, Some(Fault { exit: 3, no_report: false }));
    s.fault(mutants, no_report);
    let (code, json, stderr) = s.slimfit(ARGS);
    assert_eq!(code, 2, "{}: {json:#}\n{stderr}", lang.name);
    assert!(stderr.contains("test command exited exit status: 3"), "{}: {first}: {stderr}", lang.name);
    assert!(faults(&json).contains(&format!("standin {mutants}: writing no report")), "{json:#}");
    s.fault(first, None);
    s.fault(mutants, None);
    for step in lang.test_steps {
        s.fault(step, no_report);
        s.error(
            ARGS,
            &["_report at", "fix: fix the command's failure in its log", &format!("standin {step}: writing no report")],
        );
        s.fault(step, None);
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
    let s = measures_through_its_tools(&GO);
    faults_reach_the_agent(&s, &GO);
    // A command line the tool would not take is refused by its stand-in, so
    // a preset that drifts from its tool fails here, not on a user's host.
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

/// Every key a command prints is named on its own page (`slimfit <command>
/// --help`), as `` `key` ``: the agent reads the page, never the source.
/// Keys that are names (a suite's, a file's) are not keys to name. Each
/// command runs on the Rust sample in the states that make it print its
/// optional keys: violations, improvements, loosenings, a piece's
/// not-recorded bounds, pending mutants, mutated files.
#[test]
fn every_printed_key_is_on_its_page() {
    let s = Sample::new(&RUST);
    let mut printed: std::collections::BTreeMap<&str, Vec<Value>> = Default::default();
    let mut run = |cmd: &'static str, args: &[&str], code: i32| {
        let mut all = vec![cmd];
        all.extend(args);
        let (got, json, stderr) = s.slimfit(&all);
        assert_eq!(got, code, "slimfit {all:?}: {json:#}\n{stderr}");
        printed.entry(cmd).or_default().push(json);
    };
    let baseline = s.repo.join(".slimfit/baseline.json");
    let edit = |f: &dyn Fn(&mut Value)| {
        let mut b: Value = serde_json::from_str(&std::fs::read_to_string(&baseline).unwrap()).unwrap();
        f(&mut b["suites"]["rust"]);
        std::fs::write(&baseline, serde_json::to_string(&b).unwrap()).unwrap();
    };

    run("init", &["--mutants"], 0);
    run("next", &[], 0);
    run("next", &["--all", "--all-mutants"], 0);
    run("check", &[], 0);
    run("report", &["--points", "1"], 0);
    run("doctor", &[], 0);
    // Better than the baseline: improved.
    edit(&|b| b["uncovered_lines"] = (b["uncovered_lines"].as_u64().unwrap() + 100).into());
    run("check", &[], 0);
    // Worse: violations, then a loosening.
    edit(&|b| b["uncovered_lines"] = 0.into());
    run("check", &[], 1);
    run("ratchet", &["--loosen", "uncovered_lines", "--reason", "test"], 0);
    // A changed file and a pending one: mutated, mutants_pending.
    let lib = s.repo.join("src/lib.rs");
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap() + "// changed\n").unwrap();
    edit(&|b| b["mutants_pending"] = serde_json::json!(["src/lib.rs"]));
    run("check", &["--mutants"], 0);
    // A piece over its time bound, with no slack: not_recorded.
    edit(&|b| b["test_seconds"] = 0.into());
    let cfg = s.repo.join("slimfit.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    let text =
        text.replace("time_tolerance = 0.2", "time_tolerance = 0.0").replace("time_slack = 1.0", "time_slack = 0.0");
    std::fs::write(&cfg, text).unwrap();
    run("ratchet", &["--pending-files", "1", "--fresh"], 0);
    run("report", &["--points", "1"], 0);

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
    let mut missing = Vec::new();
    for (cmd, outputs) in &printed {
        let page = Command::new(env!("CARGO_BIN_EXE_slimfit")).args([cmd, "--help"]).output().unwrap();
        let page = String::from_utf8(page.stdout).unwrap();
        let mut ks = Default::default();
        outputs.iter().for_each(|o| keys(o, &mut ks));
        let reached: &[&str] = match *cmd {
            "init" => &["mutants_file_flag", "mutant_files"],
            "next" => &["also_killed_by", "survivors", "start", "counts", "remaining"],
            "check" => &["fix", "improved", "mutants_pending", "unviable"],
            "ratchet" => &["from", "reason", "fix"],
            "doctor" => &["findings", "kind", "suite", "what", "fix", "host", "load", "cores", "next"],
            _ => &["history", "commit", "from"],
        };
        for k in reached {
            assert!(ks.contains(*k), "{cmd} printed no {k:?}: the states above no longer reach it");
        }
        for k in ks.iter().filter(|k| !page.contains(&format!("`{k}`"))) {
            missing.push(format!("{cmd}: {k}"));
        }
    }
    assert!(missing.is_empty(), "printed but not named on the command's page:\n{}", missing.join("\n"));
}

/// `doctor` reads and writes nothing: on a fresh sample it says to run
/// `init` and names the tools the detected suite needs that are not on
/// PATH (the stand-in is `cargo` alone), and exits 0. The load is the
/// host's own, so a `host_busy` finding is left out.
#[test]
fn doctor_names_each_fix_and_changes_nothing() {
    let s = Sample::new(&RUST);
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
}
