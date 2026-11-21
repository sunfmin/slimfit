//! Stand-ins for the tools slimfit drives, for slimfit's own tests: one
//! binary, linked under each tool's name (`cargo`, `cargo-nextest`,
//! `llvm-profdata`, `llvm-cov`, `go`, `gremlins`, `swift`, `xcrun`, `muter`, `npx`,
//! `gradlew`) into a directory placed
//! first on PATH, and under a Go test binary's (`<package>.test`) where `go
//! test -c` builds one. It reads the arguments as the real tool does, refuses
//! (exit 64, no report) what the real tool would not take, and writes the
//! real reports of `testdata/<language>/reports` where the real tool
//! writes its own. Its state is one JSON file (see `state.rs`): every call
//! is recorded there, and faults are injected from it.

mod state;
#[allow(dead_code)]
#[path = "../../src/tools.rs"]
mod tools;

use state::{Call, ENV, Fault, State};
use std::path::{Path, PathBuf};
use tools::*;

/// A tool's work: the reports to write, each a source under the state's
/// `reports` and a destination; what to print on stdout; the exit status
/// the real tool would give.
#[derive(Default)]
struct Work {
    copies: Vec<(String, PathBuf)>,
    stdout: Option<String>,
    exit: i32,
    /// Files it writes beside the reports, each with its text: an archive.
    files: Vec<(PathBuf, String)>,
    /// Done before the reports: e.g. the build directory a later step reads.
    prepare: Vec<Box<dyn FnOnce()>>,
    /// cargo-mutants' `-F` and `--in-diff`: only the mutants these keep are in its reports.
    named: Option<Kept>,
}

/// The mutants cargo-mutants keeps of its list: those whose span touches a
/// line `--in-diff` adds (with `-F`, of the names given; cargo-mutants 27
/// keeps a field deletion whatever its name).
#[derive(Clone, Default)]
struct Kept {
    names: Option<Vec<String>>,
    lines: Option<std::collections::BTreeMap<String, std::collections::BTreeSet<u64>>>,
    /// The real list: an outcome names no span, so its listed entry is kept or not.
    list: Vec<serde_json::Value>,
    /// Where the build phase writes the names of the mutants that did not build.
    unviable: Option<PathBuf>,
}

impl Kept {
    fn keeps(&self, m: &serde_json::Value) -> bool {
        let name = m["name"].as_str().unwrap_or("");
        let named = m["genre"] == "StructField" || self.names.as_ref().is_none_or(|n| n.iter().any(|w| w == name));
        let span = |at: &str| m["span"][at]["line"].as_u64().unwrap_or(0);
        let file = m["file"].as_str().unwrap_or("");
        let touched = self
            .lines
            .as_ref()
            .is_none_or(|l| l.get(file).is_some_and(|l| (span("start")..=span("end")).any(|n| l.contains(&n))));
        named && touched
    }
}

/// The lines a unified diff adds, per file; refused, as cargo-mutants
/// refuses it, when one is not what the file in `dir` holds there.
fn in_diff(
    diff: &str,
    dir: &Path,
) -> Result<std::collections::BTreeMap<String, std::collections::BTreeSet<u64>>, Refusal> {
    let mut out: std::collections::BTreeMap<String, std::collections::BTreeSet<u64>> = Default::default();
    let (mut file, mut at) = (String::new(), 0u64);
    for l in diff.lines() {
        if let Some(f) = l.strip_prefix("+++ ") {
            file = f.strip_prefix("b/").unwrap_or(f).to_string();
        } else if let Some(h) = l.strip_prefix("@@ ") {
            let new = h.split(" +").nth(1).and_then(|n| n.split([',', ' ']).next()?.parse().ok());
            at = new.ok_or(format!("--in-diff: a hunk header {l:?}"))?;
        } else if let Some(added) = l.strip_prefix('+') {
            let text = std::fs::read_to_string(dir.join(&file)).map_err(|e| format!("--in-diff: {file}: {e}"))?;
            if text.lines().nth(at as usize - 1) != Some(added) {
                return Err(format!("Diff content doesn't match source file: {file} line {at}"));
            }
            out.entry(file.clone()).or_default().insert(at);
            at += 1;
        } else if l.starts_with(' ') {
            at += 1;
        }
    }
    Ok(out)
}

