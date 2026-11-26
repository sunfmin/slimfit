//! Measuring the working tree: run each suite's commands, read their
//! reports, and reduce them to `Measurement`. The result is cached under
//! `.slimfit/out/` and reused while the tree is unchanged.

use crate::config::{Config, Preset, Report, Suite};
use crate::fault::{self, Faults, fault};
use crate::formats;
use crate::landing::SuiteTree;
use crate::loc::{Kind, Totals};
use crate::model::*;
use crate::{go_source, python_source, rust_source};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

mod mutants;
mod removed;
pub mod sample;

use mutants::{Change, Mutation, Picked};
pub use mutants::{Mutants, candidates, refuse_unpickable};

pub const OUT: &str = ".slimfit/out";

/// What every measuring command may refuse, as the command pages say it.
pub fn refusals_help() -> String {
    format!(
        "It refuses with exit {} and `{{\"error\": [{{\"fault\": \"...\", \"fix\": \"...\"}}, ...]}}`: every fault it found at once, each with the `fix` that clears it. Faults: a bad command line; not in a git repository; no `slimfit.toml` (`slimfit init` writes one), or one that is not valid; a `.slimfit/baseline.json` it cannot read; a suite's tool missing, or writing no report: the fault names the report it looked for (`no tests_report at ...`) and ends with the command's log. A failing test is measured, not refused: it counts in `failures`.",
        crate::EXIT_ERROR
    )
}

/// The cache's pitfall, as the command pages say it.
pub const CACHE_HELP: &str = "The measurement is cached in `.slimfit/out/` and reused while HEAD and the contents of every file git does not ignore are unchanged. A change outside them is not seen: a tool's new version, an environment variable, a flaky test. `--fresh` measures again.";

#[derive(Default)]
pub struct Options {
    pub mutants: Mutants,
    pub fresh: bool,
    /// Write nothing in the repo: the commands write their reports to a
    /// directory of their own outside it, and the measurement is not cached.
    pub dry_run: bool,
    /// `test_seconds` must come from a test run of the caller's own
    /// priority, as a gate's `check` times it: a cached test run made inside
    /// `--mutants` (`untimed`) runs again, and its mutants are kept.
    pub timed: bool,
}

/// The last test run of a gate's priority (no `--mutants` in it), kept by
/// the contents it ran on, not by HEAD: a commit of the same files, or a
/// `--mutants` run's own test run since, leaves its time standing.
const TIMED: &str = "timed.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct Timed {
    content: String,
    measurement: Measurement,
}

/// `cached`, the measurement of the tree, or else `timed`'s, of the same
/// contents: a test run of it untimed takes `timed`'s instead, keeping its
/// mutants; `timed`'s own mutants, of a change taken from another commit
/// maybe, are not kept.
fn retimed(cached: Option<Measurement>, timed: Option<Measurement>, key: &str) -> Option<Measurement> {
    let Some(t) = timed else { return cached };
    let Some(mut c) = cached else {
        let suites = t.suites.into_iter().map(|s| SuiteMeasurement { mutants: vec![], mutated: None, ..s }).collect();
        return Some(Measurement { tree: key.into(), with_mutants: false, suites });
    };
    for (s, t) in c.suites.iter_mut().zip(t.suites).filter(|(s, _)| s.untimed) {
        *s = SuiteMeasurement { mutants: std::mem::take(&mut s.mutants), mutated: s.mutated.take(), ..t };
    }
    Some(c)
}

