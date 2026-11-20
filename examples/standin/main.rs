//! Stand-ins for the tools slimfit drives, for slimfit's own tests: one
//! binary, linked under each tool's name (`cargo`, `cargo-nextest`,
//! `llvm-profdata`, `llvm-cov`, `go`, `gremlins`, `swift`, `xcrun`, `muter`, `npx`,
//! `gradlew`) into a directory placed
//! first on PATH. It reads the arguments as the real tool does, refuses
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
        "cargo-nextest" => nextest(args),
        "go" => go(&sub, args),
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

fn need(args: &[String], name: &str) -> Result<String, Refusal> {
    value(args, name).ok_or_else(|| format!("no {name}"))
}

fn has(args: &[String], flag: &str) -> Result<(), Refusal> {
    if args.iter().any(|a| a == flag) { Ok(()) } else { Err(format!("no {flag}")) }
}

fn sub_is(sub: &str, want: &str) -> Result<(), Refusal> {
    if sub == want { Ok(()) } else { Err(format!("no subcommand {sub:?}")) }
}

/// `cargo llvm-cov nextest`, `cargo llvm-cov show-env` and `cargo mutants`.
fn cargo(sub: &str, args: &[String], dir: &Path, reports: &Path) -> Result<Work, Refusal> {
    match sub {
        // The environment the test run builds with, as the mutants command reads it.
        "llvm-cov" if args.get(1).map(String::as_str) == Some("show-env") => {
            has(args, "--sh")?;
            Ok(Work { stdout: Some("show-env.sh".into()), ..Work::default() })
        }
        "llvm-cov" => {
            if args.get(1).map(String::as_str) != Some("nextest") {
                return Err("only `llvm-cov nextest` and `llvm-cov show-env` stand in".into());
            }
            has(args, "--lcov")?;
            let lcov = need(args, "--output-path")?;
            // nextest takes a tool's config file as `tool:path`.
            let config = need(args, "--tool-config-file")?;
            let (_, config) = config.split_once(':').ok_or("--tool-config-file is not tool:path")?;
            let text = std::fs::read_to_string(config).map_err(|e| format!("{config}: {e}"))?;
            let toml: toml::Table = text.parse().map_err(|e| format!("{config}: {e}"))?;
            let junit = toml
                .get("profile")
                .and_then(|p| p.get("default"))
                .and_then(|p| p.get("junit"))
                .and_then(|j| j.get("path"))
                .and_then(|p| p.as_str())
                .ok_or("the nextest config sets no profile.default.junit.path")?;
            let mut prepare: Vec<Box<dyn FnOnce()>> = Vec::new();
            if let Some(config) = value(args, "--config") {
                let runner = runner(&config)?;
                let (reports, dir) = (reports.to_path_buf(), dir.to_path_buf());
                prepare.push(Box::new(move || run_tests(&runner, &reports, &dir)));
            }
            Ok(Work {
                copies: vec![
                    ("junit.xml".into(), dir.join(NEXTEST_DIR).join(junit)),
                    ("lcov.info".into(), lcov.into()),
                ],
                prepare,
                ..Work::default()
            })
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
            Ok(Work {
                copies: vec![("outcomes.json".into(), outcomes)],
                exit: if missed { 2 } else { 0 },
                prepare: vec![Box::new(move || test_mutants(&reports, &dir, &out, skip, &only))],
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
/// nextest run --no-run`, then, if that built, `cargo nextest run`; the
/// names of those that did not build are written to `kept.unviable`. Only
/// when there is a `cargo-nextest` to run.
fn test_mutants(reports: &Path, dir: &Path, out: &Path, skip: bool, kept: &Kept) {
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
    files.iter().for_each(|f| copy(f));
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

/// `cargo-nextest nextest run`: its tool configuration files must be
/// nextest's TOML; every test it is given passes.
fn nextest(args: &[String]) -> Result<Work, Refusal> {
    if args.first().map(String::as_str) != Some("nextest") || args.get(1).map(String::as_str) != Some("run") {
        return Err("only `nextest run` stands in".into());
    }
    for (i, _) in args.iter().enumerate().filter(|(_, a)| *a == "--tool-config-file") {
        let config = args.get(i + 1).ok_or("--tool-config-file needs tool:path")?;
        let (_, path) = config.split_once(':').ok_or("--tool-config-file is not tool:path")?;
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        text.parse::<toml::Table>().map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(Work::default())
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

/// nextest's run, as the runner sees it: each test of the real reach
/// report, run through `runner` with the variables nextest sets, as a
/// process that writes a profile where `LLVM_PROFILE_FILE` says (holding
/// its test, which the `llvm-cov` stand-in reads back).
fn run_tests(runner: &[String], reports: &Path, dir: &Path) {
    let target = dir.join("target/llvm-cov-target");
    std::fs::create_dir_all(&target).unwrap();
    let reach = std::fs::read_to_string(reports.join(REACH)).unwrap();
    for test in reach.lines().filter_map(|l| l.strip_prefix("test ")) {
        let (binary, name) = test.split_once('\t').unwrap();
        let ok = std::process::Command::new(&runner[0])
            .args(&runner[1..])
            .args(["sh", "-c", "printf %s \"$T\" > \"${LLVM_PROFILE_FILE%-%p-%m.profraw}-$$-1.profraw\""])
            .env("NEXTEST_BINARY_ID", binary)
            .env("NEXTEST_TEST_NAME", name)
            .env("LLVM_PROFILE_FILE", target.join("calc-%p-%m.profraw"))
            .env("T", test)
            .status()
            .unwrap()
            .success();
        assert!(ok, "the runner failed on {test}");
    }
}

/// `llvm-profdata merge -sparse <profile>... -o <out>`: the profiles
/// `run_tests` wrote each hold their test; the merged one holds it too.
fn llvm_profdata(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    sub_is(sub, "merge")?;
    has(args, "-sparse")?;
    let out = need(args, "-o")?;
    let profile = args.iter().skip_while(|a| *a != "-sparse").nth(1).ok_or("no profile")?;
    std::fs::copy(profile, out).map_err(|e| format!("{profile}: {e}"))?;
    Ok(Work::default())
}

/// `llvm-cov export -format=lcov -instr-profile=<profile> -object <binary>...`:
/// of a merged profile, the real report's section for its test.
fn llvm_cov(sub: &str, args: &[String], reports: &Path) -> Result<Work, Refusal> {
    sub_is(sub, "export")?;
    has(args, "-format=lcov")?;
    has(args, "-object")?;
    let profile = args.iter().find_map(|a| a.strip_prefix("-instr-profile=")).ok_or("no -instr-profile")?;
    let test = std::fs::read_to_string(profile).map_err(|e| format!("{profile}: {e}"))?;
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
/// `-run '^$'` (the build): no test runs, and nothing is written.
fn go(sub: &str, args: &[String]) -> Result<Work, Refusal> {
    sub_is(sub, "test")?;
    if value(args, "-run").as_deref() == Some("^$") {
        return Ok(Work::default());
    }
    has(args, "-json")?;
    let cover = need(args, "-coverprofile")?;
    Ok(Work { copies: vec![("cover.out".into(), cover.into())], stdout: Some("test.json".into()), ..Work::default() })
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