type Refusal = String;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tool = Path::new(&args[0]).file_name().unwrap().to_string_lossy().into_owned();
    let args = &args[1..];
    let path = PathBuf::from(std::env::var(ENV).unwrap_or_else(|_| die(&tool, &format!("{ENV} is not set"))));
    // Stand-ins run at once (the Rust preset reads each test's profile in
    // parallel): each records its call under the state's lock.
    let lock = std::fs::File::create(path.with_extension("lock")).unwrap();
    // SAFETY: flock on a file this process holds open.
    assert_eq!(unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) }, 0);
    let mut state = State::load(&path);
    let dir = std::env::current_dir().unwrap();
    let sub = args.iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_default();
    let step = match tool.as_str() {
        "gradlew" if args.iter().any(|a| a == "pitest") => "gradlew pitest".to_string(),
        "gradlew" => "gradlew test".to_string(),
        "go" if args.iter().any(|a| a == "-c") => "go test -c".to_string(),
        // A Go test binary: its step is the flag it was run with.
        t if t.ends_with(".test") => format!("{t} {}", args.first().map_or("", |a| a.split('=').next().unwrap_or(""))),
        _ => format!("{tool} {sub}"),
    };
    let target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    state.calls.push(Call { step: step.clone(), args: args.to_vec(), dir: dir.clone(), target_dir });
    state.save(&path);
    drop(lock);
    let fault = state.faults.get(&step).copied().unwrap_or_default();

    let work = match tool.as_str() {
        "cargo" => cargo(&sub, args, &dir, &state.reports),
        "llvm-profdata" => llvm_profdata(&sub, args),
        "llvm-cov" => llvm_cov(&sub, args, &state.reports),
        "cargo-nextest" => nextest(args, &dir, &state.reports, fault.first_test_fails),
        "go" => go(&sub, args, &dir, &state.reports),
        t if t.ends_with(".test") => go_test_binary(args, &state.reports),
        "gremlins" => gremlins(&sub, args),
        "swift" => swift(&sub, args, &dir),
        "xcrun" => xcrun(args, &dir),
        "muter" => muter(&sub, args),
        "gradlew" => gradlew(args),
        "npx" => npx(&sub, args),
        _ => Err(format!("no stand-in for {tool}")),
    };
    let work = work.unwrap_or_else(|why| die(&step, &why));
    run(work, &state.reports, &dir, fault, &step);
}

fn die(step: &str, why: &str) -> ! {
    eprintln!("standin {step}: {why}");
    std::process::exit(64);
}

fn run(work: Work, reports: &Path, dir: &Path, fault: Fault, step: &str) {
    let root = dir.to_string_lossy();
    for p in work.prepare {
        p();
    }
    if fault.no_report {
        eprintln!("standin {step}: writing no report (injected)");
    } else {
        for (src, dst) in &work.copies {
            match &work.named {
                Some(names) => {
                    std::fs::create_dir_all(dir.join(dst).parent().unwrap()).unwrap();
                    std::fs::write(dir.join(dst), named(&read(&reports.join(src), &root), names)).unwrap();
                }
                None => copy(&reports.join(src), &dir.join(dst), &root),
            }
        }
        for (path, text) in &work.files {
            std::fs::write(dir.join(path), text).unwrap();
        }
        if let Some(s) = &work.stdout {
            let text = read(&reports.join(s), &root);
            print!("{}", work.named.as_ref().map_or(text.clone(), |n| named(&text, n)));
        }
    }
    let exit = if fault.exit != 0 { fault.exit } else { work.exit };
    if exit != 0 {
        eprintln!("standin {step}: exiting {exit}");
    }
    std::process::exit(exit);
}

/// cargo-mutants' list (`--list --json`) or `outcomes.json`, with only the
/// mutants named (and the baseline) left in.
fn named(text: &str, kept: &Kept) -> String {
    let mut v: serde_json::Value = serde_json::from_str(text).unwrap();
    let listed = |name: &str| kept.list.iter().find(|m| m["name"] == name).is_some_and(|m| kept.keeps(m));
    let unviable: Vec<String> = kept
        .unviable
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map_or(vec![], |t| t.lines().map(String::from).collect());
    match v.as_array_mut() {
        Some(list) => list.retain(|m| kept.keeps(m)),
        None => {
            let outcomes = v["outcomes"].as_array_mut().unwrap();
            outcomes.retain(|o| {
                o["scenario"]["Mutant"].is_null() || listed(o["scenario"]["Mutant"]["name"].as_str().unwrap_or(""))
            });
            for o in outcomes.iter_mut() {
                if unviable.iter().any(|u| o["scenario"]["Mutant"]["name"] == u.as_str()) {
                    o["summary"] = "Unviable".into();
                }
            }
        }
    }
    serde_json::to_string_pretty(&v).unwrap()
}

/// The names `-F` gives, as slimfit writes each: anchored, its metacharacters escaped.
fn names(args: &[String]) -> Result<Option<Vec<String>>, Refusal> {
    let mut out = Vec::new();
    for (i, _) in args.iter().enumerate().filter(|(_, a)| *a == "-F") {
        let re = args.get(i + 1).ok_or("-F needs a regex")?;
        let inner = re.strip_prefix('^').and_then(|r| r.strip_suffix('$')).ok_or(format!("-F {re} is not anchored"))?;
        let mut name = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            name.push(if c == '\\' { chars.next().ok_or(format!("-F {re} ends in a lone \\"))? } else { c });
        }
        out.push(name);
    }
    Ok((!out.is_empty()).then_some(out))
}

/// A report's text with the project root put in for `/REPO`.
fn read(src: &Path, root: &str) -> String {
    std::fs::read_to_string(src).unwrap_or_else(|e| panic!("{}: {e}", src.display())).replace("/REPO", root)
}

