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
    /// A sample's stop, when the run holds more mutants than it wants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
}

/// A run of a sample's draw and its replacements, all at once (ADR 0011):
/// the sample is the first `viable` of the draw that build, so a mutant
/// is not built once that many of those drawn before it have built.
/// cargo-mutants runs them in its own order, not the draw's, so one
/// drawn later may build before the draw is known to need it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub viable: usize,
    /// The ranks known to build before this run (another suite's run).
    pub known: Vec<usize>,
    pub mutants: Vec<PlanMutant>,
}

/// A drawn mutant, by its rank in the draw, and the text a build directory
/// holds with it applied: the lines its diff puts in, from line `start` of
/// `file`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanMutant {
    pub rank: usize,
    pub file: String,
    pub start: u32,
    pub text: Vec<String>,
}

/// The drawn mutants of a listing (`--list --json`), each with the lines
/// its diff puts in; `rank` gives a name's rank in the draw, `None` if not drawn.
pub fn drawn(listed: &str, rank: impl Fn(&str) -> Option<usize>) -> Result<Vec<PlanMutant>> {
    let listed: serde_json::Value = serde_json::from_str(listed).context("cargo mutants --list --json")?;
    let mutant = |m: &serde_json::Value| {
        let rank = rank(m["name"].as_str()?)?;
        let mut lines = m["diff"].as_str()?.lines().skip_while(|l| !l.starts_with("@@ "));
        let header = lines.next()?;
        let mut at: u32 = header.split(" +").nth(1)?.split([',', ' ']).next()?.parse().ok()?;
        // The new side's changed lines, from the first: context before them counts.
        let mut lines = lines.filter(|l| !l.starts_with('-')).peekable();
        while lines.next_if(|l| !l.starts_with('+')).is_some() {
            at += 1;
        }
        let text = lines.map_while(|l| l.strip_prefix('+')).map(String::from).collect();
        Some(PlanMutant { rank, file: m["file"].as_str()?.into(), start: at, text })
    };
    Ok(listed.as_array().into_iter().flatten().filter_map(mutant).collect())
}

impl Budget {
    /// The drawn mutant applied in build directory `dir`, if one is.
    fn applied(&self, dir: &Path) -> Option<usize> {
        self.mutants
            .iter()
            .find(|m| {
                let Ok(text) = std::fs::read_to_string(dir.join(&m.file)) else { return false };
                let at = text.lines().skip(m.start as usize - 1);
                at.take(m.text.len()).eq(m.text.iter().map(String::as_str))
            })
            .map(|m| m.rank)
    }

    /// Whether a mutant of `rank` need not build, `built` the ranks known
    /// to have built: the draw before it already holds enough that build.
    fn spent(&self, rank: usize, built: impl Iterator<Item = usize>) -> bool {
        let before: BTreeSet<usize> = built.chain(self.known.iter().copied()).filter(|r| *r < rank).collect();
        before.len() >= self.viable
    }
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
        let tests = if ran.is_empty() || crate::rust_source::is_const(&source, start, end) {
            None
        } else {
            Some(ran.into_iter().flatten().copied().collect::<BTreeSet<u32>>().into_iter().collect())
        };
        functions.push(PlanFunction { file: file.into(), start, end, tests });
    }
    Ok(Plan { root: root.into(), tests: plan_tests, functions, budget: None })
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