/// The cached measurement of this tree (its test run, maybe without the
/// mutants a caller wants), or `None` when measuring it would run its tests.
pub fn cached(repo: &Path, tree: &crate::landing::Tree, cfg: &Config) -> Option<Measurement> {
    let wt = &tree.wt;
    let same_suites = |m: &Measurement| m.suites.iter().map(|s| &s.suite).eq(cfg.suites.iter().map(|s| &s.name));
    let tested = std::fs::read_to_string(repo.join(OUT).join("measurement.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Measurement>(&text).ok())
        .filter(|m| m.tree == wt.key && same_suites(m));
    let timed_run = std::fs::read_to_string(repo.join(OUT).join(TIMED))
        .ok()
        .and_then(|text| serde_json::from_str::<Timed>(&text).ok())
        .filter(|t| t.content == wt.content && same_suites(&t.measurement));
    retimed(tested, timed_run.map(|t| t.measurement), &wt.key)
}

/// The measurement of the tree as it is now: from the cache when it was
/// taken of this exact tree (with as many mutants as are wanted, of a change
/// taken from the same commit). A cached measurement of the tree that lacks
/// those mutants keeps its test run: only the mutation tool runs. A test
/// run of the same contents at a gate's priority stands for the tree's,
/// and for one inside `--mutants` (`untimed`).
pub fn current(repo: &Path, tree: &crate::landing::Tree, cfg: &Config, opts: &Options) -> Result<Measurement> {
    let wt = &tree.wt;
    let key = wt.key.clone();
    let cache = repo.join(OUT).join("measurement.json");
    let picks = match &opts.mutants {
        Mutants::Picks(p) => p.as_slice(),
        Mutants::None => &[],
    };
    let with_mutants = !picks.is_empty();
    let timed_cache = repo.join(OUT).join(TIMED);
    let tested = if opts.fresh { None } else { cached(repo, tree, cfg) };
    let wanted: BTreeSet<&str> = picks.iter().map(|p| p.name.as_str()).collect();
    let fresh_mutants = |m: &Measurement| m.with_mutants && mutants::picked(m) == wanted;
    let timed = |m: &Measurement| !opts.timed || m.suites.iter().all(|s| !s.untimed);
    if let Some(m) = tested.as_ref().filter(|m| (!with_mutants || fresh_mutants(m)) && timed(m)) {
        let mut m = m.clone();
        // The same picks, run already: only why each was picked may be new.
        for p in m.suites.iter_mut().filter_map(|s| s.mutated.as_mut()).flat_map(|d| d.picks.iter_mut()) {
            if let Some(w) = picks.iter().find(|w| w.name == p.name) {
                p.why = w.why.clone();
            }
        }
        if with_mutants && !opts.dry_run {
            std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
        }
        return Ok(m);
    }
    // Only the test run is measured again: the cached mutants of this tree stand.
    let kept = tested.as_ref().is_some_and(|m| !with_mutants && m.with_mutants);
    let scratch = Scratch::new(repo, opts.dry_run)?;
    let mut tested = tested.map(|m| m.suites.into_iter()).into_iter().flatten();
    // Every pick is one the change's mutants list, before anything runs.
    let mut change = if with_mutants { Some(Change::pick(repo, cfg, tree, &scratch.0, picks)?) } else { None };
    let mut suites = Vec::new();
    for s in &cfg.suites {
        eprintln!(
            "slimfit: measuring {} ({}){}",
            s.name,
            s.root,
            if with_mutants { " with the picked mutants of the change" } else { "" }
        );
        let mutants = change.as_mut().map(|c| c.take(&s.name));
        let r = tree.suite(cfg, &s.name).and_then(|st| measure_suite(repo, &scratch.0, st, mutants, tested.next()));
        suites.push(r.map_err(|e| fault::within(&format!("suite {}", s.name), e)));
    }
    // Every suite is measured, so one that fails does not hide another's faults.
    let suites = fault::all(suites)?;
    let m = Measurement { tree: key, with_mutants: with_mutants || kept, suites };
    if !opts.dry_run {
        std::fs::write(&cache, serde_json::to_string_pretty(&m)?)?;
        if m.suites.iter().all(|s| !s.untimed) {
            let t = Timed { content: wt.content.clone(), measurement: m.clone() };
            std::fs::write(&timed_cache, serde_json::to_string(&t)?)?;
        }
    }
    Ok(m)
}

/// Where the suites' commands write: `.slimfit/out`, or in a dry run a
/// directory outside the repo, removed when the measurement is done.
struct Scratch(PathBuf, bool);

impl Scratch {
    fn new(repo: &Path, dry_run: bool) -> Result<Self> {
        let dir = if dry_run {
            std::env::temp_dir().join(format!("slimfit-dry-run-{}", std::process::id()))
        } else {
            repo.join(OUT)
        };
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir, dry_run))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn measure_suite(
    repo: &Path,
    scratch: &Path,
    st: &SuiteTree,
    mutants: Option<Picked>,
    tested: Option<SuiteMeasurement>,
) -> Result<SuiteMeasurement> {
    let (suite, resolver) = (&st.suite, &st.resolver);
    let run = Run::new(repo, suite, scratch.join(&suite.name));
    // The tests of this tree already ran: only the mutants are new. Without
    // mutants a cached run is passed here only to be timed again (`untimed`),
    // keeping its mutants.
    let (mutants, kept) = match (tested, mutants) {
        (Some(mut s), Some(picked)) => {
            std::fs::create_dir_all(&run.out)?;
            picked.run(&run, resolver, &mut s)?;
            return Ok(s);
        }
        (tested, mutants) => (mutants, tested),
    };
    let _ = std::fs::remove_dir_all(&run.out);
    std::fs::create_dir_all(&run.out)?;
    run.clear(&suite.tests_report)?;
    run.clear(&suite.coverage_report)?;
    // The build runs first, on its own: the test command's CPU time is then
    // the tests', not the compiler's (ADR 0010).
    if !suite.build_command.trim().is_empty() {
        run.shell(&suite.build_command, "build", &[])?;
    }
    // Where the test command records each test's CPU time, if it does.
    let test_cpu = run.out.join(crate::tools::TEST_CPU);
    std::fs::create_dir_all(&test_cpu)?;
    let started = Instant::now();
    let before = children_cpu();
    let (log, _) = run.shell(&suite.test_command, "test", &[])?;
    let tree_cpu = children_cpu() - before;
    let command_seconds = started.elapsed().as_secs_f64();
    let tests = run.read("tests_report", &suite.tests_report, &log, formats::read_tests)?;
    let coverage = run.read("coverage_report", &suite.coverage_report, &log, formats::read_coverage)?;
    let reach = run.reach()?;

    let timing = Timing { command_seconds, tree_cpu, test_cpu: read_test_cpu(&test_cpu) };
    let mut s = reduce(st, tests, coverage, reach, None, timing);
    if let Some(picked) = mutants {
        picked.run(&run, resolver, &mut s)?;
        // At the priority mutants run at, not a gate's: see `untimed`.
        s.untimed = true;
    } else if let Some(k) = kept {
        (s.mutants, s.mutated) = (k.mutants, k.mutated);
    }
    Ok(s)
}

/// The CPU time (user + sys) of this process's children that have ended
/// and been waited for, with theirs: a command's whole tree, as `sh` waits
/// for what it starts.
fn children_cpu() -> f64 {
    // SAFETY: an all-zero rusage is valid, and getrusage only writes it.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `u` is a valid rusage for the call to write.
    unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut u) };
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    secs(u.ru_utime) + secs(u.ru_stime)
}

/// Each test's CPU seconds, by id, from the files a test command wrote in
/// `dir` (see `tools::TEST_CPU`); empty when it wrote none. A test that ran
/// more than once (a retry) spent the sum.
fn read_test_cpu(dir: &Path) -> BTreeMap<String, f64> {
    let mut cpu = BTreeMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
        let mut lines = text.lines();
        let (Some(id), Some(children)) = (lines.next(), lines.nth(1)) else { continue };
        let Some(secs) = children.split_whitespace().map(times_seconds).sum::<Option<f64>>() else { continue };
        *cpu.entry(id.to_string()).or_insert(0.0) += secs;
    }
    cpu
}

/// One of `times`' figures, `1m2.345s`, in seconds.
fn times_seconds(t: &str) -> Option<f64> {
    let (m, s) = t.strip_suffix('s')?.split_once('m')?;
    Some(m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?)
}

/// A run of one suite's commands: each runs with `sh -c` in the suite's
/// root and writes its log in `out`, the suite's scratch directory, where
/// `$SLIMFIT_OUT` points; the suite's reports are found and read from there.
struct Run<'a> {
    suite: &'a Suite,
    root: PathBuf,
    out: PathBuf,
}

impl<'a> Run<'a> {
    /// The run of `suite` of `repo`, writing in `out`.
    fn new(repo: &Path, suite: &'a Suite, out: PathBuf) -> Self {
        Self { suite, root: repo.join(&suite.root), out }
    }

    /// The suite's root as a prefix of repository paths: empty at the root.
    fn prefix(&self) -> String {
        let root = &self.suite.root;
        if root == "." { String::new() } else { format!("{}/", root.trim_end_matches('/')) }
    }