/// Copies a report file, or a directory of them, rooting it at `root`.
fn copy(src: &Path, dst: &Path, root: &str) {
    if src.is_dir() {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap().flatten() {
            copy(&e.path(), &dst.join(e.file_name()), root);
        }
    } else {
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(dst, read(src, root)).unwrap();
    }
}

/// The value of `name`, given as `name value` or `name=value`.
fn value(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name);
    at.and_then(|i| args.get(i + 1).cloned())
        .or_else(|| args.iter().find_map(|a| a.strip_prefix(&format!("{name}=")).map(String::from)))
}

/// Every value of `name`, each given as `name value` or `name=value`.
fn values(args: &[String], name: &str) -> Vec<String> {
    let prefix = format!("{name}=");
    let mut out = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if a == name {
            out.extend(args.get(i + 1).cloned());
        } else if let Some(v) = a.strip_prefix(&prefix) {
            out.push(v.to_string());
        }
    }
    out
}

fn need(args: &[String], name: &str) -> Result<String, Refusal> {
    value(args, name).ok_or_else(|| format!("no {name}"))
}

fn has(args: &[String], flag: &str) -> Result<(), Refusal> {
    if args.iter().any(|a| a == flag) { Ok(()) } else { Err(format!("no {flag}")) }
}

fn sub_is(sub: &str, want: &str) -> Result<(), Refusal> {
    if sub == want { Ok(()) } else { Err(format!("no subcommand {sub:?}")) }
}

/// `cargo llvm-cov nextest-archive`, `cargo llvm-cov show-env`, `cargo mutants`, and `cargo nextest`.
fn cargo(sub: &str, args: &[String], dir: &Path, reports: &Path) -> Result<Work, Refusal> {
    match sub {
        // The environment the test run builds with, as the mutants command reads it.
        "llvm-cov" if args.get(1).map(String::as_str) == Some("show-env") => {
            has(args, "--sh")?;
            Ok(Work { stdout: Some("show-env.sh".into()), ..Work::default() })
        }
        // The tests, built (one binary, a script that records its test where
        // `LLVM_PROFILE_FILE` says) into the archive alone, as if a build
        // elsewhere had then taken the build directory's away.
        "llvm-cov" => {
            if args.get(1).map(String::as_str) != Some("nextest-archive") {
                return Err("only `llvm-cov nextest-archive` and `llvm-cov show-env` stand in".into());
            }
            has(args, "--workspace")?;
            let archive = need(args, "--archive-file")?;
            Ok(Work { files: vec![(archive.into(), format!("{ARCHIVED}\n{TEST_BINARY}"))], ..Work::default() })
        }
        "mutants" => {
            let out = PathBuf::from(need(args, "--output")?);
            let lines = match value(args, "--in-diff") {
                Some(diff) => {
                    let text = std::fs::read_to_string(&diff).map_err(|e| format!("--in-diff {diff}: {e}"))?;
                    Some(in_diff(&text, dir)?)
                }
                None => None,
            };
            let list = std::fs::read_to_string(reports.join("mutants.list.json")).unwrap_or("[]".into());
            let list = serde_json::from_str(&list).unwrap_or_default();
            let unviable = Some(out.join("standin-unviable"));
            let kept = Kept { names: names(args)?, lines, list, unviable };
            // As cargo-mutants returns when `--in-diff` keeps no mutant: no report, exit 0.
            if kept.lines.is_some() && !kept.list.iter().any(|m| kept.keeps(m)) {
                eprintln!(" INFO No mutants to filter");
                return Ok(Work::default());
            }
            if args.iter().any(|a| a == "--list") {
                has(args, "--json")?;
                return Ok(Work { stdout: Some("mutants.list.json".into()), named: Some(kept), ..Work::default() });
            }
            let outcomes = out.join(CARGO_MUTANTS_OUTCOMES);
            // cargo-mutants exits 2 when a mutant was missed.
            let text = std::fs::read_to_string(reports.join("outcomes.json")).unwrap_or_default();
            let missed = named(&text, &kept).contains("\"MissedMutant\"");
            let (reports, dir) = (reports.to_path_buf(), dir.to_path_buf());
            let skip = value(args, "--baseline").is_some_and(|b| b == "skip");
            let only = kept.clone();
            let test_args = values(args, "--cargo-test-arg");
            Ok(Work {
                copies: vec![("outcomes.json".into(), outcomes)],
                exit: if missed { 2 } else { 0 },
                prepare: vec![Box::new(move || test_mutants(&reports, &dir, &out, skip, &only, &test_args))],
                named: Some(kept),
                ..Work::default()
            })
        }
        // As cargo runs a subcommand it does not have: `cargo-nextest` from PATH.
        "nextest" => {
            let status = std::process::Command::new("cargo-nextest").args(args).status();
            Ok(Work {
                exit: status.map_err(|e| format!("cargo-nextest: {e}"))?.code().unwrap_or(101),
                ..Work::default()
            })
        }
        _ => Err(format!("no subcommand {sub:?}")),
    }
}