/// `filter` within the filtersets `args` give before `--` (a suite's
/// `test_args`), which it takes out of them: nextest runs the tests of any
/// one of its `-E`s, so another beside them would add theirs.
fn within(args: &mut Vec<OsString>, filter: String) -> String {
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut given = Vec::new();
    let mut rest = args.drain(..end).collect::<Vec<_>>().into_iter();
    let mut kept = Vec::new();
    while let Some(a) = rest.next() {
        let a = a.to_string_lossy().into_owned();
        match a.strip_prefix("--filterset=") {
            Some(v) => given.push(v.to_string()),
            None if a == "-E" || a == "--filterset" => given.extend(rest.next().map(|v| v.to_string_lossy().into())),
            None => kept.push(a.into()),
        }
    }
    args.splice(..0, kept);
    if given.is_empty() { filter } else { format!("(({})) & ({filter})", given.join(") | (")) }
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
    let mut args: Vec<OsString> = args.into_iter().skip(1).collect();
    let test_run = args.first().is_some_and(|a| a == "nextest")
        && args.get(1).is_some_and(|a| a == "run")
        && !args.iter().any(|a| a == "--no-run");
    let build = args.first().is_some_and(|a| a == "nextest") && args.iter().any(|a| a == "--no-run");
    if build {
        std::process::exit(budgeted(&args));
    }
    test_run.then(|| narrow(&mut args));
    // Not exec: this process must exit to write its own coverage profile.
    std::process::exit(real_or_fail(&args));
}

/// A test run's `args`, narrowed to the mutant's tests; none to run: exit.
fn narrow(args: &mut Vec<OsString>) {
    match narrowed(args) {
        Ok(Some(extra)) => {
            let at = args.iter().position(|a| a == "--").unwrap_or(args.len());
            args.splice(at..at, extra);
        }
        Ok(None) => std::process::exit(0),
        Err(e) => eprintln!("slimfit: {e:#}: every test runs"),
    }
}

/// A build (`--no-run`): not made, and failed, when the plan's budget is
/// spent before the drawn mutant applied; else the real one, recording a
/// drawn mutant that built.
fn budgeted(args: &[OsString]) -> i32 {
    let plan = std::env::var_os(PLAN_ENV).map(PathBuf::from);
    let read = |p: &PathBuf| serde_json::from_str::<Plan>(&std::fs::read_to_string(p).ok()?).ok();
    let budget = plan.as_ref().and_then(|p| Some((p.parent()?.to_path_buf(), read(p)?.budget?)));
    let rank = budget.as_ref().and_then(|(_, b)| b.applied(&std::env::current_dir().ok()?));
    let (Some((dir, budget)), Some(rank)) = (budget, rank) else { return real_or_fail(args) };
    if budget.spent(rank, built(&dir)) {
        eprintln!(
            "slimfit: drawn mutant {rank}: {} drawn before it built: not built, and not in the sample",
            budget.viable
        );
        return 1;
    }
    eprintln!("slimfit: drawn mutant {rank}: building");
    let code = real_or_fail(args);
    let _ = (code == 0).then(|| std::fs::write(dir.join(format!("built-{rank}")), ""));
    code
}

/// The ranks recorded as built in `dir`.
fn built(dir: &Path) -> impl Iterator<Item = usize> {
    let names = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.file_name());
    names.filter_map(|n| n.to_str()?.strip_prefix("built-")?.parse().ok())
}

fn real_or_fail(args: &[OsString]) -> i32 {
    real(args).unwrap_or_else(|e| {
        eprintln!("slimfit: {e:#}");
        101
    })
}

/// Runs the real nextest with `args`: its exit code.
fn real(args: &[OsString]) -> Result<i32> {
    let real =
        std::env::var_os(NEXTEST_ENV).context(format!("{NEXTEST_ENV} is not set: the real {SHIM} is unknown"))?;
    let status = std::process::Command::new(&real).args(args).status();
    Ok(status.with_context(|| format!("running {}", Path::new(&real).display()))?.code().unwrap_or(101))
}

/// The arguments that narrow this run (`args`, whose own filtersets they
/// take in) to the mutant's tests, each under its timeout; `None` when no
/// test runs the mutant's function, so none runs.
fn narrowed(args: &mut Vec<OsString>) -> Result<Option<Vec<OsString>>> {
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
        extra.extend(["-E".into(), within(args, filter(&plan, c)).into()]);
    }
    Ok(Some(extra))
}