    /// Runs `command`, with `env` added, and the suite's `test_args`. A failing exit status is not an
    /// error: failing tests still write reports. The log is kept; the flag
    /// says whether the command exited 0.
    fn shell(&self, command: &str, label: &str, env: &[(String, std::ffi::OsString)]) -> Result<(PathBuf, bool)> {
        if command.trim().is_empty() {
            return Err(fault(
                format!("no {label} command configured"),
                format!("set `{label}_command` in the suite's [[suite]] in slimfit.toml"),
            ));
        }
        let log = self.out.join(format!("{label}.log"));
        let file = std::fs::File::create(&log)?;
        let status = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.root)
            .env(crate::tools::OUT_ENV, &self.out)
            .env(crate::tools::TEST_ARGS_ENV, &self.suite.test_args)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdout(file.try_clone()?)
            .stderr(file)
            .status()
            .with_context(|| format!("running {label} command: {command}"))?;
        eprintln!("slimfit: {label} command exited {status}; log {}", log.display());
        Ok((log, status.success()))
    }

    /// Where `report` is: its path, `$SLIMFIT_OUT` replaced, under the root.
    fn pattern<F>(&self, report: &Report<F>) -> String {
        let pattern = report.path.replace(&format!("${}", crate::tools::OUT_ENV), &self.out.to_string_lossy());
        if Path::new(&pattern).is_absolute() {
            pattern
        } else {
            self.root.join(&pattern).to_string_lossy().into_owned()
        }
    }

    fn paths<F>(&self, report: &Report<F>) -> Result<Vec<PathBuf>> {
        expand(&self.pattern(report))
    }

    /// Removes the files of `report` an earlier run left: they would be
    /// read as this run's.
    fn clear<F>(&self, report: &Report<F>) -> Result<()> {
        for old in self.paths(report)? {
            std::fs::remove_file(old)?;
        }
        Ok(())
    }

    /// The texts of the report's files; `field` names it in slimfit.toml,
    /// and `log` is the log of the command that should have written it.
    fn texts<F>(&self, field: &str, report: &Report<F>, log: &Path) -> Result<Vec<String>> {
        let pattern = self.pattern(report);
        // An empty file is no report: a shell redirect creates one even when
        // the tool behind it is missing.
        let paths: Vec<PathBuf> =
            expand(&pattern)?.into_iter().filter(|p| p.metadata().is_ok_and(|m| m.len() > 0)).collect();
        if paths.is_empty() {
            return Err(fault(
                format!("no {field} at {pattern}; the command's log ends:\n{}", tail(log)),
                format!(
                    "fix the command's failure in its log {}; if it ran, make it write the report there, or set \
                     `{field}` path in slimfit.toml to where it writes",
                    log.display()
                ),
            ));
        }
        paths
            .into_iter()
            .map(|p| std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display())))
            .collect()
    }

    /// The report `field` names, read in its format by `read`.
    fn read<F: Copy + serde::Serialize, T>(
        &self,
        field: &str,
        report: &Report<F>,
        log: &Path,
        read: impl FnOnce(F, &[String]) -> Result<T>,
    ) -> Result<T> {
        read(report.format, &self.texts(field, report, log)?).map_err(|e| unreadable(field, report.format, e))
    }

    /// Which functions each test ran, when the suite's test run records it.
    /// A run that wrote none (a test command of its own, a runner of the
    /// repo's own) records nothing, and every mutant then runs every test.
    fn reach(&self) -> Result<Option<Vec<formats::TestReach>>> {
        let report = &self.suite.reach_report;
        if report.path.is_empty() {
            return Ok(None);
        }
        let paths: Vec<PathBuf> = self.paths(report)?;
        if paths.is_empty() {
            eprintln!(
                "slimfit: {}: the test run wrote no reach_report at {}: no test is known to run any function",
                self.suite.name,
                self.pattern(report)
            );
            return Ok(None);
        }
        let texts = paths.iter().map(std::fs::read_to_string).collect::<std::io::Result<Vec<_>>>()?;
        formats::read_reach(report.format, &texts).map(Some).map_err(|e| unreadable("reach_report", report.format, e))
    }
}

/// A report that is not in the format slimfit.toml names: its fault, with
/// the fix. Faults the reader names itself are kept as they are.
fn unreadable(field: &str, format: impl serde::Serialize, e: anyhow::Error) -> anyhow::Error {
    if e.is::<Faults>() {
        return e;
    }
    let format = serde_json::to_value(format).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
    fault(
        format!("its {field} is not {format}: {e:#}"),
        format!(
            "make the command write {format} there, or set `{field}` in slimfit.toml to the format the tool writes"
        ),
    )
}

/// The last 30 lines of a command's log.
fn tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