/// cargo-mutants' build and test phases, as `--test-tool nextest` runs them: in a build
/// directory under `out`, unmutated first unless `skip`, then once per
/// mutant of the real list `kept` keeps, in the list's order, with the
/// lines its diff takes out put back as its diff puts them in, `cargo
/// nextest run --no-run`, then, if that built, `cargo nextest run` and
/// `test_args` (its `--cargo-test-arg`s); the
/// names of those that did not build are written to `kept.unviable`. Only
/// when there is a `cargo-nextest` to run.
fn test_mutants(reports: &Path, dir: &Path, out: &Path, skip: bool, kept: &Kept, test_args: &[String]) {
    let on_path =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|d| d.join("cargo-nextest").is_file());
    if !on_path {
        return;
    }
    let listed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(reports.join("mutants.list.json")).unwrap()).unwrap();
    let build = out.join("standin-build");
    let nextest = |phase: &[&str]| {
        let status = std::process::Command::new("cargo")
            .args(["nextest", "run"])
            .args(phase)
            .args(["--verbose", "--package=calc@0.1.0"])
            .args(if phase.is_empty() { test_args } else { &[] })
            .current_dir(&build)
            .status()
            .unwrap();
        status.code().expect("cargo nextest run was killed") == 0
    };
    // The build phase (`--no-run`), then, if it built, the test phase.
    let test = || {
        let built = nextest(&["--no-run"]);
        if built {
            nextest(&[]);
        }
        built
    };
    let copy = |file: &str| {
        std::fs::create_dir_all(build.join(file).parent().unwrap()).unwrap();
        std::fs::copy(dir.join(file), build.join(file)).unwrap();
    };
    let files: std::collections::BTreeSet<&str> =
        listed.as_array().unwrap().iter().map(|m| m["file"].as_str().unwrap()).collect();
    files.iter().chain(dir.join(NEXTEST_CONFIG).exists().then_some(&NEXTEST_CONFIG)).for_each(|f| copy(f));
    if !skip {
        test();
    }
    let mut unviable = String::new();
    for m in listed.as_array().unwrap().iter().filter(|m| kept.keeps(m)) {
        let file = m["file"].as_str().unwrap();
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        let (at, out, put) = changed(m["diff"].as_str().unwrap());
        lines.splice(at - 1..at - 1 + out, put);
        std::fs::write(build.join(file), lines.join("\n") + "\n").unwrap();
        if !test() {
            unviable.push_str(&format!("{}\n", m["name"].as_str().unwrap()));
        }
        copy(file);
    }
    if let Some(p) = &kept.unviable {
        std::fs::write(p, unviable).unwrap();
    }
}

/// What a mutant's diff (one hunk) changes: the first line it takes out,
/// how many it takes out, and the lines it puts in.
fn changed(diff: &str) -> (usize, usize, Vec<String>) {
    let mut lines = diff.lines().skip_while(|l| !l.starts_with("@@ "));
    let header = lines.next().unwrap();
    let mut at: usize = header[4..].split([',', ' ']).next().unwrap().parse().unwrap();
    let mut lines = lines.peekable();
    while lines.next_if(|l| l.starts_with(' ')).is_some() {
        at += 1;
    }
    let (mut out, mut put) = (0, Vec::new());
    for l in lines {
        if l.starts_with('-') {
            out += 1;
        } else if let Some(p) = l.strip_prefix('+') {
            put.push(p.to_string());
        } else {
            break;
        }
    }
    (at, out, put)
}

/// The archive's one test binary, where it is under the build directory,
/// and its text: it records its test (`$T`) as a profile.
const ARCHIVED: &str = "target/debug/deps/calc-standin";
const TEST_BINARY: &str = "#!/bin/sh\nprintf %s \"$T\" > \"${LLVM_PROFILE_FILE%-%p-%m.profraw}-$$-1.profraw\"\n";
/// The repository's nextest configuration: its profiles.
const NEXTEST_CONFIG: &str = ".config/nextest.toml";

