//! Which tests each mutant runs (ADR 0007). A test that never runs the
//! mutated function cannot fail on it, so each mutant runs only the tests
//! the test run saw run its function (`Reach`), each under a timeout from
//! its own duration; a mutant whose function no test runs runs none, and is
//! missed once it builds.
//!
//! cargo-mutants runs one test command for every mutant, so the choice is
//! made where that command lands: slimfit puts itself first on PATH as
//! `cargo-nextest` (the shim). For each `cargo nextest run` cargo-mutants
//! makes, the shim finds the mutant by comparing the build directory with
//! the suite's own files (the first line that differs), takes the tests of
//! the functions around that line from the plan slimfit wrote, and runs the
//! real nextest on only those. Whatever it cannot tell (no file differs, a
//! line in no listed function, a function no profile names) runs every
//! test: never fewer tests than a mutant may need.

use crate::model::{Reach, TestCase};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

/// The environment variable naming the plan the shim reads.
pub const PLAN_ENV: &str = "SLIMFIT_SELECT";
/// The environment variable naming the real `cargo-nextest`.
pub const NEXTEST_ENV: &str = "SLIMFIT_NEXTEST";
/// The name the shim answers to.
pub const SHIM: &str = "cargo-nextest";

/// A test's timeout on a mutant: this many seconds, plus `TIMEOUT_FACTOR`
/// times its duration in the test run. A test the timeout ends fails: the
/// mutant is caught.
pub const TIMEOUT_FLOOR: f64 = 5.0;
pub const TIMEOUT_FACTOR: f64 = 5.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// The suite's root: the unmutated files a build directory is compared with.
    pub root: PathBuf,
    pub tests: Vec<PlanTest>,
    pub functions: Vec<PlanFunction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTest {
    pub binary: String,
    pub name: String,
    /// Seconds.
    pub timeout: u64,
}

/// A function some mutant is in, as cargo-mutants lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanFunction {
    /// Relative to the suite's root.
    pub file: String,
    pub start: u32,
    pub end: u32,
    /// The tests that ran it, by index into `tests`; `None`: unknown, every
    /// test runs.
    pub tests: Option<Vec<u32>>,
}

/// The plan for the mutants cargo-mutants lists (`--list --json`): each
/// listed function's tests are those that ran a function the coverage
/// report starts within its lines (closures and nested functions
/// included). `repo_path` maps a listed file to its path in the repository.
pub fn plan(
    root: &Path,
    listed: &str,
    repo_path: impl Fn(&str) -> String,
    reach: &Reach,
    tests: &[TestCase],
) -> Result<Plan> {
    let listed: serde_json::Value = serde_json::from_str(listed).context("cargo mutants --list --json")?;
    let seconds = |id: &str| tests.iter().find(|t| t.id == id).map(|t| t.seconds);
    let longest = tests.iter().map(|t| t.seconds).fold(0.0, f64::max);
    let plan_tests = reach
        .tests
        .iter()
        .map(|t| PlanTest {
            binary: t.binary.clone(),
            name: t.name.clone(),
            timeout: (TIMEOUT_FLOOR + TIMEOUT_FACTOR * seconds(&t.id()).unwrap_or(longest)).ceil() as u64,
        })
        .collect();
    let mut functions: Vec<PlanFunction> = Vec::new();
    for m in listed.as_array().into_iter().flatten() {
        let (Some(file), span) = (m["file"].as_str(), &m["function"]["span"]) else { continue };
        let line = |at: &str| span[at]["line"].as_u64().map(|l| l as u32);
        let (Some(start), Some(end)) = (line("start"), line("end")) else { continue };
        if functions.iter().any(|f| f.file == file && f.start == start && f.end == end) {
            continue;
        }
        let ran: Vec<&Vec<u32>> = reach
            .functions
            .get(&repo_path(file))
            .map(|f| f.range(start..=end).map(|(_, t)| t).collect())
            .unwrap_or_default();
        let source = std::fs::read_to_string(root.join(file)).unwrap_or_default();
        let tests = if ran.is_empty() || is_const(&source, start, end) {
            None
        } else {
            Some(ran.into_iter().flatten().copied().collect::<BTreeSet<u32>>().into_iter().collect())
        };
        functions.push(PlanFunction { file: file.into(), start, end, tests });
    }
    Ok(Plan { root: root.into(), tests: plan_tests, functions })
}