/// The real `cargo-nextest`, where cargo would find it, outside `shim_dir`.
pub fn real_nextest(shim_dir: &Path) -> Option<PathBuf> {
    let path: Vec<PathBuf> = crate::doctor::tool_path().into_iter().filter(|d| d != shim_dir).collect();
    crate::doctor::find(&path, SHIM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReachTest;

    fn reach() -> Reach {
        let t = |n: &str| ReachTest { binary: "calc".into(), name: n.into(), source: None };
        Reach {
            tests: vec![t("tests::adds"), t("tests::adult"), t("tests::slow")],
            functions: [("sub/src/lib.rs".into(), [(1, vec![0, 2]), (5, vec![]), (11, vec![1]), (12, vec![2])].into())]
                .into(),
            lines: None,
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
        let tests = vec![TestCase { id: "calc::tests::slow".into(), seconds: 0.62, passed: true, cpu: 0.0 }];
        let p = plan(dir.path(), &listed, |f| format!("sub/{f}"), &reach(), &tests).unwrap();
        let got: Vec<(u32, u32, Option<Vec<u32>>)> =
            p.functions.iter().map(|f| (f.start, f.end, f.tests.clone())).collect();
        assert_eq!(
            got,
            [(1, 3, Some(vec![0, 2])), (5, 9, Some(vec![])), (11, 13, None), (15, 17, None), (19, 21, None)]
        );
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
    /// A sample's build is found by the lines its diff puts in (the real
    /// list's), and not made once as many drawn before it as the sample
    /// wants have built, here or in an earlier run.
    #[test]
    fn a_sample_builds_none_once_enough_drawn_before_it_built() {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rust");
        let list = std::fs::read_to_string(sample.join("reports/mutants.list.json")).unwrap();
        let names = ["src/lib.rs:12:9: replace >= with < in is_adult", "src/lib.rs:2:5: replace add -> i64 with 1"];
        let mutants = drawn(&list, |n| names.iter().position(|d| *d == n)).unwrap();
        let b = Budget { viable: 2, known: vec![], mutants };
        let build = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(build.path().join("src")).unwrap();
        let own = std::fs::read_to_string(sample.join("src/lib.rs")).unwrap();
        let applied = |from: &str, to: &str| {
            std::fs::write(build.path().join("src/lib.rs"), own.replacen(from, to, 1)).unwrap();
            b.applied(build.path())
        };
        assert_eq!(applied("age >= 18", "age < /* ~ changed by cargo-mutants ~ */ 18"), Some(0));
        assert_eq!(applied("    a + b", "    1 /* ~ changed by cargo-mutants ~ */"), Some(1));
        assert_eq!(applied("    a + b", "    0 /* ~ changed by cargo-mutants ~ */"), None);
        assert!(!b.spent(1, [0, 3].into_iter()));
        assert!(b.spent(4, [0, 3].into_iter()));
        assert!(Budget { known: vec![2], ..b }.spent(3, [0].into_iter()));
    }

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
            budget: None,
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
        let gone = PlanFunction { file: "gone.rs".into(), start: 1, end: 9, tests: Some(vec![0]) };
        let plan = Plan { functions: vec![gone, f(5, 7, Some(vec![]))], ..plan };
        assert_eq!(choose(&plan, build.path()).0, None, "line 8 again");
        assert_eq!(choose_at(&plan, "    2", "    3").0, Some(BTreeSet::new()));

        let config = timeouts(&plan, Some(&[2].into()));
        assert_eq!(
            config,
            "[[profile.default.overrides]]\nfilter = \"binary_id(=calc) & test(=z)\"\n\
             slow-timeout = { period = \"7s\", terminate-after = 1 }\n\n"
        );
        // A run's own filtersets (a suite's `test_args`) hold the chosen tests in.
        let mut args: Vec<OsString> =
            ["run", "-E", "a", "--profile", "fast", "--filterset=b", "--", "-E", "c"].map(Into::into).into();
        assert_eq!(within(&mut args, "x".into()), "((a) | (b)) & (x)");
        assert_eq!(args, ["run", "--profile", "fast", "--", "-E", "c"].map(OsString::from));
        assert_eq!(within(&mut args, "x".into()), "x");
        let all: toml::Table = timeouts(&plan, None).parse().unwrap();
        assert_eq!(all["profile"]["default"]["overrides"].as_array().unwrap().len(), 3);
    }
}