/// `cargo-nextest nextest run`: its tool configuration files must be
/// nextest's TOML, a `--profile` other than `default` one the repository
/// configures, and its filtersets (`-E`, any one of which runs a test) in
/// the grammar of [`matches`]. The tests are the reach report's, those
/// the filtersets match; every one passes but the first when
/// `first_fails`, and then, as nextest stops at a failure unless told how
/// many to take ([`stops_after`]), only it runs and the rest are said not
/// to. From `--archive-file`, it extracts the binary to `--extract-to` and
/// runs each test through the runner `--config` sets, writing the real
/// JUnit report, of those tests only, under the profile's store;
/// otherwise it prints each test it runs. `nextest list -T oneline` of an
/// archive prints the tests, as `<binary> <name>`, and takes no flag of a
/// run's failures.
fn nextest(args: &[String], dir: &Path, reports: &Path, first_fails: bool) -> Result<Work, Refusal> {
    let list = match (args.first().map(String::as_str), args.get(1).map(String::as_str)) {
        (Some("nextest"), Some("run")) => false,
        (Some("nextest"), Some("list")) => true,
        _ => return Err("only `nextest run` and `nextest list` stand in".into()),
    };
    let stops = if list {
        if let Some(f) = args.iter().find(|a| FAILURE_FLAGS.iter().any(|f| a.split('=').next() == Some(f))) {
            return Err(format!("unexpected argument '{f}' found"));
        }
        None
    } else {
        stops_after(args)?
    };
    let mut junit = None;
    for config in values(args, "--tool-config-file") {
        let (tool, path) = config.split_once(':').ok_or("--tool-config-file is not tool:path")?;
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let toml: toml::Value = text.parse::<toml::Table>().map_err(|e| format!("{path}: {e}"))?.into();
        let path = ["profile", "default", "junit", "path"].iter().try_fold(&toml, |v, k| v.get(k));
        junit = junit.or(path.and_then(|p| p.as_str()).filter(|_| tool == "slimfit").map(String::from));
    }
    let profile = value(args, "--profile").unwrap_or("default".into());
    let configured = std::fs::read_to_string(dir.join(NEXTEST_CONFIG)).unwrap_or_default().parse::<toml::Table>();
    if profile != "default" && configured.ok().and_then(|c| c.get("profile")?.get(&profile).cloned()).is_none() {
        return Err(format!("profile {profile:?} not found in {NEXTEST_CONFIG}"));
    }
    let filters: Vec<String> = values(args, "-E").into_iter().chain(values(args, "--filterset")).collect();
    let reach = std::fs::read_to_string(reports.join(REACH)).unwrap();
    let mut tests = Vec::new();
    for (binary, name) in reach.lines().filter_map(|l| l.strip_prefix("test ")?.split_once('\t')) {
        let hits = filters.iter().map(|f| matches(f, binary, name)).collect::<Result<Vec<bool>, _>>()?;
        if filters.is_empty() || hits.contains(&true) {
            tests.push((binary.to_string(), name.to_string()));
        }
    }
    // nextest extracts an archive only where it is not yet.
    let into = value(args, "--extract-to").map(PathBuf::from);
    if let Some(at) = into.as_ref().map(|i| i.join("target")).filter(|t| t.exists()) {
        return Err(format!("destination `{}` already exists", at.display()));
    }
    if list {
        has(args, "-T")?;
        need(args, "--archive-file")?;
        tests.iter().for_each(|(b, n)| println!("{b} {n}"));
        return Ok(Work::default());
    }
    let listed = tests.len();
    let failed = first_fails && !tests.is_empty() && !args.iter().any(|a| a == "--no-run");
    if failed && stops == Some(1) {
        tests.truncate(1);
    }
    let not_run = listed - tests.len();
    let said = move || {
        if not_run > 0 {
            eprintln!(
                "warning: {not_run}/{listed} tests were not run due to test failure (run with --no-fail-fast to run all tests, or run with --max-fail)"
            );
        }
    };
    let exit = if failed { 100 } else { 0 };
    let Some(archive) = value(args, "--archive-file") else {
        if !args.iter().any(|a| a == "--no-run") {
            tests.iter().for_each(|(b, n)| println!("standin nextest: runs {b} {n}"));
        }
        said();
        return Ok(Work { exit, ..Work::default() });
    };
    has(args, "--workspace-remap")?;
    let into = into.ok_or("no --extract-to")?;
    let text = std::fs::read_to_string(dir.join(&archive)).map_err(|e| format!("--archive-file {archive}: {e}"))?;
    let (path, binary) = text.split_once('\n').ok_or("--archive-file: not a stand-in's archive")?;
    let into = into.canonicalize().map_err(|e| format!("--extract-to {}: {e}", into.display()))?.join(path);
    std::fs::create_dir_all(into.parent().unwrap()).unwrap();
    std::fs::write(&into, binary).unwrap();
    std::fs::set_permissions(&into, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let runner = runner(&need(args, "--config")?)?;
    let junit = junit.ok_or("the nextest config sets no profile.default.junit.path")?;
    let kept = std::fs::read_to_string(reports.join("junit.xml")).unwrap();
    let ran = |l: &&str| !l.contains("<testcase ") || tests.iter().any(|(_, n)| l.contains(&format!("name=\"{n}\"")));
    let report = kept.lines().filter(ran).map(|l| format!("{l}\n")).collect();
    let store = dir.join(NEXTEST_STORE).join(&profile);
    std::fs::create_dir_all(&store).unwrap();
    Ok(Work {
        files: vec![(store.join(junit), report)],
        prepare: vec![Box::new(move || run_tests(&runner, &tests, &into)), Box::new(said)],
        exit,
        ..Work::default()
    })
}

/// nextest's flags of a run's failures; it takes each once, and a number
/// of failures (`--max-fail`) beside no other.
const FAILURE_FLAGS: &[&str] = &["--no-fail-fast", "--nff", "--fail-fast", "--ff", "--max-fail"];

/// After how many failures a run stops: `None`, every test runs. One
/// unless a flag says otherwise; the last of `--fail-fast` and
/// `--no-fail-fast` wins.
fn stops_after(args: &[String]) -> Result<Option<usize>, Refusal> {
    let given: Vec<&str> =
        args.iter().filter_map(|a| FAILURE_FLAGS.iter().find(|f| a.split('=').next() == Some(**f)).copied()).collect();
    if let Some(f) = given.iter().find(|f| given.iter().filter(|g| g == f).count() > 1) {
        return Err(format!("the argument '{f}' cannot be used multiple times"));
    }
    if given.contains(&"--max-fail") && given.len() > 1 {
        return Err("the argument '--max-fail' cannot be used with another flag of failures".into());
    }
    if let Some(n) = value(args, "--max-fail") {
        let n = n.split(':').next().unwrap_or_default();
        return if n == "all" { Ok(None) } else { n.parse().map(Some).map_err(|_| format!("--max-fail {n}")) };
    }
    Ok(if matches!(given.last(), Some(&"--no-fail-fast" | &"--nff")) { None } else { Some(1) })
}

/// Whether filterset `expr` matches test `name` of `binary`: nextest's
/// grammar as far as slimfit's runs write it (`test(=name)`, `test(part)`,
/// `binary_id(=id)`, `all()`, `not`, `&`, `|`, parentheses, `\` escaping
/// in an argument).
fn matches(expr: &str, binary: &str, name: &str) -> Result<bool, Refusal> {
    struct P<'a> {
        rest: &'a str,
        binary: &'a str,
        name: &'a str,
    }
    impl P<'_> {
        fn eat(&mut self, t: &str) -> bool {
            self.rest = self.rest.trim_start();
            self.rest.strip_prefix(t).map(|r| self.rest = r).is_some()
        }
        fn or(&mut self) -> Result<bool, Refusal> {
            let mut v = self.and()?;
            while self.eat("|") {
                v |= self.and()?;
            }
            Ok(v)
        }
        fn and(&mut self) -> Result<bool, Refusal> {
            let mut v = self.atom()?;
            while self.eat("&") {
                v &= self.atom()?;
            }
            Ok(v)
        }
        fn atom(&mut self) -> Result<bool, Refusal> {
            if self.eat("not") {
                return Ok(!self.atom()?);
            }
            if self.eat("all()") {
                return Ok(true);
            }
            if self.eat("(") {
                let v = self.or()?;
                return if self.eat(")") { Ok(v) } else { Err(format!("no `)` before {:?}", self.rest)) };
            }
            let of = if self.eat("test(") {
                self.name
            } else if self.eat("binary_id(") {
                self.binary
            } else {
                return Err(format!("no filter at {:?}", self.rest));
            };
            let mut arg = String::new();
            let mut chars = self.rest.char_indices();
            loop {
                match chars.next() {
                    Some((i, ')')) => break self.rest = &self.rest[i + 1..],
                    Some((_, '\\')) => arg.extend(chars.next().map(|(_, c)| c)),
                    Some((_, c)) => arg.push(c),
                    None => return Err("an unclosed filter".into()),
                }
            }
            Ok(arg.strip_prefix('=').map_or(of.contains(&arg), |a| of == a))
        }
    }
    let mut p = P { rest: expr, binary, name };
    let v = p.or()?;
    if p.rest.trim().is_empty() { Ok(v) } else { Err(format!("filterset {expr:?}: {:?} is left over", p.rest)) }
}