/// Whether the function in lines `start..=end` is a `const fn`: one
/// evaluated while compiling, which no test's profile shows running.
fn is_const(source: &str, start: u32, end: u32) -> bool {
    let lines = source.lines().skip(start.saturating_sub(1) as usize).take((end + 1).saturating_sub(start) as usize);
    let code: String = lines
        .map(str::trim_start)
        .filter(|l| !l.starts_with("//") && !l.starts_with("#["))
        .collect::<Vec<_>>()
        .join(" ");
    let signature = code.split('{').next().unwrap_or("");
    let words: Vec<&str> = signature.split_whitespace().collect();
    words.windows(2).any(|w| w[0] == "const" && ["fn", "unsafe", "extern", "async"].contains(&w[1]))
}

/// The tests to run in build directory `dir`, and why: `None` runs every
/// test.
pub fn choose(plan: &Plan, dir: &Path) -> (Option<BTreeSet<u32>>, String) {
    let files: BTreeSet<&str> = plan.functions.iter().map(|f| f.file.as_str()).collect();
    for file in files {
        let (Ok(mutated), Ok(own)) =
            (std::fs::read_to_string(dir.join(file)), std::fs::read_to_string(plan.root.join(file)))
        else {
            continue;
        };
        let Some(line) = first_difference(&own, &mutated) else { continue };
        let around: Vec<&PlanFunction> =
            plan.functions.iter().filter(|f| f.file == file && f.start <= line && line <= f.end).collect();
        if around.is_empty() {
            return (None, format!("{file}:{line} is in no function cargo-mutants listed: every test runs"));
        }
        if around.iter().any(|f| f.tests.is_none()) {
            return (None, format!("{file}:{line}: no test profile names its function: every test runs"));
        }
        let chosen: BTreeSet<u32> = around.iter().flat_map(|f| f.tests.iter().flatten().copied()).collect();
        let why = format!("{file}:{line}: {} of {} tests run its function", chosen.len(), plan.tests.len());
        return (Some(chosen), why);
    }
    (None, "no file differs from the suite's own: every test runs".into())
}

/// The first line (from 1) where `b` differs from `a`.
fn first_difference(a: &str, b: &str) -> Option<u32> {
    if a == b {
        return None;
    }
    let (mut x, mut y) = (a.lines(), b.lines());
    let mut n = 1;
    loop {
        match (x.next(), y.next()) {
            (Some(p), Some(q)) if p == q => n += 1,
            _ => return Some(n),
        }
    }
}

/// A nextest filterset matching exactly the tests `chosen`.
pub fn filter(plan: &Plan, chosen: &BTreeSet<u32>) -> String {
    chosen
        .iter()
        .filter_map(|i| plan.tests.get(*i as usize))
        .map(|t| format!("(binary_id(={}) & test(={}))", escape(&t.binary), escape(&t.name)))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace(',', "\\,").replace(')', "\\)")
}

/// nextest configuration giving each test of `chosen` (every test: `None`)
/// its own timeout, after which it is ended and fails.
pub fn timeouts(plan: &Plan, chosen: Option<&BTreeSet<u32>>) -> String {
    let mut out = String::new();
    for (i, t) in plan.tests.iter().enumerate() {
        if chosen.is_some_and(|c| !c.contains(&(i as u32))) {
            continue;
        }
        let filter = format!("binary_id(={}) & test(={})", escape(&t.binary), escape(&t.name));
        out.push_str(&format!(
            "[[profile.default.overrides]]\nfilter = {}\nslow-timeout = {{ period = \"{}s\", terminate-after = 1 }}\n\n",
            toml::Value::String(filter),
            t.timeout
        ));
    }
    out
}