/// Paths matching an absolute glob, walking only below its literal prefix.
fn expand(pattern: &str) -> Result<Vec<PathBuf>> {
    if !pattern.contains(['*', '?', '[']) {
        return Ok(if Path::new(pattern).exists() { vec![pattern.into()] } else { vec![] });
    }
    let literal: String = pattern.split('/').take_while(|c| !c.contains(['*', '?', '['])).collect::<Vec<_>>().join("/");
    let matcher = globset::GlobBuilder::new(pattern).literal_separator(true).build()?.compile_matcher();
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::from(if literal.is_empty() { "/" } else { &literal })];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if name != ".git" && name != "node_modules" {
                    stack.push(p);
                }
            } else if matcher.is_match(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Maps a path as a tool reported it (absolute, module-qualified,
/// package-qualified) to a file of the suite, by the longest matching
/// path suffix.
pub struct Resolver {
    repo: String,
    files: Vec<String>,
}

impl Resolver {
    pub fn new(repo: &Path, totals: &Totals) -> Self {
        Self { repo: format!("{}/", repo.to_string_lossy()), files: totals.files.keys().cloned().collect() }
    }

    /// The text of `file`, a path in the repository.
    pub fn read(&self, file: &str) -> Option<String> {
        std::fs::read_to_string(format!("{}{file}", self.repo)).ok()
    }

    pub fn resolve(&self, raw: &str) -> Option<String> {
        let p = raw.strip_prefix("file://").unwrap_or(raw);
        let p = p.strip_prefix(&self.repo).unwrap_or(p).trim_start_matches("./");
        if p.starts_with('/') {
            return None; // outside the repository: std, dependencies
        }
        if self.files.iter().any(|f| f == p) {
            return Some(p.to_string());
        }
        let parts: Vec<&str> = p.split('/').collect();
        let mut best: Option<(usize, &String)> = None;
        for f in &self.files {
            let common = f.split('/').rev().zip(parts.iter().rev()).take_while(|(a, b)| a == *b).count();
            if common > 0 && best.is_none_or(|(n, _)| common > n) {
                best = Some((common, f));
            }
        }
        best.map(|(_, f)| f.clone())
    }
}

/// How long the test command took, and the CPU time its tests spent
/// (ADR 0010).
pub(crate) struct Timing {
    pub command_seconds: f64,
    /// The CPU time of the test command's whole process tree.
    pub tree_cpu: f64,
    /// Each test's own CPU time, by id, where the test command records it
    /// (the Rust preset's runner); empty where it does not.
    pub test_cpu: BTreeMap<String, f64>,
}

pub(crate) fn reduce(
    st: &SuiteTree,
    mut tests: Vec<TestCase>,
    coverage: Coverage,
    reach: Option<Vec<formats::TestReach>>,
    mutation: Option<Mutation>,
    timing: Timing,
) -> SuiteMeasurement {
    let (suite, totals, resolver) = (&st.suite, &st.totals, &st.resolver);
    // Each test's CPU time (ADR 0010): its own where the test command
    // recorded it; otherwise the command tree's, shared by wall time.
    if timing.test_cpu.is_empty() {
        let wall: f64 = tests.iter().map(|t| t.seconds).sum();
        let n = tests.len() as f64;
        for t in &mut tests {
            t.cpu = timing.tree_cpu * if wall > 0.0 { t.seconds / wall } else { 1.0 / n };
        }
    } else {
        let missing: Vec<&str> =
            tests.iter().filter(|t| !timing.test_cpu.contains_key(&t.id)).map(|t| t.id.as_str()).collect();
        if !missing.is_empty() {
            eprintln!(
                "slimfit: {}: no CPU time recorded for {} test(s), counted as 0: {missing:?}",
                suite.name,
                missing.len()
            );
        }
        for t in &mut tests {
            t.cpu = timing.test_cpu.get(&t.id).copied().unwrap_or(0.0);
        }
    }
    // Coverage of prod lines only: test files and inline test blocks are out.
    let mut lines: BTreeMap<String, BTreeMap<u32, u64>> = BTreeMap::new();
    for (raw, hits) in coverage.lines {
        let Some(file) = resolver.resolve(&raw) else { continue };
        let Some((Kind::Source, fl)) = totals.files.get(&file) else { continue };
        let entry = lines.entry(file).or_default();
        for (n, h) in hits {
            if fl.is_source_line(n) {
                let e = entry.entry(n).or_insert(0);
                *e = (*e).max(h);
            }
        }
    }
    let mut functions: BTreeMap<String, Vec<Function>> = BTreeMap::new();
    for f in coverage.functions {
        if let Some(file) = resolver.resolve(&f.file) {
            functions.entry(file.clone()).or_default().push(Function { file, ..f });
        }
    }
    let reach = reach.map(|r| reached(r, &functions, totals, resolver, suite.preset));
    let covered = lines.values().flat_map(|l| l.values()).filter(|h| **h > 0).count() as u64;
    let uncovered_total = lines.values().flat_map(|l| l.values()).filter(|h| **h == 0).count() as u64;
    let mut uncovered = Vec::new();
    for (file, hits) in &lines {
        let text = resolver.read(file);
        let fns = functions.get(file).map(Vec::as_slice).unwrap_or(&[]);
        uncovered.extend(spans(file, hits, fns, text.as_deref()));
    }
    uncovered.sort_by(|a, b| b.lines.cmp(&a.lines).then(a.file.cmp(&b.file)).then(a.start.cmp(&b.start)));

    let metrics = Metrics {
        prod_lines: totals.prod,
        test_lines: totals.test,
        tests: tests.len() as u64,
        failures: tests.iter().filter(|t| !t.passed).count() as u64,
        test_seconds: (tests.iter().map(|t| t.cpu).sum::<f64>() * 1000.0).round() / 1000.0,
        covered_lines: covered,
        uncovered_lines: uncovered_total,
    };
    let mut s = SuiteMeasurement {
        suite: suite.name.clone(),
        metrics,
        tests,
        uncovered,
        mutants: Vec::new(),
        reach,
        mutated: None,
        command_seconds: (timing.command_seconds * 1000.0).round() / 1000.0,
        untimed: false,
    };
    if let Some(m) = mutation {
        m.record(&mut s);
    }
    s
}

/// Which tests ran each source function, from what each test ran by symbol:
/// every function the coverage report names in a source file, outside
/// tests, keyed by its first line; and the source lines each ran, outside
/// tests. Each test's own function is located, and hashed, in a Rust suite
/// (a test is a `fn`, by its path), a Go one (a top-level `func Test…`
/// of its package) and a Python one (a `def` in its module).
fn reached(
    tests: Vec<formats::TestReach>,
    functions: &BTreeMap<String, Vec<Function>>,
    totals: &Totals,
    resolver: &Resolver,
    preset: Preset,
) -> Reach {
    let mut reach = Reach { lines: Some(Vec::new()), ..Reach::default() };
    let mut at: BTreeMap<&str, (&str, u32)> = BTreeMap::new();
    for (file, fs) in functions {
        let Some((Kind::Source, fl)) = totals.files.get(file) else { continue };
        for f in fs.iter().filter(|f| fl.is_source_line(f.line)) {
            reach.functions.entry(file.clone()).or_default().entry(f.line).or_default();
            at.insert(if f.symbol.is_empty() { &f.name } else { &f.symbol }, (file, f.line));
        }
    }
    let fns = if preset == Preset::Rust {
        rust_source::functions(totals.files.keys(), |f| resolver.read(f))
    } else {
        BTreeMap::new()
    };
    let source_of = |t: &formats::TestReach| match preset {
        Preset::Rust => {
            rust_source::test_function(&fns, &t.name).and_then(|(file, line)| test_source(file, line, resolver, preset))
        }
        Preset::Go => go_source::test_function(
            totals.files.keys(),
            |f| resolver.read(f),
            |p| resolver.resolve(p),
            &t.binary,
            &t.name,
        )
        .and_then(|(file, line)| test_source(&file, line, resolver, preset)),
        Preset::Python => python_source::test_function(totals.files.keys(), |f| resolver.read(f), &t.binary, &t.name)
            .and_then(|(file, line)| test_source(&file, line, resolver, preset)),
        _ => None,
    };
    let every: Vec<(String, u32)> =
        reach.functions.iter().flat_map(|(f, ls)| ls.keys().map(|l| (f.clone(), *l))).collect();
    for (i, t) in tests.into_iter().enumerate() {
        let i = i as u32;
        let mut ran = |file: &str, line: u32| {
            let tests = reach.functions.get_mut(file).and_then(|f| f.get_mut(&line)).expect("listed above");
            if tests.last() != Some(&i) {
                tests.push(i);
            }
        };
        match &t.functions {
            Some(names) => names.iter().filter_map(|n| at.get(n.as_str())).for_each(|(f, l)| ran(f, *l)),
            None => every.iter().for_each(|(f, l)| ran(f, *l)),
        }
        let mut lines: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for (raw, ls) in &t.lines {
            let Some(file) = resolver.resolve(raw) else { continue };
            let Some((Kind::Source, fl)) = totals.files.get(&file) else { continue };
            let entry = lines.entry(file).or_default();
            entry.extend(ls.iter().filter(|n| fl.is_source_line(**n)));
            entry.sort_unstable();
            entry.dedup();
        }
        lines.retain(|_, ls| !ls.is_empty());
        reach.lines.as_mut().expect("set above").push(lines);
        let source = source_of(&t);
        reach.tests.push(ReachTest { binary: t.binary, name: t.name, source });
    }
    reach
}

/// Where the test function at `file:line` is, the hash of its text from its
/// first line to where it ends (as the language's reader cuts it), and its
/// shape.
fn test_source(file: &str, line: u32, resolver: &Resolver, preset: Preset) -> Option<TestSource> {
    let cut = match preset {
        Preset::Go => go_source::func_text,
        Preset::Python => python_source::def_text,
        _ => rust_source::item_text,
    };
    let text = cut(&resolver.read(file)?, line)?;
    let shape = crate::test_shape::shape(preset, &text);
    Some(TestSource { file: file.into(), line, hash: crate::test_shape::fnv(&text), shape })
}

/// A hash of the text of the function line `line` (1-based) of `text` is
/// in, as the language's reader cuts it (the innermost `fn`, top-level
/// `func` or `def` whose text holds the line): what a kept mutant's verdict
/// holds to. The whole file's text where the preset has no reader, or the
/// line is in no function.
pub fn function_hash(preset: Preset, text: &str, line: u32) -> String {
    let lines: Vec<&str> = text.lines().collect();
    type Cut = fn(&str, u32) -> Option<String>;
    let (starts, cut): (fn(&str) -> bool, Cut) = match preset {
        Preset::Rust => (|l| rust_source::fn_name(l).is_some(), rust_source::item_text),
        Preset::Go => (|l| l.starts_with("func "), go_source::func_text),
        Preset::Python => {
            (|l| l.trim_start().trim_start_matches("async ").starts_with("def "), python_source::def_text)
        }
        _ => return crate::test_shape::fnv(text),
    };
    let upto = (line as usize).min(lines.len());
    let found = (0..upto).rev().filter(|&i| starts(lines[i])).find_map(|i| {
        let t = cut(text, i as u32 + 1)?;
        (i + t.lines().count() >= line as usize).then_some(t)
    });
    crate::test_shape::fnv(found.as_deref().unwrap_or(text))
}

/// Runs of zero-hit lines, not interrupted by a hit line (lines the report
/// does not mention, like blanks, don't break a run). Each span is named
/// after the function it starts in, when the report names functions, and
/// hashed by the text of its lines in `text`, the file's.
fn spans(file: &str, hits: &BTreeMap<u32, u64>, functions: &[Function], text: Option<&str>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut open: Option<Span> = None;
    for (&n, &h) in hits {
        match (&mut open, h) {
            (Some(s), 0) => {
                s.end = n;
                s.lines += 1;
            }
            (None, 0) => {
                open = Some(Span { file: file.into(), start: n, end: n, lines: 1, function: None, hash: None })
            }
            (_, _) => out.extend(open.take()),
        }
    }
    out.extend(open);
    for s in &mut out {
        s.function = functions.iter().filter(|f| f.line <= s.start).max_by_key(|f| f.line).map(|f| f.name.clone());
        s.hash = text.and_then(|t| {
            let lines: Vec<&str> = t.lines().collect();
            Some(crate::test_shape::fnv(&lines.get(s.start as usize - 1..s.end as usize)?.join("\n")))
        });
    }
    out
}

#[cfg(test)]
pub(crate) mod t {
    use super::*;
    use crate::config::MutantTests;
    use crate::ratchet::{Baseline, Bound};

    /// The working tree of `repo` as it is now.
    pub(crate) fn tree(repo: &Path) -> crate::landing::Tree {
        crate::landing::Tree::read(&crate::git::open(repo).unwrap()).unwrap()
    }

    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.email=t@slimfit", "-c", "user.name=t", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// The Rust sample, its tools replaced by copies of their real reports;
    /// the mutation "tool" also records the arguments it was given (a
    /// listing's in `list-args`).
    pub(crate) fn sample_repo() -> (tempfile::TempDir, Config) {
        let reports = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/rust/reports");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for f in ["Cargo.toml", "src/lib.rs"] {
            std::fs::copy(reports.join("..").join(f), dir.path().join(f)).unwrap();
        }
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "sample"]);
        let r = reports.to_string_lossy();
        let cfg: Config = toml::from_str(&format!(
            r#"
            [[suite]]
            name = "rust"
            preset = "rust"
            test_command = "cp {r}/junit.xml {r}/lcov.info \"$SLIMFIT_OUT\" && printf 'calc::tests::slow\\n0m0.001s 0m0.001s\\n0m0.600s 0m0.025s\\n' > \"$SLIMFIT_OUT/test-cpu/slow\""
            tests_report = {{ format = "junit", path = "$SLIMFIT_OUT/junit.xml" }}
            mutants_command = "f() {{ case \" $* \" in *\" --list \"*) echo \"$@\" > \"$SLIMFIT_OUT/list-args\"; cat {r}/mutants.list.json;; *) echo \"$@\" > \"$SLIMFIT_OUT/args\"; mkdir -p \"$SLIMFIT_OUT/mutants.out\"; cp {r}/outcomes.json \"$SLIMFIT_OUT/mutants.out\";; esac; }}; f"
            "#
        ))
        .unwrap();
        (dir, cfg)
    }

    /// Picks, each with a reason.
    pub(crate) fn picks(names: &[&str]) -> Vec<Pick> {
        names
            .iter()
            .map(|n| Pick { name: n.to_string(), why: format!("why {n}"), outcome: None, first_outcome: None })
            .collect()
    }

    /// The measurement with `names` picked, and the arguments the mutation
    /// "tool" got (`None`: it did not run). Not `fresh`: a cached test run of
    /// the same tree is kept, so only the mutants run.
    fn run(repo: &Path, cfg: &Config, names: &[&str]) -> Result<(Measurement, Option<String>)> {
        let args = repo.join(OUT).join("rust/args");
        let _ = std::fs::remove_file(&args);
        let mutants = Mutants::Picks(picks(names));
        let m = current(repo, &tree(repo), cfg, &Options { mutants, fresh: false, dry_run: false, timed: false })?;
        Ok((m, std::fs::read_to_string(args).ok()))
    }

    fn mutated(m: &Measurement) -> Mutated {
        m.suites[0].mutated.clone().unwrap()
    }

    const KILLED: &str = "src/lib.rs:12:9: replace >= with < in is_adult";
    const MISSED: &str = "src/lib.rs:12:5: replace is_adult -> bool with true";

    /// One fixture through picking. The candidates are what the tool lists
    /// of the landing's diff (what HEAD adds since its merge base with the
    /// target, untracked files included); with no target, or a tool that
    /// cannot run a mutant by name, `--mutants` refuses. A pick no candidate
    /// is named is refused before anything runs; the tool runs only the
    /// picks, each by its anchored name, and each pick gets its outcome.
    /// The same picks of the same tree do not run again.
    ///
    /// `--mutants` with no cached test run of the tree runs the tests at its
    /// own (background) priority: that run is `untimed`, and holds no
    /// `test_seconds`. A plain, timed measurement of the same tree runs the
    /// tests again and keeps the mutants; then the time is held.
    #[test]
    fn only_the_picked_mutants_of_the_landing_run() {
        let (dir, mut cfg) = sample_repo();
        // Every test on every mutant: which tests run is not this test's.
        cfg.suites[0].overrides.mutants_tests = Some(MutantTests::All);
        let repo = dir.path();
        // Each test run is counted.
        let counter = tempfile::tempdir().unwrap();
        let runs = counter.path().join("test-runs");
        let test = format!("echo run >> '{}'; {}", runs.display(), cfg.suites[0].resolve().test_command);
        cfg.suites[0].overrides.test_command = Some(test);
        // A build of its own, before the tests: its CPU time is not theirs.
        let build = format!("echo build >> '{}'; i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done", runs.display());
        cfg.suites[0].overrides.build_command = Some(build);
        let ran = || std::fs::read_to_string(&runs).map_or(0, |t| t.lines().filter(|l| *l == "run").count());
        let measure = |cfg: &Config, timed| {
            current(repo, &tree(repo), cfg, &Options { mutants: Mutants::None, fresh: false, dry_run: false, timed })
                .unwrap()
        };
        let refused = |cfg: &Config, names: &[&str]| {
            let faults = crate::fault::of(run(repo, cfg, names).unwrap_err());
            assert!(!repo.join(OUT).join("rust/args").exists(), "a mutant ran");
            faults.into_iter().map(|f| f.fault).collect::<Vec<_>>().join("\n")
        };
        // No target named and no origin/HEAD: refused, no mutant run.
        assert!(refused(&cfg, &[KILLED]).contains("no branch to take the change from"));
        assert!(candidates(repo, &cfg, &tree(repo)).is_err());
        git(repo, &["branch", "base"]);
        cfg.check.target = Some("base".into());
        // A tool that cannot run a mutant by name: refused up front.
        let mut unnamed = cfg.clone();
        unnamed.suites[0].overrides.mutants_pick_flag = Some(String::new());
        assert!(refused(&unnamed, &[KILLED]).contains("cannot list a change's mutants and run them by name"));
        // No source line added: nothing listed, the tool not run.
        assert_eq!(candidates(repo, &cfg, &tree(repo)).unwrap()["rust"], []);
        assert!(!repo.join(OUT).join("rust/list-args").exists());

        let text = std::fs::read_to_string(repo.join("src/lib.rs")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), text.replacen("    age >= 18\n", "    age >= 18 // x\n", 1)).unwrap();
        std::fs::write(repo.join("src/new.rs"), "pub fn n() -> u8 {\n    1\n}\n").unwrap();
        let listed = candidates(repo, &cfg, &tree(repo)).unwrap();
        let first = &listed["rust"][0];
        assert_eq!((listed["rust"].len(), first.file.as_str(), first.line), (26, "src/lib.rs", 2));
        assert_eq!((first.function.as_str(), first.replacement.as_str()), ("add", "0"));
        let diff = repo.join(OUT).join("rust/change.diff");
        let list = std::fs::read_to_string(repo.join(OUT).join("rust/list-args")).unwrap();
        assert_eq!(list, format!("--in-diff {} --list --json\n", diff.display()));
        let patch = std::fs::read_to_string(&diff).unwrap();
        assert!(patch.contains("+    age >= 18 // x") && patch.contains("+++ b/src/new.rs"), "{patch}");

        // Not listed, or picked twice: refused, nothing run.
        let refusal = refused(&cfg, &[KILLED, "src/lib.rs:99:1: replace nothing", KILLED]);
        assert!(refusal.contains("no mutant of the change is named `src/lib.rs:99:1: replace nothing`"), "{refusal}");
        assert!(refusal.contains("is picked twice"), "{refusal}");

        // The tool runs the picks alone, each by its anchored name; of what it
        // reports, only the picks are the change's.
        let (m, args) = run(repo, &cfg, &[KILLED, MISSED]).unwrap();
        let args = args.unwrap();
        assert!(
            args.starts_with(&format!(
                "--in-diff {} -F ^src/lib\\.rs:12:9: replace >= with < in is_adult$ -F ",
                diff.display()
            )),
            "{args}"
        );
        assert!(args.ends_with("-F ^src/lib\\.rs:12:5: replace is_adult \\-> bool with true$\n"), "{args}");
        let outcomes: Vec<_> = mutated(&m).picks.iter().map(|p| (p.name.clone(), p.why.clone(), p.outcome)).collect();
        assert_eq!(
            outcomes,
            [
                (KILLED.to_string(), format!("why {KILLED}"), Some(MutantStatus::Killed)),
                (MISSED.to_string(), format!("why {MISSED}"), Some(MutantStatus::Survived))
            ]
        );
        assert_eq!(mutated(&m).change, MutantCounts { killed: 1, survived: 1, unviable: 0, equivalent: 0 });
        assert_eq!(m.suites[0].mutants.len(), 2);
        let v = crate::ratchet::check(&Baseline::default(), &m, &Default::default(), None);
        assert_eq!(v.violations.iter().map(|v| v.metric).collect::<Vec<_>>(), [Bound::SurvivedMutants]);

        // No cached test run of the tree: `--mutants` ran the tests, untimed.
        assert!(m.suites[0].untimed && ran() == 1);
        // The sample's test command records the slow test's CPU time.
        assert_eq!(m.suites[0].metrics.test_seconds, 0.625);
        assert_eq!(std::fs::read_to_string(&runs).unwrap(), "build\nrun\n");
        // A baseline far under it: not answered, and the ratchet keeps it.
        let held = crate::config::CheckConfig::default();
        let tight = Metrics { test_seconds: 0.001, ..m.suites[0].metrics.clone() };
        let base = Baseline { suites: [("rust".to_string(), tight)].into(), ..Baseline::default() };
        assert!(crate::ratchet::check(&base, &m, &held, None).test_seconds.is_empty());
        let loosen = [Bound::SurvivedMutants];
        let next = crate::ratchet::ratchet(&base, &m, &held, &loosen, Some("t")).unwrap();
        assert_eq!(next.suites["rust"].test_seconds, 0.001);
        // Measurements that take no time from it reuse it as it is.
        assert_eq!((measure(&cfg, false), ran()), (m.clone(), 1));
        // A plain check times the tests again, and keeps the mutants.
        let timed = measure(&cfg, true);
        assert_eq!(ran(), 2);
        assert!(!timed.suites[0].untimed && timed.with_mutants);
        assert_eq!((&timed.suites[0].mutants, &timed.suites[0].mutated), (&m.suites[0].mutants, &m.suites[0].mutated));
        let v = crate::ratchet::check(&base, &timed, &held, None);
        let failed: Vec<_> = v.violations.iter().map(|v| v.metric).collect();
        assert!(failed == [Bound::SurvivedMutants] && v.test_seconds[0].current == 0.625, "{v:?}");
        // Cached now, timed: nothing runs again.
        assert_eq!((measure(&cfg, true), ran()), (timed, 2));

        // The same picks again: nothing runs (the tests included), a new
        // reason is kept.
        let mutants = Mutants::Picks(vec![
            Pick { why: "new".into(), ..picks(&[KILLED])[0].clone() },
            picks(&[MISSED])[0].clone(),
        ]);
        let _ = std::fs::remove_file(repo.join(OUT).join("rust/args"));
        let again =
            current(repo, &tree(repo), &cfg, &Options { mutants, fresh: false, dry_run: false, timed: false }).unwrap();
        assert!(!repo.join(OUT).join("rust/args").exists() && ran() == 2);
        assert_eq!(mutated(&again).picks[0].why, "new");
        // Other picks of the same tree run.
        assert!(run(repo, &cfg, &[KILLED]).unwrap().1.is_some());

        // The suite moved to `sub/`, its report found by a glob: the diff is
        // relative to its root, the candidates' files to the repository.
        std::fs::create_dir(repo.join("sub")).unwrap();
        for f in ["Cargo.toml", "src"] {
            std::fs::rename(repo.join(f), repo.join("sub").join(f)).unwrap();
        }
        cfg.suites[0].root = "sub".into();
        let glob = "$SLIMFIT_OUT/mutants.out/outcome*.json";
        cfg.suites[0].overrides.mutants_report =
            Some(crate::config::Report { format: crate::formats::MutantsFormat::CargoMutants, path: glob.into() });
        let (m, _) = run(repo, &cfg, &[KILLED]).unwrap();
        assert_eq!(mutated(&m).picks[0].outcome, Some(MutantStatus::Killed));
        assert_eq!(candidates(repo, &cfg, &tree(repo)).unwrap()["rust"][0].file, "sub/src/lib.rs");
        let patch = std::fs::read_to_string(repo.join(OUT).join("rust/change.diff")).unwrap();
        assert!(patch.contains("+++ b/src/lib.rs"), "{patch}");
    }

    /// Each test's CPU time is its own where the test command recorded it
    /// (0 for a test it has no record of); otherwise the command tree's,
    /// shared by the tests' wall times. `test_seconds` is their sum.
    #[test]
    fn each_tests_cpu_time_is_its_own_or_its_share_of_the_trees() {
        let tree = crate::landing::Tree::at(Path::new("/r"), vec![]);
        let cfg = Config::one("rust", crate::config::Preset::Rust);
        let case = |id: &str, seconds| TestCase { id: id.into(), seconds, passed: true, cpu: 0.0 };
        let measure = |tests, test_cpu| {
            let timing = Timing { command_seconds: 9.87654, tree_cpu: 12.0, test_cpu };
            reduce(tree.suite(&cfg, "rust").unwrap(), tests, Coverage::default(), None, None, timing)
        };
        let own = measure(vec![case("a", 3.0), case("b", 1.0)], [("a".to_string(), 0.25)].into());
        assert_eq!(own.tests.iter().map(|t| t.cpu).collect::<Vec<_>>(), [0.25, 0.0]);
        assert_eq!((own.metrics.test_seconds, own.command_seconds), (0.25, 9.877));
        let tree = measure(vec![case("a", 3.0), case("b", 1.0)], BTreeMap::new());
        assert_eq!(tree.tests.iter().map(|t| t.cpu).collect::<Vec<_>>(), [9.0, 3.0]);
        let untimed = measure(vec![case("a", 0.0), case("b", 0.0)], BTreeMap::new());
        assert_eq!(untimed.tests.iter().map(|t| t.cpu).collect::<Vec<_>>(), [6.0, 6.0]);
    }

    /// A record per test process: the id, then `times`; a retried test
    /// spent the sum, and a record without the children's line is skipped.
    #[test]
    fn test_cpu_records_read_times_children_line() {
        let dir = tempfile::tempdir().unwrap();
        let write = |f: &str, text: &str| std::fs::write(dir.path().join(f), text).unwrap();
        write("1", "b::t\n0m0.001s 0m0.002s\n0m1.5s 0m0.25s\n");
        write("2", "b::t\n0m0.001s 0m0.002s\n1m0.000s 0m0.000s\n");
        write("3", "b::u\n0m0.001s 0m0.002s\n");
        write("4", "b::v\n0m0.001s 0m0.002s\n0m1.5 0m0.25s\n");
        assert_eq!(read_test_cpu(dir.path()), [("b::t".to_string(), 61.75)].into());
        assert!(read_test_cpu(&dir.path().join("none")).is_empty());
    }

    /// Every function of a source file's coverage is keyed by its first
    /// line, with the tests that ran it by symbol; a test whose profile was
    /// unreadable ran them all; test code is left out, of functions and of
    /// lines. A test's own function is found by its path and hashed.
    #[test]
    fn reach_keys_each_source_function_by_its_first_line() {
        let repo = tempfile::tempdir().unwrap();
        let text = "fn a() {}\nfn b() {}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        a();\n    }\n}\n";
        std::fs::create_dir(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/a.rs"), text).unwrap();
        let tree = crate::landing::Tree::at(repo.path(), vec!["src/a.rs".into()]);
        let st = tree.suite(&Config::one("rust", crate::config::Preset::Rust), "rust").unwrap();
        let at = format!("{}/src/a.rs", repo.path().display());
        let f = |name: &str, line, symbol: &str| Function {
            file: at.clone(),
            name: name.into(),
            line,
            hits: 0,
            symbol: symbol.into(),
        };
        // Two symbols of one line: a generic function's instantiations.
        let t_symbol = "_ZN4calc5tests1t17h0123456789abcdefE";
        let functions = vec![f("a", 1, "_Ra"), f("a2", 1, "_Ra2"), f("b", 2, "_Rb"), f("t", 5, t_symbol)];
        let coverage = Coverage { lines: Default::default(), functions };
        let t = |name: &str, ran: Option<&[&str]>, lines: &[u32]| formats::TestReach {
            binary: "calc".into(),
            name: name.into(),
            functions: ran.map(|r| r.iter().map(|s| s.to_string()).collect()),
            lines: [(at.clone(), lines.iter().copied().collect())].into(),
        };
        let reach = vec![
            t("tests::t", Some(&["_Ra", "_Ra2", t_symbol]), &[1, 5, 6, 7]),
            t("y", None, &[]),
            t("z", Some(&["_Rgone"]), &[2]),
        ];
        let timing = Timing { command_seconds: 0.0, tree_cpu: 0.0, test_cpu: BTreeMap::new() };
        let m = reduce(st, vec![], coverage, Some(reach), None, timing);
        let reach = m.reach.unwrap();
        assert_eq!(reach.functions["src/a.rs"], [(1, vec![0, 1]), (2, vec![1])].into());
        assert_eq!(reach.tests.iter().map(ReachTest::id).collect::<Vec<_>>(), ["calc::tests::t", "calc::y", "calc::z"]);
        let lines = |ls: &[u32]| BTreeMap::from([("src/a.rs".to_string(), ls.to_vec())]);
        assert_eq!(reach.lines.unwrap(), [lines(&[1]), BTreeMap::new(), lines(&[2])]);
        let source = reach.tests[0].source.clone().unwrap();
        assert_eq!((source.file.as_str(), source.line), ("src/a.rs", 5));
        assert_eq!(reach.tests[1].source, None);
        // The hash is of the function's text: an edit inside it changes it, one outside does not.
        let hash = |text: &str| {
            std::fs::write(repo.path().join("src/a.rs"), text).unwrap();
            test_source("src/a.rs", 5, &st.resolver, Preset::Rust).map(|s| s.hash)
        };
        assert_eq!(hash(&text.replace("fn b() {}", "fn c() {}")), Some(source.hash.clone()));
        assert_ne!(hash(&text.replace("a();", "b();")), Some(source.hash.clone()));
        assert!(hash("fn a() {}\n").is_none(), "no line 5");
        // One rule ends it, whatever the shape: a test rustfmt did not shape
        // is hashed too, so `keep` has its text to hold a verdict to.
        let unshaped = text.replace("    }\n}", "  }\n}");
        assert!(hash(&unshaped).is_some_and(|h| h != source.hash), "closing brace not under its fn");
        assert!(hash("\n\n\n\n    fn t()\n{ a();\n        if x { y() } }\n").is_some(), "brace on its own line");
        assert!(hash("\n\n\n\n    fn t() {\n        a();\n").is_none(), "no closing brace");
        assert!(hash("\n\n\n\n    fn t() { a(); }\n").is_some(), "one line");
    }

    /// No reach_report configured, or none written: nothing is recorded,
    /// and every mutant runs every test. One that is not the format
    /// configured is refused, with its fix.
    #[test]
    fn a_reach_report_is_read_when_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut suite = crate::config::preset(crate::config::Preset::Rust);
        let read = |s: &Suite| Run::new(dir.path(), s, dir.path().into()).reach();
        assert!(read(&suite).unwrap().is_none());
        std::fs::write(dir.path().join(crate::tools::REACH), "test calc\tx\n").unwrap();
        assert_eq!(read(&suite).unwrap().unwrap().len(), 1);
        std::fs::write(dir.path().join(crate::tools::REACH), "garbled\n").unwrap();
        let f = crate::fault::of(read(&suite).unwrap_err());
        assert!(f[0].fault.starts_with("its reach_report is not lcov"), "{f:?}");
        suite.reach_report.path = String::new();
        assert!(read(&suite).unwrap().is_none());
    }

    #[test]
    fn resolver_matches_longest_suffix() {
        let mut totals = Totals::default();
        for f in ["svc/pkg/a.go", "svc/other/a.go", "app/src/main/kotlin/com/x/A.kt"] {
            totals.files.insert(f.into(), (Kind::Source, Default::default()));
        }
        let r = Resolver::new(Path::new("/r"), &totals);
        assert_eq!(r.resolve("example.com/m/pkg/a.go").as_deref(), Some("svc/pkg/a.go"));
        assert_eq!(r.resolve("/r/svc/other/a.go").as_deref(), Some("svc/other/a.go"));
        assert_eq!(r.resolve("com/x/A.kt").as_deref(), Some("app/src/main/kotlin/com/x/A.kt"));
        assert_eq!(r.resolve("/rustc/library/core/src/x.rs"), None);
    }

    #[test]
    fn spans_join_zero_runs_and_name_functions() {
        let hits: BTreeMap<u32, u64> = [(1, 1), (3, 0), (4, 0), (6, 0), (7, 2), (9, 0)].into();
        let f = vec![Function { file: "a".into(), name: "g".into(), line: 2, hits: 0, symbol: String::new() }];
        let text = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let s = spans("a", &hits, &f, Some(text));
        assert_eq!(s.iter().map(|s| (s.start, s.end, s.lines)).collect::<Vec<_>>(), vec![(3, 6, 3), (9, 9, 1)]);
        assert_eq!(s[0].function.as_deref(), Some("g"));
        // Hashed by its own lines' text: an edit in them changes it, one outside does not.
        let hash = |text: &str| spans("a", &hits, &f, Some(text))[0].hash.clone();
        assert_eq!(hash(text), Some(crate::test_shape::fnv("3\n4\n5\n6")));
        assert_eq!(hash(&text.replace('8', "x")), hash(text));
        assert_ne!(hash(&text.replace('5', "x")), hash(text));
        assert_eq!((hash("1\n2\n3\n"), spans("a", &hits, &f, None)[0].hash.clone()), (None, None), "no such lines");
    }
    /// A mutant's function is the innermost one whose text holds its line,
    /// as each language's reader cuts it: an edit in it changes the hash,
    /// one outside it does not; past any function, or in a language with
    /// no reader, it is the whole file.
    #[test]
    fn a_mutant_is_hashed_by_its_function() {
        let fh = |preset, text: &str, line| function_hash(preset, text, line);
        let rust = "fn f() {\n    fn g() { 1 }\n    x < y\n}\nfn h() {}\n";
        assert_eq!(fh(Preset::Rust, rust, 3), crate::test_shape::fnv("fn f() {\n    fn g() { 1 }\n    x < y\n}"));
        assert_eq!(fh(Preset::Rust, rust, 2), crate::test_shape::fnv("    fn g() { 1 }"));
        assert_eq!(fh(Preset::Rust, &rust.replace("h() {}", "h() { 2 }"), 3), fh(Preset::Rust, rust, 3));
        assert_eq!(
            fh(Preset::Rust, "use x;\nconst A: u8 = 1;\n", 2),
            crate::test_shape::fnv("use x;\nconst A: u8 = 1;\n")
        );
        let go = "package p\n\nfunc F() {\n\tif x < y {\n\t}\n}\n";
        assert_eq!(fh(Preset::Go, go, 4), crate::test_shape::fnv("func F() {\n\tif x < y {\n\t}\n}"));
        let py = "class A:\n    async def f(self):\n        return x < y\n\ndef g():\n    pass\n";
        assert_eq!(fh(Preset::Python, py, 3), crate::test_shape::fnv("    async def f(self):\n        return x < y"));
        assert_eq!(fh(Preset::Typescript, go, 4), crate::test_shape::fnv(go));
    }
}