/// The runner `--config` sets for every target (`target.'cfg(all())'.runner
/// = [...]`), as nextest takes it from cargo's configuration.
fn runner(config: &str) -> Result<Vec<String>, Refusal> {
    let table: toml::Table = config.parse().map_err(|e| format!("--config {config}: {e}"))?;
    let runner = table
        .get("target")
        .and_then(|t| t.get("cfg(all())"))
        .and_then(|t| t.get("runner"))
        .and_then(|r| r.as_array())
        .ok_or(format!("--config {config}: no target.'cfg(all())'.runner"))?;
    Ok(runner.iter().filter_map(|v| v.as_str().map(String::from)).collect())
}

/// nextest's run, as the runner sees it: each test run through `runner`
/// with the variables nextest sets, as the extracted `binary`, which
/// writes a profile where `LLVM_PROFILE_FILE` says (holding its test,
/// which the `llvm-cov` stand-in reads back).
fn run_tests(runner: &[String], tests: &[(String, String)], binary: &Path) {
    for (b, name) in tests {
        let ok = std::process::Command::new(&runner[0])
            .args(&runner[1..])
            .arg(binary)
            .env("NEXTEST_BINARY_ID", b)
            .env("NEXTEST_TEST_NAME", name)
            .env("T", format!("{b}\t{name}"))
            .status()
            .unwrap()
            .success();
        assert!(ok, "the runner failed on {b} {name}");
    }
}