/// The shim, run by cargo as `cargo-nextest nextest <args>`: a test run
/// (`nextest run`, not `--no-run`) runs only the mutant's tests; anything
/// else goes to the real nextest as it came.
pub fn shim(args: Vec<OsString>) -> ! {
    let Some(real) = std::env::var_os(NEXTEST_ENV) else {
        eprintln!("slimfit: {NEXTEST_ENV} is not set: the real cargo-nextest is unknown");
        std::process::exit(101);
    };
    let mut args: Vec<OsString> = args.into_iter().skip(1).collect();
    let test_run = args.first().is_some_and(|a| a == "nextest")
        && args.get(1).is_some_and(|a| a == "run")
        && !args.iter().any(|a| a == "--no-run");
    if test_run {
        match narrowed(&args) {
            Ok(Some(extra)) => {
                let at = args.iter().position(|a| a == "--").unwrap_or(args.len());
                args.splice(at..at, extra);
            }
            Ok(None) => std::process::exit(0),
            Err(e) => eprintln!("slimfit: {e:#}: every test runs"),
        }
    }
    let e = std::process::Command::new(&real).args(&args).exec();
    eprintln!("slimfit: running {}: {e}", Path::new(&real).display());
    std::process::exit(101);
}

/// The arguments that narrow this run to the mutant's tests, each under its
/// timeout; `None` when no test runs the mutant's function, so none runs.
fn narrowed(_args: &[OsString]) -> Result<Option<Vec<OsString>>> {
    let path = PathBuf::from(std::env::var_os(PLAN_ENV).context(format!("{PLAN_ENV} is not set"))?);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let plan: Plan = serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    let (chosen, why) = choose(&plan, &std::env::current_dir()?);
    eprintln!("slimfit: {why}");
    if chosen.as_ref().is_some_and(BTreeSet::is_empty) {
        return Ok(None);
    }
    let config = path.with_file_name(format!("nextest-{}.toml", std::process::id()));
    std::fs::write(&config, timeouts(&plan, chosen.as_ref()))?;
    let mut extra: Vec<OsString> = vec!["--tool-config-file".into(), format!("slimfit:{}", config.display()).into()];
    if let Some(c) = &chosen {
        extra.extend(["-E".into(), filter(&plan, c).into()]);
    }
    Ok(Some(extra))
}