#[cfg(test)]
mod faults {
    use super::*;
    use t::tree;

    /// One suite's fault does not hide another's: each failing suite is
    /// named with its fault and its fix, and the healthy one is measured.
    #[test]
    fn every_suite_that_cannot_be_measured_names_its_fault_and_fix() {
        let (dir, mut cfg) = t::sample_repo();
        let bad: Config = toml::from_str(
            r#"
            [[suite]]
            name = "nocmd"
            preset = "kotlin"
            test_command = ""
            [[suite]]
            name = "noreport"
            preset = "rust"
            test_command = "echo built"
            [[suite]]
            name = "garbled"
            preset = "rust"
            test_command = "echo '<testsuites' > \"$SLIMFIT_OUT/junit.xml\""
            tests_report = { format = "junit", path = "$SLIMFIT_OUT/junit.xml" }
            "#,
        )
        .unwrap();
        cfg.suites.extend(bad.suites);
        let opts = Options { mutants: Mutants::None, fresh: true, dry_run: false, timed: false };
        let faults = crate::fault::of(current(dir.path(), &tree(dir.path()), &cfg, &opts).unwrap_err());
        let got: Vec<(&str, &str)> =
            faults.iter().map(|f| (f.fault.split(':').next().unwrap(), f.fix.as_str())).collect();
        assert_eq!(got.len(), 3, "{faults:?}");
        assert_eq!(got[0], ("suite nocmd", "set `test_command` in the suite's [[suite]] in slimfit.toml"));
        assert_eq!(got[1].0, "suite noreport");
        assert!(faults[1].fault.contains("no tests_report at") && faults[1].fault.contains("built"), "{faults:?}");
        assert!(got[1].1.ends_with("or set `tests_report` path in slimfit.toml to where it writes"), "{faults:?}");
        assert_eq!(got[2].0, "suite garbled");
        assert!(faults[2].fault.contains("its tests_report is not junit"), "{faults:?}");
        assert!(got[2].1.starts_with("make the command write junit there, or set `tests_report`"), "{faults:?}");
    }
}