/// `llvm-profdata merge -sparse <profile>... -o <out>`: the profiles
/// `run_tests` wrote each hold their test; the merged one holds it too.
/// Merged from a list (`-f`), of every test's: the whole run's, `MERGED`.
fn llvm_profdata(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    sub_is(sub, "merge")?;
    has(args, "-sparse")?;
    let out = need(args, "-o")?;
    if let Some(list) = value(args, "-f") {
        std::fs::read_to_string(&list).map_err(|e| format!("-f {list}: {e}"))?;
        std::fs::write(out, MERGED).unwrap();
        return Ok(Work::default());
    }
    let profile = args.iter().skip_while(|a| *a != "-sparse").nth(1).ok_or("no profile")?;
    std::fs::copy(profile, out).map_err(|e| format!("{profile}: {e}"))?;
    Ok(Work::default())
}

const MERGED: &str = "every test";

/// `llvm-cov export -format=lcov -instr-profile=<profile> -object <binary>...`:
/// of a merged profile, the real report's section for its test; of the
/// whole run's, the real report. Each object must be there.
fn llvm_cov(sub: &str, args: &[String], reports: &Path) -> Result<Work, Refusal> {
    sub_is(sub, "export")?;
    has(args, "-format=lcov")?;
    has(args, "-object")?;
    if let Some(gone) = values(args, "-object").into_iter().find(|o| !Path::new(o).is_file()) {
        return Err(format!("{gone}: No such file or directory"));
    }
    let profile = args.iter().find_map(|a| a.strip_prefix("-instr-profile=")).ok_or("no -instr-profile")?;
    let test = std::fs::read_to_string(profile).map_err(|e| format!("{profile}: {e}"))?;
    if test == MERGED {
        return Ok(Work { stdout: Some("lcov.info".into()), ..Work::default() });
    }
    let reach = std::fs::read_to_string(reports.join(REACH)).unwrap();
    let section: Vec<&str> = reach
        .lines()
        .skip_while(|l| l.strip_prefix("test ") != Some(test.as_str()))
        .skip(1)
        .take_while(|l| !l.starts_with("test "))
        .collect();
    print!("{}", section.iter().map(|l| format!("{}\n", l.replace("/REPO", "."))).collect::<String>());
    Ok(Work::default())
}

/// `go test -json -coverprofile=<file>`: the JSON on stdout. With
/// `-run '^$'` (the build): no test runs, and nothing is written. With `-c
/// -cover -o <binary>`: the package's test binary, this stand-in under it.
/// `go list -f <template> ./...`: the sample's one package, by the import
/// path the real per-test report gives it, and its directory.
fn go(sub: &str, args: &[String], dir: &Path, reports: &Path) -> Result<Work, Refusal> {
    let package = || go_tests(reports)[0].0.clone();
    if sub == "list" {
        need(args, "-f")?;
        println!("{}\t{}", package(), dir.display());
        return Ok(Work::default());
    }
    sub_is(sub, "test")?;
    if args.iter().any(|a| a == "-c") {
        has(args, "-cover")?;
        let binary = PathBuf::from(need(args, "-o")?);
        if args.last() != Some(&package()) {
            return Err(format!("no package {} last", package()));
        }
        let me = std::env::current_exe().unwrap();
        let prepare: Box<dyn FnOnce()> = Box::new(move || std::os::unix::fs::symlink(me, binary).unwrap());
        return Ok(Work { prepare: vec![prepare], ..Work::default() });
    }
    if value(args, "-run").as_deref() == Some("^$") {
        return Ok(Work::default());
    }
    has(args, "-json")?;
    let cover = need(args, "-coverprofile")?;
    Ok(Work { copies: vec![("cover.out".into(), cover.into())], stdout: Some("test.json".into()), ..Work::default() })
}

/// The (package, name) of each test of the sample's real per-test report.
fn go_tests(reports: &Path) -> Vec<(String, String)> {
    let reach = std::fs::read_to_string(reports.join(REACH)).unwrap();
    let tests = reach.lines().filter_map(|l| l.strip_prefix("test ")?.split_once('\t'));
    tests.map(|(p, n)| (p.to_string(), n.to_string())).collect()
}

/// A Go test binary: `-test.list <regexp>` lists the sample's tests;
/// `-test.run ^<name>$ -test.coverprofile=<file>` writes, as that test's
/// profile, the real per-test report's blocks for it.
fn go_test_binary(args: &[String], reports: &Path) -> Result<Work, Refusal> {
    if value(args, "-test.list").is_some() {
        go_tests(reports).iter().for_each(|(_, t)| println!("{t}"));
        return Ok(Work::default());
    }
    let reach = std::fs::read_to_string(reports.join(REACH)).unwrap();
    let run = need(args, "-test.run")?;
    let name =
        run.strip_prefix('^').and_then(|r| r.strip_suffix('$')).ok_or(format!("-test.run {run}: not one test"))?;
    let profile = need(args, "-test.coverprofile")?;
    let section: String = reach
        .lines()
        .skip_while(|l| l.strip_prefix("test ").and_then(|t| t.split_once('\t')).map(|(_, n)| n) != Some(name))
        .skip(1)
        .take_while(|l| !l.starts_with("test "))
        .map(|l| format!("{l}\n"))
        .collect();
    Ok(Work { files: vec![(profile.into(), format!("mode: set\n{section}"))], ..Work::default() })
}