/// The real `cargo-nextest`: on PATH outside `shim_dir`, else in cargo's
/// bin directory, as cargo looks for its subcommands.
pub fn real_nextest(shim_dir: &Path) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")));
    std::env::split_paths(&path)
        .filter(|d| d != shim_dir)
        .chain(cargo_home.map(|h| h.join("bin")))
        .map(|d| d.join(SHIM))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReachTest;

    fn reach() -> Reach {
        let t = |n: &str| ReachTest { binary: "calc".into(), name: n.into() };
        Reach {
            tests: vec![t("tests::adds"), t("tests::adult"), t("tests::slow")],
            functions: [("sub/src/lib.rs".into(), [(1, vec![0, 2]), (5, vec![]), (11, vec![1]), (12, vec![2])].into())]
                .into(),
        }
    }

    /// The real list of the Rust sample's mutants, read into a plan: a
    /// function's tests are those that ran a function starting in its
    /// lines; one no profile names, or a `const fn`, is unknown; each test's
    /// timeout comes from its own duration.
    #[test]
    fn plans_each_listed_functions_tests() {
        let dir = tempfile::tempdir().unwrap();
        let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rust");
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let lib = std::fs::read_to_string(sample.join("src/lib.rs")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), lib.replace("pub fn is_adult", "pub const fn is_adult")).unwrap();
        let listed = std::fs::read_to_string(sample.join("reports/mutants.list.json")).unwrap();
        let tests = vec![TestCase { id: "calc::tests::slow".into(), seconds: 0.62, passed: true, start: None }];
        let p = plan(dir.path(), &listed, |f| format!("sub/{f}"), &reach(), &tests).unwrap();
        let got: Vec<(u32, u32, Option<Vec<u32>>)> =
            p.functions.iter().map(|f| (f.start, f.end, f.tests.clone())).collect();
        assert_eq!(got, [(1, 3, Some(vec![0, 2])), (5, 9, Some(vec![])), (11, 13, None)]);
        assert_eq!(p.tests.iter().map(|t| t.timeout).collect::<Vec<_>>(), [9, 9, 9]);
        let mut unknown = reach();
        unknown.functions.clear();
        let p = plan(dir.path(), &listed, |f| f.into(), &unknown, &tests).unwrap();
        assert!(p.functions.iter().all(|f| f.tests.is_none()), "{p:?}");
    }

    /// The mutant is found at the first line that differs from the suite's
    /// own file; its function's tests run, each under its timeout. A line
    /// in no function, a function of unknown tests, or no difference at
    /// all: every test.
    #[test]
    fn chooses_the_tests_of_the_mutated_function() {
        let (own, build) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let text = "fn a() {\n    1\n}\n\nfn b() {\n    2\n}\n// c\n";
        for d in [&own, &build] {
            std::fs::write(d.path().join("lib.rs"), text).unwrap();
        }
        let f = |start, end, tests: Option<Vec<u32>>| PlanFunction { file: "lib.rs".into(), start, end, tests };
        let t = |b: &str, n: &str| PlanTest { binary: b.into(), name: n.into(), timeout: 7 };
        let plan = Plan {
            root: own.path().into(),
            tests: vec![t("calc", "x"), t("calc::it", "a,b)\\"), t("calc", "z")],
            functions: vec![f(1, 3, Some(vec![0, 1])), f(5, 7, None), f(5, 5, Some(vec![]))],
        };
        let choose_at = |plan: &Plan, from: &str, to: &str| {
            std::fs::write(build.path().join("lib.rs"), text.replacen(from, to, 1)).unwrap();
            choose(plan, build.path())
        };
        assert_eq!(choose_at(&plan, "", "").0, None);
        let (chosen, why) = choose_at(&plan, "    1", "    0 /* ~ changed by cargo-mutants ~ */");
        assert_eq!((chosen.clone(), why.as_str()), (Some([0, 1].into()), "lib.rs:2: 2 of 3 tests run its function"));
        assert_eq!(
            filter(&plan, &chosen.unwrap()),
            r"(binary_id(=calc) & test(=x)) | (binary_id(=calc::it) & test(=a\,b\)\\))"
        );
        assert_eq!(choose_at(&plan, "    2", "    3").0, None, "a function of unknown tests");
        assert!(choose_at(&plan, "// c", "// d").1.contains("lib.rs:8 is in no function"));
        let plan = Plan { functions: vec![f(5, 7, Some(vec![]))], ..plan };
        assert_eq!(choose(&plan, build.path()).0, None, "line 8 again");
        assert_eq!(choose_at(&plan, "    2", "    3").0, Some(BTreeSet::new()));

        let config = timeouts(&plan, Some(&[2].into()));
        assert_eq!(
            config,
            "[[profile.default.overrides]]\nfilter = \"binary_id(=calc) & test(=z)\"\n\
             slow-timeout = { period = \"7s\", terminate-after = 1 }\n\n"
        );
        let all: toml::Table = timeouts(&plan, None).parse().unwrap();
        assert_eq!(all["profile"]["default"]["overrides"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_const_fn_is_known_by_its_signature() {
        let src = "/// a const fn\n#[inline]\npub const fn a() -> u8 {\n    1\n}\nfn b() { const fn c() {} }\n";
        assert!(is_const(src, 1, 5));
        assert!(!is_const(src, 6, 6));
    }
}