fn gremlins(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    sub_is(sub, "unleash")?;
    Ok(Work { copies: vec![("gremlins.json".into(), need(args, "--output")?.into())], ..Work::default() })
}

/// `swift test`: XCTest's xunit at the path given, swift-testing's beside
/// it, and the build a coverage export reads: the test bundle and the
/// profile, under `.build/debug`, a symlink as SwiftPM makes it.
/// `swift build --build-tests` (the build) writes nothing.
fn swift(sub: &str, args: &[String], dir: &Path) -> Result<Work, Refusal> {
    if sub == "build" {
        has(args, "--build-tests")?;
        return Ok(Work::default());
    }
    sub_is(sub, "test")?;
    has(args, "--enable-code-coverage")?;
    has(args, "--parallel")?;
    let xunit = PathBuf::from(need(args, "--xunit-output")?);
    let testing = xunit.with_file_name(format!("{}-swift-testing.xml", xunit.file_stem().unwrap().to_string_lossy()));
    let build = dir.join(".build");
    let prepare: Box<dyn FnOnce()> = Box::new(move || {
        let real = build.join("arm64-apple-macosx/debug");
        let bundle = real.join("StandinPackageTests.xctest/Contents/MacOS");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("StandinPackageTests"), "").unwrap();
        std::fs::create_dir_all(real.join("codecov")).unwrap();
        std::fs::write(real.join("codecov/default.profdata"), "").unwrap();
        let link = build.join("debug");
        if !link.exists() {
            std::os::unix::fs::symlink("arm64-apple-macosx/debug", link).unwrap();
        }
    });
    Ok(Work {
        copies: vec![("xunit.xml".into(), xunit), ("xunit-swift-testing.xml".into(), testing)],
        prepare: vec![prepare],
        ..Work::default()
    })
}

/// `xcrun llvm-cov export -format=lcov -instr-profile=<p> <binary>`: the
/// LCOV on stdout, when the profile and the binary exist.
fn xcrun(args: &[String], dir: &Path) -> Result<Work, Refusal> {
    if args.first().map(String::as_str) != Some("llvm-cov") || args.get(1).map(String::as_str) != Some("export") {
        return Err("only `xcrun llvm-cov export` stands in".into());
    }
    has(args, "-format=lcov")?;
    let profile = need(args, "-instr-profile")?;
    let binary = args.last().filter(|a| !a.starts_with('-')).ok_or("no binary")?;
    for p in [&profile, binary] {
        if !dir.join(p).is_file() {
            return Err(format!("{p}: no such file"));
        }
    }
    Ok(Work { stdout: Some("lcov.info".into()), ..Work::default() })
}

fn muter(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    match sub {
        "init" => Ok(Work { copies: vec![("../muter.conf.yml".into(), "muter.conf.yml".into())], ..Work::default() }),
        "run" => {
            if value(args, "--format").as_deref() != Some("json") {
                return Err("no --format json".into());
            }
            Ok(Work { copies: vec![("muter-survived.json".into(), need(args, "--output")?.into())], ..Work::default() })
        }
        _ => Err(format!("no subcommand {sub:?}")),
    }
}

/// `./gradlew <task>...`: each task's report where its plugin writes it.
fn gradlew(args: &[String]) -> Result<Work, Refusal> {
    let mut copies = Vec::new();
    for task in args.iter().filter(|a| !a.starts_with('-')) {
        match task.as_str() {
            "test" => copies.push((
                "junit-com.example.CalcTest.xml".into(),
                PathBuf::from(GRADLE_TEST_RESULTS).join("TEST-com.example.CalcTest.xml"),
            )),
            "koverXmlReport" => copies.push(("kover.xml".into(), KOVER_XML.into())),
            "pitest" => copies.push(("mutations.xml".into(), PITEST_XML.into())),
            "testClasses" => {}
            _ => return Err(format!("Task '{task}' not found in root project")),
        }
    }
    Ok(Work { copies, ..Work::default() })
}

/// `npx vitest run ...` and `npx stryker run ...`.
fn npx(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    let run = args.get(1).map(String::as_str) == Some("run");
    match sub {
        "vitest" if run => {
            has(args, "--coverage.enabled")?;
            if value(args, "--coverage.reporter").as_deref() != Some("lcov") {
                return Err("no --coverage.reporter=lcov".into());
            }
            if value(args, "--reporter").as_deref() != Some("junit") {
                return Err("no --reporter=junit".into());
            }
            let cov = PathBuf::from(need(args, "--coverage.reportsDirectory")?);
            Ok(Work {
                copies: vec![
                    ("junit.xml".into(), need(args, "--outputFile")?.into()),
                    ("lcov.info".into(), cov.join("lcov.info")),
                ],
                ..Work::default()
            })
        }
        "stryker" if run => {
            if value(args, "--reporters").as_deref() != Some("json") {
                return Err("no --reporters json".into());
            }
            Ok(Work { copies: vec![("mutation.json".into(), STRYKER_JSON.into())], ..Work::default() })
        }
        _ => Err(format!("no `{sub} run`")),
    }
}
